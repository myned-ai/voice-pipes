/// The entry writes what the engine's built-in `ElevenLabs` TTS entry did, and
/// stores the key in `.env`, never in the TOML.
#[test]
fn the_wizard_entry_renders_the_config_the_built_in_did() {
    let choice = super::wizard_choices()
        .into_iter()
        .next()
        .expect("offers itself");
    assert_eq!(choice.label, "ElevenLabs TTS (Cloud)");
    assert_eq!(choice.feature.as_deref(), Some("tts-elevenlabs"));
    assert_eq!(
        choice.render_defaults("provider", "elevenlabs").toml,
        "provider = \"elevenlabs\"\nmodel_name = \"eleven_flash_v2_5\"\nvoice = \"21m00Tcm4TlvDq8ikWAM\"\n"
    );
    let answered = choice
        .render("provider", "elevenlabs", |f| {
            Ok(if f.key == "ELEVENLABS_API_KEY" {
                "sk-test".into()
            } else {
                f.default.clone()
            })
        })
        .unwrap();
    assert_eq!(
        answered.secrets,
        vec![("ELEVENLABS_API_KEY".to_string(), "sk-test".to_string())]
    );
    assert!(!answered.toml.contains("sk-test"));
}
