//! The setup wizard's entries: the cloud service and the `VibeVoice` GPU
//! sidecar speaking the same API.

use crate::config::{DEFAULT_MODEL, DEFAULT_VOICE};

/// How this provider offers itself in the setup wizard.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};
    vec![
        WizardChoice::new(
            "OpenAI TTS (Cloud)",
            WizardDeploy::Cloud {
                key_env: "OPENAI_API_KEY".into(),
            },
        )
        .with_feature(Some("tts-openai".into()))
        .with_fields(vec![
            WizardField::fixed("model_name", DEFAULT_MODEL),
            WizardField::fixed("voice", DEFAULT_VOICE),
            WizardField::secret("OPENAI_API_KEY", "Enter OPENAI_API_KEY"),
        ])
        .with_menu_rank(10),
        // A Dockerized GPU sidecar (scripts/vibevoice/server.py on port
        // 8091, the `vibevoice` compose service) speaking this API; the
        // real model identity is resolved at session setup via
        // /v1/models. Weights and voices are volume-mounted and fetched
        // on first boot. The placeholder key only satisfies the
        // endpoint check; the sidecar ignores it.
        WizardChoice::new(
            "VibeVoice-Realtime-0.5B (host GPU sidecar, quality-first)",
            WizardDeploy::DockerSidecar {
                profile: "vibevoice".into(),
                service: "vibevoice".into(),
                model_env: None,
                requires_gpu: true,
                ensure_dirs: vec!["vendor/vibevoice/demo/voices/streaming_model".into()],
            },
        )
        .with_feature(Some("tts-openai".into()))
        .with_fields(vec![
            WizardField::text(
                "endpoint",
                "VibeVoice endpoint (as seen from the Docker worker)",
                "http://vibevoice:8091/v1",
            ),
            WizardField::fixed("model_name", "VibeVoice-Realtime-0.5B"),
            WizardField::text("voice", "VibeVoice voice alias", "en-Carter_man"),
            WizardField::fixed("api_key", "local"),
        ])
        .with_menu_rank(11),
    ]
}
