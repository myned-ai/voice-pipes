//! How the provider offers itself in the setup wizard.

/// How this provider offers itself in the setup wizard.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};
    // Not the ElevenLabs Agents speech-to-speech provider; the same
    // ELEVENLABS_API_KEY serves both.
    vec![
        WizardChoice::new(
            "ElevenLabs TTS (Cloud)",
            WizardDeploy::Cloud {
                key_env: "ELEVENLABS_API_KEY".into(),
            },
        )
        .with_feature(Some("tts-elevenlabs".into()))
        .with_fields(vec![
            WizardField::text("model_name", "ElevenLabs model id", "eleven_flash_v2_5"),
            WizardField::text("voice", "ElevenLabs voice id", "21m00Tcm4TlvDq8ikWAM"),
            WizardField::secret("ELEVENLABS_API_KEY", "Enter ELEVENLABS_API_KEY"),
        ])
        .with_menu_rank(50),
    ]
}
