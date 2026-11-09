//! How the English Nemotron offers itself in the setup wizard.

use voice_pipes_sdk::WizardChoice;

/// The Cargo feature the worker image needs for this recogniser.
#[expect(
    dead_code,
    reason = "kept for feature inventory and tests while wizard offering is disabled"
)]
const FEATURE: &str = "stt-nemotron";

/// The recogniser offers no setup-wizard entry: the ONNX files are not
/// distributed pre-exported (the `.nemo` checkpoint from Hugging Face
/// requires a manual `NeMo` -> ONNX export), so offering it in the interactive
/// wizard causes immediate container crashes for users without local weights.
/// Use `Nemotron 3.5 Streaming` instead, which auto-downloads from Hugging Face.
/// Advanced users with an existing export can still configure this provider
/// manually in `pipeline.toml` via `provider = "nemotron"`.
pub fn wizard_choices() -> Vec<WizardChoice> {
    Vec::new()
}
