//! Terminal-native Speech-to-Speech (S2S) interactive loop client command.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::AsyncBufReadExt;
use tracing::{info, warn};

use crate::audio;
use crate::config::{PipelineConfig, RealtimeProviderConfig};
use crate::realtime::{
    Modalities, RealtimeClient, RealtimeEvent, RealtimeProtocol, SessionConfig, TurnDetection,
    lookup_realtime_factory, realtime_config, realtime_factories,
};

/// Interactive real-time S2S voice loop client.
pub async fn run_interactive(
    provider: String,
    silence_ms: u64,
    echo_gating: bool,
    cfg: &PipelineConfig,
) -> Result<(), anyhow::Error> {
    // 1. The provider named on the command line, by name or alias.
    let factory = lookup_realtime_factory(&provider.to_lowercase()).ok_or_else(|| {
        let known: Vec<&str> = realtime_factories().iter().map(|f| f.name()).collect();
        anyhow::anyhow!(
            "unknown speech-to-speech provider \"{provider}\"; this build has {known:?}"
        )
    })?;

    // 2. The config's [realtime_provider] section, or a bare one.
    let mut rt_cfg = cfg
        .realtime_provider
        .clone()
        .unwrap_or_else(|| RealtimeProviderConfig {
            provider: factory.name().to_string(),
            model: None,
            voice: None,
            vad: Some("server_vad".to_string()),
            instructions: None,
            knowledge_base_path: None,
            api_key_env: None,
            options: std::collections::HashMap::new(),
            client_overrides: crate::transport::serve::client_overrides::default_allowlist(),
            endpoint_observer: None,
        });
    rt_cfg.provider = factory.name().to_string();

    // Missing credentials and settings are reported before connecting.
    let provider_cfg = realtime_config(&rt_cfg, factory.default_api_key_env());
    let errors = factory.validate(&provider_cfg);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("\n❌ {}: {}", e.field, e.message);
        }
        return Err(anyhow::anyhow!(
            "the [realtime_provider] settings for {} are not usable",
            factory.name()
        ));
    }

    println!(
        "\n⚡ Initializing voice-pipes S2S client ({})...",
        rt_cfg.provider
    );

    // 3. Connect S2S WS client and run main loop
    let proto = factory.build(&provider_cfg, &voice_pipes_sdk::BuildContext::new())?;
    let emit_rate = rt_cfg
        .output_emit_rate()
        .map_err(|e| anyhow::anyhow!("{}: {}", e.field, e.message))?;
    let mut client = RealtimeClient::connect(proto, emit_rate).await?;
    run_client_loop(&mut client, silence_ms, echo_gating, &rt_cfg).await?;

    Ok(())
}

