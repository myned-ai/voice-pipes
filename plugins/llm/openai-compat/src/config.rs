//! Configuration for the OpenAI-compatible text processor.
//!
//! Parses values from `TextProcessorConfig.options` (the flat TOML map)
//! and resolves API keys from environment variables.

use serde_json::Value;
use std::collections::HashMap;

// ── Defaults ────────────────────────────────────────────────

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";
/// The variable the host reads the API key from when the section sets
/// neither `api_key` nor `api_key_env`.
pub const API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a helpful assistant. Respond briefly.";
pub const DEFAULT_MAX_TOKENS: u32 = 256;
pub const DEFAULT_TEMPERATURE: f64 = 0.7;
/// Longest wait for the next byte from the server: the response headers
/// first, then each streamed chunk. A slow but steadily streaming
/// response is never cut; a stalled one is.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// TCP connect budget; separate from the read timeout so an unreachable
/// host fails fast without shortening the streaming budget.
pub const CONNECT_TIMEOUT_SECS: u64 = 5;
pub const DEFAULT_MAX_RETRIES: u32 = 2;
/// Upper bound on `max_retries`; keeps the retry loop from turning one
/// bad turn into minutes of back-off.
pub const MAX_RETRIES_LIMIT: u32 = 5;
/// Rolling conversation-history window, in user/assistant turn pairs.
/// Without a cap the prompt grows every turn until latency spikes or the
/// backend rejects it with a context-length error.
pub const DEFAULT_HISTORY_MAX_TURNS: u32 = 20;

// ── Config struct ───────────────────────────────────────────

/// Parsed configuration for a single OpenAI-compatible processor instance.
#[derive(Clone)]
pub struct OpenAiCompatConfig {
    /// Base URL (e.g. `https://api.openai.com/v1` or `http://localhost:8080/v1`).
    pub base_url: String,
    /// Model name (e.g. `gpt-4o`, `gpt-4o-mini`, `local-model`).
    pub model: String,
    /// API key. Empty is allowed for local servers.
    pub api_key: voice_pipes_sdk::secrets::Secret,
    /// System prompt prepended to every conversation.
    pub system_prompt: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,

    // ── Sampling parameters ──
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub seed: Option<u64>,
    pub stop: Option<Vec<String>>,

    // ── Network parameters ──
    /// Longest wait, in seconds, for the next byte from the server
    /// (response headers, then each streamed chunk). At least 1.
    pub timeout_secs: u64,
    /// Number of retries on transient failures (5xx, 429, timeouts).
    /// Clamped to `MAX_RETRIES_LIMIT`.
    pub max_retries: u32,

    // ── Response control ──
    /// Response format type: `"text"` (default) or `"json_object"`.
    pub response_format: Option<String>,
    /// Tool-choice policy sent when client-side tools are declared:
    /// `"auto"` (provider default when unset), `"none"`, or `"required"`.
    pub tool_choice: Option<String>,
    /// Fields added to every request body as given, for what a server
    /// takes beyond the common API. A thinking model served by
    /// llama-server spends its whole token budget on reasoning the
    /// pipeline never speaks unless the template is told
    /// `chat_template_kwargs = { enable_thinking = false }`.
    pub extra_body: Option<serde_json::Map<String, Value>>,

    // ── History control ──
    /// Rolling conversation-history window in turn pairs; oldest turns
    /// evict past it. Clamped to at least 1.
    pub history_max_turns: u32,
}

/// The API key never appears in logs or diagnostics; only whether one
/// is set.
impl std::fmt::Debug for OpenAiCompatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .field("system_prompt", &self.system_prompt)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("top_p", &self.top_p)
            .field("presence_penalty", &self.presence_penalty)
            .field("frequency_penalty", &self.frequency_penalty)
            .field("seed", &self.seed)
            .field("stop", &self.stop)
            .field("timeout_secs", &self.timeout_secs)
            .field("max_retries", &self.max_retries)
            .field("response_format", &self.response_format)
            .field("tool_choice", &self.tool_choice)
            .field("extra_body", &self.extra_body)
            .field("history_max_turns", &self.history_max_turns)
            .finish()
    }
}

// ── Parsing from TOML options map ───────────────────────────

