use super::*;

/// `Debug` on the connection target shows where, never what: the URL
/// may carry a key in its query string and the headers carry tokens.
#[test]
fn connect_info_debug_hides_credentials() {
    let info = ConnectInfo::new("wss://generativelanguage.googleapis.com/ws?key=AIza-secret")
        .with_header("Authorization", "Bearer sk-secret")
        .with_header("xi-api-key", "xi-secret");
    let s = format!("{info:?}");
    assert!(s.contains("generativelanguage.googleapis.com"), "{s}");
    assert!(s.contains("Authorization"), "{s}");
    for leaked in ["AIza-secret", "sk-secret", "xi-secret", "key="] {
        assert!(!s.contains(leaked), "leaked {leaked}: {s}");
    }
}

/// `Debug` on a session's settings redacts a secret-named provider option
/// and shows the rest.
#[test]
fn session_config_debug_redacts_secret_options() {
    let cfg = SessionConfig::new("be brief").with_provider_options(
        [
            ("user_api_key".to_string(), serde_json::json!("sk-secret")),
            ("language".to_string(), serde_json::json!("hr")),
        ]
        .into_iter()
        .collect(),
    );
    let s = format!("{cfg:?}");
    assert!(!s.contains("sk-secret"), "{s}");
    assert!(s.contains("hr"), "{s}");
}

/// A session waits to be spoken to and transcribes what it hears unless
/// told otherwise: what an agent answering a person needs.
#[test]
fn a_session_defaults_to_answering_and_transcribing() {
    let cfg = SessionConfig::new("x");
    assert!(!cfg.speaks_first);
    assert!(cfg.transcribe_input);
    let caller = cfg.with_speaks_first(true).with_transcribe_input(false);
    assert!(caller.speaks_first && !caller.transcribe_input);
}

/// The close observed live from `ElevenLabs` when account credits run
/// out (code 1002 + quota text) — must not be retried.
#[test]
fn elevenlabs_quota_close_is_fatal() {
    assert_eq!(
        default_classify_close(Some(1002), "This request exceeds your quota limit."),
        CloseClass::Fatal
    );
}

#[test]
fn policy_violation_code_is_fatal_regardless_of_text() {
    assert_eq!(default_classify_close(Some(1008), ""), CloseClass::Fatal);
}

#[test]
fn auth_texts_are_fatal() {
    for reason in [
        "API key not valid. Please pass a valid API key.",
        "Incorrect API key provided",
        "401 Unauthorized",
        "Permission denied on resource",
    ] {
        assert_eq!(
            default_classify_close(Some(1011), reason),
            CloseClass::Fatal,
            "{reason}"
        );
    }
}

#[test]
fn ordinary_closes_are_transient() {
    for (code, reason) in [
        (Some(1000), "normal closure"),
        (Some(1001), "going away"),
        (Some(1011), "internal server error"),
        (None, "stream ended (None)"),
        (Some(1006), "abnormal closure"),
    ] {
        assert_eq!(
            default_classify_close(code, reason),
            CloseClass::Transient,
            "{reason}"
        );
    }
}

/// The host is all `Debug` keeps of a URL: credentials in the authority,
/// the port, the path and the query are dropped, and IPv6 brackets are
/// taken off.
#[test]
fn only_the_host_is_taken_from_a_url() {
    for (url, host) in [
        (
            "wss://api.openai.com/v1/realtime?model=x",
            Some("api.openai.com"),
        ),
        ("wss://user:pw@example.com:8443/ws", Some("example.com")),
        ("ws://127.0.0.1:9000", Some("127.0.0.1")),
        ("ws://[::1]:9000/ws", Some("::1")),
        ("wss://host?key=secret", Some("host")),
        ("not a url", None),
        ("wss:///path", None),
    ] {
        assert_eq!(url_host(url), host, "{url}");
    }
}
