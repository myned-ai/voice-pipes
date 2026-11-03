use voice_pipes_sdk::WizardDeploy;

/// The two entries, in the engine's former menu order: the cloud service,
/// then the `VibeVoice` sidecar speaking the same API.
fn choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    let choices = super::wizard_choices();
    assert_eq!(choices.len(), 2);
    choices
}

/// The cloud entry writes what the engine's built-in `OpenAI` TTS entry did,
/// and stores the key in `.env`, never in the TOML.
#[test]
fn the_cloud_entry_renders_the_config_the_built_in_did() {
    let cloud = &choices()[0];
    assert_eq!(cloud.label, "OpenAI TTS (Cloud)");
    assert_eq!(cloud.feature.as_deref(), Some("tts-openai"));
    assert_eq!(
        cloud.render_defaults("provider", "openai").toml,
        "provider = \"openai\"\nmodel_name = \"tts-1\"\nvoice = \"alloy\"\n"
    );
    let answered = cloud
        .render("provider", "openai", |f| {
            Ok(if f.key == "OPENAI_API_KEY" {
                "sk-test".into()
            } else {
                f.default.clone()
            })
        })
        .unwrap();
    assert_eq!(
        answered.secrets,
        vec![("OPENAI_API_KEY".to_string(), "sk-test".to_string())]
    );
    assert!(!answered.toml.contains("sk-test"));
}

/// The `VibeVoice` entry writes what the built-in did and declares its GPU
/// sidecar and the directory its voices are mounted from.
#[test]
fn the_vibevoice_entry_renders_the_config_and_sidecar_the_built_in_did() {
    let vibevoice = &choices()[1];
    assert_eq!(
        vibevoice.label,
        "VibeVoice-Realtime-0.5B (host GPU sidecar, quality-first)"
    );
    assert_eq!(
        vibevoice.render_defaults("provider", "openai").toml,
        "provider = \"openai\"\nendpoint = \"http://vibevoice:8091/v1\"\n\
         model_name = \"VibeVoice-Realtime-0.5B\"\nvoice = \"en-Carter_man\"\napi_key = \"local\"\n"
    );
    assert_eq!(
        vibevoice.deploy,
        WizardDeploy::DockerSidecar {
            profile: "vibevoice".into(),
            service: "vibevoice".into(),
            model_env: None,
            requires_gpu: true,
            ensure_dirs: vec!["VibeVoice/demo/voices/streaming_model".into()],
        }
    );
}
