//! Provider configuration types shared between SDK and framework.
//!
//! Both [`SttConfig`] and [`TtsConfig`] live here so plugin crates depend
//! only on this crate and never on the full `voice-pipes` binary.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use crate::secrets::{Secret, debug_options};

/// Configuration for an STT provider, parsed from TOML `[stt]`.
///
/// The framework populates this from the worker's TOML and passes it to
/// [`super::stt::SttFactory::validate`] and [`super::stt::SttFactory::build`].
#[derive(Clone)]
#[non_exhaustive]
pub struct SttConfig {
    /// Provider name as it appears in TOML `provider = "..."`.
    pub provider: String,
    /// Directory containing the required ONNX model files (local providers).
    pub model_dir: Option<PathBuf>,
    /// Path to a specific model file (local providers, overrides `model_dir`).
    pub model: Option<PathBuf>,
    /// API key for hosted STT providers (Deepgram, Google, Azure, etc.).
    pub api_key: Option<Secret>,
    /// API endpoint URL (for custom or self-hosted endpoints).
    pub endpoint: Option<String>,
    /// Model name for API providers (e.g. `"nova-3"`, `"whisper-large-v3"`).
    pub model_name: Option<String>,
    /// Provider-specific key-value options (anything not matched by the above fields).
    pub options: HashMap<String, serde_json::Value>,
}

// ── Custom serde — flatten unknown fields into `options` ─────

impl<'de> Deserialize<'de> for SttConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawSttConfig {
            provider: String,
            model_dir: Option<PathBuf>,
            model: Option<PathBuf>,
            api_key: Option<Secret>,
            endpoint: Option<String>,
            model_name: Option<String>,
            #[serde(flatten)]
            extra: HashMap<String, serde_json::Value>,
        }

        let mut raw = RawSttConfig::deserialize(deserializer)?;

        let mut options = HashMap::new();
        if let Some(serde_json::Value::Object(map)) = raw.extra.remove("options") {
            for (k, v) in map {
                options.insert(k, v);
            }
        }
        for (k, v) in raw.extra {
            options.insert(k, v);
        }

        Ok(SttConfig {
            provider: raw.provider,
            model_dir: raw.model_dir,
            model: raw.model,
            api_key: raw.api_key,
            endpoint: raw.endpoint,
            model_name: raw.model_name,
            options,
        })
    }
}

impl Serialize for SttConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct SerializingSttConfig<'a> {
            provider: &'a String,
            #[serde(skip_serializing_if = "Option::is_none")]
            model_dir: &'a Option<PathBuf>,
            #[serde(skip_serializing_if = "Option::is_none")]
            model: &'a Option<PathBuf>,
            #[serde(skip_serializing_if = "Option::is_none")]
            api_key: &'a Option<Secret>,
            #[serde(skip_serializing_if = "Option::is_none")]
            endpoint: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            model_name: &'a Option<String>,
            /// Secret-named values replaced, like `api_key` above.
            #[serde(flatten)]
            options: serde_json::Value,
        }

        let temp = SerializingSttConfig {
            provider: &self.provider,
            model_dir: &self.model_dir,
            model: &self.model,
            api_key: &self.api_key,
            endpoint: &self.endpoint,
            model_name: &self.model_name,
            options: crate::secrets::debug_options(&self.options),
        };
        temp.serialize(serializer)
    }
}

impl SttConfig {
    /// A reader over this section's options (`stt.<key>` in
    /// errors), with the keys the engine itself reads already accounted for.
    pub fn options(&self) -> crate::options::ProviderOptions<'_> {
        crate::options::ProviderOptions::new("stt", &self.options)
            .with_engine_keys(&[crate::options::ALLOW_INSECURE_OPTION])
    }

    /// Returns JSON descriptions of every STT configuration field.
    #[deprecated(
        since = "1.1.0",
        note = "nothing reads it: the console describes a provider's settings from its `param_schema`"
    )]
    pub fn metadata_descriptions() -> serde_json::Value {
        serde_json::json!({
            "provider":   "The framework or service providing STT capabilities",
            "model_dir":  "Directory containing the required model files",
            "model":      "Path to the specific STT model file",
            "api_key":    "API key for hosted STT providers",
            "endpoint":   "API endpoint URL for custom endpoints",
            "model_name": "Model name for API providers",
            "pool_size":  "Number of concurrent inference sessions (default: 1)"
        })
    }
}

