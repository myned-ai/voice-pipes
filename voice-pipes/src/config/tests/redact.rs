use super::*;

#[test]
fn redacts_every_secret_like_field() {
    let mut v = serde_json::json!({
        "stt": { "provider": "assemblyai", "api_key": "sk-live-123" },
        "tts": { "openai_api_key": "sk-oai", "voice": "alloy" },
        "text_processor": [
            { "api_key": "sk-tp", "auth_token": "tok", "model": "gemma" }
        ],
        "transport": { "some_secret": "hunter2", "password": "pw" },
    });
    redact_secrets(&mut v);
    assert_eq!(v["stt"]["api_key"], REDACTED);
    assert_eq!(v["tts"]["openai_api_key"], REDACTED);
    assert_eq!(v["text_processor"][0]["api_key"], REDACTED);
    assert_eq!(v["text_processor"][0]["auth_token"], REDACTED);
    assert_eq!(v["transport"]["some_secret"], REDACTED);
    assert_eq!(v["transport"]["password"], REDACTED);
    // Non-secret siblings are untouched.
    assert_eq!(v["stt"]["provider"], "assemblyai");
    assert_eq!(v["tts"]["voice"], "alloy");
    assert_eq!(v["text_processor"][0]["model"], "gemma");
}

#[test]
fn leaves_env_names_token_counts_and_empty_values() {
    let mut v = serde_json::json!({
        "api_key_env": "GEMINI_API_KEY",
        "client_token_env": "VP_CLIENT_TOKEN",
        "max_tokens": 1024,
        "tokens_per_sec": 30,
        "api_key": "",
    });
    redact_secrets(&mut v);
    assert_eq!(v["api_key_env"], "GEMINI_API_KEY");
    assert_eq!(v["client_token_env"], "VP_CLIENT_TOKEN");
    assert_eq!(v["max_tokens"], 1024);
    assert_eq!(v["tokens_per_sec"], 30);
    assert_eq!(v["api_key"], "");
}

/// The corpus every redactor must clear: nested tables and arrays, each
/// value type under a secret key, and URLs carrying credentials.
fn corpus() -> serde_json::Value {
    serde_json::json!({
        "stt": { "provider": "google", "api_key": "sk-live-123",
                 "endpoint": "https://user:pw@speech.example/v2?key=AIza" },
        "headers": { "x-api-key": "hdr-1", "Authorization": "Bearer t-1" },
        "session": { "client_secret": { "value": "ek_live_abc" }, "instructions": "Be brief." },
        "numbers": { "token": 4242, "max_tokens": 256 },
        "list": [ { "apiKey": "list-key" }, "plain", "wss://x.example/ws?access_token=at-1" ],
        "key": "gemini-1",
        "keyterms": ["voice"]
    })
}

const LEAKS: &[&str] = &[
    "sk-live-123",
    "user:pw",
    "AIza",
    "hdr-1",
    "Bearer t-1",
    "ek_live_abc",
    "4242",
    "list-key",
    "at-1",
    "gemini-1",
];

/// Nested values, every marker and URL credentials are blanked, and
/// the non-secret neighbours survive.
#[test]
fn config_redactor_clears_the_corpus() {
    let mut v = corpus();
    redact_secrets(&mut v);
    let dump = v.to_string();
    for leaked in LEAKS {
        assert!(!dump.contains(leaked), "leaked {leaked}: {dump}");
    }
    assert_eq!(v["stt"]["provider"], "google");
    assert_eq!(v["session"]["instructions"], "Be brief.");
    assert_eq!(v["numbers"]["max_tokens"], 256);
    assert_eq!(v["list"][1], "plain");
    assert_eq!(v["keyterms"][0], "voice");
    assert_eq!(
        v["stt"]["endpoint"],
        "https://***REDACTED***@speech.example/v2?key=***REDACTED***"
    );
}

/// The provider frame scrubber and the config redactor agree on the
/// same corpus: what one blanks, the other blanks.
#[cfg(feature = "realtime")]
#[test]
fn frame_scrubber_and_config_redactor_agree() {
    let mut via_config = corpus();
    redact_secrets(&mut via_config);
    let via_frames = crate::realtime::client::redact_secrets(&corpus());
    let (a, b) = (via_config.to_string(), via_frames.to_string());
    for leaked in LEAKS {
        assert!(!a.contains(leaked), "config redactor leaked {leaked}");
        assert!(!b.contains(leaked), "frame scrubber leaked {leaked}");
    }
    // Same shape once the placeholders are normalised.
    assert_eq!(a.replace(REDACTED, "***"), b);
}
