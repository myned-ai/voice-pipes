//! How Parakeet offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this recogniser.
const FEATURE: &str = "stt-parakeet";

/// The recogniser's entry: the English v2 export, first in its menu (rank 1,
/// the place it had as a built-in), so preselected unless a linked plugin
/// claims the stage's preselection (`AssemblyAI` does). It names its files
/// through the model registry (`parakeet`), so an operator's
/// `config/wizard_models.json` override still applies. Language fine-tunes
/// such as the Greek one are not offered here: they ship their own configs
/// and are fetched by `voice-pipes models fetch <key>`.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "Parakeet (Local ONNX)",
            WizardDeploy::Registry {
                key: "parakeet".into(),
            },
        )
        .with_feature(Some(FEATURE.into()))
        .with_fields(vec![WizardField::path(
            "model_dir",
            "Parakeet model directory path",
            "models/parakeet-tdt",
        )])
        .with_menu_rank(1),
    ]
}
