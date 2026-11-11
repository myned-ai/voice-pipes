//! Streaming audit trail -- correlates mic, STT, LLM, and TTS artefacts
//! by **session** and **utterance** so every pipeline pass can be replayed
//! and inspected offline.
//!
//! When backed by [`LocalFsAuditStore`], keys map to files under:
//!
//! ```text
//! {captures_dir}/
//!   {session_id}/
//!     {utt:03}_mic_raw.wav
//!     {utt:03}_mic_16k.wav
//!     {utt:03}_stt.json
//!     {utt:03}_llm_input.txt
//!     {utt:03}_llm_output.txt
//!     {utt:03}_tts_frag_{n:03}.wav
//!     {utt:03}_turn_resolved.json
//! ```

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::audio::AudioFrame;
use crate::audio::wav_writer;
use voice_pipes_sdk::TenantId;

use super::store::AuditStore;

// ── Event types ─────────────────────────────────────────────

/// Everything that can happen during one utterance's lifecycle.
#[derive(Debug)]
pub enum AuditEvent {
    /// Raw microphone audio before any processing.
    MicRaw {
        utterance_id: u32,
        audio: AudioFrame,
    },
    /// Microphone audio of an utterance that produced no turn: the
    /// recogniser heard nothing in it, too little, or never answered.
    /// Kept so a session with no turns can still be diagnosed.
    MicDropped {
        utterance_id: u32,
        /// Which drop under this id, from 1: the id stays until a turn
        /// commits, so one id can lose several utterances in a row.
        attempt: u32,
        reason: String,
        audio: AudioFrame,
    },
    /// Audio after resampling to STT's expected rate.
    MicResampled {
        utterance_id: u32,
        audio: AudioFrame,
    },
    /// STT transcription result.
    SttResult {
        utterance_id: u32,
        text: String,
        latency: Duration,
    },
    /// Text sent as input to the LLM / text-processor chain.
    LlmInput { utterance_id: u32, text: String },
    /// Complete LLM / text-processor output (accumulated fragments).
    LlmOutput {
        utterance_id: u32,
        text: String,
        spoken_text: Option<String>,
        tts_directives: Vec<voice_pipes_sdk::TtsDirectiveRecord>,
        stripped_directives: Vec<voice_pipes_sdk::StrippedDirectiveRecord>,
        latency: Duration,
        is_partial: bool,
    },
    /// One TTS audio fragment.
    TtsFragment {
        utterance_id: u32,
        fragment_idx: u32,
        text: String,
        spoken_text: Option<String>,
        audio: AudioFrame,
    },
    /// Utterance fully processed (or interrupted/terminated).
    ///
    /// Carries lifecycle fields only. All per-turn latencies live in the
    /// canonical `TurnMetricsSnapshot` (emitted as `PipelineEvent::TurnMetricsReady`
    /// and persisted to `pipeline_events.jsonl`) — the single source of truth.
    /// The `*_turn_resolved.json` artifact this writes is lifecycle metadata
    /// for turn enumeration; consumers read metrics from the snapshot.
    UtteranceDone {
        utterance_id: u32,
        interrupted: bool,
        /// Session disconnected while this turn was still in the pipeline.
        terminated: bool,
        stt_provider: String,
        tts_provider: String,
    },
    /// A generic trace event to be added to metadata.json
    Trace(crate::audit::TraceEvent),
    /// A structured pipeline lifecycle event, with when it happened.
    /// Written as one JSON line in `pipeline_events.jsonl`.
    ///
    /// The time is part of the variant rather than taken when the line is
    /// written, because writing happens later, on another task. Without it an
    /// interruption could be reported but not placed: its row in a session's
    /// timeline would read `t_s: null` and sort to the end, after events that
    /// followed it.
    Pipeline(
        crate::pipeline::events::PipelineEvent,
        std::time::SystemTime,
    ),
    /// Realtime-provider per-turn usage payload (token counts emitted
    /// by `OpenAI` Realtime / Gemini Live in their `response.done`-like
    /// envelopes). Stored as an opaque JSON value so new fields surface
    /// without touching this enum.
    RealtimeUsage {
        utterance_id: u32,
        payload: serde_json::Value,
    },
    /// Client-reported per-turn playback health (`playback_quality` frame).
    /// Written as `NNN_playback.json` — the primary source for the admin
    /// backend's playback-health metrics and the as-heard reconstruction.
    PlaybackHealth {
        utterance_id: u32,
        health: crate::api::types::PlaybackHealth,
    },
    /// One assistant `transcript_delta` from a realtime provider,
    /// tagged with the cumulative TTS audio offset (ms since the
    /// turn's first audio sample) at the moment the delta was
    /// received. Consumers (admin UI, playground captions) reconstruct
    /// per-word audio↔text mapping by treating consecutive entries as
    /// `[audio_ms_start, next.audio_ms_start)` spans.
    AssistantTranscriptDelta {
        utterance_id: u32,
        delta_idx: u32,
        text: String,
        audio_ms: u64,
    },
}

