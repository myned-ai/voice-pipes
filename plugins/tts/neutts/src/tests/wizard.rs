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
        "provider = \"neutts\"\nmodel = \"models/neutts-air-Q4_0.gguf\"\nvocoder = \"models/neucodec_decoder.onnx\"\n"
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
    assert_eq!(names, ["neutts-air-Q4_0.gguf", "neucodec_decoder.onnx"]);
    assert!(files[0].url.ends_with("/neutts-air-Q4_0.gguf"));
    assert!(files[1].url.ends_with("/model.onnx"));
}
