//! Real-time mic-based commands: Live (full loop) and Listen (STT only).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

use tracing::{info, instrument};

use crate::audit::AuditSink;
use crate::config::PipelineConfig;
use crate::stt::SttProvider;
use crate::tts::TtsProvider;
use crate::{audio, audit, config, pipeline, text};

use super::{AUDIT_SINK_CAP, HARDWARE_FLUSH_DELAY, REQUEST_COUNTER, TRACE_CHANNEL_CAP};

/// How long to wait for a component's warm-up before starting anyway.
///
/// Generous, because a cold GGUF on CPU legitimately takes a while, but
/// bounded: warm-up only buys a faster first turn, so a provider that stalls
/// must not cost the whole session.
const WARM_UP_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

// ── Live: mic -> STT -> TTS -> speaker ────────────────────────

#[instrument(skip_all)]
pub async fn run_live(
    silence_ms: u64,
    save_input: Option<PathBuf>,
    save_output: Option<PathBuf>,
    cfg: &PipelineConfig,
    audit_sink: Option<&AuditSink>,
) -> Result<(), anyhow::Error> {
    if cfg.realtime_provider.is_some() {
        anyhow::bail!(
            "The `live` command runs cascade pipelines (STT -> LLM -> TTS). For Speech-to-Speech (realtime_provider) pipelines, use `voice-pipes interactive` for terminal mic/speaker loop, or `voice-pipes serve` for browser / WebSocket."
        );
    }

    // Step 1: Load providers (wrapped in Arc for sharing across tasks)
    let (trace_tx, mut trace_rx) = tokio::sync::mpsc::channel(TRACE_CHANNEL_CAP);
    let stt_provider: Arc<dyn SttProvider> =
        Arc::from(config::build_stt_provider(cfg, Some(trace_tx.clone()))?);
    let tts_provider: Arc<dyn TtsProvider> = Arc::from(config::build_tts_provider(
        cfg,
        Some(trace_tx.clone()),
        None,
    )?);
    let text_processors: Arc<Vec<Box<dyn text::TextProcessor>>> =
        Arc::new(config::build_text_processors(cfg, Some(trace_tx), None)?);
    info!(
        stt = stt_provider.name(),
        tts = tts_provider.name(),
        text_processors = text_processors.len(),
        "loading providers"
    );

    // Warm up components in parallel to amortise cold-start costs. The
    // handles are kept, not dropped: the first thing someone needs to know at
    // a microphone is whether it is listening yet, and a warm-up running
    // invisibly in the background means the prompt to speak appears while a
    // model is still loading.
    let mut warm_ups: Vec<(&'static str, tokio::task::JoinHandle<()>)> = Vec::new();
    warm_ups.push(("STT", {
        let stt = Arc::clone(&stt_provider);
        tokio::spawn(async move {
            if let Err(e) = stt.warm_up().await {
                tracing::warn!(provider = %stt.name(), error = %e, "STT warm-up failed");
            } else {
                tracing::debug!(provider = %stt.name(), "STT warm-up completed");
            }
        })
    }));
    warm_ups.push(("TTS", {
        let tts = Arc::clone(&tts_provider);
        tokio::spawn(async move {
            if let Err(e) = tts.warm_up().await {
                tracing::warn!(provider = %tts.name(), error = %e, "TTS warm-up failed");
            } else {
                tracing::debug!(provider = %tts.name(), "TTS warm-up completed");
            }
        })
    }));
    warm_ups.push(("LLM", {
        let procs = Arc::clone(&text_processors);
        tokio::spawn(async move {
            for proc in procs.iter() {
                if let Err(e) = proc.warm_up().await {
                    tracing::warn!(processor = %proc.name(), error = %e, "LLM warm-up failed");
                } else {
                    tracing::debug!(processor = %proc.name(), "LLM warm-up completed");
                }
            }
        })
    }));

    // Step 2: List audio devices (diagnostic)
    audio::io::list_devices();

    // Step 3: Set up Ctrl+C handler
    let stop = Arc::new(AtomicBool::new(false));
    let stop_signal = std::sync::Arc::clone(&stop);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop_signal.store(true, Ordering::Relaxed);
    });

    // Create save directories if requested
    if let Some(ref dir) = save_input {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(ref dir) = save_output {
        std::fs::create_dir_all(dir)?;
    }

    // Create streaming audit sink if save_input directory is set.
    let streaming_audit = save_input.as_ref().map(|dir| {
        let store: std::sync::Arc<dyn audit::store::AuditStore> =
            std::sync::Arc::new(audit::store::LocalFsAuditStore::new(dir.clone()));
        let session_id = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        audit::StreamingAuditSink::spawn(store, session_id, AUDIT_SINK_CAP)
    });

    if let Some(audit) = streaming_audit.clone() {
        tokio::spawn(async move {
            while let Some(event) = trace_rx.recv().await {
                audit.record(crate::audit::streaming::AuditEvent::Trace(event));
            }
        });
    }

    // Continuous two-sided recording, when `[pipeline.audit]` asks for it.
    // The serve runners tee this themselves; live has its own audio path, so
    // it wires the same recorder to its own two ends.
    let session_recorder = save_input.as_ref().and_then(|dir| {
        let audit_cfg = &cfg.pipeline.audit;
        if !audit_cfg.record_session_audio {
            return None;
        }
        let store: std::sync::Arc<dyn audit::store::AuditStore> =
            std::sync::Arc::new(audit::store::LocalFsAuditStore::new(dir.clone()));
        let session_id = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        Some(audit::SessionRecorder::spawn(
            store,
            session_id,
            audit_cfg.session_audio_max_seconds,
        ))
    });

    let cfg_output_rate = cfg.output_rate();

    // Step 4: Create pipeline channels (raw mic -> VAD -> splitter -> STT + interrupt monitor)
    let (raw_mic_tx, raw_mic_rx) = tokio::sync::mpsc::channel::<pipeline::AudioChunk>(64);
    let (mic_tx, mic_rx) = tokio::sync::mpsc::channel::<pipeline::AudioChunk>(64);
    let (stt_tx, mut stt_rx) = tokio::sync::mpsc::channel::<pipeline::Utterance>(4);

    // Shared VAD filter task (same logic as the transport/serve path).
    let vad = config::build_vad(cfg, None)?;
    let filter_chain = pipeline::build_filter_chain(cfg, None)?;
    // With recording on, the raw mic passes through a tee first: the VAD is
    // about to discard the silence between turns, which is exactly what a
    // turn-taking model reads.
    let vad_input_rx = match session_recorder.clone() {
        None => raw_mic_rx,
        Some(recorder) => {
            let (tee_tx, tee_rx) = tokio::sync::mpsc::channel::<pipeline::AudioChunk>(64);
            let mut raw_mic_rx = raw_mic_rx;
            tokio::spawn(async move {
                while let Some(chunk) = raw_mic_rx.recv().await {
                    recorder.record(
                        audit::SessionAudioSide::User,
                        &chunk.samples,
                        chunk.sample_rate,
                        chunk.channels,
                    );
                    if tee_tx.send(chunk).await.is_err() {
                        break;
                    }
                }
            });
            tee_rx
        }
    };
    tokio::spawn(pipeline::vad_filter_task_with_filter(
        vad_input_rx,
        mic_tx,
        vad,
        filter_chain,
        silence_ms,
        cfg.vad
            .as_ref()
            .map_or(crate::config::DEFAULT_VAD_PRE_ROLL_MS, |v| {
                v.effective_pre_roll_ms()
            }),
        None,
    ));

    // Build barge-in settings from config.
    let barge_in_enabled = cfg.barge_in.as_ref().is_some_and(|bi| bi.enabled);
    let barge_in_settings = cfg
        .barge_in
        .as_ref()
        .map(|bi| pipeline::BargeInSettings::new(bi.speech_threshold, bi.min_chunks, false))
        .unwrap_or_default();

    // If barge-in is enabled, split mic audio to both STT and interrupt monitor.
    // Otherwise, mic goes directly to STT (no splitter overhead).
    let (stt_mic_rx, turn_state, cancel_watch_tx, _cancel_watch_rx, vad_hook_tx) =
        if barge_in_enabled {
            let (stt_mic_tx, stt_mic_rx) = tokio::sync::mpsc::channel::<pipeline::AudioChunk>(64);
            let (monitor_mic_tx, monitor_mic_rx) =
                tokio::sync::mpsc::channel::<pipeline::AudioChunk>(64);
            tokio::spawn(pipeline::mic_audio_splitter(
                mic_rx,
                stt_mic_tx,
                monitor_mic_tx,
            ));

            // Pipeline state shared between orchestrator and interrupt monitor.
            let turn_state = pipeline::new_turn_state();

            // Watch channel: orchestrator sends the current utterance's cancel token.
            let noop_token = tokio_util::sync::CancellationToken::new();
            let (cancel_watch_tx, cancel_watch_rx) = tokio::sync::watch::channel(noop_token);

            let monitor_state = std::sync::Arc::clone(&turn_state);

            let (vad_tx, mut vad_rx) =
                tokio::sync::mpsc::channel::<crate::vad::engine::VadEvent>(32);

            let rms_vad = crate::vad::engine::RmsVadEngine::new(
                monitor_mic_rx,
                std::sync::Arc::clone(&monitor_state),
                barge_in_settings.clone(),
            );

            let (stt_vad_hook_tx, stt_vad_hook_rx) =
                tokio::sync::mpsc::channel::<crate::stt::TranscriptionResult>(32);
            let provider_vad = crate::vad::engine::ProviderVadEngine::from_source(
                Box::new(crate::vad::engine::SttResultSignalSource::new(
                    stt_vad_hook_rx,
                )),
                monitor_state,
                barge_in_settings,
            );

            let mut composite_vad = crate::vad::engine::CompositeVadEngine::new(vec![
                Box::new(rms_vad),
                Box::new(provider_vad),
            ]);

            let vad_tx_clone = vad_tx.clone();
            tokio::spawn(async move {
                use crate::vad::engine::VadEngine;
                composite_vad.start(vad_tx_clone).await;
            });

            let vad_cancel_rx = cancel_watch_rx.clone();
            tokio::spawn(async move {
                use crate::vad::engine::VadEvent;
                while let Some(event) = vad_rx.recv().await {
                    if let VadEvent::SpeechStarted { .. } = event {
                        let token = vad_cancel_rx.borrow().clone();
                        if !token.is_cancelled() {
                            tracing::debug!("VAD triggered interruption, cancelling current token");
                            token.cancel();
                        }
                    }
                }
            });

            (
                stt_mic_rx,
                turn_state,
                Some(cancel_watch_tx),
                Some(cancel_watch_rx),
                Some(stt_vad_hook_tx),
            )
        } else {
            let turn_state = pipeline::new_turn_state();
            let (cancel_watch_tx, cancel_watch_rx) =
                tokio::sync::watch::channel(tokio_util::sync::CancellationToken::new());
            (
                mic_rx,
                turn_state,
                Some(cancel_watch_tx),
                Some(cancel_watch_rx),
                None,
            )
        };

    // Step 5: Spawn long-lived STT task (mic -> STT)
    let turn_detector = config::build_turn_detector(cfg, None)?;
    let stt_handle = tokio::spawn(pipeline::stt_task(
        stt_mic_rx,
        stt_tx,
        None, // speculative_tx
        std::sync::Arc::clone(&stt_provider),
        std::sync::Arc::clone(&turn_detector),
        streaming_audit.clone(),
        None,
        cfg.pipeline.min_utterance_chars,
        vad_hook_tx,
        None,
    ));

    // Step 6: Streaming raw mic capture on a blocking thread (no VAD -- handled by vad_filter_task).
    let stop_mic = std::sync::Arc::clone(&stop);
    let mic_handle = tokio::task::spawn_blocking(move || {
        if let Err(e) = audio::io::mic_capture_raw(raw_mic_tx, stop_mic, 20) {
            tracing::error!(error = %e, "mic capture error");
        }
    });

    // Nothing is ready to answer until the models are loaded, so say so and
    // then say when that changes. The microphone is already capturing, so
    // anything said during warm-up is still heard -- it just may not be
    // answered promptly, which is the part worth being honest about.
    println!("\n⏳ Warming up {} components...", warm_ups.len());
    for (label, handle) in warm_ups {
        // Bounded: a provider that never finishes warming up must not leave
        // someone staring at a prompt that never arrives. The session starts
        // either way -- warm-up only buys a faster first turn.
        match tokio::time::timeout(WARM_UP_BUDGET, handle).await {
            Ok(Ok(())) => println!("   ✓ {label}"),
            Ok(Err(_)) => {
                tracing::warn!(component = label, "warm-up task did not complete");
                println!("   ! {label} (warm-up did not complete; first turn may be slow)");
            }
            Err(_) => {
                tracing::warn!(
                    component = label,
                    seconds = WARM_UP_BUDGET.as_secs(),
                    "warm-up still running; starting anyway"
                );
                println!("   … {label} still warming (starting anyway)");
            }
        }
    }
    // Say whether the session is being recorded, and where. Recording is two
    // opt-ins that have to agree -- the `--save-input` directory and the
    // config flag -- and when they disagree the run looks entirely normal
    // while producing none of the thing it was started for.
    match (&save_input, &session_recorder) {
        (Some(dir), Some(_)) => println!(
            "🎙  Recording both sides to {}/<session>/session_audio.wav",
            dir.display()
        ),
        (Some(_), None) => println!(
            "⚠  Not recording session audio: set `[pipeline.audit] \
             record_session_audio = true` to turn it on"
        ),
        (None, _) => {
            println!("⚠  Not recording: pass --save-input <dir> to keep this conversation");
        }
    }

    println!(
        "\n✅ Ready. Speak, then pause -- the agent answers when it hears you stop.\n\
            Ctrl+C to stop (this writes the recording).\n"
    );

    // Step 7: Playback + per-utterance orchestration
    let stt_name = stt_provider.name().to_string();
    let tts_name = tts_provider.name().to_string();
    let mut frag_idx = 0u32;
    let pipeline_start = Instant::now();

    // Open persistent cpal output stream for gapless playback.
    let (playback_ring, playback_stream, device_rate) = audio::playback::open_playback_stream();
    if let Some(ref stream) = playback_stream {
        use cpal::traits::StreamTrait;
        let _ = stream.play();
    }

    // Use explicit config rate, or fall back to the device rate so TTS
    // audio (e.g. 22050 Hz) gets resampled to match the output device.
    let output_rate = cfg_output_rate.or((device_rate > 0).then_some(device_rate));

    // Per-utterance audio channel: process_utterance sends chunks here.
    let (utterance_audio_tx, mut utterance_audio_rx) =
        tokio::sync::mpsc::channel::<pipeline::AudioChunk>(32);

    // Orchestrator loop: receive utterances from STT and process them.
    while let Some(utterance) = stt_rx.recv().await {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        pipeline::set_turn_state(&turn_state, pipeline::TurnState::Processing);
        tracing::info!(
            utterance_id = utterance.utterance_id,
            chars = utterance.text.chars().count(),
            confidence = ?utterance.confidence,
            "orchestrator: processing utterance"
        );
        match utterance.confidence {
            Some(c) => println!("💬 ({:.0}%) {}", c * 100.0, utterance.text),
            None => println!("💬 {}", utterance.text),
        }

        // Create a per-utterance session (fresh cancel token).
        let session = pipeline::PipelineTurn::new();
        let cancel = session.cancel.clone();

        // Notify interrupt monitor of the current cancel token.
        if let Some(ref tx) = cancel_watch_tx {
            let _ = tx.send(cancel.clone());
        }

        // Spawn process_utterance (LLM -> TTS) for this utterance.
        let proc_tx = utterance_audio_tx.clone();
        let procs = std::sync::Arc::clone(&text_processors);
        let tts = std::sync::Arc::clone(&tts_provider);
        let cancel_proc = cancel.clone();
        let utt_audit = streaming_audit.clone();
        let output_chunking = cfg.output_chunking();
        let live_sanitize_cfg = cfg
            .tts
            .as_ref()
            .map(|t| voice_pipes_sdk::SanitizeConfig::from_tts(Some(t), &t.provider, &[]));
        let utt_id = utterance.utterance_id;
        // Discard event-channel messages in live mode (no transport to forward to).
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let utterance_handle = tokio::spawn(async move {
            pipeline::process_utterance(
                &utterance,
                proc_tx,
                event_tx,
                procs,
                pipeline::empty_chain(),
                tts,
                None,
                output_rate,
                output_chunking,
                cancel_proc,
                utt_audit,
                None,
                std::sync::Arc::new(parking_lot::Mutex::new(pipeline::AudioLedger::default())),
                // Live mode does not benchmark cold-start; opt out semantically
                // by starting from `false` so every turn reports `cold_start = false`.
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                None,
                live_sanitize_cfg,
            )
            .await;
        });

        // Transition to Speaking once we start receiving audio.
        let mut first_audio = true;
        let cancel_playback = cancel.clone();
        let mut interrupted = false;

        loop {
            tokio::select! {
                biased;
                () = cancel_playback.cancelled() => {
                    // Interrupted! Clear the ring buffer for immediate silence.
                    if let Some(ref ring) = playback_ring {
                        audio::playback::clear_playback_ring(ring);
                    }
                    interrupted = true;
                    tracing::info!("orchestrator: utterance interrupted");
                    break;
                }
                chunk = utterance_audio_rx.recv() => match chunk {
                    Some(c) => {
                        if first_audio && !c.samples.is_empty() {
                            pipeline::set_turn_state(
                                &turn_state,
                                pipeline::TurnState::Speaking,
                            );
                            first_audio = false;
                        }

                        let frame = c.to_frame();
                        turn_detector.feed_agent_audio(&frame.samples, frame.sample_rate);

                        if let Some(ref recorder) = session_recorder {
                            recorder.record(
                                audit::SessionAudioSide::Agent,
                                &frame.samples,
                                frame.sample_rate,
                                frame.channels,
                            );
                        }

                        // Save TTS output
                        if let Some(ref dir) = save_output
                            && !c.samples.is_empty() {
                                let path = dir.join(format!("{frag_idx:03}_output.wav"));
                                if let Err(e) = audio::wav_writer::write_wav(&frame, &path) {
                                    tracing::error!(path = %path.display(), "failed to save TTS output: {e}");
                                } else {
                                    info!(path = %path.display(), "saved TTS fragment");
                                }
                            }

                        // Feed ring buffer
                        if !c.samples.is_empty()
                            && let Some(ref ring) = playback_ring {
                                audio::playback::feed_playback_ring(ring, &frame, audio::playback::playback_device_channels());
                            }

                        // Audit
                        if let Some(sink) = audit_sink {
                            sink.record(audit::AuditRecord {
                                timestamp: SystemTime::now(),
                                request_id: format!("req-{}", REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)),
                                input_text: format!("fragment_{frag_idx}"),
                                audio: frame,
                                provider: format!("{stt_name}->{tts_name}"),
                                latency: pipeline_start.elapsed(),
                            });
                        }

                        frag_idx += 1;

                        if c.is_end_of_utterance {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }

        // Wait for the utterance tasks to finish.
        utterance_handle.abort();
        let _ = utterance_handle.await;

        // Drain stale audio chunks left in the shared channel.
        while utterance_audio_rx.try_recv().is_ok() {}

        // Emit UtteranceDone audit event.
        if let Some(ref sa) = streaming_audit {
            sa.record(audit::AuditEvent::UtteranceDone {
                utterance_id: utt_id,
                interrupted,
                terminated: false,
                stt_provider: stt_name.clone(),
                tts_provider: tts_name.clone(),
            });
        }

        if !interrupted {
            // Drain remaining audio before going back to listening.
            if let Some(ref ring) = playback_ring {
                tokio::select! {
                    () = audio::playback::drain_playback_ring(ring) => {}
                    () = cancel_playback.cancelled() => {
                        audio::playback::clear_playback_ring(ring);
                        tracing::info!("orchestrator: utterance interrupted during drain");
                    }
                }
            }
        }

        pipeline::set_turn_state(&turn_state, pipeline::TurnState::Listening);
        println!("🎤 Listening...\n");
    }

    // Drain remaining audio.
    if let Some(ref ring) = playback_ring {
        audio::playback::drain_playback_ring(ring).await;
    }
    // Let hardware buffer flush.
    tokio::time::sleep(HARDWARE_FLUSH_DELAY).await;
    drop(playback_stream);

    // Wait for long-lived tasks to complete
    let _ = tokio::join!(mic_handle, stt_handle);

    // Write the continuous recording. Awaited rather than left to the writer
    // task, so the file is on disk by the time the command returns.
    if let Some(recorder) = session_recorder {
        recorder.finish().await;
        println!("🎙  session_audio.wav written");
    }

    println!("\n👋 Stopped.");
    Ok(())
}

// ── Listen: mic -> STT only ──────────────────────────────────

#[instrument(skip_all)]
pub async fn run_listen(
    silence_ms: u64,
    save_input: Option<PathBuf>,
    cfg: &PipelineConfig,
) -> Result<(), anyhow::Error> {
    let stt_provider: Arc<dyn SttProvider> = Arc::from(config::build_stt_provider(cfg, None)?);
    info!(stt = stt_provider.name(), "loading STT provider");

    // Warm up STT in the background
    {
        let stt = Arc::clone(&stt_provider);
        tokio::spawn(async move {
            if let Err(e) = stt.warm_up().await {
                tracing::warn!(provider = %stt.name(), error = %e, "STT warm-up failed");
            } else {
                tracing::debug!(provider = %stt.name(), "STT warm-up completed");
            }
        })
    };

    audio::io::list_devices();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_signal = std::sync::Arc::clone(&stop);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop_signal.store(true, Ordering::Relaxed);
    });

    // Streaming audit (saves mic WAVs + STT results when --save-input is set).
    let streaming_audit = save_input.map(|dir| {
        let _ = std::fs::create_dir_all(&dir);
        let store: std::sync::Arc<dyn audit::store::AuditStore> =
            std::sync::Arc::new(audit::store::LocalFsAuditStore::new(dir));
        let session_id = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        audit::StreamingAuditSink::spawn(store, session_id, AUDIT_SINK_CAP)
    });

    let (raw_mic_tx, raw_mic_rx) = tokio::sync::mpsc::channel::<pipeline::AudioChunk>(64);
    let (mic_tx, mic_rx) = tokio::sync::mpsc::channel::<pipeline::AudioChunk>(64);
    let (stt_tx, mut stt_rx) = tokio::sync::mpsc::channel::<pipeline::Utterance>(4);

    let turn_detector = config::build_turn_detector(cfg, None)?;
    let _turn_state = pipeline::new_turn_state();
    let (_, _cancel_watch_rx) =
        tokio::sync::watch::channel(tokio_util::sync::CancellationToken::new());

    let stt_handle = tokio::spawn(pipeline::stt_task(
        mic_rx,
        stt_tx,
        None, // speculative_tx
        std::sync::Arc::clone(&stt_provider),
        turn_detector,
        streaming_audit,
        None,
        cfg.pipeline.min_utterance_chars,
        None,
        None,
    ));

    let vad = config::build_vad(cfg, None)?;
    let filter_chain = pipeline::build_filter_chain(cfg, None)?;
    let pre_roll_ms = cfg
        .vad
        .as_ref()
        .map_or(crate::config::DEFAULT_VAD_PRE_ROLL_MS, |v| {
            v.effective_pre_roll_ms()
        });
    tokio::spawn(pipeline::vad_filter_task_with_filter(
        raw_mic_rx,
        mic_tx,
        vad,
        filter_chain,
        silence_ms,
        pre_roll_ms,
        None,
    ));

    let stop_mic = std::sync::Arc::clone(&stop);
    let mic_handle = tokio::task::spawn_blocking(move || {
        println!("\n🎤 Listening (STT only)... speak and pause. Ctrl+C to stop.\n");
        if let Err(e) = audio::io::mic_capture_raw(raw_mic_tx, stop_mic, 20) {
            tracing::error!(error = %e, "mic capture error");
        }
    });

    while let Some(utterance) = stt_rx.recv().await {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match utterance.confidence {
            Some(c) => println!(
                "[STT #{:03}] ({:.0}%) {}",
                utterance.utterance_id,
                c * 100.0,
                utterance.text
            ),
            None => println!("[STT #{:03}] {}", utterance.utterance_id, utterance.text),
        }
    }

    let _ = tokio::join!(mic_handle, stt_handle);
    println!("\n👋 Stopped.");
    Ok(())
}
