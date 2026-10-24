use super::*;

/// `Debug` on a provider's settings shows neither the key nor a
/// secret-named option, and still shows the rest.
#[test]
fn realtime_config_debug_redacts_the_key_and_secret_options() {
    let cfg = RealtimeConfig::new("acme")
        .with_api_key(Some("sk-live-secret".into()))
        .with_api_key_var("ACME_API_KEY".into())
        .with_options(
            [
                (
                    "webhook_api_key".to_string(),
                    serde_json::json!("wh-secret"),
                ),
                ("thinking_budget".to_string(), serde_json::json!(512)),
            ]
            .into_iter()
            .collect(),
        );
    let s = format!("{cfg:?}");
    for leaked in ["sk-live-secret", "wh-secret"] {
        assert!(!s.contains(leaked), "leaked {leaked}: {s}");
    }
    for shown in ["acme", "ACME_API_KEY", "thinking_budget", "512"] {
        assert!(s.contains(shown), "{shown} missing: {s}");
    }
}

/// A missing key is reported against the section's key setting, naming
/// the variable the engine looked in.
#[test]
fn a_missing_key_names_the_variable_to_set() {
    let cfg = RealtimeConfig::new("acme").with_api_key_var("ACME_API_KEY".into());
    let e = cfg.missing_key_error("Acme").expect("reported");
    assert_eq!(e.field, "realtime_provider.api_key_env");
    assert!(e.message.contains("ACME_API_KEY"), "{}", e.message);
    assert!(
        cfg.with_api_key(Some("k".into()))
            .missing_key_error("Acme")
            .is_none()
    );
}
