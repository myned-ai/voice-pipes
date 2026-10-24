//! Golden protocol-trace support.
//!
//! A regression safety net: a deterministic, human-readable capture of the
//! *control-plane* message sequence a driven session emits. By pinning the
//! ordered sequence of server control messages — the surface driven by the
//! turn-state machine — we catch accidental wire/ordering regressions in
//! the session drivers (behind the `EngineDriver` seam) the moment they appear.
//!
//! Why control-plane only: audio and structured `Message` frames
//! interleave with timing jitter and vary in count with TTS chunking, so a
//! byte-for-byte trace of them would be flaky. The control sequence
//! (`session_ready`, `state_changed:*`, `turn_metrics`, `clear_audio`,
//! `text_truncated`, ...) is what the state machine deterministically drives
//! and is exactly what a change to a driver risks breaking.
//!
//! Volatile fields (session ids, tokens, wall-clock timestamps, latencies)
//! are deliberately dropped so the trace is stable across runs.

use crate::transport::{PipelineState, ServerControl, ServerMessage};

/// Normalize a single server message into a stable token, or `None` for
/// non-control frames (audio / structured messages) which are not part of
/// the control-plane golden trace.
pub(crate) fn normalize_control(msg: &ServerMessage) -> Option<String> {
    let ServerMessage::Control(ctrl) = msg else {
        return None;
    };
    Some(match ctrl {
        ServerControl::SessionReady(_) => "session_ready".to_string(),
        ServerControl::Pong { .. } => "pong".to_string(),
        ServerControl::StateChanged { state } => {
            format!("state_changed:{}", state_name(*state))
        }
        ServerControl::QualityUpdate { .. } => "quality_update".to_string(),
        ServerControl::Error { code, .. } => format!("error:{code}"),
        ServerControl::Closing { .. } => "closing".to_string(),
        ServerControl::SessionSummary {
            turns,
            interrupted_turns,
            ..
        } => format!("session_summary:turns={turns}:interrupted={interrupted_turns}"),
        ServerControl::TurnMetrics { snapshot } => {
            format!("turn_metrics:interrupted={}", snapshot.interrupted)
        }
        ServerControl::ClearAudio { reason, .. } => format!("clear_audio:{reason}"),
        ServerControl::TextTruncated { final_text, .. } => {
            format!("text_truncated:'{final_text}'")
        }
        ServerControl::UtteranceDropped { reason, .. } => format!("utterance_dropped:{reason}"),
        ServerControl::TtsOnlyResult { .. } => "tts_only_result".to_string(),
        ServerControl::TtsOnlyError { .. } => "tts_only_error".to_string(),
    })
}

/// Stable lowercase name for a [`PipelineState`].
fn state_name(state: PipelineState) -> &'static str {
    match state {
        PipelineState::Listening => "listening",
        PipelineState::Processing => "processing",
        PipelineState::Speaking => "speaking",
        PipelineState::Draining => "draining",
    }
}

/// Reduce a raw stream of server messages to the control-plane golden trace:
/// control tokens only, with consecutive duplicates collapsed (so a run of
/// audio-driven `state_changed:speaking` re-emissions, if any, does not
/// destabilize the trace).
pub(crate) fn control_trace<'a>(
    messages: impl IntoIterator<Item = &'a ServerMessage>,
) -> Vec<String> {
    let mut trace: Vec<String> = Vec::new();
    for msg in messages {
        if let Some(token) = normalize_control(msg)
            && trace.last() != Some(&token)
        {
            trace.push(token);
        }
    }
    trace
}
