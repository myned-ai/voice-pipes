/// The entry writes what the engine's built-in `AssemblyAI` entry did, keeps
/// the key in `.env`, and stays the wizard's default recogniser.
#[test]
fn the_wizard_entry_renders_the_config_the_built_in_did() {
    let choice = super::wizard_choices()
        .into_iter()
        .next()
        .expect("offers itself");
    assert_eq!(choice.label, "AssemblyAI (Cloud API)");
    assert_eq!(choice.feature.as_deref(), Some("stt-assemblyai"));
    assert!(choice.stage_default);
    assert_eq!(
        choice.render_defaults("provider", "assemblyai").toml,
        "provider = \"assemblyai\"\nmodel_name = \"u3-rt-pro\"\n"
    );
    let answered = choice
        .render("provider", "assemblyai", |f| {
            Ok(if f.key == "ASSEMBLYAI_API_KEY" {
                "sk-test".into()
            } else {
                f.default.clone()
            })
        })
        .unwrap();
    assert_eq!(
        answered.secrets,
        vec![("ASSEMBLYAI_API_KEY".to_string(), "sk-test".to_string())]
    );
}
