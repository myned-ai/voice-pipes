//! Turn lifecycle tracking and metrics emission.
//!
//! Bundles the mutable per-turn state and provides helpers for the
//! common patterns: starting a turn, recording TTFB, emitting the
//! audit + client metrics on turn end, and resetting to Listening.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditEvent, StreamingAuditSink};
use crate::pipeline::events::PipelineEvent;
use crate::pipeline::metrics::TurnMetricsSnapshot;
use crate::pipeline::{AudioChunk, SharedTurnState, TurnState, get_turn_state};
use crate::transport::serve::gateway::state_machine::{self, TurnTransition, TurnTrigger};
use crate::transport::{PipelineState, ServerControl, ServerMessage};

/// Short grace period for `process_utterance` to finish sending metrics
/// after the end-of-utterance audio chunk has been forwarded.
pub(super) const METRICS_GRACE: Duration = Duration::from_millis(500);

/// Emit a telemetry/lifecycle record on a bounded orchestrator channel without
/// ever blocking the runner. On a full channel the record is dropped
/// with a warning (a stalled orchestrator forwarder is a real backpressure
/// signal); a closed channel (consumer gone, e.g. shutdown) is silent.
pub(super) fn emit_record<T>(tx: &mpsc::Sender<T>, record: T, kind: &str) {
    // Only a full channel is worth a warning (real backpressure); `Ok` and a
    // closed channel (consumer gone at shutdown) both fall through silently.
    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(record) {
        tracing::warn!(kind, "orchestrator telemetry channel full; dropping record");
    }
}

/// Turn a client playback report into an observation.
///
/// `arrival_rtf` is the sharpest downlink signal available: wall time to
/// receive a turn's audio over the duration of that audio. Above 1.0 the
/// client could not be fed in real time. Both fields come free with a report
/// the client already sends after every turn.
pub(crate) fn playback_observation(
    health: &crate::api::types::PlaybackHealth,
) -> crate::transport::adaptive::Observation {
    let arrival_rtf = (health.audio_ms > 0 && health.arrival_wall_ms > 0)
        .then(|| health.arrival_wall_ms as f32 / health.audio_ms as f32);
    crate::transport::adaptive::Observation {
        arrival_rtf,
        underruns: Some(health.underrun_count),
        ..Default::default()
    }
}

// ── Turn tracker ────────────────────────────────────────────

/// Groups all mutable per-turn state in the orchestration loop.
pub(super) struct TurnTracker {
    /// Microphone chunks dropped because the speech path was full, and
    /// whether it is full now (so the warning is logged once per backlog).
    pub mic_chunks_dropped: u64,
    pub mic_backlogged: bool,
    pub current_cancel: Option<CancellationToken>,
    pub utterance_start: Option<Instant>,
    pub current_utterance_id: u32,
    pub ttfb_recorded: Option<Duration>,
    pub text_utterance_counter: u32,
    pub audio_chunk_count: u64,
    /// Highest utterance id for which a terminal client playback event
    /// (`PlaybackComplete` or `PlaybackReport`) has already been processed.
    /// Utterance ids are monotonically increasing, so any subsequent
    /// playback event with `utterance_id <= last_finalized_utterance_id`
    /// is a stale duplicate and must be ignored.
    pub last_finalized_utterance_id: Option<u32>,
    /// Client-reported timing events captured during the current turn,
    /// in arrival order.  Drained into `SessionDetail.client_timings`
    /// when the `TurnRecord` is built; cleared on every `start_turn`.
    pub client_timings: Vec<crate::api::types::ClientTimingEntry>,
    /// Client-reported playback health for the current turn, from the
    /// `playback_quality` control frame. Taken into
    /// `SessionDetail.playback_health` when the turn record is built;
    /// cleared on every `start_turn`. NOTE: playback (and therefore the
    /// quality frame) usually outlives server-side turn completion — when
    /// the frame arrives after the record was emitted, the health still
    /// reaches the admin via the `NNN_playback.json` capture artifact,
    /// which is the primary consumer.
    pub playback_health: Option<crate::api::types::PlaybackHealth>,
    /// Running session-level aggregate metrics, updated on every turn
    /// completion via [`emit_turn_metrics`]. Drained into the final
    /// `ServerControl::SessionSummary` frame just before `Closing`.
    pub session_aggregator: SessionAggregator,
    /// The end-of-utterance chunk for the current turn has been forwarded
    /// to the client: the pipeline has nothing more to produce and the
    /// client may legitimately report playback complete.
    pub eou_sent: bool,
    /// `TurnMetrics` for the current turn have been emitted (and folded
    /// into the session aggregate). An interrupt arriving after this point
    /// only cuts the client-side tail; it must not emit a second snapshot.
    pub metrics_emitted: bool,
}

