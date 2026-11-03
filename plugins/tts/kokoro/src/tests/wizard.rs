use voice_pipes_sdk::{WizardDeploy, WizardLayout};

/// The entry writes what the engine's own Kokoro entry did, so configs made
/// before the move and after it are the same file.
#[test]
fn the_wizard_entry_renders_the_config_the_built_in_did() {
    let choice = super::wizard_choices()
        .into_iter()
        .next()
        .expect("offers itself");
    assert_eq!(choice.label, "Kokoro (Local ONNX)");
    assert_eq!(choice.feature.as_deref(), Some("tts-kokoro"));
    assert_eq!(
        choice.render_defaults("provider", "kokoro").toml,
        "provider = \"kokoro\"\nmodel = \"models/kokoro-v1.0.onnx\"\nvoices = \"models/voices-v1.0.bin\"\n"
    );
}

/// The wizard fetches the model and the voices next to the model path.
#[test]
fn the_wizard_downloads_the_model_and_the_voices() {
    let choice = super::wizard_choices().into_iter().next().unwrap();
    let WizardDeploy::Download { layout, files } = choice.deploy else {
        panic!("expected a download deploy");
    };
    assert_eq!(layout, WizardLayout::File);
    let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["kokoro-v1.0.onnx", "voices-v1.0.bin"]);
    for f in &files {
        assert!(f.url.ends_with(&format!("/{}", f.name)), "{}", f.url);
    }
}
