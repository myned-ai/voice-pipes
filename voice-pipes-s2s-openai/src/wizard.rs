//! How `OpenAI` Realtime offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this provider.
const FEATURE: &str = "s2s-openai";

/// The provider's one entry: the hosted API.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![WizardChoice::new("OpenAI Realtime", WizardDeploy::Cloud {
            key_env: "OPENAI_API_KEY".into(),
        }).with_feature(Some(FEATURE.into())).with_fields(vec![
            // GA model id — the dated `gpt-4o-realtime-preview-*` SKUs were
            // retired with the GA launch and fail the websocket connect.
            WizardField::text("model", "OpenAI model name", "gpt-realtime"),
            WizardField::text("voice", "OpenAI voice identifier", "alloy"),
            WizardField::fixed("vad", "server_vad"),
            WizardField::fixed("api_key_env", "OPENAI_API_KEY"),
            WizardField::fixed(
                "instructions",
                "You are a concise voice assistant. Speak English unless the user explicitly asks for another language.",
            ),
            WizardField::fixed_literal("options.output_emit_rate", "16000"),
            WizardField::secret("OPENAI_API_KEY", "OpenAI API key"),
        ]).with_menu_rank(20)]
}
