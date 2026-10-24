use super::*;
use tonic::Code;

#[test]
fn stt_errors_map_to_expected_codes() {
    assert_eq!(
        stt_error_to_status(&SttError::InvalidAudio("bad".into())).code(),
        Code::InvalidArgument
    );
    assert_eq!(
        stt_error_to_status(&SttError::AuthError("x".into())).code(),
        Code::Unauthenticated
    );
    assert_eq!(
        stt_error_to_status(&SttError::RateLimited { retry_after: None }).code(),
        Code::ResourceExhausted
    );
    assert_eq!(
        stt_error_to_status(&SttError::InferenceFailed("boom".into())).code(),
        Code::Internal
    );
    // Internal errors must not leak the underlying message.
    let s = stt_error_to_status(&SttError::InferenceFailed("secret-model-path".into()));
    assert!(!s.message().contains("secret-model-path"));
}

#[test]
fn tts_errors_map_to_expected_codes() {
    assert_eq!(
        tts_error_to_status(&TtsError::ConfigError("bad".into())).code(),
        Code::InvalidArgument
    );
    assert_eq!(
        tts_error_to_status(&TtsError::RateLimited { retry_after: None }).code(),
        Code::ResourceExhausted
    );
    assert_eq!(
        tts_error_to_status(&TtsError::ApiError {
            status: 503,
            message: "down".into()
        })
        .code(),
        Code::Unavailable
    );
    assert_eq!(
        tts_error_to_status(&TtsError::InferenceFailed("boom".into())).code(),
        Code::Internal
    );
}

#[test]
fn api_status_maps_http_ranges() {
    assert_eq!(api_status(400, "TTS").code(), Code::InvalidArgument);
    assert_eq!(api_status(401, "TTS").code(), Code::Unauthenticated);
    assert_eq!(api_status(429, "TTS").code(), Code::ResourceExhausted);
    assert_eq!(api_status(500, "TTS").code(), Code::Unavailable);
}