/// Running aggregate of per-turn metrics for a single session.
///
/// Fed by [`emit_turn_metrics`] on every turn completion (interrupted
/// or otherwise) and consumed once at session shutdown to build
/// [`ServerControl::SessionSummary`].
#[derive(Debug, Default)]
pub(super) struct SessionAggregator {
    pub turns: u32,
    /// User-initiated turns only — the denominator for `avg_total_ms`
    /// (agent-initiated turns are excluded from latency averages).
    pub user_turns: u32,
    pub interrupted_turns: u32,
    pub ttft_sum_ms: u64,
    pub ttft_count: u32,
    pub ttfb_sum_ms: u64,
    pub ttfb_count: u32,
    pub total_ms_sum: u64,
    pub total_audio_duration_ms: u64,
}

impl SessionAggregator {
    /// Fold a completed turn's snapshot into the running totals.
    pub fn record(&mut self, snap: &TurnMetricsSnapshot) {
        self.turns += 1;
        if snap.interrupted {
            self.interrupted_turns += 1;
        }
        // Agent-initiated turns have no user-utterance anchor, so their
        // latencies are measured from turn-open — folding them into the
        // user-perceived averages would skew the session summary. They
        // still count toward turn totals and audio duration.
        let user_initiated = snap.initiator.is_user();
        if user_initiated {
            if let Some(v) = snap.ttft_ms() {
                self.ttft_sum_ms = self.ttft_sum_ms.saturating_add(v);
                self.ttft_count += 1;
            }
            if let Some(v) = snap.ttfb_ms() {
                self.ttfb_sum_ms = self.ttfb_sum_ms.saturating_add(v);
                self.ttfb_count += 1;
            }
            self.total_ms_sum = self.total_ms_sum.saturating_add(snap.total_ms);
            self.user_turns += 1;
        }
        if let Some(v) = snap
            .component_by_kind("tts")
            .and_then(|c| c.extras.get("audio_duration_ms"))
            .and_then(serde_json::Value::as_u64)
        {
            self.total_audio_duration_ms = self.total_audio_duration_ms.saturating_add(v);
        }
    }

    fn avg(sum: u64, count: u32) -> Option<u64> {
        if count == 0 {
            None
        } else {
            Some(sum / count as u64)
        }
    }

    /// Build the wire-shaped [`ServerControl::SessionSummary`] payload.
    pub fn build_summary(&self) -> ServerControl {
        ServerControl::SessionSummary {
            turns: self.turns,
            interrupted_turns: self.interrupted_turns,
            avg_ttft_ms: Self::avg(self.ttft_sum_ms, self.ttft_count),
            avg_ttfb_ms: Self::avg(self.ttfb_sum_ms, self.ttfb_count),
            avg_total_ms: Self::avg(self.total_ms_sum, self.user_turns),
            total_audio_duration_ms: self.total_audio_duration_ms,
        }
    }
}

impl TurnTracker {
    pub fn new() -> Self {
        Self {
            mic_chunks_dropped: 0,
            mic_backlogged: false,
            current_cancel: None,
            utterance_start: None,
            current_utterance_id: 0,
            ttfb_recorded: None,
            text_utterance_counter: crate::transport::TEXT_UTTERANCE_ID_BASE,
            audio_chunk_count: 0,
            last_finalized_utterance_id: None,
            client_timings: Vec::new(),
            playback_health: None,
            session_aggregator: SessionAggregator::default(),
            eou_sent: false,
            metrics_emitted: false,
        }
    }

