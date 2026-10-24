//! How the OpenAI-compatible LLM offers itself in the setup wizard: a server
//! the operator already runs, and Gemma presets the wizard downloads and
//! serves from the llama-server sidecar.

use voice_pipes_sdk::{
    WizardChoice, WizardDeploy, WizardDownload, WizardField, WizardLayout, WizardModel,
};

/// The Cargo feature the worker image needs for this provider.
const FEATURE: &str = "llm-openai-compat";

/// Every entry this provider offers, in menu order.
pub fn wizard_choices() -> Vec<WizardChoice> {
    let mut choices = vec![host_server()];
    choices.extend(GEMMA.iter().map(gemma_preset));
    choices
}

/// A server the operator runs, asked for by URL and model name. The LLM
/// stage's out-of-the-box choice.
fn host_server() -> WizardChoice {
    WizardChoice::new(
        "OpenAI-Compatible (e.g. llama-server on host)",
        WizardDeploy::None,
    )
    .with_feature(Some(FEATURE.into()))
    .with_fields(vec![
        WizardField::text(
            "base_url",
            "OpenAI-compatible Base URL",
            "http://host.docker.internal:8081/v1",
        ),
        WizardField::text("model", "LLM Model Name", "gemma4-e2b"),
        WizardField::fixed(
            "system_prompt",
            "You are a helpful voice assistant. Be concise.",
        ),
        WizardField::fixed_literal("max_tokens", "256"),
        WizardField::fixed_literal("temperature", "0.7"),
    ])
    .with_stage_default(true)
    .with_menu_rank(10)
}

/// A Gemma model served by the llama-server sidecar.
struct Gemma {
    label: &'static str,
    /// Its name in the wizard's model registry.
    key: &'static str,
    /// The model name llama-server answers to, and the GGUF's stem.
    model: &'static str,
    /// The Hugging Face repository the GGUF comes from.
    repo: &'static str,
    menu_rank: u16,
}

const GEMMA: &[Gemma] = &[
    Gemma {
        label: "Gemma 4 E2B via llama-server sidecar (2.3B effective, ~3 GB; CPU or low VRAM)",
        key: "gemma4-e2b",
        model: "gemma-4-E2B-it",
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        menu_rank: 20,
    },
    Gemma {
        label: "Gemma 4 E4B via llama-server sidecar (4.5B effective, ~5 GB; ~6 GB VRAM)",
        key: "gemma4-e4b",
        model: "gemma-4-E4B-it",
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        menu_rank: 21,
    },
    Gemma {
        label: "Gemma 4 12B via llama-server sidecar (12B dense, 256k context, ~7.1 GB; ~10 GB VRAM)",
        key: "gemma4-12b",
        // Lowercase "12b" matches the unsloth repository and file names
        // (unlike the uppercase E2B/E4B ones); the download is named after it.
        model: "gemma-4-12b-it",
        repo: "unsloth/gemma-4-12b-it-GGUF",
        menu_rank: 22,
    },
    Gemma {
        label: "Gemma 3 1B via llama-server sidecar (GPU, ~700 MB)",
        key: "gemma3-1b",
        model: "gemma-3-1b-it",
        repo: "unsloth/gemma-3-1b-it-GGUF",
        menu_rank: 30,
    },
    Gemma {
        label: "Gemma 3 4B via llama-server sidecar (GPU, ~2.7 GB)",
        key: "gemma3-4b",
        model: "gemma-3-4b-it",
        repo: "unsloth/gemma-3-4b-it-GGUF",
        menu_rank: 31,
    },
];

fn gemma_preset(g: &Gemma) -> WizardChoice {
    let file = format!("{}-Q4_K_M.gguf", g.model);
    WizardChoice::new(
        g.label,
        WizardDeploy::DockerSidecarModel {
            profile: "llama".into(),
            service: "llama-server".into(),
            model_env: "LLAMA_MODEL".into(),
            requires_gpu: false,
            ensure_dirs: vec![],
            model: WizardModel::new(
                g.key,
                format!("models/{}/{file}", g.key),
                WizardLayout::File,
                vec![WizardDownload::new(
                    &file,
                    &format!("https://huggingface.co/{}/resolve/main/{file}", g.repo),
                )],
            ),
        },
    )
    .with_feature(Some(FEATURE.into()))
    .with_fields(vec![
        WizardField::fixed("base_url", "http://llama-server:8081/v1"),
        WizardField::fixed("model", g.model),
        WizardField::fixed("api_key", ""),
        WizardField::fixed(
            "system_prompt",
            "You are a friendly voice assistant. Keep responses to one or two short sentences.",
        ),
        WizardField::fixed_literal("max_tokens", "1024"),
        WizardField::fixed_literal("temperature", "1.0"),
        WizardField::fixed_literal("top_p", "0.95"),
    ])
    .with_menu_rank(g.menu_rank)
}

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod tests;
