use super::*;

/// Every marker in every spelling hits; the documented carve-outs do not.
#[test]
fn key_test_covers_every_marker_and_carve_out() {
    for secret in [
        "api_key",
        "openai_api_key",
        "apiKey",
        "api-key",
        "x-api-key",
        "xi-api-key",
        "secret",
        "client_secret",
        "some_secret",
        "password",
        "passwd",
        "token",
        "auth_token",
        "access_token",
        "Authorization",
        "bearer",
        "bearer_token",
        "credential",
        "credentials",
        "private_key",
        "key",
        "KEY",
    ] {
        assert!(is_secret_key(secret), "{secret} must be a secret");
    }
    for plain in [
        "api_key_env",
        "client_token_env",
        "max_tokens",
        "tokens_per_sec",
        "keyterms",
        "monkey",
        "voice",
        "model",
        "endpoint",
    ] {
        assert!(!is_secret_key(plain), "{plain} must not be a secret");
    }
}

/// Table-driven walk: nested tables and arrays, every value type under
/// a secret key, and URLs carrying credentials.
#[test]
fn redaction_corpus() {
    let mut v = serde_json::json!({
        "stt": { "provider": "assemblyai", "api_key": "sk-live-123", "api_key_env": "AAI_KEY" },
        "tts": { "openai_api_key": "sk-oai", "voice": "alloy", "max_tokens": 1024 },
        "text_processor": [
            { "api_key": "sk-tp", "auth_token": "tok", "model": "gemma" },
            { "options": { "headers": { "x-api-key": "hdr", "Authorization": "Bearer t" } } }
        ],
        "numbers": { "token": 12345 },
        "flags": { "password": true },
        "nested": { "client_secret": { "value": "ek_live", "expires_at": 1 } },
        "lists": { "credentials": ["a", "b"] },
        "unset": { "api_key": "", "secret": null },
        "urls": {
            "endpoint": "https://user:pw@host.example/v1?x=1&key=AIza#frag",
            "plain": "https://host.example/v1/models",
            "not_a_url": "admin:hunter@host",
            "query_only": "wss://api.example/ws?token=abc&model=gpt"
        },
        "key": "gemini-key",
        "keyterms": ["voice", "pipes"]
    });
    redact_in_place(&mut v, "***");
    let dump = v.to_string();
    for leaked in [
        "sk-live-123",
        "sk-oai",
        "sk-tp",
        "\"tok\"",
        "hdr",
        "Bearer t",
        "12345",
        "ek_live",
        "\"a\"",
        "user:pw",
        "AIza",
        "\"abc\"",
        "gemini-key",
    ] {
        assert!(!dump.contains(leaked), "leaked {leaked}: {dump}");
    }
    assert_eq!(v["stt"]["provider"], "assemblyai");
    assert_eq!(v["stt"]["api_key_env"], "AAI_KEY");
    assert_eq!(v["tts"]["max_tokens"], 1024);
    assert_eq!(v["numbers"]["token"], "***");
    assert_eq!(v["flags"]["password"], "***");
    assert_eq!(v["nested"]["client_secret"], "***");
    assert_eq!(v["lists"]["credentials"], "***");
    assert_eq!(v["unset"]["api_key"], "");
    assert!(v["unset"]["secret"].is_null());
    assert_eq!(
        v["urls"]["endpoint"],
        "https://***@host.example/v1?x=1&key=***#frag"
    );
    assert_eq!(v["urls"]["plain"], "https://host.example/v1/models");
    assert_eq!(
        v["urls"]["not_a_url"], "admin:hunter@host",
        "only URL-shaped strings are scrubbed"
    );
    assert_eq!(
        v["urls"]["query_only"],
        "wss://api.example/ws?token=***&model=gpt"
    );
    assert_eq!(v["keyterms"][0], "voice");
}

#[test]
fn url_scrub_edge_cases() {
    assert_eq!(redact_url("no scheme here", "*"), None);
    assert_eq!(redact_url("https://host/path", "*"), None);
    assert_eq!(
        redact_url("http://u@host", "*").as_deref(),
        Some("http://*@host")
    );
    assert_eq!(
        redact_url("http://host?api_key=1", "*").as_deref(),
        Some("http://host?api_key=*")
    );
    assert_eq!(
        redact_url("http://host/p?a=1&password=x&b=2", "*").as_deref(),
        Some("http://host/p?a=1&password=*&b=2")
    );
    assert_eq!(redact_url("://host", "*"), None);
    assert_eq!(redact_url("bad scheme://host", "*"), None);
}

// ── Secret ───────────────────────────────────────────────────

#[test]
fn a_secret_prints_and_serialises_as_the_placeholder() {
    let key = Secret::from("sk-live-123");
    assert_eq!(format!("{key:?}"), "Secret(***REDACTED***)");
    assert_eq!(
        format!("{:?}", Some(key.clone())),
        "Some(Secret(***REDACTED***))"
    );
    assert_eq!(
        serde_json::to_value(&key).unwrap(),
        serde_json::json!(REDACTED)
    );
    assert_eq!(key.expose(), "sk-live-123");
}