// ── Sink ────────────────────────────────────────────────────

/// Default per-session capture budget when the sink is spawned without
/// one; matches the `[pipeline.audit] max_session_bytes` default.
pub const DEFAULT_MAX_SESSION_BYTES: u64 = 512 * 1024 * 1024;

/// Consecutive store failures after which a writer stops trying. A
/// store that keeps failing (disk full, permissions, a vanished mount)
/// would otherwise log on every event for the rest of the session.
pub const MAX_CONSECUTIVE_WRITE_FAILURES: u32 = 10;

/// Tracks a writer's consecutive failures and switches it off past
/// [`MAX_CONSECUTIVE_WRITE_FAILURES`]. Shared by every audit writer.
#[derive(Debug, Default)]
pub(crate) struct WriterHealth {
    consecutive_failures: u32,
    disabled: bool,
}

impl WriterHealth {
    /// True once the writer has given up; events are then discarded.
    pub(crate) fn disabled(&self) -> bool {
        self.disabled
    }

    /// Record one write outcome. Failures are logged individually until
    /// the cap, then once more as the writer disables itself.
    pub(crate) fn observe(&mut self, what: &str, result: &anyhow::Result<()>) {
        match result {
            Ok(()) => self.consecutive_failures = 0,
            Err(e) => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= MAX_CONSECUTIVE_WRITE_FAILURES {
                    self.disabled = true;
                    warn!(
                        failures = self.consecutive_failures,
                        error = %e,
                        "{what}: store keeps failing; writer disabled for the rest of the session"
                    );
                } else {
                    warn!(error = %e, "{what} failed");
                }
            }
        }
    }
}

/// Non-blocking streaming audit sink.  Events are sent to a background
/// task that writes them via an [`AuditStore`].  Dropping events is
/// preferred over blocking the pipeline.
#[derive(Debug, Clone)]
pub struct StreamingAuditSink {
    tx: mpsc::Sender<AuditEvent>,
}

impl StreamingAuditSink {
    /// Spawn the background writer.  `session_id` is used as the key
    /// prefix so all artifacts for a session are grouped together.
    pub fn spawn(store: Arc<dyn AuditStore>, session_id: String, buffer_size: usize) -> Self {
        Self::spawn_with_budget(store, session_id, buffer_size, DEFAULT_MAX_SESSION_BYTES)
    }

    /// [`spawn`](Self::spawn) with an explicit disk budget for the
    /// session; `0` means no budget. Once the budget is spent, audio
    /// artefacts are dropped and text artefacts keep being written.
    pub fn spawn_with_budget(
        store: Arc<dyn AuditStore>,
        session_id: String,
        buffer_size: usize,
        max_session_bytes: u64,
    ) -> Self {
        Self::spawn_with_tenant(
            store,
            None,
            session_id,
            buffer_size,
            max_session_bytes,
            false,
        )
    }