/// Generic runtime loop managing terminal io, mic capture, and WS frame processing.
async fn run_client_loop<P: RealtimeProtocol>(
    client: &mut RealtimeClient<P>,
    silence_ms: u64,
    echo_gating: bool,
    rt_cfg: &RealtimeProviderConfig,
) -> Result<(), anyhow::Error> {
    // Compile and apply SessionConfig
    let session_cfg = SessionConfig::new(rt_cfg.instructions.clone().unwrap_or_else(|| {
        "You are a helpful assistant. Keep your responses short and natural. You are talking via a real-time voice call.".to_string()
    }))
    .with_voice(rt_cfg.voice.clone())
    .with_modalities(Modalities::AudioAndText)
    .with_turn_detection(TurnDetection::ServerVad {
        silence_ms: Some(silence_ms as u32),
        threshold: None,
        prefix_padding_ms: None,
    });
    client.apply_session_config(session_cfg).await?;

    println!("🌐 Connected/configured S2S session.");

    // Open persistent CPAL output stream for gapless playback
    let (playback_ring, playback_stream, device_rate) = audio::playback::open_playback_stream();
    if let Some(ref stream) = playback_stream {
        use cpal::traits::StreamTrait;
        let _ = stream.play();
    }

    // Capture microphone input
    let stop_mic = Arc::new(AtomicBool::new(false));
    let stop_mic_c = std::sync::Arc::clone(&stop_mic);
    let (mic_tx, mut mic_rx) = tokio::sync::mpsc::channel::<crate::pipeline::AudioChunk>(256);

    info!("launching raw mic capture thread");
    std::thread::spawn(move || {
        // Chunk sizes of 50ms are standard for S2S
        if let Err(e) = audio::io::mic_capture_raw(mic_tx, stop_mic_c, 50) {
            warn!("raw mic capture error: {e}");
        }
    });

    println!("\n🎤 Microphone capturing active... (You can speak!)");
    println!(
        "⌨️  Type a message and press Enter, or press Enter on an empty line while the model is speaking to interrupt."
    );
    println!(
        "👉 Standard console commands: /mute to mute, /gate to toggle echo-gating, /exit or /quit to close."
    );
    if echo_gating {
        println!(
            "🔒 Echo-gating is ENABLED (mic is muted while model speaks to avoid speaker feedback loops)."
        );
    } else {
        println!(
            "🔓 Echo-gating is DISABLED (barge-in freely! Recommended for headphone use only)."
        );
    }
    println!("\n--- CHAT START ---\n");

    let muted = Arc::new(AtomicBool::new(false));
    let gating_enabled = Arc::new(AtomicBool::new(echo_gating));
    let stop_loop = Arc::new(AtomicBool::new(false));

    // Stdin lines reader task
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<String>(32);
    let stop_loop_c = std::sync::Arc::clone(&stop_loop);
    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        loop {
            if stop_loop_c.load(Ordering::Relaxed) {
                break;
            }
            line.clear();
            if reader.read_line(&mut line).await.is_ok() {
                let trimmed = line.trim().to_string();
                if stdin_tx.send(trimmed).await.is_err() {
                    break;
                }
            } else {
                break;
            }
        }
    });

    let mut is_model_speaking = false;
    let mut current_model_output = String::new();
    let mut current_user_transcript = String::new();
    // A provider that never closes its input transcripts leaves the
    // last partial pending; it is printed when the reply starts.
    let mut saw_final_transcript = false;

    loop {
        if stop_loop.load(Ordering::Relaxed) {
            break;
        }

        tokio::select! {
            // 1. Process microphone chunks
            chunk_opt = mic_rx.recv() => {
                if let Some(chunk) = chunk_opt {
                    // Playback Gating: Filter/drop microphone audio chunks if the model is speaking.
                    // Since native command-line streams lack browser-native echo cancellation, speaker output
                    // easily bleeds back into the microphone. Dropping audio during playback prevents false VAD
                    // barge-in triggers, letting the model finish speaking. To interrupt, simply hit Enter on an empty line.
                    let is_muted = muted.load(Ordering::Relaxed);
                    let is_gating = gating_enabled.load(Ordering::Relaxed);
                    let should_drop = is_gating && is_model_speaking;

                    if !is_muted && !should_drop && !chunk.samples.is_empty() {
                        // client.send_audio automatically resamples samples from chunk.sample_rate to native_input_sample_rate
                        if let Err(e) = client.send_audio(&chunk.samples, chunk.sample_rate).await {
                            warn!("audio publish error: {e}");
                        }
                    }
                } else {
                    warn!("mic channel closed");
                    break;
                }
            }

            // 2. Process keystroke / written input
            stdin_opt = stdin_rx.recv() => {
                if let Some(cmd) = stdin_opt {
                    if cmd == "/quit" || cmd == "/exit" {
                        println!("\n👋 Exiting session...");
                        stop_loop.store(true, Ordering::Relaxed);
                        break;
                    } else if cmd == "/mute" {
                        let curr = muted.load(Ordering::Relaxed);
                        muted.store(!curr, Ordering::Relaxed);
                        if curr {
                            println!("\n🔔 Microphone UNMUTED.");
                        } else {
                            println!("\n🔕 Microphone MUTED.");
                        }
                    } else if cmd == "/gate" {
                        let curr = gating_enabled.load(Ordering::Relaxed);
                        gating_enabled.store(!curr, Ordering::Relaxed);
                        if curr {
                            println!("\n🔓 Echo-gating DISABLED (Barge-in freely! Useful for headphone tests).");
                        } else {
                            println!("\n🔒 Echo-gating ENABLED (Model voice blocks mic input to prevent room feedback loops).");
                        }
                    } else if cmd.is_empty() {
                        // Empty enter keypress -- if model is speaking, treat as a barge-in (interruption)
                        if is_model_speaking {
                            println!("\n⚡ [User Interrupted!]");
                            if let Some(ref ring) = playback_ring {
                                audio::playback::clear_playback_ring(ring);
                            }
                            if let Err(e) = client.cancel().await {
                                warn!("failed to send cancel request: {e}");
                            }
                            is_model_speaking = false;
                            current_model_output.clear();
                        }
                    } else {
                        // Send text message to the model
                        println!("\n⌨️  [Text prompt]: {cmd}");
                        current_user_transcript.clear();
                        if let Err(e) = client.send_text(&cmd).await {
                            eprintln!("❌ Failed to send text: {e}");
                        }
                    }
                }
            }

            // 3. Process WebSocket server events
            ev_opt = client.next_event() => {
                if let Some(ev) = ev_opt {
                    match ev {
                        RealtimeEvent::InputSpeechStarted => {
                            info!("Server-side VAD: user speech started");
                            // Server detected user voice; if the bot is playing back, clear our playback ring-buffer immediately
                            if is_model_speaking {
                                println!("\n⚡ User talking (Barge-in detected by server)");
                                if let Some(ref ring) = playback_ring {
                                    audio::playback::clear_playback_ring(ring);
                                }
                                if let Err(e) = client.cancel().await {
                                    warn!("barge-in cancel error: {e}");
                                }
                                is_model_speaking = false;
                                current_model_output.clear();
                            }
                        }
                        RealtimeEvent::InputSpeechStopped => {
                            info!("Server-side VAD: user speech stopped");
                        }
                        RealtimeEvent::InputAudioCommitted => {
                            info!("Server commitment: user utterance committed");
                        }
                        RealtimeEvent::InputTranscript { text, is_final } => {
                            if !text.trim().is_empty() {
                                if is_final {
                                    println!("\n🎤 [User]: {text}");
                                    current_user_transcript.clear();
                                    saw_final_transcript = true;
                                } else {
                                    current_user_transcript.clone_from(&text);
                                    print!("\r🎤 [User (partial)]: {text}       ");
                                    let _ = std::io::Write::flush(&mut std::io::stdout());
                                }
                            }
                        }
                        RealtimeEvent::ResponseStarted { response_id } => {
                            if !saw_final_transcript && !current_user_transcript.is_empty() {
                                println!("\n🎤 [User]: {current_user_transcript}");
                                current_user_transcript.clear();
                            }
                            info!(?response_id, "Model response generation started");
                            is_model_speaking = true;
                            current_model_output.clear();
                            print!("\n🤖 [Model]: ");
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        }
                        RealtimeEvent::ResponseOutputItemAdded { item_id } => {
                            info!(?item_id, "Model item added");
                        }
                        RealtimeEvent::ResponseAudioDelta { samples, sample_rate, .. } => {
                            if !is_model_speaking {
                                if !saw_final_transcript && !current_user_transcript.is_empty() {
                                    println!("\n🎤 [User]: {current_user_transcript}");
                                    current_user_transcript.clear();
                                }
                                is_model_speaking = true;
                                current_model_output.clear();
                                print!("\n🤖 [Model]: ");
                                let _ = std::io::Write::flush(&mut std::io::stdout());
                            }
                            // Feed to the playback stream ring
                            if !samples.is_empty() {
                                let frame = crate::audio::AudioFrame {
                                    samples,
                                    sample_rate,
                                    channels: 1,
                                };
                                // Resample to device target rate if they don't match
                                let frame_to_play = if sample_rate == device_rate || device_rate == 0 {
                                    frame
                                } else {
                                    // Audio that cannot be converted is not
                                    // played at the wrong speed.
                                    crate::audio::resample::resample(frame, device_rate)
                                        .unwrap_or_else(|e| {
                                            tracing::warn!(error = %e, "model audio not played");
                                            crate::audio::AudioFrame {
                                                samples: Vec::new(),
                                                sample_rate: device_rate,
                                                channels: 1,
                                            }
                                        })
                                };

                                if let Some(ref ring) = playback_ring {
                                    crate::audio::playback::feed_playback_ring(
                                        ring,
                                        &frame_to_play,
                                        crate::audio::playback::playback_device_channels()
                                    );
                                }
                            }
                        }
                        RealtimeEvent::ResponseTextDelta { text } => {
                            if !is_model_speaking {
                                if !saw_final_transcript && !current_user_transcript.is_empty() {
                                    println!("\n🎤 [User]: {current_user_transcript}");
                                    current_user_transcript.clear();
                                }
                                is_model_speaking = true;
                                current_model_output.clear();
                                print!("\n🤖 [Model]: ");
                                let _ = std::io::Write::flush(&mut std::io::stdout());
                            }
                            print!("{text}");
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                            current_model_output.push_str(&text);
                        }
                        RealtimeEvent::ResponseCompleted { response_id } => {
                            info!(?response_id, "Model response generated fully");
                            is_model_speaking = false;
                            println!();
                        }
                        RealtimeEvent::ResponseCancelled { response_id } => {
                            info!(?response_id, "In-flight response cancelled by server (barge-in)");
                            if let Some(ref ring) = playback_ring {
                                audio::playback::clear_playback_ring(ring);
                            }
                            is_model_speaking = false;
                            current_model_output.clear();
                            println!("\n⚡ [Interrupted by Server]");
                        }
                        RealtimeEvent::FunctionCall { call_id, name, arguments } => {
                            warn!("Model triggered tool call {name} ({call_id}); args: {arguments} (not supported in CLI yet)");
                        }
                        RealtimeEvent::SessionUpdated => {
                            info!("Session established / settings updated");
                        }
                        RealtimeEvent::Error { code, message, recoverable } => {
                            // Suppress non-alarming expected race warnings like "response_cancel_not_active"
                            let is_benign_cancel_race = code.as_deref() == Some("response_cancel_not_active");
                            if !is_benign_cancel_race {
                                eprintln!("\n⚠️  S2S Error (code={code:?}, recoverable={recoverable}): {message}");
                            }
                            if !recoverable {
                                println!("Exiting due to fatal error...");
                                stop_loop.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                        RealtimeEvent::Closed { reason, .. } => {
                            println!("\n❌ WebSocket closed: {reason}");
                            stop_loop.store(true, Ordering::Relaxed);
                            break;
                        }
                        RealtimeEvent::ResponseUsage { response_id, total_tokens, input_tokens, output_tokens, .. } => {
                            info!(?response_id, total_tokens, input_tokens, output_tokens, "turn tokens consumption");
                        }
                        RealtimeEvent::RateLimits { items } => {
                            info!(?items, "rates constraints update");
                        }
                        // An event newer than this build: nothing to show.
                        other => {
                            info!(event = ?other, "unhandled realtime event");
                        }
                    }
                } else {
                    println!("\n❌ S2S event stream terminated.");
                    stop_loop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
    }

    // Stop microphone stream running inside task
    stop_mic.store(true, Ordering::Relaxed);
    Ok(())
}
