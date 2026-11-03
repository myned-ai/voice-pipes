//! The provider's settings: defaults, the option table it reads, and the
//! config the provider runs with.

use std::time::Duration;

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::options::as_f32;

// ── Constants ────────────────────────────────────────────────

pub(crate) const DEFAULT_MODEL: &str = "tts-1";
pub(crate) const DEFAULT_VOICE: &str = "alloy";
pub(crate) const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1";

// ── Provider config ─────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct OpenAiConfig {
    pub(crate) model_name: String,
    /// Default voice. Held in an `ArcSwap` so the live parameter editor can
    /// replace it at runtime; the synthesis hot path takes a cheap snapshot.
    pub(crate) voice: voice_pipes_sdk::tts::VoiceCell,
    pub(crate) endpoint: String,
    pub(crate) api_key: voice_pipes_sdk::secrets::Secret,
    /// The longest wait for the response or its next bytes
    /// (`request_timeout_ms`); a reply streams for as long as it keeps coming.
    pub(crate) request_timeout: Duration,
    pub(crate) speed: Option<f32>,
    pub(crate) instructions: Option<String>,
}

// never print the key, even through a future
// `{config:?}` or `#[instrument]` without `skip`.
impl std::fmt::Debug for OpenAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiConfig")
            .field("model_name", &self.model_name)
            .field("voice", &*self.voice.load())
            .field("endpoint", &self.endpoint)
            .field("api_key", &"***")
            .field("request_timeout", &self.request_timeout)
            .field("speed", &self.speed)
            .field("instructions", &self.instructions)
            .finish()
    }
}

/// The settings `OpenAI` TTS reads from its option table.
pub(crate) struct OpenAiOptions {
    speed: Option<f32>,
    timeout_ms: u64,
    instructions: Option<String>,
}

impl OpenAiOptions {
    pub(crate) fn read(cfg: &TtsConfig) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = cfg.options();
        let speed = o.optional_f64_in("speed", 0.25..=4.0).map(as_f32);
        let timeout_ms = o.u64_in("request_timeout_ms", 1..=600_000, 30_000);
        let instructions = o.string("instructions");
        o.finish()?;
        Ok(Self {
            speed,
            timeout_ms,
            instructions,
        })
    }
}

impl OpenAiConfig {
    pub(crate) fn from_tts_config(cfg: &TtsConfig) -> Result<Self, anyhow::Error> {
        let OpenAiOptions {
            speed,
            timeout_ms,
            instructions,
        } = OpenAiOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let api_key = voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref())
            .cloned()
            .unwrap_or_default();

        let model_name = cfg
            .model_name
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        let voice = cfg
            .voice
            .clone()
            .unwrap_or_else(|| DEFAULT_VOICE.to_string());

        let endpoint = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());

        Ok(Self {
            model_name,
            voice: voice_pipes_sdk::tts::voice_cell(voice),
            endpoint,
            api_key,
            request_timeout: Duration::from_millis(timeout_ms),
            speed,
            instructions,
        })
    }

    /// Snapshot the current default voice (cheap clone of an `Arc<String>`).
    pub(crate) fn current_voice(&self) -> std::sync::Arc<String> {
        self.voice.load_full()
    }
}
