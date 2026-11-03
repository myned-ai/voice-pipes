//! Provider-owned parameter schema for the OpenAI-compatible text processor:
//! what the graph editor shows for it, and which of its settings can change
//! while a session runs.

use crate::config as oac;
use voice_pipes_sdk::TextProcessorConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

const KNOWN_KEYS: &[&str] = &[
    "base_url",
    "model",
    "timeout_secs",
    "max_retries",
    "response_format",
    "system_prompt",
    "max_tokens",
    "temperature",
    "top_p",
    "stop",
];

pub fn schema(cfg: &TextProcessorConfig) -> ProviderSchema {
    let mut s = ProviderSchema::default();
    let opts = &cfg.options;

    // ── Identity / transport (static) ─────────────────────
    if let Some(v) = opts.get("base_url") {
        s.push(
            ParamDescriptor::static_str(
                "base_url",
                "Base URL",
                "Base URL of the OpenAI-compatible chat completions endpoint \
                 (e.g. `https://api.openai.com/v1` or a llama.cpp server).",
            ),
            v.clone(),
        );
    }
    if let Some(v) = opts.get("model") {
        s.push(
            ParamDescriptor::static_str(
                "model",
                "Model",
                "Model identifier sent in the request body (e.g. `gpt-4o-mini`).",
            ),
            v.clone(),
        );
    }
    // `api_key` itself is a secret -- surface only a "set" boolean.
    s.push(
        ParamDescriptor::static_bool(
            "api_key_set",
            "API key",
            "Whether an API key is configured (env: `OPENAI_API_KEY`).",
        ),
        cfg.api_key.is_some(),
    );
    if let Some(v) = opts.get("timeout_secs") {
        s.push(
            ParamDescriptor::static_int_display(
                "timeout_secs",
                "Timeout (s)",
                "Per-request HTTP timeout, in seconds.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = opts.get("max_retries") {
        s.push(
            ParamDescriptor::static_int_display(
                "max_retries",
                "Max retries",
                "Number of retry attempts on transient HTTP failures.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = opts.get("response_format") {
        s.push(
            ParamDescriptor::static_str(
                "response_format",
                "Response format",
                "OpenAI `response_format.type` value (e.g. `json_object`).",
            ),
            v.clone(),
        );
    }

    // ── Mutable cells (live ParamRegistry) ────────────────
    // Always emitted -- defaults are injected when the TOML omits them so the
    // editor row always renders.
    s.push(
        ParamDescriptor::mutable_str(
            "system_prompt",
            "System prompt",
            "Instruction prepended as the `system` message on every request. \
             Changes take effect on the next turn.",
        ),
        opts.get("system_prompt")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(oac::DEFAULT_SYSTEM_PROMPT)),
    );
    s.push(
        ParamDescriptor::mutable_int(
            "max_tokens",
            "Max tokens",
            "Upper bound on tokens generated per response.",
            1,
            32_768,
        ),
        opts.get("max_tokens")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(oac::DEFAULT_MAX_TOKENS)),
    );
    s.push(
        ParamDescriptor::mutable_float(
            "temperature",
            "Temperature",
            "Sampling temperature (0.0 = greedy, 2.0 = very random).",
            0.0,
            2.0,
        ),
        opts.get("temperature")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(oac::DEFAULT_TEMPERATURE)),
    );
    s.push(
        ParamDescriptor::mutable_float(
            "top_p",
            "Top-p",
            "Nucleus sampling cutoff (0.0-1.0). Empty / null disables `top_p`.",
            0.0,
            1.0,
        ),
        opts.get("top_p")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    s.push(
        ParamDescriptor::mutable_str(
            "stop",
            "Stop sequences",
            "Newline-separated stop strings (max 4). Empty disables.",
        ),
        opts.get("stop")
            .cloned()
            .unwrap_or_else(|| serde_json::json!("")),
    );

    s.passthrough_options(
        &cfg.options,
        KNOWN_KEYS,
        "OpenAI-compat provider-specific option.",
    );

    s
}
