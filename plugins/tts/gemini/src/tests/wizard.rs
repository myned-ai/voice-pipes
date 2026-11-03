/// The entry writes what the engine's built-in Gemini TTS entry did, and
/// stores the key in `.env`, never in the TOML.
#[test]
fn the_wizard_entry_renders_the_config_the_built_in_did() {
    let choice = super::wizard_choices()
        .into_iter()
        .next()
        .expect("offers itself");
    assert_eq!(choice.label, "Gemini TTS (Cloud)");
    assert_eq!(choice.feature.as_deref(), Some("tts-gemini"));
    assert_eq!(
        choice.render_defaults("provider", "gemini").toml,
        "provider = \"gemini\"\nmodel_name = \"gemini-3.1-flash-tts-preview\"\nvoice = \"Kore\"\n"
    );
    let answered = choice
        .render("provider", "gemini", |f| {
            Ok(if f.key == "GOOGLE_API_KEY" {
                "sk-test".into()
            } else {
                f.default.clone()
            })
        })
        .unwrap();
    assert_eq!(
        answered.secrets,
        vec![("GOOGLE_API_KEY".to_string(), "sk-test".to_string())]
    );
    assert!(!answered.toml.contains("sk-test"));
}
