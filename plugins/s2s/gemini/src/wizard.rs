//! How Gemini Live offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this provider.
const FEATURE: &str = "s2s-gemini";

/// The provider's one entry: the hosted API, the speech-to-speech stage's
/// out-of-the-box choice.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![WizardChoice::new("Gemini Live", WizardDeploy::Cloud {
            key_env: "GEMINI_API_KEY".into(),
        }).with_feature(Some(FEATURE.into())).with_fields(vec![
            WizardField::text(
                "model",
                "Gemini model name",
                "gemini-2.5-flash-native-audio-latest",
            ),
            WizardField::text("voice", "Gemini voice identifier", "Puck"),
            WizardField::fixed("vad", "server_vad"),
            WizardField::fixed("api_key_env", "GEMINI_API_KEY"),
            WizardField::fixed(
                "instructions",
                "You are a concise voice assistant. Speak English unless the user explicitly asks for another language.",
            ),
            WizardField::comment(
                "Native-audio models default to dynamic thinking, which adds a\n\
                 variable pre-speech delay; 0 disables it (-1 = dynamic, 1..32768 = cap).",
            ),
            WizardField::fixed_literal("options.thinking_budget", "0"),
            WizardField::comment(
                "Gemini decides when the caller has finished. At its defaults a clean\n\
                 request followed by silence once went unanswered for 21 s; these make\n\
                 it close the turn sooner. Raise vad_silence_ms if it cuts callers off.",
            ),
            WizardField::fixed("options.end_sensitivity", "high"),
            WizardField::fixed_literal("options.vad_silence_ms", "700"),
            WizardField::secret("GEMINI_API_KEY", "Gemini API key"),
        ]).with_stage_default(true).with_menu_rank(10)]
}
