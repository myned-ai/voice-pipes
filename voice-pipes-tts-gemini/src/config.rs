//! The provider's typed config: defaults, the option table it reads, and
//! the parse from the shared `TtsConfig`.

use std::time::Duration;

use voice_pipes_sdk::TtsConfig;

// ── Constants ────────────────────────────────────────────────

pub(crate) const DEFAULT_MODEL: &str = "gemini-3.1-flash-tts-preview";
pub(crate) const DEFAULT_VOICE: &str = "Kore";
pub(crate) const DEFAULT_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

// ── Provider config ─────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct GeminiConfig {
    pub(crate) model_name: String,
    /// Default voice. Held in an `ArcSwap` so the live parameter editor can
    /// replace it; each synthesis takes a snapshot.
    pub(crate) voice: voice_pipes_sdk::tts::VoiceCell,
    pub(crate) prompt: String,
    pub(crate) endpoint: String,
    pub(crate) api_key: voice_pipes_sdk::secrets::Secret,
    pub(crate) request_timeout: Duration,
}

// Manual impl: `derive(Debug)` would print the API key.
impl std::fmt::Debug for GeminiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiConfig")
            .field("model_name", &self.model_name)
            .field("voice", &*self.voice.load())
            .field("prompt", &self.prompt)
            .field("endpoint", &self.endpoint)
            .field("api_key", &"***")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

/// The settings Gemini TTS reads from its option table.
pub(crate) struct GeminiOptions {
    prompt: String,
    timeout_ms: u64,
}

impl GeminiOptions {
    pub(crate) fn read(cfg: &TtsConfig) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = cfg.options();
        let prompt = o.string_or("prompt", "");
        let timeout_ms = o.u64_in("request_timeout_ms", 1..=600_000, 30_000);
        o.finish()?;
        Ok(Self { prompt, timeout_ms })
    }
}

impl GeminiConfig {
    pub(crate) fn from_tts_config(cfg: &TtsConfig) -> Result<Self, anyhow::Error> {
        let GeminiOptions { prompt, timeout_ms } =
            GeminiOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let api_key = voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("Gemini TTS requires `api_key` in config or GOOGLE_API_KEY env var")
            })?;

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
            prompt,
            endpoint,
            api_key,
            request_timeout: Duration::from_millis(timeout_ms),
        })
    }
}
