use voice_pipes_sdk::{WizardDeploy, WizardLayout};

#[test]
fn the_wizard_entry_renders_expected_config() {
    let choice = super::wizard_choices()
        .into_iter()
        .next()
        .expect("offers wizard choice");
    assert_eq!(choice.label, "NeuTTS-Air (Local GGUF + NeuCodec)");
    assert_eq!(choice.feature.as_deref(), Some("tts-neutts"));
    assert_eq!(
        choice.render_defaults("provider", "neutts").toml,
        "provider = \"neutts\"\nmodel = \"models/neutts-air-q4_k_m.gguf\"\nvocoder = \"models/neucodec_decoder.onnx\"\n"
    );
}

#[test]
fn the_wizard_downloads_air_and_codec_files() {
    let choice = super::wizard_choices().into_iter().next().unwrap();
    let WizardDeploy::Download { layout, files } = choice.deploy else {
        panic!("expected download deployment");
    };
    assert_eq!(layout, WizardLayout::File);
    let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["neutts-air-q4_k_m.gguf", "neucodec_decoder.onnx"]);
    for f in &files {
        assert!(f.url.ends_with(&format!("/{}", f.name)), "{}", f.url);
    }
}
