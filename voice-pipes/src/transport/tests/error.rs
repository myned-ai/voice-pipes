use super::{ErrorCode, looks_like_connection_failure, with_backend_hint};

#[test]
fn error_code_roundtrips_through_u16() {
    // Every variant must survive as_u16 -> from_u16 so the wire protocol
    // and the client can resolve codes back to their meaning. Guards
    // against adding a variant but forgetting the `from_u16` arm.
    for code in [
        ErrorCode::HandshakeTimeout,
        ErrorCode::SessionCreateFailed,
        ErrorCode::SessionResumeFailed,
        ErrorCode::InvalidHandshake,
        ErrorCode::RealtimeConnectFailed,
        ErrorCode::RealtimeConfigureFailed,
        ErrorCode::RealtimeProviderError,
        ErrorCode::VadInitializationFailed,
        ErrorCode::SttWarmupFailed,
        ErrorCode::LlmRetriesExhausted,
        ErrorCode::LlmWarmupFailed,
        ErrorCode::InjectionFailed,
        ErrorCode::TtsSynthesisFailed,
        ErrorCode::TtsWarmupFailed,
    ] {
        assert_eq!(ErrorCode::from_u16(code.as_u16()), Some(code));
    }
}

#[test]
fn detects_connection_failures() {
    // The exact shape the user reported (Windows, connection refused).
    let real = "OpenAI TTS request failed: error sending request for url \
        (http://localhost:8091/v1/audio/speech): error trying to connect: \
        tcp connect error: No connection could be made because the target \
        machine actively refused it. (os error 10061)";
    assert!(looks_like_connection_failure(real));
    assert!(looks_like_connection_failure(
        "Connection refused (os error 111)"
    ));
    assert!(looks_like_connection_failure(
        "dns error: could not resolve host"
    ));
}

#[test]
fn ignores_non_connection_errors() {
    assert!(!looks_like_connection_failure("model 'foo' not found"));
    assert!(!looks_like_connection_failure("invalid voice id"));
    assert!(!looks_like_connection_failure(
        "HTTP 500 internal server error"
    ));
}

#[test]
fn appends_hint_only_for_connection_failures() {
    let conn = with_backend_hint("tcp connect error: actively refused".into(), "TTS");
    assert!(conn.contains("TTS backend appears to be unreachable"));
    assert!(conn.contains("running and reachable"));

    let other = with_backend_hint("invalid voice id".into(), "TTS");
    assert_eq!(other, "invalid voice id");
}
