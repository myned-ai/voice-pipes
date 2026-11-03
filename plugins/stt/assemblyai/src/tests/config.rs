use super::*;

/// The Debug rendering must never contain the key value.
/// An out-of-range rate is a config error, not a panic in
/// `transcribe()` (`chunks(0)`) or an endless post-connect retry.
#[test]
fn validate_rejects_out_of_range_sample_rate() {
    use voice_pipes_sdk::SttFactory;
    for bad in [0u32, 4_000, 96_000] {
        let mut src = base_stt_config();
        src.options
            .insert("sample_rate".into(), serde_json::json!(bad));
        let errs = crate::AssemblyAiFactory.validate(&src);
        assert!(
            errs.iter().any(|e| e.field == "stt.sample_rate"),
            "{bad} must be rejected: {errs:?}"
        );
        assert!(
            AssemblyAiConfig::from_stt_config(&src).is_err(),
            "{bad} builds"
        );
    }
    assert!(
        crate::AssemblyAiFactory
            .validate(&base_stt_config())
            .iter()
            .all(|e| e.field != "stt.sample_rate")
    );
}

#[test]
fn debug_redacts_api_key() {
    let mut cfg_src = base_stt_config();
    cfg_src.api_key = Some("super-secret-key".into());
    let cfg = AssemblyAiConfig::from_stt_config(&cfg_src).unwrap();
    let s = format!("{cfg:?}");
    assert!(!s.contains("super-secret-key"), "{s}");
    assert!(s.contains("***"), "{s}");
}

fn base_stt_config() -> voice_pipes_sdk::SttConfig {
    voice_pipes_sdk::SttConfig::new("assemblyai")
        .with_api_key(Some("test-key".into()))
        .with_model_name(Some("u3-rt-pro".into()))
}

#[test]
fn from_stt_config_defaults() {
    let cfg = AssemblyAiConfig::from_stt_config(&base_stt_config()).unwrap();
    assert_eq!(cfg.api_key.expose(), "test-key");
    assert_eq!(cfg.endpoint, DEFAULT_ENDPOINT);
    assert_eq!(cfg.speech_model, "u3-rt-pro");
    assert_eq!(cfg.sample_rate, DEFAULT_SAMPLE_RATE);
    assert!(cfg.min_turn_silence.is_none());
    assert!(cfg.keyterms_prompt.is_none());
}

#[test]
fn from_stt_config_with_options() {
    let mut cfg = base_stt_config();
    cfg.options
        .insert("min_turn_silence".into(), serde_json::json!(200));
    cfg.options
        .insert("max_turn_silence".into(), serde_json::json!(3000));
    cfg.options.insert(
        "keyterms_prompt".into(),
        serde_json::json!(["hello", "world"]),
    );

    let ac = AssemblyAiConfig::from_stt_config(&cfg).unwrap();
    assert_eq!(ac.min_turn_silence, Some(200));
    assert_eq!(ac.max_turn_silence, Some(3000));
    assert_eq!(
        ac.keyterms_prompt.as_deref(),
        Some(&["hello".to_string(), "world".to_string()][..])
    );
}

#[test]
fn from_stt_config_missing_api_key() {
    let mut cfg = base_stt_config();
    cfg.api_key = None;
    // The host resolves the key from the environment; the provider sees
    // only the config, so this holds on a machine that has one set.
    let err = AssemblyAiConfig::from_stt_config(&cfg).unwrap_err();
    assert!(err.to_string().contains("API key"));
}

#[test]
fn from_stt_config_missing_model_name() {
    let mut cfg = base_stt_config();
    cfg.model_name = None;
    let err = AssemblyAiConfig::from_stt_config(&cfg).unwrap_err();
    assert!(err.to_string().contains("model_name"));
}

#[test]
fn validate_unknown_model() {
    let mut cfg = base_stt_config();
    cfg.model_name = Some("non-existent-model".into());
    let ac = AssemblyAiConfig::from_stt_config(&cfg).unwrap();
    let errors = ac.validate();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("unknown speech model"));
}

#[test]
fn validate_keyterm_limits() {
    let mut cfg = base_stt_config();
    let long_terms: Vec<serde_json::Value> = (0..101)
        .map(|i| serde_json::json!(format!("term{i}")))
        .collect();
    cfg.options.insert(
        "keyterms_prompt".into(),
        serde_json::Value::Array(long_terms),
    );

    let ac = AssemblyAiConfig::from_stt_config(&cfg).unwrap();
    let errors = ac.validate();
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("too many key terms"))
    );
}

#[test]
fn validate_keyterm_length() {
    let mut cfg = base_stt_config();
    cfg.options.insert(
        "keyterms_prompt".into(),
        serde_json::json!(["x".repeat(51)]),
    );

    let ac = AssemblyAiConfig::from_stt_config(&cfg).unwrap();
    let errors = ac.validate();
    assert!(errors.iter().any(|e| e.message.contains("too long")));
}

/// The limit is in characters: 30 two-byte letters are 30 chars, not 60.
#[test]
fn validate_keyterm_length_counts_chars_not_bytes() {
    let mut cfg = base_stt_config();
    cfg.options.insert(
        "keyterms_prompt".into(),
        serde_json::json!(["č".repeat(30)]),
    );

    let ac = AssemblyAiConfig::from_stt_config(&cfg).unwrap();
    let errors = ac.validate();
    assert!(errors.is_empty(), "{errors:?}");
}
