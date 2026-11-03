//! Typed configuration for the Google Cloud Speech-to-Text v2 provider.
//!
//! Parsed from the shared [`SttConfig`](voice_pipes_sdk::config::SttConfig): the
//! generic fields (`model_name`, `endpoint`) plus provider-specific keys in
//! the `[stt]` / `[stt.options]` TOML tables.

use std::path::PathBuf;
use std::time::Duration;

/// Default model — the only one verified for Croatian.
const DEFAULT_MODEL: &str = "chirp_3";
/// Default multi-region. `chirp_3` is only served from `us` / `eu`.
const DEFAULT_LOCATION: &str = "us";
/// Default input sample rate; Google recommends 16 kHz for best results.
const DEFAULT_SAMPLE_RATE: u32 = 16_000;
/// Rotate the upstream stream proactively at 4 minutes: Google closes it at 5.
const DEFAULT_STREAM_ROTATE_MS: u64 = 240_000;
/// Google's hard per-stream cap; `stream_rotate_ms` must stay below it.
pub const GOOGLE_STREAM_CAP_MS: u64 = 300_000;
const DEFAULT_MAX_RECONNECT_ATTEMPTS: u32 = 5;
const DEFAULT_RECONNECT_DELAY_MS: u64 = 1_000;
const DEFAULT_RECONNECT_DELAY_MAX_MS: u64 = 30_000;

/// Env fallback for `project_id` (the name the Google client libraries use).
pub const PROJECT_ENV_VAR: &str = "GOOGLE_CLOUD_PROJECT";
/// Env fallback for `credentials_path` — the standard ADC override, read by
/// `gcp_auth` itself; named here only for validation messages.
pub const CREDENTIALS_ENV_VAR: &str = "GOOGLE_APPLICATION_CREDENTIALS";
/// The variable the host reads a pre-minted OAuth token from (dev-only)
/// when the section sets none.
pub const ACCESS_TOKEN_ENV_VAR: &str = "GOOGLE_OAUTH_ACCESS_TOKEN";

/// Models accepted by `StreamingRecognize`. `chirp_3` is the one that
/// covers Croatian; `chirp_2`'s streaming is restricted to 18 languages.
pub const KNOWN_MODELS: &[&str] = &[
    "chirp_3",
    "chirp_2",
    "chirp",
    "long",
    "short",
    "telephony",
    "latest_long",
    "latest_short",
];

/// Locations `chirp_3` streaming is served from. Elsewhere it fails at
/// connect time, not at config validation — so we validate it here.
pub const CHIRP_3_LOCATIONS: &[&str] = &["us", "eu"];

/// Google's `EndpointingSensitivity` knob, as accepted in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointingSensitivity {
    Standard,
    Short,
    SuperShort,
}

impl EndpointingSensitivity {
    pub const NAMES: &[&str] = &["standard", "short", "supershort"];

    pub fn parse(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_'], "")
            .as_str()
        {
            "standard" => Some(Self::Standard),
            "short" => Some(Self::Short),
            "supershort" => Some(Self::SuperShort),
            _ => None,
        }
    }
}

/// Validated, provider-specific configuration for a Google STT session.
#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per RecognitionFeatures / StreamingRecognitionFeatures wire field"
)]
pub struct GoogleSttConfig {
    /// GCP project the recognizer path and quota attribution use. `None`
    /// means "ask the credential" (service accounts and ADC carry one).
    pub project_id: Option<String>,
    /// Multi-region / region: `us`, `eu`, `global`, ...
    pub location: String,
    /// Recognition model (`chirp_3`, `chirp_2`, `long`, ...).
    pub model: String,
    /// BCP-47 language codes; Chirp 3 accepts several.
    pub language_codes: Vec<String>,
    /// gRPC endpoint (`https://{location}-speech.googleapis.com` unless
    /// overridden — an `http://` override selects a plaintext channel, used
    /// by the in-process test server).
    pub endpoint: String,
    /// PCM input sample rate in Hz (8000–48000).
    pub sample_rate: u32,
    pub interim_results: bool,
    pub enable_voice_activity_events: bool,
    pub enable_automatic_punctuation: bool,
    pub enable_word_time_offsets: bool,
    pub enable_word_confidence: bool,
    pub profanity_filter: bool,
    pub endpointing_sensitivity: Option<EndpointingSensitivity>,
    /// `voice_activity_timeout.speech_start_timeout`.
    pub speech_start_timeout: Option<Duration>,
    /// `voice_activity_timeout.speech_end_timeout`.
    pub speech_end_timeout: Option<Duration>,
    /// Service-account JSON key file. `None` → Application Default
    /// Credentials (`GOOGLE_APPLICATION_CREDENTIALS`, the gcloud ADC file,
    /// the GCE metadata server, then `gcloud auth print-access-token`).
    pub credentials_path: Option<PathBuf>,
    /// Pre-minted OAuth access token; bypasses credential discovery. Tokens
    /// expire in ~1 h, so this is for probes and tests, not deployments.
    pub access_token: Option<voice_pipes_sdk::secrets::Secret>,
    /// Proactive stream rotation interval (must be < 5 min).
    pub stream_rotate: Duration,
    /// Keep one upstream stream open across utterances instead of one
    /// stream per utterance. Only sensible with continuous (pass-through
    /// VAD) audio: Google times out a stream that goes quiet.
    pub persistent_session: bool,
    pub max_reconnect_attempts: u32,
    pub reconnect_delay: Duration,
    pub reconnect_delay_max: Duration,
}

