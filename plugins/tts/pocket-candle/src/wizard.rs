//! Config-wizard entry for native Rust/Candle Pocket-TTS.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardDownload, WizardField, WizardLayout};

const HF_MODEL_URL: &str = "https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/resolve/main/languages/english/model.safetensors";
const HF_TOKENIZER_URL: &str =
    "https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/resolve/main/tokenizer.model";
const HF_ALBA_VOICE_URL: &str = "https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/resolve/main/languages/english/embeddings/alba.safetensors";

/// Menu choice the wizard adds under TTS when `tts-pocket-candle` is linked.
pub fn wizard_choices() -> Vec<WizardChoice> {
    vec![
        WizardChoice::new(
            "Pocket-TTS Candle (native Rust, zero-copy multi-threaded)",
            WizardDeploy::Download {
                layout: WizardLayout::Dir,
                files: vec![
                    WizardDownload::new("model.safetensors", HF_MODEL_URL),
                    WizardDownload::new("tokenizer.model", HF_TOKENIZER_URL),
                    WizardDownload::new("voices/alba.safetensors", HF_ALBA_VOICE_URL),
                ],
            },
        )
        .with_feature(Some("tts-pocket-candle".into()))
        .with_fields(vec![
            WizardField::path("model_dir", "Model directory", "models/pocket-tts"),
            WizardField::fixed("voice", "alba"),
        ])
        .with_menu_rank(32),
    ]
}