impl OpenAiCompatConfig {
    /// The processor's config: its options, and the API key the host
    /// resolved (from `api_key`, the variable `api_key_env` names, or
    /// `OPENAI_API_KEY`). No key is valid, for a local server.
    ///
    /// # Errors
    /// Every problem with the options.
    pub fn from_config(
        cfg: &voice_pipes_sdk::TextProcessorConfig,
    ) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut config = Self::from_options(&cfg.options)?;
        config.api_key = voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref())
            .cloned()
            .unwrap_or_default();
        Ok(config)
    }

    /// Parse from `TextProcessorConfig.options`: every value typed and
    /// bounded, every problem and every key this processor does not read
    /// reported. The key is not among them; [`from_config`](Self::from_config)
    /// adds it.
    ///
    /// # Errors
    /// Every problem with the options.
    pub fn from_options(
        opts: &HashMap<String, Value>,
    ) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = voice_pipes_sdk::options::ProviderOptions::new("text_processor", opts)
            .with_engine_keys(&[
                voice_pipes_sdk::options::ALLOW_INSECURE_OPTION,
                voice_pipes_sdk::text::AUDIT_FULL_PROMPTS_OPTION,
            ]);

        let stop = match o.value("stop") {
            None => None,
            Some(Value::String(s)) => Some(vec![s.clone()]),
            Some(Value::Array(arr)) => {
                let strings: Option<Vec<String>> =
                    arr.iter().map(|s| s.as_str().map(String::from)).collect();
                if strings.is_none() {
                    o.error("stop", "must be a string or a list of strings");
                }
                strings.filter(|s| !s.is_empty())
            }
            Some(other) => {
                o.error(
                    "stop",
                    format!("must be a string or a list of strings, got {other}"),
                );
                None
            }
        };

        let base_url = o.string_or("base_url", DEFAULT_BASE_URL);
        if base_url.is_empty() {
            o.error("base_url", "must not be empty");
        }
        let tool_choice = o.string("tool_choice");
        if let Some(tc) = &tool_choice
            && !matches!(tc.as_str(), "auto" | "none" | "required")
        {
            o.error(
                "tool_choice",
                format!("must be \"auto\", \"none\", or \"required\", got \"{tc}\""),
            );
        }
        let extra_body = match o.value("extra_body") {
            None => None,
            Some(Value::Object(map)) => {
                // A key the common API owns is set by its own option, so
                // the two cannot disagree about what is sent.
                let owned = [
                    "model",
                    "messages",
                    "stream",
                    "tools",
                    "tool_choice",
                    "max_tokens",
                ];
                for key in map.keys().filter(|k| owned.contains(&k.as_str())) {
                    o.error(
                        "extra_body",
                        format!("`{key}` is set by its own option, not here"),
                    );
                }
                Some(map.clone())
            }
            Some(other) => {
                o.error("extra_body", format!("must be a table, got {other}"));
                None
            }
        };
        let config = Self {
            base_url,
            model: o.string_or("model", DEFAULT_MODEL),
            api_key: voice_pipes_sdk::secrets::Secret::default(),
            system_prompt: o.string_or("system_prompt", DEFAULT_SYSTEM_PROMPT),
            max_tokens: o.u32_in("max_tokens", 1..=1_000_000, DEFAULT_MAX_TOKENS),
            temperature: Some(o.f64_in("temperature", 0.0..=2.0, DEFAULT_TEMPERATURE)),
            top_p: o.optional_f64_in("top_p", 0.0..=1.0),
            presence_penalty: o.optional_f64_in("presence_penalty", -2.0..=2.0),
            frequency_penalty: o.optional_f64_in("frequency_penalty", -2.0..=2.0),
            seed: o.optional_u64_in("seed", 0..=u64::MAX),
            stop,
            timeout_secs: o.u64_in("timeout_secs", 1..=3_600, DEFAULT_TIMEOUT_SECS),
            max_retries: o.u32_in("max_retries", 0..=MAX_RETRIES_LIMIT, DEFAULT_MAX_RETRIES),
            response_format: o.string("response_format"),
            tool_choice,
            extra_body,
            // 0 would drop the turn being recorded.
            history_max_turns: o.u32_in("history_max_turns", 1..=10_000, DEFAULT_HISTORY_MAX_TURNS),
        };
        o.finish()?;
        Ok(config)
    }

    /// The full URL for the chat completions endpoint.
    pub fn completions_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    /// Longest wait for the next byte from the server.
    pub fn read_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.max(1))
    }
}

// ── Validation ──────────────────────────────────────────────

/// Every problem with the options, and a key sent in plain text to a
/// non-local server.
pub fn validate(cfg: &voice_pipes_sdk::TextProcessorConfig) -> Vec<voice_pipes_sdk::ConfigError> {
    let resolved = match OpenAiCompatConfig::from_config(cfg) {
        Ok(resolved) => resolved,
        Err(problems) => return problems,
    };
    let mut errs = Vec::new();
    if let Some(e) = voice_pipes_net::credential_endpoint_error(
        "text_processor.base_url",
        &resolved.base_url,
        !resolved.api_key.expose().trim().is_empty(),
        &cfg.options,
    ) {
        errs.push(e);
    }
    errs
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
