use super::*;

/// The voice is the model file; a `voice` setting is not read, so the
/// schema does not present one.
#[test]
fn schema_does_not_list_the_unread_voice_field() {
    let cfg = TtsConfig::new("piper")
        .with_model(Some("models/en_US-lessac-medium.onnx".into()))
        .with_voice(Some("speaker-2".into()));
    let keys: Vec<String> = schema(&cfg)
        .descriptors
        .into_iter()
        .map(|d| d.key)
        .collect();
    assert!(keys.iter().any(|k| k == "model"), "{keys:?}");
    assert!(!keys.iter().any(|k| k == "voice"), "{keys:?}");
}
