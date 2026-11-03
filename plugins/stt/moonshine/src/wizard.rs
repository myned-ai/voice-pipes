//! How Moonshine offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this recogniser.
const FEATURE: &str = "stt-moonshine";

/// The recogniser's one entry, second in its menu (rank 2, the place it had
/// as a built-in, after Parakeet). Its files are the model registry's
/// `moonshine` entry, so an operator's `config/wizard_models.json` override
/// still applies.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "Moonshine (Local ONNX)",
            WizardDeploy::Registry {
                key: "moonshine".into(),
            },
        )
        .with_feature(Some(FEATURE.into()))
        .with_fields(vec![WizardField::path(
            "model_dir",
            "Moonshine model directory path",
            "models/moonshine",
        )])
        .with_menu_rank(2),
    ]
}