// Manual impl: never print the access token.
impl std::fmt::Debug for GoogleSttConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleSttConfig")
            .field("project_id", &self.project_id)
            .field("location", &self.location)
            .field("model", &self.model)
            .field("language_codes", &self.language_codes)
            .field("endpoint", &self.endpoint)
            .field("sample_rate", &self.sample_rate)
            .field("interim_results", &self.interim_results)
            .field(
                "enable_voice_activity_events",
                &self.enable_voice_activity_events,
            )
            .field(
                "enable_automatic_punctuation",
                &self.enable_automatic_punctuation,
            )
            .field("enable_word_time_offsets", &self.enable_word_time_offsets)
            .field("enable_word_confidence", &self.enable_word_confidence)
            .field("profanity_filter", &self.profanity_filter)
            .field("endpointing_sensitivity", &self.endpointing_sensitivity)
            .field("speech_start_timeout", &self.speech_start_timeout)
            .field("speech_end_timeout", &self.speech_end_timeout)
            .field("credentials_path", &self.credentials_path)
            .field("access_token", &self.access_token.as_ref().map(|_| "***"))
            .field("stream_rotate", &self.stream_rotate)
            .field("persistent_session", &self.persistent_session)
            .field("max_reconnect_attempts", &self.max_reconnect_attempts)
            .field("reconnect_delay", &self.reconnect_delay)
            .field("reconnect_delay_max", &self.reconnect_delay_max)
            .finish()
    }
}

/// `language_codes` is repeated upstream; a bare string is accepted for the
/// common single-language case, and `language_code` as an alias.
fn read_language_codes(o: &mut voice_pipes_sdk::options::ProviderOptions<'_>) -> Vec<String> {
    let codes = o.value("language_codes");
    let code = o.value("language_code");
    match codes.or(code) {
        None => Vec::new(),
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(arr)) => {
            if let Some(codes) = arr
                .iter()
                .map(|v| v.as_str().map(String::from))
                .collect::<Option<Vec<_>>>()
            {
                codes
            } else {
                o.error("language_codes", "all entries must be strings");
                Vec::new()
            }
        }
        Some(other) => {
            o.error(
                "language_codes",
                format!("must be a string or a list of strings, got {other}"),
            );
            Vec::new()
        }
    }
}

fn read_endpointing_sensitivity(
    o: &mut voice_pipes_sdk::options::ProviderOptions<'_>,
) -> Option<EndpointingSensitivity> {
    let s = o.string("endpointing_sensitivity")?;
    let parsed = EndpointingSensitivity::parse(&s);
    if parsed.is_none() {
        o.error(
            "endpointing_sensitivity",
            format!(
                "\"{s}\" is not one of {}",
                EndpointingSensitivity::NAMES.join(", ")
            ),
        );
    }
    parsed
}

impl GoogleSttConfig {
    /// Build from the generic [`SttConfig`](voice_pipes_sdk::config::SttConfig).
    ///
    /// # Errors
    /// Every problem with the options, as one error.
    pub fn from_stt_config(
        cfg: &voice_pipes_sdk::config::SttConfig,
    ) -> Result<Self, anyhow::Error> {
        Ok(Self::parse(cfg).map_err(voice_pipes_sdk::options::refused)?)
    }