// ── Validation error ─────────────────────────────────────────

/// A single configuration validation error, keyed to the field that is wrong.
#[derive(Debug)]
#[non_exhaustive]
pub struct ConfigError {
    /// TOML key of the invalid field (e.g. `"model_dir"`, `"api_key"`).
    pub field: String,
    /// Human-readable explanation of what is wrong.
    pub message: String,
    /// What kind of mistake it is, for code that acts on it (the message
    /// is for people and may be reworded).
    pub kind: ConfigErrorKind,
}

impl ConfigError {
    /// An invalid value at `field`, explained by `message`.
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            kind: ConfigErrorKind::Invalid,
        }
    }

    /// The same error, of another kind.
    #[must_use]
    pub fn with_kind(mut self, kind: ConfigErrorKind) -> Self {
        self.kind = kind;
        self
    }
}

/// What kind of mistake a [`ConfigError`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigErrorKind {
    /// A value that is out of range, malformed, missing or inconsistent.
    Invalid,
    /// A name nothing in this build answers to: a provider, mode, type or
    /// model that is not registered or not compiled in.
    UnknownName,
    /// A setting the component does not read (a misspelt or leftover key).
    UnknownKey,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ConfigError {}

// ── TTS configuration ─────────────────────────────────────────

/// Configuration for a TTS provider, parsed from TOML `[tts]`.
///
/// The framework populates this from the worker's TOML and passes it to
/// [`super::tts::TtsFactory::validate`] and [`super::tts::TtsFactory::build`].
#[derive(Clone)]
#[non_exhaustive]
pub struct TtsConfig {
    /// Provider name as it appears in TOML `provider = "..."`.
    pub provider: String,
    /// Path to the specific TTS model file (local providers).
    pub model: Option<PathBuf>,
    /// API key for hosted TTS providers (`ElevenLabs`, `OpenAI`, Azure, etc.).
    pub api_key: Option<Secret>,
    /// API endpoint URL (for custom/self-hosted endpoints).
    pub endpoint: Option<String>,
    /// Model name for API providers (e.g. `"tts-1"`, `"eleven_multilingual_v2"`).
    pub model_name: Option<String>,
    /// Voice or speaker ID for hosted TTS providers.
    pub voice: Option<String>,
    /// Provider-specific key-value options (anything not matched by the above).
    pub options: HashMap<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for TtsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTtsConfig {
            provider: String,
            model: Option<PathBuf>,
            api_key: Option<Secret>,
            endpoint: Option<String>,
            model_name: Option<String>,
            voice: Option<String>,
            #[serde(flatten)]
            extra: HashMap<String, serde_json::Value>,
        }

        let mut raw = RawTtsConfig::deserialize(deserializer)?;

        let mut options = HashMap::new();
        if let Some(serde_json::Value::Object(map)) = raw.extra.remove("options") {
            for (k, v) in map {
                options.insert(k, v);
            }
        }
        for (k, v) in raw.extra {
            options.insert(k, v);
        }

        Ok(TtsConfig {
            provider: raw.provider,
            model: raw.model,
            api_key: raw.api_key,
            endpoint: raw.endpoint,
            model_name: raw.model_name,
            voice: raw.voice,
            options,
        })
    }
}

impl Serialize for TtsConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct SerializingTtsConfig<'a> {
            provider: &'a String,
            #[serde(skip_serializing_if = "Option::is_none")]
            model: &'a Option<PathBuf>,
            #[serde(skip_serializing_if = "Option::is_none")]
            api_key: &'a Option<Secret>,
            #[serde(skip_serializing_if = "Option::is_none")]
            endpoint: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            model_name: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            voice: &'a Option<String>,
            /// Secret-named values replaced, like `api_key` above.
            #[serde(flatten)]
            options: serde_json::Value,
        }

        SerializingTtsConfig {
            provider: &self.provider,
            model: &self.model,
            api_key: &self.api_key,
            endpoint: &self.endpoint,
            model_name: &self.model_name,
            voice: &self.voice,
            options: crate::secrets::debug_options(&self.options),
        }
        .serialize(serializer)
    }
}