    /// Decide whether a client-reported playback event for `utterance_id`
    /// should be acted on or rejected as stale/out-of-order.
    ///
    /// Returns `Some(reason)` describing why the event is stale (suitable
    /// for logging), or `None` when the event is current and should be
    /// processed. The decision is based purely on monotonic utterance
    /// identity:
    ///
    /// - The reported `utterance_id` does not match the active turn
    ///   (out-of-order, replayed, or for a turn that never started).
    /// - This utterance has already been finalized by a prior playback
    ///   event (duplicate / replayed delivery).
    pub fn stale_playback_reason(&self, utterance_id: u32) -> Option<&'static str> {
        if utterance_id != self.current_utterance_id {
            return Some("utterance id mismatch");
        }
        if let Some(last) = self.last_finalized_utterance_id
            && last >= utterance_id
        {
            return Some("already finalized");
        }
        None
    }

    /// Record that a terminal playback event has been processed for the
    /// given utterance id, so duplicates can be detected and dropped.
    pub fn mark_playback_finalized(&mut self, utterance_id: u32) {
        let new_high = match self.last_finalized_utterance_id {
            Some(prev) => prev.max(utterance_id),
            None => utterance_id,
        };
        self.last_finalized_utterance_id = Some(new_high);
    }

    /// Begin a new turn with the given utterance ID and cancellation token.
    #[cfg(test)]
    pub fn start_turn(&mut self, utterance_id: u32, cancel: CancellationToken) {
        self.start_turn_at(utterance_id, cancel, None);
    }

    /// Begin a new turn anchored at `committed_at` (when available from
    /// [`crate::pipeline::Utterance::committed_at`]) or `Instant::now()`.
    pub fn start_turn_at(
        &mut self,
        utterance_id: u32,
        cancel: CancellationToken,
        committed_at: Option<Instant>,
    ) {
        self.current_cancel = Some(cancel);
        self.utterance_start = Some(committed_at.unwrap_or_else(Instant::now));
        self.current_utterance_id = utterance_id;
        self.ttfb_recorded = None;
        self.client_timings.clear();
        self.playback_health = None;
        self.eou_sent = false;
        self.metrics_emitted = false;
    }

    /// Allocate the next text-input utterance ID.
    pub fn next_text_id(&mut self) -> u32 {
        self.text_utterance_counter += 1;
        self.text_utterance_counter
    }

    /// Record TTFB on the first non-empty audio chunk.
    pub fn record_ttfb(&mut self, samples_non_empty: bool) {
        if self.ttfb_recorded.is_none()
            && samples_non_empty
            && let Some(start) = self.utterance_start
        {
            self.ttfb_recorded = Some(start.elapsed());
        }
    }

    /// Elapsed time since the current turn started.
    pub fn total_latency(&self) -> Duration {
        self.utterance_start
            .map(|s| s.elapsed())
            .unwrap_or_default()
    }

    /// Clear turn-specific state after a turn ends.
    pub fn finish_turn(&mut self) {
        self.utterance_start = None;
    }
}

// ── Turn-end helpers ────────────────────────────────────────

/// Discard any snapshot still queued on the metrics channel.
///
/// Called before a new turn starts: a snapshot left behind by a turn that
/// was abandoned (interrupted after its grace period, or cut short by a
/// client `PlaybackComplete`) would otherwise be collected as the next
/// turn's metrics.
pub(super) fn drain_stale_metrics(
    session_id: &str,
    metrics_rx: &mut mpsc::Receiver<TurnMetricsSnapshot>,
) {
    let mut dropped = 0u32;
    while let Ok(stale) = metrics_rx.try_recv() {
        dropped += 1;
        tracing::debug!(
            session_id,
            stale_utterance_id = stale.utterance_id,
            "dropping metrics snapshot left over from an abandoned turn"
        );
    }
    if dropped > 0 {
        tracing::warn!(
            session_id,
            dropped,
            "stale turn metrics discarded before starting a new turn"
        );
    }
}

/// Wire state name for a turn state.
fn pipeline_state(state: TurnState) -> PipelineState {
    match state {
        TurnState::Listening => PipelineState::Listening,
        TurnState::Processing => PipelineState::Processing,
        TurnState::Speaking => PipelineState::Speaking,
        TurnState::Draining => PipelineState::Draining,
    }
}

