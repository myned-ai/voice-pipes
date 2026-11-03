use super::*;

#[test]
fn schema_never_leaks_credentials() {
    let mut options = std::collections::HashMap::new();
    options.insert("access_token".into(), serde_json::json!("ya29.secret"));
    options.insert(
        "credentials_path".into(),
        serde_json::json!("/keys/sa.json"),
    );
    let cfg = SttConfig::new("google")
        .with_model_name(Some("chirp_3".into()))
        .with_options(options);
    let s = schema(&cfg);
    let rendered = serde_json::to_string(&s.current_values).unwrap();
    assert!(!rendered.contains("secret"), "{rendered}");
    assert!(!rendered.contains("sa.json"), "{rendered}");
    assert_eq!(
        s.current_values.get("credentials_set"),
        Some(&serde_json::json!(true))
    );
}