impl TtsConfig {
    /// A reader over this section's options (`tts.<key>` in
    /// errors), with the keys the engine itself reads already accounted for.
    pub fn options(&self) -> crate::options::ProviderOptions<'_> {
        crate::options::ProviderOptions::new("tts", &self.options)
            .with_engine_keys(&[crate::options::ALLOW_INSECURE_OPTION])
    }

    /// Returns JSON descriptions of every TTS configuration field.
    #[deprecated(
        since = "1.1.0",
        note = "nothing reads it: the console describes a provider's settings from its `param_schema`"
    )]
    pub fn metadata_descriptions() -> serde_json::Value {
        serde_json::json!({
            "provider":   "The framework or service providing TTS capabilities",
            "model":      "Path to the specific TTS model file",
            "api_key":    "API key for hosted TTS providers",
            "endpoint":   "API endpoint URL for custom endpoints",
            "model_name": "Model name for API providers",
            "voice":      "Voice or speaker ID for hosted TTS providers",
            "pool_size":  "Number of concurrent inference engine copies (default: 1)"
        })
    }
}

// ── Text chunking configuration ───────────────────────────────

/// Sentence-splitting tuning passed to TTS providers that do their own
/// streaming sentence segmentation.
///
/// Parsed from TOML `[text_chunking]`. Providers that don't do sentence
/// splitting may ignore this.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TextChunkingConfig {
    /// Minimum character count before a soft boundary (`,`, `;`, `:`) is
    /// treated as a flush point. Default: `36`.
    #[serde(default = "TextChunkingConfig::default_min_soft_split_chars")]
    pub min_soft_split_chars: usize,
    /// Minimum word count before a soft boundary is a flush point. Default: `6`.
    #[serde(default = "TextChunkingConfig::default_min_soft_split_words")]
    pub min_soft_split_words: usize,
}

impl TextChunkingConfig {
    /// Serde default for [`min_soft_split_chars`](Self::min_soft_split_chars).
    pub fn default_min_soft_split_chars() -> usize {
        36
    }

    /// Serde default for [`min_soft_split_words`](Self::min_soft_split_words).
    pub fn default_min_soft_split_words() -> usize {
        6
    }
}

impl Default for TextChunkingConfig {
    fn default() -> Self {
        Self {
            min_soft_split_chars: Self::default_min_soft_split_chars(),
            min_soft_split_words: Self::default_min_soft_split_words(),
        }
    }
}

// ── Text processor configuration ──────────────────────────────

/// Configuration for a single text processor in the chain.
///
/// Uses a flat struct with `type` as the discriminator and all remaining
/// fields captured into a generic `options` map. Each factory extracts
/// the fields it needs from `options`.
///
/// The framework populates this from the worker's TOML and passes it to
/// [`super::text::TextProcessorFactory::validate`] and
/// [`super::text::TextProcessorFactory::build`].
///
/// Serialising it writes secret-named options (a key taken as `api_key`) as
/// [`crate::secrets::REDACTED`], as `Debug` does: a serialised config is for
/// display and hashing, never for sending a credential on.
#[derive(Clone, Deserialize)]
#[non_exhaustive]
pub struct TextProcessorConfig {
    /// Processor type: `"llm"`, `"echo"`, `"openai-compat"`, etc.
    #[serde(rename = "type")]
    pub processor_type: String,
    /// API key for a hosted processor. The host fills it before
    /// `validate` and `build`, from the variable `api_key_env` names or
    /// the factory's [`default_api_key_env`](crate::text::TextProcessorFactory::default_api_key_env).
    #[serde(default)]
    pub api_key: Option<Secret>,
    /// The environment variable the host reads the key from, overriding
    /// the factory's default.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// All other TOML fields — each factory interprets its own subset.
    #[serde(flatten)]
    pub options: std::collections::HashMap<String, serde_json::Value>,
}

