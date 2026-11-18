//! How Parakeet offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this recogniser.
const FEATURE: &str = "stt-parakeet";

/// The recogniser's entry: the English v2 export, first in its menu (rank 1,
/// the place it had as a built-in), marked stage default so it is preselected
/// out of the box in the setup wizard. It names its files
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
        .with_stage_default(true)
        .with_menu_rank(1),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parakeet_is_stage_default_in_wizard() {
        let choices = wizard_choices();
        let choice = choices.first().expect("parakeet wizard choice present");
        assert_eq!(choice.label, "Parakeet (Local ONNX)");
        assert!(choice.stage_default);
        assert_eq!(choice.menu_rank, 1);
        assert_eq!(choice.feature.as_deref(), Some(FEATURE));
    }
}