/// Apply a turn-state trigger and tell the client only when the machine
/// actually moved.
///
/// The state machine is the authority: a rejected trigger leaves the shared
/// state untouched and sends nothing, so the client is never told about a
/// state the server did not enter. Rejections are logged because they mean
/// two code paths disagree about where the turn is; an idempotent repeat
/// (`NoChange`) is silent. Every state change in the classic runner goes
/// through here.
pub(super) async fn transition(
    session_id: &str,
    turn_state: &SharedTurnState,
    out_tx: &mpsc::Sender<ServerMessage>,
    trigger: TurnTrigger,
) -> Option<TurnState> {
    let from = get_turn_state(turn_state);
    match state_machine::apply_verdict(turn_state, trigger) {
        TurnTransition::To(to) => {
            let _ = out_tx
                .send(ServerMessage::Control(ServerControl::StateChanged {
                    state: pipeline_state(to),
                }))
                .await;
            Some(to)
        }
        TurnTransition::NoChange => None,
        TurnTransition::Rejected => {
            tracing::warn!(
                session_id,
                from = ?from,
                trigger = ?trigger,
                "turn-state transition rejected; state unchanged and client not notified"
            );
            None
        }
    }
}

/// Collect pipeline metrics, optionally waiting with a grace period.
pub(super) async fn collect_metrics(
    metrics_rx: &mut mpsc::Receiver<TurnMetricsSnapshot>,
    wait: bool,
) -> TurnMetricsSnapshot {
    if wait {
        tokio::time::timeout(METRICS_GRACE, metrics_rx.recv())
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    } else {
        metrics_rx.try_recv().ok().unwrap_or_default()
    }
}

