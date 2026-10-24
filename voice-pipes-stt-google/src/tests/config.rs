use super::*;

fn base_stt_config() -> voice_pipes_sdk::config::SttConfig {
    let mut options = std::collections::HashMap::new();
    options.insert("project_id".into(), serde_json::json!("proj-1"));
    options.insert("language_codes".into(), serde_json::json!(["hr-HR"]));
    voice_pipes_sdk::config::SttConfig::new("google")
        .with_model_name(Some("chirp_3".into()))
        .with_options(options)
}

#[test]
fn defaults() {
    let cfg = GoogleSttConfig::from_stt_config(&base_stt_config()).unwrap();
    assert_eq!(cfg.model, "chirp_3");
    assert_eq!(cfg.location, "us");
    assert_eq!(cfg.endpoint, "https://us-speech.googleapis.com");
    assert_eq!(cfg.sample_rate, 16_000);
    assert!(cfg.interim_results);
    assert!(cfg.enable_voice_activity_events);
    assert!(cfg.enable_automatic_punctuation);
    assert!(!cfg.enable_word_time_offsets);
    assert!(!cfg.persistent_session);
    assert_eq!(cfg.stream_rotate, Duration::from_mins(4));
    assert_eq!(cfg.language_codes, vec!["hr-HR".to_string()]);
    assert_eq!(cfg.project_id.as_deref(), Some("proj-1"));
    assert!(cfg.validate().is_empty(), "{:?}", cfg.validate());
}

#[test]
fn model_name_defaults_to_chirp_3() {
    let mut src = base_stt_config();
    src.model_name = None;
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert_eq!(cfg.model, "chirp_3");
}

#[test]
fn options_round_trip() {
    let mut src = base_stt_config();
    src.options
        .insert("location".into(), serde_json::json!("eu"));
    src.options
        .insert("interim_results".into(), serde_json::json!(false));
    src.options
        .insert("enable_word_time_offsets".into(), serde_json::json!(true));
    src.options.insert(
        "endpointing_sensitivity".into(),
        serde_json::json!("SUPER_SHORT"),
    );
    src.options
        .insert("speech_end_timeout_ms".into(), serde_json::json!(800));
    src.options
        .insert("stream_rotate_ms".into(), serde_json::json!(200_000));
    src.options
        .insert("persistent_session".into(), serde_json::json!(true));
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert_eq!(cfg.location, "eu");
    assert_eq!(cfg.endpoint, "https://eu-speech.googleapis.com");
    assert!(!cfg.interim_results);
    assert!(cfg.enable_word_time_offsets);
    assert_eq!(
        cfg.endpointing_sensitivity,
        Some(EndpointingSensitivity::SuperShort)
    );
    assert_eq!(cfg.speech_end_timeout, Some(Duration::from_millis(800)));
    assert_eq!(cfg.stream_rotate, Duration::from_secs(200));
    assert!(cfg.persistent_session);
    assert!(cfg.validate().is_empty());
}

#[test]
fn bare_string_language_code_is_a_one_element_list() {
    let mut src = base_stt_config();
    src.options
        .insert("language_codes".into(), serde_json::json!("en-US"));
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert_eq!(cfg.language_codes, vec!["en-US".to_string()]);

    src.options.remove("language_codes");
    src.options
        .insert("language_code".into(), serde_json::json!("de-DE"));
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert_eq!(cfg.language_codes, vec!["de-DE".to_string()]);
}

#[test]
fn missing_language_codes_is_a_validation_error() {
    let mut src = base_stt_config();
    src.options.remove("language_codes");
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    let errors = cfg.validate();
    assert!(
        errors.iter().any(|e| e.field == "language_codes"),
        "{errors:?}"
    );
}

#[test]
fn unknown_model_rejected() {
    let mut src = base_stt_config();
    src.model_name = Some("chirp_9".into());
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    let errors = cfg.validate();
    assert!(
        errors
            .iter()
            .any(|e| e.field == "model_name" && e.message.contains("unknown")),
        "{errors:?}"
    );
}

#[test]
fn chirp_3_outside_us_eu_rejected() {
    let mut src = base_stt_config();
    src.options
        .insert("location".into(), serde_json::json!("global"));
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert_eq!(cfg.endpoint, "https://speech.googleapis.com");
    let errors = cfg.validate();
    assert!(
        errors
            .iter()
            .any(|e| e.field == "location" && e.message.contains("chirp_3")),
        "{errors:?}"
    );

    // Other models are fine on `global`.
    src.model_name = Some("long".into());
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert!(cfg.validate().is_empty(), "{:?}", cfg.validate());
}

#[test]
fn stream_rotate_must_be_below_google_cap() {
    let mut src = base_stt_config();
    src.options
        .insert("stream_rotate_ms".into(), serde_json::json!(300_000));
    let err = GoogleSttConfig::parse(&src).unwrap_err();
    assert!(
        err.iter().any(|e| e.field == "stt.stream_rotate_ms"),
        "{err:?}"
    );
}

#[test]
fn bad_endpointing_sensitivity_is_a_parse_error() {
    let mut src = base_stt_config();
    src.options.insert(
        "endpointing_sensitivity".into(),
        serde_json::json!("aggressive"),
    );
    let err = GoogleSttConfig::from_stt_config(&src).unwrap_err();
    assert!(err.to_string().contains("endpointing_sensitivity"), "{err}");
}

#[test]
fn missing_credentials_file_rejected() {
    let mut src = base_stt_config();
    src.options.insert(
        "credentials_path".into(),
        serde_json::json!("/definitely/not/here.json"),
    );
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert!(cfg.validate().iter().any(|e| e.field == "credentials_path"));
}

/// The Debug rendering must never contain the token value.
#[test]
fn debug_redacts_access_token() {
    let mut src = base_stt_config();
    src.options.insert(
        "access_token".into(),
        serde_json::json!("ya29.super-secret"),
    );
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    let s = format!("{cfg:?}");
    assert!(!s.contains("super-secret"), "{s}");
    assert!(s.contains("***"), "{s}");
}

#[test]
fn recognizer_path_uses_default_recognizer() {
    let cfg = GoogleSttConfig::from_stt_config(&base_stt_config()).unwrap();
    assert_eq!(
        cfg.recognizer_path("proj-1"),
        "projects/proj-1/locations/us/recognizers/_"
    );
}

#[test]
fn explicit_endpoint_override_wins() {
    let mut src = base_stt_config();
    src.endpoint = Some("http://127.0.0.1:1234".into());
    let cfg = GoogleSttConfig::from_stt_config(&src).unwrap();
    assert_eq!(cfg.endpoint, "http://127.0.0.1:1234");
    assert!(cfg.plaintext());
}