    /// Spawn with tenant partitioning and privacy controls (ZDR).
    pub fn spawn_with_tenant(
        store: Arc<dyn AuditStore>,
        tenant_id: Option<TenantId>,
        session_id: String,
        buffer_size: usize,
        max_session_bytes: u64,
        zero_data_retention: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel(buffer_size);
        tokio::spawn(streaming_audit_writer(
            rx,
            store,
            tenant_id.clone(),
            session_id.clone(),
            max_session_bytes,
            zero_data_retention,
        ));
        info!(
            ?tenant_id,
            session_id, zero_data_retention, "streaming audit sink started"
        );
        Self { tx }
    }

    /// Record an event.  Never blocks -- drops if the channel is full.
    pub fn record(&self, event: AuditEvent) {
        if self.tx.try_send(event).is_err() {
            warn!("streaming audit channel full, dropping event");
        }
    }
}

// ── Background writer ───────────────────────────────────────

/// Bytes an event will put on disk, before encoding; audio dominates,
/// so WAV payloads are counted exactly and text events by their text.
fn event_bytes(event: &AuditEvent) -> u64 {
    match event {
        AuditEvent::MicRaw { audio, .. }
        | AuditEvent::MicDropped { audio, .. }
        | AuditEvent::MicResampled { audio, .. }
        | AuditEvent::TtsFragment { audio, .. } => (audio.samples.len() * 2) as u64,
        AuditEvent::SttResult { text, .. }
        | AuditEvent::LlmInput { text, .. }
        | AuditEvent::LlmOutput { text, .. }
        | AuditEvent::AssistantTranscriptDelta { text, .. } => text.len() as u64,
        AuditEvent::UtteranceDone { .. }
        | AuditEvent::Trace(_)
        | AuditEvent::Pipeline(..)
        | AuditEvent::RealtimeUsage { .. }
        | AuditEvent::PlaybackHealth { .. } => 256,
    }
}

fn is_audio_event(event: &AuditEvent) -> bool {
    matches!(
        event,
        AuditEvent::MicRaw { .. }
            | AuditEvent::MicDropped { .. }
            | AuditEvent::MicResampled { .. }
            | AuditEvent::TtsFragment { .. }
    )
}

async fn streaming_audit_writer(
    mut rx: mpsc::Receiver<AuditEvent>,
    store: Arc<dyn AuditStore>,
    tenant_id: Option<TenantId>,
    session_id: String,
    max_session_bytes: u64,
    zero_data_retention: bool,
) {
    let key_prefix = match &tenant_id {
        Some(t) => format!("{t}/{session_id}"),
        None => session_id.clone(),
    };
    let mut health = WriterHealth::default();
    let mut written: u64 = 0;
    let mut budget_warned = false;
    while let Some(event) = rx.recv().await {
        if health.disabled() {
            continue;
        }
        // Zero Data Retention (ZDR): discard all audio immediately, retain only telemetry/events
        if zero_data_retention && is_audio_event(&event) {
            continue;
        }
        // Text artefacts are small and stay useful past the budget, so
        // only audio is held back once it is spent.
        let cost = event_bytes(&event);
        let over_budget = max_session_bytes > 0 && written.saturating_add(cost) > max_session_bytes;
        if over_budget && is_audio_event(&event) {
            if !budget_warned {
                budget_warned = true;
                warn!(
                    session_id,
                    written,
                    max_session_bytes,
                    "session capture budget spent; dropping audio artefacts from here on"
                );
            }
            continue;
        }
        written = written.saturating_add(cost);

        if let AuditEvent::Trace(trace_event) = event {
            let key = format!("{key_prefix}/events.jsonl");
            let mut buf = match serde_json::to_vec(&trace_event) {
                Ok(b) => b,
                Err(e) => {
                    warn!("streaming audit serialize failed: {e}");
                    continue;
                }
            };
            buf.push(b'\n');

            // Append directly to the file
            let result = store.append(&key, &buf).await;
            health.observe("streaming audit events write", &result);
            continue;
        }

        let result = write_event(&event, &*store, &key_prefix).await;
        health.observe("streaming audit write", &result);
    }

    info!("streaming audit writer shutting down");
}

