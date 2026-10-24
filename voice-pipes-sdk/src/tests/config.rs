use super::*;

/// A text processor's unknown options are named with their section; the
/// option the engine adds for audits is never one of them.
#[test]
#[expect(deprecated, reason = "the deprecated reader keeps working until 2.0")]
fn text_processor_unknown_options_skip_the_engines_own() {
    let cfg = TextProcessorConfig {
        processor_type: "llm".into(),
        api_key: None,
        api_key_env: None,
        options: [
            ("model".to_string(), serde_json::json!("m.gguf")),
            ("modle".to_string(), serde_json::json!("m.gguf")),
            (
                crate::text::AUDIT_FULL_PROMPTS_OPTION.to_string(),
                serde_json::json!(true),
            ),
        ]
        .into_iter()
        .collect(),
    };
    let errors = cfg.unknown_options(&["model"]);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].field, "text_processor.modle");
    assert!(
        errors[0].message.contains("\"llm\""),
        "{}",
        errors[0].message
    );
}
