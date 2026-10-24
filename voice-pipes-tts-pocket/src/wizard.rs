//! The setup wizard's entry for this provider.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// How this provider offers itself in the setup wizard.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    vec![
        WizardChoice::new(
            "Pocket-TTS (HTTP sidecar)",
            WizardDeploy::DockerSidecar {
                profile: "pocket".into(),
                service: "pocket-tts".into(),
                model_env: None,
                requires_gpu: false,
                // The sidecar runs as the host user and keeps its package
                // and model caches here; created before it starts, so it is
                // the user's and not a root-owned directory Docker made.
                ensure_dirs: vec!["models/.cache/pocket-tts".into()],
            },
        )
        .with_feature(Some("tts-pocket".into()))
        .with_fields(vec![
            WizardField::text("endpoint", "Pocket-TTS endpoint", "http://pocket-tts:8000"),
            WizardField::fixed("voice", "alba"),
        ])
        .with_menu_rank(30),
    ]
}
