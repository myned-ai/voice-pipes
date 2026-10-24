use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tracing::info;

use crate::config::{OutputChunkingConfig, PipelineConfig};
use crate::transport::{ErrorCode, ServerControl, ServerMessage, SessionConfig};

use super::turn::TurnTracker;

pub fn effective_output_chunking(
    cfg: &PipelineConfig,
    client_config: &Option<SessionConfig>,
) -> Option<OutputChunkingConfig> {
    let mut output_chunking = cfg.output_chunking();

    let Some(client_config) = client_config.as_ref() else {
        return output_chunking;
    };

    if let Some(enabled) = client_config
        .options
        .get("output_chunking_enabled")
        .and_then(serde_json::Value::as_bool)
    {
        let current = output_chunking.get_or_insert_with(OutputChunkingConfig::default);
        current.enabled = enabled;
    }

    if let Some(frame_ms) = client_config
        .options
        .get("output_chunk_frame_ms")
        .and_then(serde_json::Value::as_u64)
    {
        let current = output_chunking.get_or_insert_with(OutputChunkingConfig::default);
        current.frame_ms = frame_ms as u32;
    }

    output_chunking
}

pub async fn inject_text_utterance(
    text: &str,
    tracker: &mut TurnTracker,
    text_utt_tx: &mpsc::Sender<crate::pipeline::Utterance>,
) {
    if text.is_empty() {
        return;
    }
    let id = tracker.next_text_id();
    let utterance = crate::pipeline::Utterance {
        text: text.to_string(),
        utterance_id: id,
        confidence: None,
        stt_latency: None,
        source: crate::pipeline::UtteranceSource::Text,
        endpointing_latency: None,
        inference_metrics: None,
        committed_at: Some(std::time::Instant::now()),
    };
    let _ = text_utt_tx.send(utterance).await;
}

pub fn handle_inject_audio(
    session_id: &str,
    source: String,
    scenario_id: Option<String>,
    experiment_id: Option<String>,
    speed: f32,
    audio_tx: &mpsc::Sender<crate::pipeline::AudioChunk>,
    out_tx: &mpsc::Sender<ServerMessage>,
) {
    info!(
        session_id = %session_id,
        source = %source,
        scenario_id = ?scenario_id,
        experiment_id = ?experiment_id,
        speed,
        "received audio injection request"
    );
    // `source` is a client-supplied filesystem path. The policy
    // is resolved at serve startup (dev = allowed, client-auth on =
    // denied, VP_ENABLE_AUDIO_INJECTION overrides).
    if !crate::transport::inject::INJECTION_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(session_id = %session_id, "rejected audio injection (disabled by policy)");
        let msg = ServerMessage::Control(ServerControl::Error {
            code: ErrorCode::InjectionFailed,
            message: "audio injection is disabled on this server \
                      (set VP_ENABLE_AUDIO_INJECTION=1 to allow)"
                .to_string(),
            severity: crate::transport::ErrorSeverity::Error,
            turn_id: None,
            component: Some(crate::transport::ErrorComponent::Pipeline),
        });
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let _ = out_tx.send(msg).await;
        });
        return;
    }
    let path = std::path::PathBuf::from(&source);
    let inject_tx = audio_tx.clone();
    let inject_out_tx = out_tx.clone();
    tokio::spawn(async move {
        match crate::transport::inject::inject_audio_file(&path, &inject_tx, speed).await {
            Ok(result) => {
                info!(
                    chunks = result.chunks_sent,
                    duration_s = result.audio_duration_s,
                    "audio injection completed"
                );
                let msg = ServerMessage::Message(crate::transport::PipelineMessage {
                    kind: "injection_complete".to_string(),
                    text: None,
                    payload: Some(serde_json::json!({
                        "source": result.source,
                        "chunks_sent": result.chunks_sent,
                        "samples_sent": result.samples_sent,
                        "audio_duration_s": result.audio_duration_s,
                        "scenario_id": scenario_id,
                        "experiment_id": experiment_id,
                    })),
                    binary: None,
                    role: crate::transport::MessageRole::Assistant,
                    turn_id: None,
                    message_id: uuid::Uuid::new_v4().to_string(),
                });
                let _ = inject_out_tx.send(msg).await;
            }
            Err(e) => {
                tracing::error!(error = %e, source = %source, "audio injection failed");
                let msg = ServerMessage::Control(ServerControl::Error {
                    code: ErrorCode::InjectionFailed,
                    message: format!("audio injection failed: {e}"),
                    severity: crate::transport::ErrorSeverity::Error,
                    turn_id: None,
                    component: Some(crate::transport::ErrorComponent::Pipeline),
                });
                let _ = inject_out_tx.send(msg).await;
            }
        }
    });
}

