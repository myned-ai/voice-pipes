use voice_pipes_sdk::SttFactory;

fn endpoint_errors(extra: serde_json::Value) -> Vec<voice_pipes_sdk::ConfigError> {
    let mut cfg = serde_json::json!({
        "provider": "google",
        "language_codes": ["hr-HR"],
    });
    cfg.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let cfg: voice_pipes_sdk::SttConfig = serde_json::from_value(cfg).unwrap();
    super::GoogleSttFactory
        .validate(&cfg)
        .into_iter()
        .filter(|e| e.field == "stt.endpoint")
        .collect()
}

/// The OAuth token goes on every stream, so a plain-HTTP endpoint on an
/// internet host is refused like every other hosted provider's; Google's
/// own endpoint, a local test server and the operator's opt-in pass.
#[test]
fn the_token_is_never_sent_in_plain_text_to_an_internet_host() {
    let refused = endpoint_errors(serde_json::json!({ "endpoint": "http://speech.example.com" }));
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].message.contains("allow_insecure_endpoint"),
        "{refused:?}"
    );

    assert!(endpoint_errors(serde_json::json!({})).is_empty());
    assert!(
        endpoint_errors(serde_json::json!({ "endpoint": "http://127.0.0.1:50051" })).is_empty()
    );
    assert!(
        endpoint_errors(serde_json::json!({
            "endpoint": "http://speech.example.com",
            "options": { "allow_insecure_endpoint": true },
        }))
        .is_empty()
    );
}