impl Serialize for TextProcessorConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct SerializingTextProcessorConfig<'a> {
            #[serde(rename = "type")]
            processor_type: &'a String,
            #[serde(skip_serializing_if = "Option::is_none")]
            api_key: &'a Option<Secret>,
            #[serde(skip_serializing_if = "Option::is_none")]
            api_key_env: &'a Option<String>,
            #[serde(flatten)]
            options: serde_json::Value,
        }

        SerializingTextProcessorConfig {
            processor_type: &self.processor_type,
            api_key: &self.api_key,
            api_key_env: &self.api_key_env,
            options: crate::secrets::debug_options(&self.options),
        }
        .serialize(serializer)
    }
}

impl TextProcessorConfig {
    /// A reader over this section's options (`text_processor.<key>` in
    /// errors), with the keys the engine itself reads already accounted for.
    pub fn options(&self) -> crate::options::ProviderOptions<'_> {
        crate::options::ProviderOptions::new("text_processor", &self.options).with_engine_keys(&[
            crate::options::ALLOW_INSECURE_OPTION,
            crate::text::AUDIT_FULL_PROMPTS_OPTION,
        ])
    }

    /// An error for every option not in `known`, so a misspelt or
    /// leftover key is refused rather than silently ignored. Keys the
    /// engine adds itself ([`crate::text::AUDIT_FULL_PROMPTS_OPTION`]) pass.
    #[deprecated(
        since = "1.1.0",
        note = "read the options with `options()`, whose `finish()` refuses every key nothing read"
    )]
    pub fn unknown_options(&self, known: &[&str]) -> Vec<ConfigError> {
        let mut keys: Vec<&String> = self
            .options
            .keys()
            .filter(|k| {
                !known.contains(&k.as_str()) && k.as_str() != crate::text::AUDIT_FULL_PROMPTS_OPTION
            })
            .collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                ConfigError::new(
                    format!("text_processor.{k}"),
                    format!(
                        "not a setting of text processor type \"{}\"",
                        self.processor_type
                    ),
                )
                .with_kind(ConfigErrorKind::UnknownKey)
            })
            .collect()
    }

    /// Returns JSON descriptions for common LLM text-processor options.
    /// Useful for generating schema documentation or help text.
    #[deprecated(
        since = "1.1.0",
        note = "nothing reads it: the console describes a provider's settings from its `param_schema`"
    )]
    pub fn llm_metadata_descriptions() -> serde_json::Value {
        serde_json::json!({
            "chat_template":   "Chat prompt template format (e.g. llama3, chatml)",
            "device":          "Compute device for inference (cpu, cuda, metal)",
            "system_prompt":   "System prompt that defines the assistant personality",
            "max_tokens":      "Maximum number of tokens to generate per response",
            "temperature":     "Sampling temperature -- lower is more deterministic (0.0-2.0)",
            "prefill_cache":   "Pre-fill KV cache on load for faster first response",
            "top_p":           "Nucleus sampling probability cutoff (0.0-1.0)",
            "repeat_penalty":  "Penalty factor for repeated tokens (1.0 = no penalty)"
        })
    }
}

// ── VAD configuration ─────────────────────────────────────────

/// Configuration a VAD plugin receives, resolved from TOML `[vad]`.
///
/// Thresholds arrive already resolved (the deprecated `threshold` key and the
/// defaults applied), so a backend does not reimplement that precedence.
/// Utterance timing -- `min_speech_ms`, `min_silence_ms`, `pre_roll_ms` --
/// belongs to the pipeline, not the backend, and is not passed.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct VadConfig {
    /// The `mode` that selected this backend.
    pub mode: String,
    /// Model file, for backends that load one.
    pub model: Option<PathBuf>,
    /// Speech-probability threshold (resolved; default 0.5).
    pub speech_prob: f32,
    /// Probability below which a speech segment ends, when set.
    pub neg_threshold: Option<f32>,
    /// RMS amplitude threshold for energy-style backends (resolved; default 200.0).
    pub energy_threshold: f64,
    /// Backend-specific settings from `[vad.options]`.
    pub options: HashMap<String, serde_json::Value>,
}

