//! How Smart Turn offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardDownload, WizardField, WizardLayout};

/// The Cargo feature the worker image needs for this detector.
const FEATURE: &str = "turn-smart";

/// The detector's one entry: the CPU export, downloaded by the wizard.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![WizardChoice::new("Smart (Pipecat Smart Turn v3.2 — semantic end-of-turn model)", WizardDeploy::Download {
            layout: WizardLayout::File,
            files: vec![WizardDownload::new(
                "smart-turn-v3.2-cpu.onnx",
                "https://huggingface.co/pipecat-ai/smart-turn-v3/resolve/main/smart-turn-v3.2-cpu.onnx",
            )],
        }).with_feature(Some(FEATURE.into())).with_fields(vec![
            WizardField::fixed_literal("silence_timeout_ms", "2000"),
            // Relative to the worker's working directory (/app in the
            // images), matching the mounted models/ volume.
            WizardField::path(
                "smart_model_path",
                "Smart Turn model path",
                "models/smart-turn-v3.2-cpu.onnx",
            ),
            WizardField::fixed_literal("smart_threshold", "0.5"),
            WizardField::fixed_literal("smart_window_secs", "8"),
            WizardField::fixed_literal("smart_min_audio_ms", "500"),
        ]).with_notes(vec![
            "Smart Turn semantic end-of-turn detection reduces mid-thought cutoffs; it runs on \
             CPU (~tens of ms). silence_timeout_ms is the hard fallback."
                .into(),
        ]).with_menu_rank(10)]
}