    /// Read every option, bounded; each problem, and each key Google STT
    /// does not read, is an error.
    pub(crate) fn parse(
        cfg: &voice_pipes_sdk::config::SttConfig,
    ) -> Result<Self, Vec<voice_pipes_sdk::config::ConfigError>> {
        let mut o = cfg.options();

        let model = cfg
            .model_name
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let location = o.string_or("location", DEFAULT_LOCATION).trim().to_string();
        let project_id = o
            .string("project_id")
            .or_else(|| std::env::var(PROJECT_ENV_VAR).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let language_codes = read_language_codes(&mut o);

        let endpoint = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| default_endpoint(&location));

        let endpointing_sensitivity = read_endpointing_sensitivity(&mut o);

        // A token written in the section wins over `api_key`, which the host
        // may have filled from the environment.
        let access_token = o
            .string("access_token")
            .map(voice_pipes_sdk::secrets::Secret::from)
            .or_else(|| cfg.api_key.clone())
            .filter(|s| !s.expose().trim().is_empty());

        let config = Self {
            project_id,
            location,
            model,
            language_codes,
            endpoint,
            // 16000 is optimal.
            sample_rate: o.u32_in("sample_rate", 8_000..=48_000, DEFAULT_SAMPLE_RATE),
            interim_results: o.bool("interim_results", true),
            enable_voice_activity_events: o.bool("enable_voice_activity_events", true),
            enable_automatic_punctuation: o.bool("enable_automatic_punctuation", true),
            enable_word_time_offsets: o.bool("enable_word_time_offsets", false),
            enable_word_confidence: o.bool("enable_word_confidence", false),
            profanity_filter: o.bool("profanity_filter", false),
            endpointing_sensitivity,
            speech_start_timeout: o
                .optional_u64_in("speech_start_timeout_ms", 0..=600_000)
                .map(Duration::from_millis),
            speech_end_timeout: o
                .optional_u64_in("speech_end_timeout_ms", 0..=600_000)
                .map(Duration::from_millis),
            credentials_path: o
                .string("credentials_path")
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from),
            access_token,
            // Below Google's per-stream cap.
            stream_rotate: Duration::from_millis(o.u64_in(
                "stream_rotate_ms",
                1..=GOOGLE_STREAM_CAP_MS - 1,
                DEFAULT_STREAM_ROTATE_MS,
            )),
            persistent_session: o.bool("persistent_session", false),
            max_reconnect_attempts: o.u32_in(
                "max_reconnect_attempts",
                0..=1_000,
                DEFAULT_MAX_RECONNECT_ATTEMPTS,
            ),
            reconnect_delay: Duration::from_millis(o.u64_in(
                "reconnect_delay_ms",
                0..=600_000,
                DEFAULT_RECONNECT_DELAY_MS,
            )),
            reconnect_delay_max: Duration::from_millis(o.u64_in(
                "reconnect_delay_max_ms",
                0..=3_600_000,
                DEFAULT_RECONNECT_DELAY_MAX_MS,
            )),
        };
        o.finish()?;
        Ok(config)
    }

    pub fn validate(&self) -> Vec<voice_pipes_sdk::config::ConfigError> {
        use voice_pipes_sdk::config::ConfigError;
        let mut errors = Vec::new();

        if !KNOWN_MODELS.contains(&self.model.as_str()) {
            errors.push(
                ConfigError::new(
                    "model_name",
                    format!(
                        "unknown Google STT model \"{}\"; known: {}",
                        self.model,
                        KNOWN_MODELS.join(", ")
                    ),
                )
                .with_kind(voice_pipes_sdk::ConfigErrorKind::UnknownName),
            );
        }

        if self.location.is_empty() {
            errors.push(ConfigError::new(
                "location",
                "must be a Google Cloud location (e.g. \"us\", \"eu\")",
            ));
        } else if self.model == "chirp_3" && !CHIRP_3_LOCATIONS.contains(&self.location.as_str()) {
            errors.push(ConfigError::new(
                "location",
                format!(
                    "chirp_3 streaming is only served from {} (got \"{}\"); \
                     the wrong host fails at connect time, not here",
                    CHIRP_3_LOCATIONS.join(" / "),
                    self.location
                ),
            ));
        }

        if self.language_codes.is_empty() {
            errors.push(ConfigError::new(
                "language_codes",
                "at least one BCP-47 language code is required (e.g. [\"hr-HR\"])",
            ));
        }

        if let Some(path) = &self.credentials_path
            && !path.is_file()
        {
            errors.push(ConfigError::new(
                "credentials_path",
                format!("service-account key file not found: {}", path.display()),
            ));
        }

        errors
    }

    /// `projects/{project}/locations/{location}/recognizers/_`.
    pub fn recognizer_path(&self, project_id: &str) -> String {
        format!(
            "projects/{project_id}/locations/{}/recognizers/_",
            self.location
        )
    }

    /// Whether the endpoint is a plaintext (`http://`) channel — only the
    /// in-process test server; Google itself requires TLS.
    pub fn plaintext(&self) -> bool {
        self.endpoint.starts_with("http://")
    }
}

/// `https://{location}-speech.googleapis.com`, or the global host for
/// `global`.
pub fn default_endpoint(location: &str) -> String {
    if location == "global" || location.is_empty() {
        "https://speech.googleapis.com".to_string()
    } else {
        format!("https://{location}-speech.googleapis.com")
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
