//! How Piper offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this voice.
const FEATURE: &str = "tts-piper";

/// The voice's one entry, first in its menu (rank 1, the place it had as a
/// built-in). Its files are the model registry's `piper` entry, so an
/// operator's `config/wizard_models.json` override still applies.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "Piper (Local ONNX)",
            WizardDeploy::Registry {
                key: "piper".into(),
            },
        )
        .with_feature(Some(FEATURE.into()))
        .with_fields(vec![WizardField::path(
            "model",
            "Piper ONNX model path",
            "models/en_US-lessac-medium.onnx",
        )])
        .with_menu_rank(1),
    ]
}
