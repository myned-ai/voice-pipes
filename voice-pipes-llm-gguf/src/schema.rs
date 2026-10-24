//! Provider-owned parameter schema for the local LLM text processor.
//!

use voice_pipes_sdk::TextProcessorConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

const KNOWN_KEYS: &[&str] = &[
    "model",
    "chat_template",
    "device",
    "prefill_cache",
    "system_prompt",
    "max_tokens",
    "temperature",
    "top_p",
    "repeat_penalty",
];

pub fn schema(cfg: &TextProcessorConfig) -> ProviderSchema {
    let mut s = ProviderSchema::default();

    if let Some(v) = cfg.options.get("model") {
        s.push(
            ParamDescriptor::static_path("model", "Model", "Path to the GGUF model file."),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("chat_template") {
        s.push(
            ParamDescriptor::static_str(
                "chat_template",
                "Chat template",
                "Prompt template format: 'chatml', 'llama3', 'gemma3' or 'gemma4'; it must match the model.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("device") {
        s.push(
            ParamDescriptor::static_str(
                "device",
                "Device",
                "Compute device for inference: 'cpu', 'cuda' or 'metal'.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("prefill_cache") {
        s.push(
            ParamDescriptor::static_bool(
                "prefill_cache",
                "Prefill cache",
                "Pre-fill the system prompt's KV cache on load; refused, with a warning, by backends that cannot continue a batch at an offset (both candle architectures today).",
            ),
            v.clone(),
        );
    }

    // Mutable cells (wired through Layer-2 ParamRegistry).
    if let Some(v) = cfg.options.get("system_prompt") {
        s.push(
            ParamDescriptor::mutable_str(
                "system_prompt",
                "System prompt",
                "Instruction that defines the assistant's persona and task. \
                 Changes take effect on the next turn.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("max_tokens") {
        s.push(
            ParamDescriptor::mutable_int(
                "max_tokens",
                "Max tokens",
                "Maximum number of tokens to generate per response.",
                1,
                32_768,
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("temperature") {
        s.push(
            ParamDescriptor::mutable_float(
                "temperature",
                "Temperature",
                "Sampling temperature -- lower is more deterministic (0.0 = greedy, 2.0 = very random).",
                0.0,
                2.0,
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("top_p") {
        s.push(
            ParamDescriptor::mutable_float(
                "top_p",
                "Top-p",
                "Nucleus sampling cutoff -- only tokens with cumulative probability \
                 up to this value are considered (0.0-1.0).",
                0.0,
                1.0,
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("repeat_penalty") {
        s.push(
            ParamDescriptor::mutable_float(
                "repeat_penalty",
                "Repeat penalty",
                "Penalty multiplier for repeated tokens -- values > 1.0 discourage repetition.",
                1.0,
                2.0,
            ),
            v.clone(),
        );
    }

    s.passthrough_options(&cfg.options, KNOWN_KEYS, "LLM provider-specific option.");

    s
}