impl VadConfig {
    /// A reader over this section's options (`vad.<key>` in
    /// errors), with the keys the engine itself reads already accounted for.
    pub fn options(&self) -> crate::options::ProviderOptions<'_> {
        crate::options::ProviderOptions::new("vad.options", &self.options).with_engine_keys(&[])
    }
}

// ── Debug that cannot print a credential ─────────────────────
//
// `api_key` is a `Secret`; option maps are printed with secret-named values
// replaced, since a provider may take its key as an option.

impl fmt::Debug for SttConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SttConfig")
            .field("provider", &self.provider)
            .field("model_dir", &self.model_dir)
            .field("model", &self.model)
            .field("api_key", &self.api_key)
            .field("endpoint", &self.endpoint)
            .field("model_name", &self.model_name)
            .field("options", &debug_options(&self.options))
            .finish()
    }
}

impl fmt::Debug for TtsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TtsConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &self.api_key)
            .field("endpoint", &self.endpoint)
            .field("model_name", &self.model_name)
            .field("voice", &self.voice)
            .field("options", &debug_options(&self.options))
            .finish()
    }
}

impl fmt::Debug for TextProcessorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextProcessorConfig")
            .field("processor_type", &self.processor_type)
            .field("api_key", &self.api_key)
            .field("api_key_env", &self.api_key_env)
            .field("options", &debug_options(&self.options))
            .finish()
    }
}

impl fmt::Debug for VadConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VadConfig")
            .field("mode", &self.mode)
            .field("model", &self.model)
            .field("speech_prob", &self.speech_prob)
            .field("neg_threshold", &self.neg_threshold)
            .field("energy_threshold", &self.energy_threshold)
            .field("options", &debug_options(&self.options))
            .finish()
    }
}

// ── Construction ─────────────────────────────────────────────
//
// The configs are non-exhaustive, so a field added later is not a break;
// outside this crate they are built with `new` and the `with_*` setters.

impl SttConfig {
    /// Settings for `provider`, everything else unset.
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model_dir: None,
            model: None,
            api_key: None,
            endpoint: None,
            model_name: None,
            options: HashMap::new(),
        }
    }

    with_setters! {
        with_model_dir => model_dir: Option<PathBuf>,
        with_model => model: Option<PathBuf>,
        with_api_key => api_key: Option<Secret>,
        with_endpoint => endpoint: Option<String>,
        with_model_name => model_name: Option<String>,
        with_options => options: HashMap<String, serde_json::Value>,
    }
}

impl TtsConfig {
    /// Settings for `provider`, everything else unset.
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: None,
            api_key: None,
            endpoint: None,
            model_name: None,
            voice: None,
            options: HashMap::new(),
        }
    }

    with_setters! {
        with_model => model: Option<PathBuf>,
        with_api_key => api_key: Option<Secret>,
        with_endpoint => endpoint: Option<String>,
        with_model_name => model_name: Option<String>,
        with_voice => voice: Option<String>,
        with_options => options: HashMap<String, serde_json::Value>,
    }
}

impl TextProcessorConfig {
    /// Settings for a processor of `processor_type`, everything else unset.
    pub fn new(processor_type: impl Into<String>) -> Self {
        Self {
            processor_type: processor_type.into(),
            api_key: None,
            api_key_env: None,
            options: HashMap::new(),
        }
    }

    with_setters! {
        with_api_key => api_key: Option<Secret>,
        with_api_key_env => api_key_env: Option<String>,
        with_options => options: HashMap<String, serde_json::Value>,
    }
}

impl VadConfig {
    /// Settings for the backend `mode`, with the engine's default
    /// thresholds (speech probability 0.5, energy 200).
    pub fn new(mode: impl Into<String>) -> Self {
        Self {
            mode: mode.into(),
            speech_prob: 0.5,
            energy_threshold: 200.0,
            ..Self::default()
        }
    }

    with_setters! {
        with_model => model: Option<PathBuf>,
        with_speech_prob => speech_prob: f32,
        with_neg_threshold => neg_threshold: Option<f32>,
        with_energy_threshold => energy_threshold: f64,
        with_options => options: HashMap<String, serde_json::Value>,
    }
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