/// Emit the `UtteranceDone` audit event, the `TurnMetrics` control frame
/// to the connected client, and the corresponding `TurnMetricsReady`
/// pipeline event for external consumers.
///
/// `pipe_metrics` is the snapshot built by `pipeline::process_utterance`;
/// this helper amends it with the runner-side `total_ms`, `interrupted`
/// flag, and TTFB (which is observed at the runner, not in the pipeline
/// task). Returns the final snapshot so the caller can re-use it (e.g.
/// to populate the persisted `TurnRecord` without re-deriving fields).
pub(super) async fn emit_turn_metrics(
    tracker: &mut TurnTracker,
    interrupted: bool,
    pipe_metrics: &TurnMetricsSnapshot,
    audit: &Option<StreamingAuditSink>,
    out_tx: &mpsc::Sender<ServerMessage>,
    pe_tx: &tokio::sync::broadcast::Sender<PipelineEvent>,
    stt_name: &str,
    tts_name: &str,
) -> TurnMetricsSnapshot {
    let total_latency = tracker.total_latency();

    if let Some(sink) = audit {
        sink.record(AuditEvent::UtteranceDone {
            utterance_id: tracker.current_utterance_id,
            interrupted,
            terminated: false,
            stt_provider: stt_name.to_string(),
            tts_provider: tts_name.to_string(),
        });
    }

    // Amend the pipeline-side snapshot with runner-observed fields.
    let mut snapshot = pipe_metrics.clone();
    snapshot.utterance_id = tracker.current_utterance_id;
    snapshot.total_ms = total_latency.as_millis() as u64;
    snapshot.interrupted = interrupted;
    if let Some(ttfb) = tracker.ttfb_recorded {
        // `tracker.ttfb_recorded` is the commit-anchored runner span
        // (`t_first_sent_audio - commit_start`). Do NOT overwrite
        // `tts.extras["ttfb_ms"]` (which is the TTS-only wait, `tts_wait_ms`);
        // instead update `commit_to_first_audio_ms` and `output_wait_ms` so the
        // telescoping sum `llm_wait_ms + tts_wait_ms + output_wait_ms ==
        // commit_to_first_audio_ms` includes the runner's `proc_audio_tx` hop.
        let runner_commit_ms = crate::pipeline::extras::round_tenths_ms(ttfb);
        let llm_wait = snapshot
            .component_by_kind("llm")
            .and_then(|c| c.extras.get("llm_wait_ms"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let tts_wait = snapshot
            .component_by_kind("tts")
            .and_then(|c| c.extras.get("tts_wait_ms"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let runner_output_ms =
            (((runner_commit_ms - llm_wait - tts_wait).max(0.0)) * 10.0).round() / 10.0;
        if let Some(tts) = snapshot.components.iter_mut().find(|c| c.kind == "tts") {
            tts.extras.insert(
                "commit_to_first_audio_ms".into(),
                serde_json::json!(runner_commit_ms),
            );
            tts.extras
                .insert("output_wait_ms".into(), serde_json::json!(runner_output_ms));
        }
        if let Some(output) = snapshot.components.iter_mut().find(|c| c.kind == "output") {
            output.extras.insert(
                "commit_to_first_audio_ms".into(),
                serde_json::json!(runner_commit_ms),
            );
            output
                .extras
                .insert("output_wait_ms".into(), serde_json::json!(runner_output_ms));
        }
    }

    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::TurnMetrics {
            snapshot: snapshot.clone(),
        }))
        .await;

    let _ = pe_tx.send(PipelineEvent::TurnMetricsReady {
        utterance_id: tracker.current_utterance_id,
        metrics: snapshot.clone(),
    });

    tracker.session_aggregator.record(&snapshot);
    tracker.metrics_emitted = true;

    snapshot
}

/// Transition the pipeline to Draining: drain stale audio, update turn
/// state, and notify the client. We wait for client `PlaybackComplete` before moving to Listening.
pub(super) async fn transition_to_draining(
    session_id: &str,
    tracker: &mut TurnTracker,
    turn_state: &SharedTurnState,
    out_tx: &mpsc::Sender<ServerMessage>,
    proc_audio_rx: &mut mpsc::Receiver<AudioChunk>,
) {
    tracker.finish_turn();
    while proc_audio_rx.try_recv().is_ok() {}
    transition(session_id, turn_state, out_tx, TurnTrigger::Draining).await;
}

/// Server-side short-circuit for the `Draining -> Listening` handshake when
/// the client has no way to drive it.
///
/// The normal `Draining -> Listening` transition is gated on the client
/// sending `PlaybackComplete` (after its Web Audio queue empties or after
/// it processes a `ClearAudio` frame). For an *early* server-VAD barge
/// where the turn was cancelled while still in `Processing`, no audio
/// frame was ever emitted, no `ClearAudio` is sent, and the client never
/// schedules anything in the Web Audio queue -- so neither `handleClearAudio`
/// nor `handlePlaybackEnd` fires and `PlaybackComplete` never arrives.
/// Without this short-circuit the session hangs in `Draining` forever.
///
/// This helper performs the full `Draining -> Listening` sequence
/// server-side and marks the utterance as finalized so any late
/// `PlaybackComplete`/`PlaybackReport` for the same id is deterministically
/// rejected as stale by [`TurnTracker::stale_playback_reason`].
pub(super) async fn transition_early_barge_to_listening(
    session_id: &str,
    tracker: &mut TurnTracker,
    turn_state: &SharedTurnState,
    out_tx: &mpsc::Sender<ServerMessage>,
    proc_audio_rx: &mut mpsc::Receiver<AudioChunk>,
) -> u32 {
    transition_to_draining(session_id, tracker, turn_state, out_tx, proc_audio_rx).await;
    let utt_id = tracker.current_utterance_id;
    tracker.mark_playback_finalized(utt_id);
    transition(
        session_id,
        turn_state,
        out_tx,
        TurnTrigger::PlaybackFinalized,
    )
    .await;
    utt_id
}

// ── Record builders ─────────────────────────────────────────

/// Build a `TurnRecord` for the orchestrator after a completed turn.
///
/// `snapshot` is the same `TurnMetricsSnapshot` that was sent to the
/// client and broadcast on `PipelineEvent::TurnMetricsReady`; persisting
/// it directly on `SessionDetail.latency_breakdown` guarantees the
/// audit store stays in sync with the live wire view.
///
/// Drains the tracker's `client_timings` buffer into
/// `SessionDetail.client_timings` so per-turn client-side playback
/// timing is preserved on the persisted record.
pub(super) fn build_turn_record(
    session_id: &str,
    snapshot: &TurnMetricsSnapshot,
    tracker: &mut TurnTracker,
    stt_name: &str,
    tts_name: &str,
    client_sample_rate: u32,
    negotiated_output: Option<u32>,
    pipeline_name: &Option<String>,
    tags: Vec<String>,
) -> crate::api::types::TurnRecord {
    let total_ms = snapshot.total_ms;
    let client_timings = std::mem::take(&mut tracker.client_timings);
    let playback_health = tracker.playback_health.take();
    crate::api::types::TurnRecord {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        worker_id: String::new(),
        started_at: chrono::Utc::now() - chrono::Duration::milliseconds(total_ms as i64),
        completed_at: Some(chrono::Utc::now()),
        status: crate::api::types::SessionStatus::Completed,
        total_ms: Some(total_ms),
        stt_provider: Some(stt_name.to_string()),
        tts_provider: Some(tts_name.to_string()),
        text_preview: None,
        otel_trace_id: None,
        error: None,
        detail: Some(crate::api::types::SessionDetail {
            pipeline_name: pipeline_name.clone(),
            input: None,
            stt: Some(crate::api::types::SessionStt {
                provider: stt_name.to_string(),
                input_sample_rate: client_sample_rate,
                resampled_from: None,
                // Preserve `None` instead of pushing `0` placeholders that
                // would skew the orchestrator latency histogram.
                latency_ms: snapshot.stt_latency_ms(),
                text: String::new(),
            }),
            tts: snapshot.ttfb_ms().map(|ms| crate::api::types::SessionTts {
                provider: tts_name.to_string(),
                latency_ms: Some(ms),
                native_sample_rate: 0,
                output_sample_rate: negotiated_output.unwrap_or(0),
                resampled: false,
                audio_duration_s: 0.0,
            }),
            text_latency_ms: snapshot.ttft_ms(),
            realtime_factor: None,
            latency_breakdown: Some(snapshot.clone()),
            client_timings,
            playback_health,
        }),
        pipeline_name: pipeline_name.clone(),
        pipeline_config_hash: None,
        tags,
        events: vec![],
    }
}

/// Build a `TurnRecord` for a realtime (S2S) turn.
///
/// Mirrors [`build_turn_record`] but does not require a [`TurnTracker`]
/// — the realtime runner does not maintain one (no STT/LLM/TTS
/// component pipeline to track), so the snapshot returned by
/// [`RealtimeTurn::finish`] is the sole source of per-turn data.
///
/// `provider_label` is recorded as both the `stt_provider` and
/// `tts_provider` so the admin Sessions / Turns views show the
/// realtime provider name (e.g. `"openai-realtime"`) where they
/// would normally show the STT and TTS components.  `text_preview`
/// is the assistant's transcript so the list view shows a readable
/// summary instead of a blank row.
#[expect(
    clippy::too_many_arguments,
    reason = "assembles a record from every per-turn signal; a param struct would just relocate the list"
)]
#[cfg(feature = "realtime")]
pub(super) fn build_realtime_turn_record(
    session_id: &str,
    snapshot: &TurnMetricsSnapshot,
    provider_label: &str,
    client_sample_rate: u32,
    output_sample_rate: u32,
    pipeline_name: &Option<String>,
    tags: Vec<String>,
    user_transcript: Option<String>,
    playback_health: Option<crate::api::types::PlaybackHealth>,
) -> crate::api::types::TurnRecord {
    let total_ms = snapshot.total_ms;
    let assistant_text = snapshot.llm_response_text.clone().unwrap_or_default();
    let text_preview = if assistant_text.is_empty() {
        None
    } else {
        // Trim to a reasonable preview length to match what
        // [`build_turn_record`] would produce after the dashboard
        // truncates long transcripts client-side.
        let trimmed = assistant_text.trim();
        Some(trimmed.chars().take(280).collect::<String>())
    };
    crate::api::types::TurnRecord {
        id: format!("{}-{}", session_id, snapshot.utterance_id),
        session_id: session_id.to_string(),
        worker_id: String::new(),
        started_at: chrono::Utc::now() - chrono::Duration::milliseconds(total_ms as i64),
        completed_at: Some(chrono::Utc::now()),
        // Realtime turns persist with `Completed` status even when
        // interrupted; the `latency_breakdown.interrupted` flag and
        // the `interrupted_*` events on the dashboard convey the
        // barge-in. This matches the non-realtime convention where
        // an interrupted turn is still a successful turn.
        status: crate::api::types::SessionStatus::Completed,
        total_ms: Some(total_ms),
        stt_provider: Some(provider_label.to_string()),
        tts_provider: Some(provider_label.to_string()),
        text_preview,
        otel_trace_id: None,
        error: None,
        detail: Some(crate::api::types::SessionDetail {
            pipeline_name: pipeline_name.clone(),
            input: None,
            stt: Some(crate::api::types::SessionStt {
                provider: provider_label.to_string(),
                input_sample_rate: client_sample_rate,
                resampled_from: None,
                latency_ms: snapshot.stt_latency_ms(),
                // Populated from the realtime provider's
                // `InputTranscript` event, captured by the runner
                // and passed in here. Empty string when the turn
                // finished before the transcript arrived (the
                // admin UI treats this the same as a missing
                // value).
                text: user_transcript.unwrap_or_default(),
            }),
            tts: snapshot.ttfb_ms().map(|ms| crate::api::types::SessionTts {
                provider: provider_label.to_string(),
                latency_ms: Some(ms),
                native_sample_rate: 0,
                output_sample_rate,
                resampled: false,
                audio_duration_s: 0.0,
            }),
            text_latency_ms: snapshot.ttft_ms(),
            realtime_factor: None,
            latency_breakdown: Some(snapshot.clone()),
            client_timings: Vec::new(),
            playback_health,
        }),
        pipeline_name: pipeline_name.clone(),
        pipeline_config_hash: None,
        tags,
        events: vec![],
    }
}

