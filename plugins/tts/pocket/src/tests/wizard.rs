use voice_pipes_sdk::WizardDeploy;

/// The entry writes what the engine's built-in Pocket-TTS entry did.
#[test]
fn the_wizard_entry_renders_the_config_the_built_in_did() {
    let choice = super::wizard_choices()
        .into_iter()
        .next()
        .expect("offers itself");
    assert_eq!(choice.label, "Pocket-TTS (HTTP sidecar)");
    assert_eq!(choice.feature.as_deref(), Some("tts-pocket"));
    assert_eq!(
        choice.render_defaults("provider", "pocket-http").toml,
        "provider = \"pocket-http\"\nendpoint = \"http://pocket-tts:8000\"\nvoice = \"alba\"\n"
    );
}

/// Choosing it has the launcher start the `pocket-tts` sidecar, no GPU.
#[test]
fn the_wizard_starts_the_sidecar() {
    let choice = super::wizard_choices().into_iter().next().unwrap();
    let WizardDeploy::DockerSidecar {
        profile,
        service,
        requires_gpu,
        ..
    } = choice.deploy
    else {
        panic!("expected a sidecar deploy");
    };
    assert_eq!(
        (profile.as_str(), service.as_str(), requires_gpu),
        ("pocket", "pocket-tts", false)
    );
}
