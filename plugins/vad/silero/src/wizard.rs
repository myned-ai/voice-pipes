//! What Silero gives the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The `[vad]` settings the wizard writes when Silero is the auto backend:
/// its model, the model registry's `silero` entry, which the wizard
/// downloads (an operator's `config/wizard_models.json` override still
/// applies). A VAD has no menu, so this is the one entry.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "Silero (Local ONNX)",
            WizardDeploy::Registry {
                key: "silero".into(),
            },
        )
        .with_fields(vec![WizardField::path(
            "model",
            "Silero VAD model path",
            "models/silero_vad.onnx",
        )]),
    ]
}
