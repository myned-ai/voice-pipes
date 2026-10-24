//! How the English Nemotron offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this recogniser.
const FEATURE: &str = "stt-nemotron";

/// The recogniser's one entry. The ONNX files are not distributed
/// pre-exported: the `.nemo` checkpoint comes from Hugging Face
/// (nvidia/nemotron-speech-streaming-en-0.6b) and is exported once, so the
/// wizard only points at the resulting directory.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "Nemotron-Streaming (Local ONNX, requires one-time export)",
            WizardDeploy::LocalManual,
        )
        .with_feature(Some(FEATURE.into()))
        .with_fields(vec![WizardField::path(
            "model_dir",
            "Nemotron exported model directory path",
            "models/nemotron-streaming/7-10-7",
        )])
        .with_notes(vec![
            "Nemotron needs a one-time model export (NeMo -> ONNX). If the directory is \
             empty, follow scripts/README-export-nemotron.md to produce encoder/decoder/\
             joint.onnx + export_metadata.json + tokenizer.vocab."
                .into(),
        ])
        .with_menu_rank(5),
    ]
}
