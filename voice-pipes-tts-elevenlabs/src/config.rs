//! Provider settings: defaults, option-table parsing and validation.

use std::time::Duration;

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::options::as_f32;

// ── Constants ────────────────────────────────────────────────

/// Low-latency conversational model; override via `model_name`.
pub(crate) const DEFAULT_MODEL: &str = "eleven_flash_v2_5";
/// "Rachel" -- a default premade voice available on every account.
pub(crate) const DEFAULT_VOICE: &str = "21m00Tcm4TlvDq8ikWAM";
pub(crate) const DEFAULT_ENDPOINT: &str = "https://api.elevenlabs.io/v1";
pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 24_000;

/// PCM rates the API accepts as `output_format=pcm_{rate}`.
/// (44.1 kHz requires a Pro-tier key -- validated server-side.)
const ALLOWED_SAMPLE_RATES: &[u32] = &[8_000, 16_000, 22_050, 24_000, 44_100];

// ── Provider config ─────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct ElevenLabsConfig {
    pub(crate) model_id: String,
    /// Default voice id. Held in an `ArcSwap` so the live parameter editor
    /// can replace it at runtime; the synthesis hot path takes a snapshot.
    pub(crate) voice: voice_pipes_sdk::tts::VoiceCell,
    pub(crate) endpoint: String,
    pub(crate) api_key: voice_pipes_sdk::secrets::Secret,
    pub(crate) sample_rate: u32,
    /// The longest wait for the response or its next bytes
    /// (`request_timeout_ms`); a reply streams for as long as it keeps coming.
    pub(crate) request_timeout: Duration,
    /// ISO 639-1 language enforcement (Turbo/Flash v2.5 models only).
    pub(crate) language_code: Option<String>,
    pub(crate) speed: Option<f32>,
    pub(crate) stability: Option<f32>,
    pub(crate) similarity_boost: Option<f32>,
    pub(crate) style: Option<f32>,
    pub(crate) use_speaker_boost: Option<bool>,
    /// When true (default) synthesis uses the `with-timestamps` endpoint
    /// and emits word boundaries. `word_timestamps = false` opts out and
    /// streams raw PCM (~25% less downlink -- no base64 framing).
    pub(crate) word_timestamps: bool,
}

// never print the key, even through a future
// `{config:?}` or `#[instrument]` without `skip`.
impl std::fmt::Debug for ElevenLabsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElevenLabsConfig")
            .field("model_id", &self.model_id)
            .field("voice", &*self.voice.load())
            .field("endpoint", &self.endpoint)
            .field("api_key", &"***")
            .field("sample_rate", &self.sample_rate)
            .field("request_timeout", &self.request_timeout)
            .field("language_code", &self.language_code)
            .field("speed", &self.speed)
            .field("stability", &self.stability)
            .field("similarity_boost", &self.similarity_boost)
            .field("style", &self.style)
            .field("use_speaker_boost", &self.use_speaker_boost)
            .field("word_timestamps", &self.word_timestamps)
            .finish()
    }
}

/// The settings `ElevenLabs` TTS reads from its option table.
pub(crate) struct ElevenLabsOptions {
    sample_rate: u32,
    timeout_ms: u64,
    language_code: Option<String>,
    speed: Option<f32>,
    stability: Option<f32>,
    similarity_boost: Option<f32>,
    style: Option<f32>,
    use_speaker_boost: Option<bool>,
    word_timestamps: bool,
}

impl ElevenLabsOptions {
    pub(crate) fn read(cfg: &TtsConfig) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = cfg.options();
        let sample_rate = o.u32_in("sample_rate", 1..=u32::MAX, DEFAULT_SAMPLE_RATE);
        if !ALLOWED_SAMPLE_RATES.contains(&sample_rate) {
            o.error(
                "sample_rate",
                format!("must be one of {ALLOWED_SAMPLE_RATES:?}, got {sample_rate}"),
            );
        }
        let timeout_ms = o.u64_in("request_timeout_ms", 1..=600_000, 30_000);
        let language_code = o.string("language_code");
        let speed = o.optional_f64_in("speed", 0.7..=1.2).map(as_f32);
        let stability = o.optional_f64_in("stability", 0.0..=1.0).map(as_f32);
        let similarity_boost = o.optional_f64_in("similarity_boost", 0.0..=1.0).map(as_f32);
        let style = o.optional_f64_in("style", 0.0..=1.0).map(as_f32);
        let use_speaker_boost = o
            .value("use_speaker_boost")
            .map(|_| o.bool("use_speaker_boost", false));
        let word_timestamps = o.bool("word_timestamps", true);
        o.finish()?;
        Ok(Self {
            sample_rate,
            timeout_ms,
            language_code,
            speed,
            stability,
            similarity_boost,
            style,
            use_speaker_boost,
            word_timestamps,
        })
    }
}

impl ElevenLabsConfig {
    pub(crate) fn from_tts_config(cfg: &TtsConfig) -> Result<Self, anyhow::Error> {
        let ElevenLabsOptions {
            sample_rate,
            timeout_ms,
            language_code,
            speed,
            stability,
            similarity_boost,
            style,
            use_speaker_boost,
            word_timestamps,
        } = ElevenLabsOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let api_key = voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref())
            .cloned()
            .unwrap_or_default();

        let model_id = cfg
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
            model_id,
            voice: voice_pipes_sdk::tts::voice_cell(voice),
            endpoint,
            api_key,
            sample_rate,
            request_timeout: Duration::from_millis(timeout_ms),
            language_code,
            speed,
            stability,
            similarity_boost,
            style,
            use_speaker_boost,
            word_timestamps,
        })
    }

    /// Snapshot the current default voice (cheap clone of an `Arc<String>`).
    pub(crate) fn current_voice(&self) -> std::sync::Arc<String> {
        self.voice.load_full()
    }
}