#[test]
fn an_empty_secret_serialises_as_empty_so_unset_stays_visible() {
    let key = Secret::default();
    assert!(key.is_empty());
    assert_eq!(serde_json::to_value(&key).unwrap(), serde_json::json!(""));
}

#[test]
fn a_secret_deserialises_from_a_plain_string() {
    let key: Secret = serde_json::from_value(serde_json::json!("abc")).unwrap();
    assert_eq!(key.expose(), "abc");
}

#[test]
fn config_debug_never_shows_a_credential() {
    use crate::config::{SttConfig, TextProcessorConfig, TtsConfig};
    let stt: SttConfig = toml_like(r#"{"provider": "x", "api_key": "sk-stt", "token": "t-1"}"#);
    let tts: TtsConfig = toml_like(r#"{"provider": "x", "api_key": "sk-tts", "voice": "v"}"#);
    let text: TextProcessorConfig =
        toml_like(r#"{"type": "openai-compat", "api_key": "sk-llm", "model": "m"}"#);
    for shown in [format!("{stt:?}"), format!("{tts:?}"), format!("{text:?}")] {
        assert!(!shown.contains("sk-"), "{shown}");
        assert!(!shown.contains("t-1"), "{shown}");
        assert!(shown.contains(REDACTED), "{shown}");
    }
    // Everything that is not a credential is still there to debug with.
    assert!(format!("{text:?}").contains("\"m\""));
}

fn toml_like<T: serde::de::DeserializeOwned>(json: &str) -> T {
    serde_json::from_str(json).expect("parses")
}

/// Serialising a config (for a hash, a display, an API answer) never writes
/// a credential either, including one taken as an option.
#[test]
fn config_serialisation_never_writes_a_credential() {
    use crate::config::{SttConfig, TextProcessorConfig, TtsConfig};
    let stt: SttConfig = toml_like(r#"{"provider": "x", "api_key": "sk-stt", "token": "t-1"}"#);
    let tts: TtsConfig =
        toml_like(r#"{"provider": "x", "voice": "v", "options": {"api_key": "sk-opt"}}"#);
    let text: TextProcessorConfig =
        toml_like(r#"{"type": "openai-compat", "api_key": "sk-llm", "model": "m"}"#);
    for written in [
        serde_json::to_string(&stt).unwrap(),
        serde_json::to_string(&tts).unwrap(),
        serde_json::to_string(&text).unwrap(),
    ] {
        assert!(!written.contains("sk-"), "{written}");
        assert!(!written.contains("t-1"), "{written}");
        assert!(written.contains(REDACTED), "{written}");
    }
    let text = serde_json::to_value(&text).unwrap();
    assert_eq!(text["type"], "openai-compat");
    assert_eq!(text["model"], "m");
}

/// A configured key wins when it is not blank; a blank one falls through to
/// the variable, a blank variable counts as unset, and nothing at all is
/// `None`.
#[test]
fn a_key_resolves_from_config_then_environment() {
    let env = |name: &str| match name {
        "SET" => Some("sk-env".to_string()),
        "BLANK" => Some("  ".to_string()),
        _ => None,
    };
    let configured = Secret::from("sk-config");
    let blank = Secret::from("   ");
    let resolved = |key, var| resolve_key(key, var, &env).map(|k| k.expose().to_string());
    assert_eq!(
        resolved(Some(&configured), Some("SET")).as_deref(),
        Some("sk-config")
    );
    assert_eq!(
        resolved(Some(&blank), Some("SET")).as_deref(),
        Some("sk-env")
    );
    assert_eq!(resolved(None, Some("SET")).as_deref(), Some("sk-env"));
    assert_eq!(resolved(None, Some("BLANK")), None);
    assert_eq!(resolved(None, Some("UNSET")), None);
    assert_eq!(resolved(Some(&blank), None), None);
    assert!(configured_key(Some(&blank)).is_none());
    assert!(configured_key(Some(&configured)).is_some());
}

/// A parse error keeps its kind and position, never the value it quotes.
#[test]
fn a_json_error_never_repeats_the_payload() {
    #[derive(Debug, serde::Deserialize)]
    #[expect(dead_code, reason = "only the parse is under test")]
    struct Msg {
        n: u32,
    }
    let e = serde_json::from_str::<Msg>(r#"{"n": "the caller's private words"}"#).unwrap_err();
    assert!(
        e.to_string().contains("private words"),
        "serde quotes it: {e}"
    );
    let shown = json_error(&e);
    assert!(!shown.contains("private"), "{shown}");
    assert_eq!(shown, "JSON unexpected shape at line 1 column 34");

    let e = serde_json::from_str::<Msg>("{\"n\": 1").unwrap_err();
    assert_eq!(json_error(&e), "JSON ended early at line 1 column 7");
    let e = serde_json::from_str::<Msg>("hello").unwrap_err();
    assert!(json_error(&e).starts_with("JSON not valid JSON"));
}