/// Build a `SessionRecord` for session start/end events.
/// Session record sent to the orchestrator at session start and end.
/// `sample_rates` is `(input, output)` once negotiated; the end record
/// passes `None` and the orchestrator keeps what the start record carried.
pub(super) fn make_session_record(
    session_id: &str,
    status: crate::api::types::SessionStatus,
    ended_at: Option<chrono::DateTime<chrono::Utc>>,
    tags: Vec<String>,
    sample_rates: Option<(u32, u32)>,
    state: &super::state::TransportState,
) -> crate::api::types::SessionRecord {
    // Realtime (S2S) pipelines do not have separate `[stt]` / `[tts]`
    // sections — the provider plays both roles. Fall back to the
    // realtime provider's factory name so the admin Sessions list +
    // metadata panel show a useful identifier instead of `null`.
    let (stt_provider, tts_provider) = match (
        state.config.stt.as_ref(),
        state.config.tts.as_ref(),
        state.config.realtime_provider.as_ref(),
    ) {
        (Some(s), Some(t), _) => (Some(s.provider.clone()), Some(t.provider.clone())),
        (Some(s), None, Some(rt)) => (Some(s.provider.clone()), Some(rt.provider.clone())),
        (None, Some(t), Some(rt)) => (Some(rt.provider.clone()), Some(t.provider.clone())),
        (Some(s), None, None) => (Some(s.provider.clone()), None),
        (None, Some(t), None) => (None, Some(t.provider.clone())),
        (None, None, Some(rt)) => (Some(rt.provider.clone()), Some(rt.provider.clone())),
        (None, None, None) => (None, None),
    };
    crate::api::types::SessionRecord {
        id: session_id.to_string(),
        worker_id: String::new(),
        started_at: chrono::Utc::now(),
        ended_at,
        status,
        pipeline_name: state.config.pipeline.name.clone(),
        pipeline_config_hash: Some(crate::config::config_hash(&state.config)),
        stt_provider,
        tts_provider,
        input_sample_rate: sample_rates.map(|(input, _)| input),
        output_sample_rate: sample_rates.map(|(_, output)| output),
        tags,
        turn_count: 0,
    }
}

#[cfg(test)]
#[path = "tests/turn.rs"]
mod tests;
