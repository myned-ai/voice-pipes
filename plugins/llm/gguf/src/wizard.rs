//! How the local GGUF model offers itself in the setup wizard.

use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

/// The Cargo feature the worker image needs for this processor.
const FEATURE: &str = "llm-gguf";

/// A `SmolLM2` size the wizard offers.
struct Size {
    label: &'static str,
    /// Its name in the model registry (`config/wizard_models.json`), which
    /// lists the GGUF and its tokenizer.
    key: &'static str,
    /// Where the registry puts the GGUF; the tokenizer lands beside it,
    /// where the processor looks for it when the config names none.
    path: &'static str,
    menu_rank: u16,
}

/// The sizes, in menu order. 360M is first: on a laptop CPU it answers at
/// about 20 tokens/s and stays on topic, where 135M rambles and 1.7B runs at
/// about 9 tokens/s.
const SIZES: &[Size] = &[
    Size {
        label: "Local GGUF: SmolLM2-360M (in-process, ~270 MB, CPU)",
        key: "smollm2-360m",
        path: "models/smollm2-360m/SmolLM2-360M-Instruct-Q4_K_M.gguf",
        menu_rank: 5,
    },
    Size {
        label: "Local GGUF: SmolLM2-1.7B (in-process, ~1 GB, better replies, slower on CPU)",
        key: "smollm2-1.7b",
        path: "models/smollm2-1.7b/SmolLM2-1.7B-Instruct-Q4_K_M.gguf",
        menu_rank: 6,
    },
    Size {
        label: "Local GGUF: SmolLM2-135M (in-process, ~80 MB, fastest, weakest replies)",
        key: "smollm2-135m",
        path: "models/smollm2/SmolLM2-135M-Instruct-Q4_K_M.gguf",
        menu_rank: 7,
    },
];

/// The processor's entries, one per size.
pub fn wizard_choices() -> Vec<WizardChoice> {
    SIZES.iter().map(choice).collect()
}

fn choice(size: &Size) -> WizardChoice {
    WizardChoice::new(
        size.label,
        WizardDeploy::Registry {
            key: size.key.into(),
        },
    )
    .with_feature(Some(FEATURE.into()))
    .with_fields(vec![
        WizardField::path("model", "Local GGUF model file path", size.path),
        WizardField::fixed(
            "system_prompt",
            "You are a helpful voice assistant. Answer in one or two short spoken sentences.",
        ),
    ])
    .with_menu_rank(size.menu_rank)
}
