//! What `DeepFilterNet` gives the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The audio filter settings the wizard writes when `DeepFilterNet` is selected.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "DeepFilterNet3 (Offline Noise Suppression)",
            WizardDeploy::Registry {
                key: "deepfilter".into(),
            },
        )
        .with_fields(vec![WizardField::path(
            "model",
            "DeepFilterNet3 model path (optional: runs pure-Rust DSP when omitted)",
            "models/deepfilternet3_ll.onnx",
        )]),
    ]
}
