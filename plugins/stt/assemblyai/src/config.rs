//! Typed configuration for the `AssemblyAI` streaming STT provider.
//!
//! Parsed from the shared [`SttConfig`](voice_pipes_sdk::SttConfig) and
//! provider-specific fields in the `[stt]` TOML table.

use std::time::Duration;

/// Default WebSocket endpoint for `AssemblyAI` V3 streaming API.
const DEFAULT_ENDPOINT: &str = "wss://streaming.assemblyai.com/v3/ws";

/// Default audio sample rate expected by `AssemblyAI` (16 kHz PCM).
const DEFAULT_SAMPLE_RATE: u32 = 16_000;

/// Maximum number of automatic reconnection attempts on transient errors.
const DEFAULT_MAX_RECONNECT_ATTEMPTS: u32 = 5;

/// Base delay (in milliseconds) for exponential backoff between reconnects.
const DEFAULT_RECONNECT_DELAY_MS: u64 = 1_000;

/// Upper-bound delay (in milliseconds) for exponential backoff.
const DEFAULT_RECONNECT_DELAY_MAX_MS: u64 = 30_000;

/// Environment variable fallback for the API key.
pub(crate) const API_KEY_ENV_VAR: &str = "ASSEMBLYAI_API_KEY";

/// Validated, provider-specific configuration for an `AssemblyAI` session.
#[derive(Clone)]
pub struct AssemblyAiConfig {
    /// API key (from config or `ASSEMBLYAI_API_KEY` env).
    pub api_key: voice_pipes_sdk::secrets::Secret,
    /// WebSocket endpoint URL.
    pub endpoint: String,
    /// Speech model name (e.g. `"u3-rt-pro"`).
    pub speech_model: String,
    /// Input audio sample rate in Hz.
    pub sample_rate: u32,
    /// Minimum silence (ms) before a turn can end.
    pub min_turn_silence: Option<u32>,
    /// Maximum silence (ms) before a turn is forced to end.
    pub max_turn_silence: Option<u32>,
    /// Confidence threshold (0-1) for end-of-turn detection.
    pub end_of_turn_confidence_threshold: Option<f64>,
    /// Whether to apply formatting to turns.
    pub format_turns: Option<bool>,
    /// Key terms for contextual prompting (max 100, each <=50 chars).
    pub keyterms_prompt: Option<Vec<String>>,
    /// Free-form prompting string.
    pub prompt: Option<String>,
    /// Enable automatic language detection.
    pub language_detection: Option<bool>,
    /// Maximum reconnect attempts on transient failure.
    pub max_reconnect_attempts: u32,
    /// Base delay for exponential backoff.
    pub reconnect_delay: Duration,
    /// Maximum backoff delay.
    pub reconnect_delay_max: Duration,
    /// Keep one WebSocket session open across utterances (end-of-utterance
    /// sends `ForceEndpoint` instead of `Terminate`). Reduces session churn
    /// and concurrency pressure; off by default until proven in live runs.
    pub persistent_session: bool,
}

// Manual impl: `derive(Debug)` would print the API key.
impl std::fmt::Debug for AssemblyAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssemblyAiConfig")
            .field("api_key", &"***")
            .field("endpoint", &self.endpoint)
            .field("speech_model", &self.speech_model)
            .field("sample_rate", &self.sample_rate)
            .field("min_turn_silence", &self.min_turn_silence)
            .field("max_turn_silence", &self.max_turn_silence)
            .field(
                "end_of_turn_confidence_threshold",
                &self.end_of_turn_confidence_threshold,
            )
            .field("format_turns", &self.format_turns)
            .field("keyterms_prompt", &self.keyterms_prompt)
            .field("prompt", &self.prompt)
            .field("language_detection", &self.language_detection)
            .field("max_reconnect_attempts", &self.max_reconnect_attempts)
            .field("reconnect_delay", &self.reconnect_delay)
            .field("reconnect_delay_max", &self.reconnect_delay_max)
            .field("persistent_session", &self.persistent_session)
            .finish()
    }
}

/// Known valid speech model identifiers.
///
/// The first three are the models the current streaming API reference lists;
/// `u3-rt-pro` and `whisper-rt` are older aliases the endpoint still accepts.
const KNOWN_MODELS: &[&str] = &[
    "universal-3-5-pro",
    "universal-streaming-english",
    "universal-streaming-multilingual",
    "u3-rt-pro",
    "whisper-rt",
];

/// Maximum key-terms allowed by the API.
const MAX_KEYTERMS: usize = 100;

/// Maximum character length of each key term.
const MAX_KEYTERM_LEN: usize = 50;