async fn write_event(
    event: &AuditEvent,
    store: &dyn AuditStore,
    key_prefix: &str,
) -> anyhow::Result<()> {
    match event {
        AuditEvent::MicRaw {
            utterance_id,
            audio,
        } => {
            let key = format!("{key_prefix}/{utterance_id:03}_mic_raw.wav");
            store.put(&key, &wav_writer::encode_wav(audio)?).await?;
        }
        AuditEvent::MicDropped {
            utterance_id,
            attempt,
            reason,
            audio,
        } => {
            // Its own name: the id is reused by the turn that follows, whose
            // own recording must not be overwritten, and by the next dropped
            // utterance, whose recording must not overwrite this one.
            let key = if *attempt <= 1 {
                format!("{key_prefix}/{utterance_id:03}_dropped_{reason}_mic_raw.wav")
            } else {
                format!("{key_prefix}/{utterance_id:03}_dropped_{reason}_{attempt}_mic_raw.wav")
            };
            store.put(&key, &wav_writer::encode_wav(audio)?).await?;
        }
        AuditEvent::MicResampled {
            utterance_id,
            audio,
        } => {
            let key = format!("{key_prefix}/{utterance_id:03}_mic_16k.wav");
            store.put(&key, &wav_writer::encode_wav(audio)?).await?;
        }
        AuditEvent::SttResult {
            utterance_id,
            text,
            latency,
        } => {
            let key = format!("{key_prefix}/{utterance_id:03}_stt.json");
            let json = serde_json::json!({
                "utterance_id": utterance_id,
                "text": text,
                "latency_ms": latency.as_millis() as u64,
            });
            store
                .put(&key, serde_json::to_string_pretty(&json)?.as_bytes())
                .await?;
        }
        AuditEvent::LlmInput { utterance_id, text } => {
            let key = format!("{key_prefix}/{utterance_id:03}_llm_input.txt");
            store.put(&key, text.as_bytes()).await?;
        }
        AuditEvent::LlmOutput {
            utterance_id,
            text,
            spoken_text,
            tts_directives,
            stripped_directives,
            latency,
            is_partial,
        } => {
            let key = format!("{key_prefix}/{utterance_id:03}_llm_output.json");
            let mut json = serde_json::json!({
                "utterance_id": utterance_id,
                "text": text,
                "latency_ms": latency.as_millis() as u64,
                "is_partial": is_partial,
            });
            if let Some(spoken) = spoken_text {
                json["spoken_text"] = serde_json::Value::String(spoken.clone());
            }
            if !tts_directives.is_empty() {
                json["tts_directives"] = serde_json::to_value(tts_directives)?;
            }
            if !stripped_directives.is_empty() {
                json["stripped_directives"] = serde_json::to_value(stripped_directives)?;
            }
            store
                .put(&key, serde_json::to_string_pretty(&json)?.as_bytes())
                .await?;
        }
        AuditEvent::TtsFragment {
            utterance_id,
            fragment_idx,
            text,
            spoken_text,
            audio,
        } => {
            let wav_key = format!("{key_prefix}/{utterance_id:03}_tts_frag_{fragment_idx:03}.wav");
            store.put(&wav_key, &wav_writer::encode_wav(audio)?).await?;
            let txt_key = format!("{key_prefix}/{utterance_id:03}_tts_frag_{fragment_idx:03}.txt");
            store.put(&txt_key, text.as_bytes()).await?;
            if let Some(spoken) = spoken_text
                && spoken != text
            {
                let spoken_key =
                    format!("{key_prefix}/{utterance_id:03}_tts_frag_{fragment_idx:03}_spoken.txt");
                store.put(&spoken_key, spoken.as_bytes()).await?;
            }
        }
        AuditEvent::UtteranceDone {
            utterance_id,
            interrupted,
            terminated,
            stt_provider,
            tts_provider,
        } => {
            // Lifecycle metadata only — per-turn latencies live in the
            // canonical TurnMetricsSnapshot (pipeline_events.jsonl).
            let key = format!("{key_prefix}/{utterance_id:03}_turn_resolved.json");
            let json = serde_json::json!({
                "utterance_id": utterance_id,
                "interrupted": interrupted,
                "terminated": terminated,
                "stt_provider": stt_provider,
                "tts_provider": tts_provider,
                "timestamp": SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            });
            store
                .put(&key, serde_json::to_string_pretty(&json)?.as_bytes())
                .await?;
        }
        AuditEvent::Trace(_) => {
            // Handled directly before `write_event`
        }
        AuditEvent::Pipeline(pe, at) => {
            // Append the PipelineEvent as a JSON line to a shared events log.
            let key = format!("{key_prefix}/pipeline_events.jsonl");
            let mut buf = match pipeline_event_line(pe, *at) {
                Ok(b) => b,
                Err(e) => {
                    warn!("pipeline event serialize failed: {e}");
                    return Ok(());
                }
            };
            buf.push(b'\n');
            store.append(&key, &buf).await?;
        }
        AuditEvent::RealtimeUsage {
            utterance_id,
            payload,
        } => {
            let key = format!("{key_prefix}/{utterance_id:03}_realtime_usage.json");
            store
                .put(&key, serde_json::to_string_pretty(&payload)?.as_bytes())
                .await?;
        }
        AuditEvent::PlaybackHealth {
            utterance_id,
            health,
        } => {
            let key = format!("{key_prefix}/{utterance_id:03}_playback.json");
            store
                .put(&key, serde_json::to_string_pretty(&health)?.as_bytes())
                .await?;
        }
        AuditEvent::AssistantTranscriptDelta {
            utterance_id,
            delta_idx,
            text,
            audio_ms,
        } => {
            // Append-only JSONL — one line per delta. End offset is
            // implicit (= next entry's `audio_ms`, or the turn's total
            // audio_ms for the last entry). Keeps the on-disk cost to
            // a single u64 per delta as planned.
            let key = format!("{key_prefix}/{utterance_id:03}_transcript_deltas.jsonl");
            let line = serde_json::json!({
                "utterance_id": utterance_id,
                "delta_idx": delta_idx,
                "text": text,
                "audio_ms": audio_ms,
            });
            let mut buf = serde_json::to_vec(&line)?;
            buf.push(b'\n');
            store.append(&key, &buf).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/streaming.rs"]
mod tests;

/// One `pipeline_events.jsonl` line: the event's own fields plus a `timestamp`
/// in the shape trace events already use (`secs_since_epoch` /
/// `nanos_since_epoch`), so one reader handles both files.
fn pipeline_event_line(
    event: &crate::pipeline::events::PipelineEvent,
    at: std::time::SystemTime,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut json = serde_json::to_value(event)?;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("timestamp".into(), serde_json::to_value(at)?);
    }
    serde_json::to_vec(&json)
}

#[cfg(test)]
mod pipeline_line_tests {
    use super::*;

    /// An interruption with no time can be reported but not placed.
    #[test]
    fn a_pipeline_event_line_says_when_it_happened() {
        let at = std::time::UNIX_EPOCH + std::time::Duration::new(1_790_013_214, 15_000_000);
        let event = crate::pipeline::events::PipelineEvent::Interrupted {
            utterance_id: 2,
            trigger: crate::pipeline::events::InterruptionTrigger::ServerVad,
            played_ms: None,
        };
        let line: serde_json::Value =
            serde_json::from_slice(&pipeline_event_line(&event, at).unwrap()).unwrap();

        // The event's own fields are untouched, so existing readers keep working.
        assert_eq!(line["kind"], "interrupted");
        assert_eq!(line["utterance_id"], 2);
        assert_eq!(line["trigger"], "server_vad");
        // And it carries its time, in the shape the report's reader looks for.
        assert_eq!(line["timestamp"]["secs_since_epoch"], 1_790_013_214u64);
        assert_eq!(line["timestamp"]["nanos_since_epoch"], 15_000_000u64);
    }
}
