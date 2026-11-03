//! The provider's entry in the setup wizard.

/// How this provider offers itself in the setup wizard.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};
    // Gemini TTS reads `api_key` or GOOGLE_API_KEY (not GEMINI_API_KEY);
    // the launcher passes GOOGLE_API_KEY through to the worker.
    vec![
        WizardChoice::new(
            "Gemini TTS (Cloud)",
            WizardDeploy::Cloud {
                key_env: "GOOGLE_API_KEY".into(),
            },
        )
        .with_feature(Some("tts-gemini".into()))
        .with_fields(vec![
            WizardField::text(
                "model_name",
                "Gemini TTS model name",
                "gemini-3.1-flash-tts-preview",
            ),
            WizardField::text("voice", "Gemini voice name", "Kore"),
            WizardField::secret("GOOGLE_API_KEY", "Enter GOOGLE_API_KEY"),
        ])
        .with_menu_rank(40),
    ]
}