/// The settings `AssemblyAI` reads from its option table.
pub(crate) struct AaiOptions {
    sample_rate: u32,
    min_turn_silence: Option<u32>,
    max_turn_silence: Option<u32>,
    end_of_turn_confidence_threshold: Option<f64>,
    format_turns: Option<bool>,
    keyterms_prompt: Option<Vec<String>>,
    prompt: Option<String>,
    language_detection: Option<bool>,
    max_reconnect_attempts: u32,
    reconnect_delay_ms: u64,
    reconnect_delay_max_ms: u64,
    persistent_session: bool,
}

impl AaiOptions {
    /// Read and bound every option; each problem, and each key this
    /// provider does not read, is an error.
    pub(crate) fn read(
        cfg: &voice_pipes_sdk::SttConfig,
    ) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = cfg.options();
        let options = Self {
            // `transcribe()` chunks by `sample_rate / 10`, which panics at
            // 0, and the API refuses rates outside this range only after a
            // successful connect.
            sample_rate: o.u32_in("sample_rate", 8_000..=48_000, DEFAULT_SAMPLE_RATE),
            min_turn_silence: o.optional_u32_in("min_turn_silence", 0..=60_000),
            max_turn_silence: o.optional_u32_in("max_turn_silence", 0..=60_000),
            end_of_turn_confidence_threshold: o
                .optional_f64_in("end_of_turn_confidence_threshold", 0.0..=1.0),
            format_turns: o.optional_bool("format_turns"),
            keyterms_prompt: o.string_list("keyterms_prompt"),
            prompt: o.string("prompt"),
            language_detection: o.optional_bool("language_detection"),
            max_reconnect_attempts: o.u32_in(
                "max_reconnect_attempts",
                0..=1_000,
                DEFAULT_MAX_RECONNECT_ATTEMPTS,
            ),
            reconnect_delay_ms: o.u64_in(
                "reconnect_delay_ms",
                0..=600_000,
                DEFAULT_RECONNECT_DELAY_MS,
            ),
            reconnect_delay_max_ms: o.u64_in(
                "reconnect_delay_max_ms",
                0..=3_600_000,
                DEFAULT_RECONNECT_DELAY_MAX_MS,
            ),
            persistent_session: o.bool("persistent_session", false),
        };
        o.finish()?;
        Ok(options)
    }
}

impl AssemblyAiConfig {
    /// Build from the generic [`SttConfig`](voice_pipes_sdk::SttConfig).
    ///
    /// Falls back to `ASSEMBLYAI_API_KEY` for the API key and uses
    /// sensible defaults for unspecified optional fields.
    pub fn from_stt_config(cfg: &voice_pipes_sdk::SttConfig) -> Result<Self, anyhow::Error> {
        let api_key = voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AssemblyAI requires an API key via `api_key` config or {API_KEY_ENV_VAR} env var"
                )
            })?;

        let endpoint = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());

        let speech_model = cfg.model_name.clone().ok_or_else(|| {
            anyhow::anyhow!("AssemblyAI requires `model_name` (e.g. \"u3-rt-pro\")")
        })?;

        let options = AaiOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;

        Ok(Self {
            api_key,
            endpoint,
            speech_model,
            sample_rate: options.sample_rate,
            min_turn_silence: options.min_turn_silence,
            max_turn_silence: options.max_turn_silence,
            end_of_turn_confidence_threshold: options.end_of_turn_confidence_threshold,
            format_turns: options.format_turns,
            keyterms_prompt: options.keyterms_prompt,
            prompt: options.prompt,
            language_detection: options.language_detection,
            max_reconnect_attempts: options.max_reconnect_attempts,
            reconnect_delay: Duration::from_millis(options.reconnect_delay_ms),
            reconnect_delay_max: Duration::from_millis(options.reconnect_delay_max_ms),
            persistent_session: options.persistent_session,
        })
    }

    /// Validate the speech model is known and keyterms are within limits.
    pub fn validate(&self) -> Vec<voice_pipes_sdk::ConfigError> {
        let mut errors = Vec::new();

        if !KNOWN_MODELS.contains(&self.speech_model.as_str()) {
            errors.push(
                voice_pipes_sdk::ConfigError::new(
                    "model_name",
                    format!(
                        "unknown speech model \"{}\"; known: {}",
                        self.speech_model,
                        KNOWN_MODELS.join(", ")
                    ),
                )
                .with_kind(voice_pipes_sdk::ConfigErrorKind::UnknownName),
            );
        }

        if let Some(ref terms) = self.keyterms_prompt {
            if terms.len() > MAX_KEYTERMS {
                errors.push(voice_pipes_sdk::ConfigError::new(
                    "keyterms_prompt",
                    format!("too many key terms ({}, max {MAX_KEYTERMS})", terms.len()),
                ));
            }
            for (i, term) in terms.iter().enumerate() {
                let chars = term.chars().count();
                if chars > MAX_KEYTERM_LEN {
                    errors.push(voice_pipes_sdk::ConfigError::new(
                        "keyterms_prompt",
                        format!("term [{i}] too long ({chars} chars, max {MAX_KEYTERM_LEN})"),
                    ));
                }
            }
        }

        errors
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
