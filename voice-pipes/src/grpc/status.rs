//! Map domain provider errors to gRPC [`Status`] codes.
//!
//! Two goals: give clients an actionable `Code` (not a blanket `Internal`), and
//! don't echo internal error text (model paths, stack detail) to the wire.
//! Client-actionable errors (bad request, auth, rate-limit) carry a short
//! message; internal failures map to a generic message and are logged
//! server-side by the caller.

use tonic::Status;

use crate::stt::SttError;
use crate::text::TextProcessorError;
use crate::tts::TtsError;

/// Map an upstream HTTP-ish status to a gRPC status without leaking the body.
fn api_status(status: u16, kind: &str) -> Status {
    match status {
        400 | 422 => Status::invalid_argument(format!("{kind} provider rejected the request")),
        401 | 403 => Status::unauthenticated(format!("{kind} provider authentication failed")),
        404 => Status::not_found(format!("{kind} provider resource not found")),
        429 => Status::resource_exhausted(format!("{kind} provider rate limited")),
        500..=599 => Status::unavailable(format!("{kind} provider unavailable")),
        _ => Status::internal(format!("{kind} provider error")),
    }
}

/// Map [`SttError`] to a gRPC [`Status`].
pub(crate) fn stt_error_to_status(e: &SttError) -> Status {
    match e {
        SttError::ConfigError(m) | SttError::InvalidAudio(m) => Status::invalid_argument(m.clone()),
        SttError::AuthError(_) => Status::unauthenticated("authentication failed"),
        SttError::RateLimited { .. } => Status::resource_exhausted("rate limited by provider"),
        SttError::Timeout(_) => Status::deadline_exceeded("transcription timed out"),
        SttError::ChannelClosed => Status::aborted("transcription stream closed"),
        SttError::ApiError { status, .. } => api_status(*status, "STT"),
        SttError::NetworkError(_) => Status::unavailable("upstream network error"),
        // ModelLoadFailed / InferenceFailed and any future (non_exhaustive)
        // variant map to a generic internal error (detail logged server-side).
        _ => Status::internal("internal transcription error"),
    }
}

/// Map [`TtsError`] to a gRPC [`Status`].
pub(crate) fn tts_error_to_status(e: &TtsError) -> Status {
    match e {
        TtsError::ConfigError(m) => Status::invalid_argument(m.clone()),
        TtsError::AuthError(_) => Status::unauthenticated("authentication failed"),
        TtsError::RateLimited { .. } => Status::resource_exhausted("rate limited by provider"),
        TtsError::ApiError { status, .. } => api_status(*status, "TTS"),
        TtsError::NetworkError(_) => Status::unavailable("upstream network error"),
        // PhonemizationFailed / InferenceFailed / ModelLoadFailed and any future
        // (non_exhaustive) variant map to a generic internal error.
        _ => Status::internal("internal synthesis error"),
    }
}

/// Map [`TextProcessorError`] to a gRPC [`Status`]. The struct carries no kind,
/// so it maps to a generic internal error; the detail is logged server-side.
pub(crate) fn text_error_to_status(e: &TextProcessorError) -> Status {
    tracing::debug!(processor = %e.processor, error = %e.message, "text processor error");
    Status::internal("internal text-processing error")
}

#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;