pub fn handle_tts_only(
    session_id: String,
    request_id: String,
    text: String,
    language: Option<String>,
    voice: Option<String>,
    tts: Arc<dyn crate::tts::TtsProvider>,
    tts_cold_start: Arc<AtomicBool>,
    captures_dir: Option<std::path::PathBuf>,
    out_tx: mpsc::Sender<ServerMessage>,
) {
    tokio::spawn(async move {
        // Take the cold flag *before* synthesis so a concurrent normal turn
        // cannot race us into a false-warm reading.
        let cold_start = tts_cold_start.swap(false, Ordering::AcqRel);

        let chars = text.chars().count() as u32;
        let provider = tts.name().to_string();
        let provider_rate = tts.sample_rate();
        let hints = crate::tts::SynthesisHints::default()
            .with_voice(voice.clone())
            .with_language(language.clone());

        let (frame_tx, mut frame_rx) = mpsc::channel::<crate::audio::AudioFrame>(8);
        let synth_start = std::time::Instant::now();
        let synth_text = text.clone();
        let synth_handle = tokio::spawn(async move {
            tts.synthesize_stream(
                crate::tts::SynthesisRequest::new(&synth_text).with_hints(Some(&hints)),
                frame_tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
        });

        let mut samples: Vec<i16> = Vec::new();
        let mut sample_rate: u32 = provider_rate;
        let mut channels: u16 = 1;
        let mut ttfb_ms: Option<u64> = None;
        while let Some(frame) = frame_rx.recv().await {
            if ttfb_ms.is_none() && !frame.samples.is_empty() {
                ttfb_ms = Some(synth_start.elapsed().as_millis() as u64);
            }
            sample_rate = frame.sample_rate;
            channels = frame.channels;
            samples.extend_from_slice(&frame.samples);
        }
        let synthesis_ms = synth_start.elapsed().as_millis() as u64;

        let synth_result = synth_handle.await;
        let provider_error: Option<String> = match synth_result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("{e:?}")),
            Err(join_err) => Some(format!("synthesis task panicked: {join_err}")),
        };

        if samples.is_empty() {
            // Restore the cold flag so the next request can still measure
            // a true cold start.  Use compare_exchange to avoid clobbering
            // a successful concurrent synthesis.
            let _ = tts_cold_start.compare_exchange(
                false,
                cold_start,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            let error = provider_error.unwrap_or_else(|| "tts produced no audio".to_string());
            tracing::warn!(
                session_id = %session_id,
                request_id = %request_id,
                provider = %provider,
                error = %error,
                "tts_only synthesis produced no audio"
            );
            let _ = out_tx
                .send(ServerMessage::Control(ServerControl::TtsOnlyError {
                    id: request_id,
                    error,
                }))
                .await;
            return;
        }

        // Compute audio duration from sample count for accuracy.
        let audio_duration_ms = if sample_rate == 0 {
            0
        } else {
            (samples.len() as u64 * 1000) / u64::from(sample_rate)
        };
        // RTF: synthesis_ms / audio_duration_ms; lower is faster than realtime.
        let rtf = if audio_duration_ms == 0 {
            0.0
        } else {
            synthesis_ms as f32 / audio_duration_ms as f32
        };

        // Pick a base directory for the WAV: the configured captures_dir
        // (so it lands next to other audit artefacts) or the OS temp dir
        // as a safe fallback for unconfigured runs.
        let base =
            captures_dir.unwrap_or_else(|| std::env::temp_dir().join("voice-pipes-tts-only"));
        let dir = base.join(&session_id).join("tts-only");
        // `request_id` is client-supplied — collapse it to one
        // safe filename component so `..\..\x` cannot escape the dir.
        let path = dir.join(format!("{}.wav", sanitize_file_component(&request_id)));

        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::error!(
                session_id = %session_id,
                request_id = %request_id,
                error = %e,
                dir = %dir.display(),
                "failed to create tts_only output dir"
            );
            let _ = out_tx
                .send(ServerMessage::Control(ServerControl::TtsOnlyError {
                    id: request_id,
                    error: format!("create_dir_all failed: {e}"),
                }))
                .await;
            return;
        }

        let frame = crate::audio::AudioFrame {
            samples,
            sample_rate,
            channels,
        };
        if let Err(e) = crate::audio::wav_writer::write_wav(&frame, &path) {
            tracing::error!(
                session_id = %session_id,
                request_id = %request_id,
                error = %e,
                path = %path.display(),
                "failed to write tts_only wav"
            );
            let _ = out_tx
                .send(ServerMessage::Control(ServerControl::TtsOnlyError {
                    id: request_id,
                    error: format!("write_wav failed: {e}"),
                }))
                .await;
            return;
        }

        let (peak_amplitude, speech_ratio) = speech_stats(&frame.samples, sample_rate);
        let metrics = crate::transport::TtsOnlyMetrics {
            ttfb_ms: Some(ttfb_ms.unwrap_or(synthesis_ms)),
            synthesis_ms: Some(synthesis_ms),
            audio_duration_ms: Some(audio_duration_ms),
            sample_rate: Some(sample_rate),
            chars: Some(chars),
            rtf: Some(rtf),
            provider: Some(provider.clone()),
            cold_start,
            error: None,
            voice,
            peak_amplitude: Some(peak_amplitude),
            speech_ratio: Some(speech_ratio),
        };
        let audio_ref = crate::transport::AudioRef {
            path: path.display().to_string(),
            format: "wav".to_string(),
            sample_rate,
            channels,
            duration_ms: audio_duration_ms,
        };
        info!(
            session_id = %session_id,
            request_id = %request_id,
            provider = %provider,
            chars,
            synthesis_ms,
            audio_duration_ms,
            rtf,
            cold_start,
            "tts_only synthesis complete"
        );
        let _ = out_tx
            .send(ServerMessage::Control(ServerControl::TtsOnlyResult {
                id: request_id,
                metrics,
                audio_ref,
            }))
            .await;
    });
}

