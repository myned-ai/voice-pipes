//! Setup wizard entries for NeuTTS-Air.

use voice_pipes_sdk::{
    WizardChoice, WizardDeploy, WizardDownload, WizardField, WizardLayout,
};

const HF_AIR_BASE: &str = "https://huggingface.co/neuphonic/neutts-air-q4-gguf/resolve/main";
const HF_CODEC_BASE: &str = "https://huggingface.co/neuphonic/neucodec-onnx-decoder/resolve/main";

/// Choices this provider offers to `voice-pipes-cli config-wizard`.
#[must_use]
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "NeuTTS-Air (Local GGUF + NeuCodec)",
            WizardDeploy::Download {
                layout: WizardLayout::File,
                files: vec![
                    WizardDownload::new(
                        "neutts-air-q4_k_m.gguf",
                        &format!("{HF_AIR_BASE}/neutts-air-q4_k_m.gguf"),
                    ),
                    WizardDownload::new(
                        "neucodec_decoder.onnx",
                        &format!("{HF_CODEC_BASE}/neucodec_decoder.onnx"),
                    ),
                ],
            },
        )
        .with_feature(Some("tts-neutts".into()))
        .with_fields(vec![
            WizardField::path(
                "model",
                "NeuTTS-Air GGUF model path",
                "models/neutts-air-q4_k_m.gguf",
            ),
            WizardField::fixed("vocoder", "models/neucodec_decoder.onnx"),
        ])
        .with_menu_rank(25),
    ]
}
