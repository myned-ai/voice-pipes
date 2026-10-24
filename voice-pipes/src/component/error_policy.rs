//! Per-component error policy and streaming-error classification.

use super::{Component, ErrorPolicy};

/// What the orchestrator should do when a streaming provider call returns
/// an error. Distilled from [`ErrorPolicy`] by [`classify_streaming_error`]
/// so that the call sites in `pipeline::orchestrate` and `pipeline::tts`
/// stay simple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    /// Bubble the error to the client (emit `ServerControl::Error`).
    Propagate,
    /// Swallow the error: do not emit a client-facing `Error` message.
    /// The pipeline continues; whatever frames were already streamed remain.
    Suppress,
    /// Retry the call up to `max_attempts` times with exponential backoff.
    /// Only emitted for components that support a retry-aware call site
    /// (today: TTS via [`pipeline::tts::spawn_synthesis_task`]). Other call
    /// sites should treat this the same as [`ErrorAction::Propagate`] --
    /// see [`classify_streaming_error`] for which kinds get `Retry`.
    Retry { max_attempts: u8 },
}

/// Resolve a streaming-call error against the component's [`ErrorPolicy`].
///
/// Honoured policies:
/// * `Propagate` -- default, everywhere.
/// * `LogAndContinue` -- everywhere; suppresses the client-facing error.
/// * `RetryWithBackoff { max }` -- **TTS and text (LLM)** today. Returned
///   as [`ErrorAction::Retry`]. The TTS task replays the synthesis call
///   up to `max` times with exponential backoff, gated on no audio having
///   been forwarded yet (so the client never hears duplicated audio). The
///   LLM call site applies the same gate against the first emitted token
///   so the client never sees duplicated text. STT is intentionally
///   excluded -- its bidirectional audio stream cannot be safely replayed
///   mid-utterance; falls through to `Propagate` with a warning.
pub fn classify_streaming_error(component: Option<&dyn Component>, kind_str: &str) -> ErrorAction {
    let (policy, comp_id) = match component {
        Some(c) => (c.error_policy(), Some(c.id().to_string())),
        None => (ErrorPolicy::Propagate, None),
    };
    match policy {
        ErrorPolicy::LogAndContinue => {
            tracing::warn!(
                component = ?comp_id,
                kind = kind_str,
                policy = "log_and_continue",
                "swallowing provider error per component policy"
            );
            ErrorAction::Suppress
        }
        ErrorPolicy::RetryWithBackoff { max } => {
            // TTS and text (LLM) have retry-aware call sites; both gate
            // retries on no output having been forwarded to the client
            // yet. STT cannot safely retry a partial bidirectional audio
            // stream, so it keeps the previous warn+propagate behaviour.
            if kind_str == "tts" || kind_str == "text" {
                tracing::info!(
                    component = ?comp_id,
                    kind = kind_str,
                    policy = "retry_with_backoff",
                    max,
                    "retry policy active; failing call will be replayed before output is forwarded"
                );
                ErrorAction::Retry { max_attempts: max }
            } else {
                tracing::warn!(
                    component = ?comp_id,
                    kind = kind_str,
                    policy = "retry_with_backoff",
                    max,
                    "retry policy not yet supported for this component kind; falling back to propagate"
                );
                ErrorAction::Propagate
            }
        }
        // `Propagate`, and a policy this build does not know: surface the error.
        _ => ErrorAction::Propagate,
    }
}

#[cfg(test)]
#[path = "tests/error_policy.rs"]
mod tests;