/// RMS (int16 scale) above which a 100 ms window counts as speech. Room
/// noise and codec hiss sit far below; speech peaks in the thousands.
const SPEECH_WINDOW_RMS: f64 = 200.0;

/// Peak absolute amplitude and the fraction of 100 ms windows carrying
/// speech-level energy — the two numbers that separate "synthesized a
/// sentence" from "returned N seconds of near-silence", which no timing
/// metric can tell apart.
pub(crate) fn speech_stats(samples: &[i16], sample_rate: u32) -> (u16, f32) {
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    let window = (sample_rate as usize / 10).max(1);
    let windows: Vec<&[i16]> = samples.chunks_exact(window).collect();
    if windows.is_empty() {
        return (peak, 0.0);
    }
    let loud = windows
        .iter()
        .filter(|w| {
            let energy: f64 = w.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            (energy / w.len() as f64).sqrt() > SPEECH_WINDOW_RMS
        })
        .count();
    (peak, loud as f32 / windows.len() as f32)
}

/// Collapse a client-supplied identifier to a single safe filename
/// component: only `[A-Za-z0-9._-]` survives, leading dots
/// are stripped (no `..`), and an empty result gets a placeholder.
fn sanitize_file_component(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    let cleaned = cleaned.trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
#[path = "tests/utils.rs"]
mod tests;
