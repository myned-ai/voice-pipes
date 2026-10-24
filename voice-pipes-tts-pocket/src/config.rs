//! The provider's settings: defaults, the option table it reads, and the
//! parsed config the provider and factory share.

use std::time::Duration;

use voice_pipes_sdk::TtsConfig;

// ── Constants ───────────────────────────────────────────────

/// Pocket-TTS uses the Mimi codec at a fixed 24 kHz output rate.
pub(crate) const SAMPLE_RATE: u32 = 24_000;

/// Default `pocket-tts serve` endpoint.
pub(crate) const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8000";

/// Default voice when none is configured (a built-in English voice).
const DEFAULT_VOICE: &str = "alba";

// ── Provider config ─────────────────────────────────────────

/// Default probe window for the silent-start guard (crate docs).
pub(crate) const DEFAULT_SILENT_START_MS: u64 = 2_000;
/// Default int16 peak below which the probe window counts as silent.
pub(crate) const DEFAULT_SILENT_START_PEAK: i16 = 500;
/// Default number of re-synthesis attempts after a silent start.
pub(crate) const DEFAULT_SILENT_START_RETRIES: u32 = 2;

/// Silent-start guard settings (crate docs). `None` = disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SilentStartGuard {
    /// Samples (at [`SAMPLE_RATE`]) inspected before deciding.
    pub(crate) window_samples: usize,
    /// Peak amplitude the window must exceed to count as speech.
    pub(crate) peak_threshold: i16,
    /// Extra attempts after the first silent one.
    pub(crate) retries: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct PocketHttpConfig {
    pub(crate) endpoint: String,
    /// Default voice. Held in an `ArcSwap` so the live parameter editor can
    /// replace it at runtime; the synthesis hot path takes a cheap snapshot.
    pub(crate) voice: voice_pipes_sdk::tts::VoiceCell,
    /// The longest wait for the response or its next bytes
    /// (`request_timeout_ms`); a reply streams for as long as it keeps coming.
    pub(crate) request_timeout: Duration,
    pub(crate) silent_start: Option<SilentStartGuard>,
    /// The sidecar's `voice_url` accepts `http(s)://` and `hf://`
    /// and fetches them. The operator's configured `voice` may be one; a
    /// per-turn *hint* (which can come from an LLM tool result or a
    /// client-side tool) may not, unless `allow_voice_urls = true`.
    pub(crate) allow_voice_urls: bool,
}

/// A voice value the sidecar would treat as something to download.
pub(crate) fn is_remote_voice(voice: &str) -> bool {
    voice.contains("://")
}

/// The settings Pocket reads from its option table.
pub(crate) struct PocketOptions {
    timeout_ms: u64,
    silent_start_ms: u64,
    silent_start_peak: i16,
    silent_start_retries: u32,
    allow_voice_urls: bool,
}

impl PocketOptions {
    pub(crate) fn read(cfg: &TtsConfig) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = cfg.options();
        let timeout_ms = o.u64_in("request_timeout_ms", 1..=600_000, 60_000);
        let silent_start_ms = o.u64_in("silent_start_ms", 0..=10_000, DEFAULT_SILENT_START_MS);
        let silent_start_peak = o
            .optional_u64_in("silent_start_peak", 1..=i16::MAX.unsigned_abs().into())
            .and_then(|v| i16::try_from(v).ok())
            .unwrap_or(DEFAULT_SILENT_START_PEAK);
        let silent_start_retries =
            o.u32_in("silent_start_retries", 0..=10, DEFAULT_SILENT_START_RETRIES);
        let allow_voice_urls = o.bool("allow_voice_urls", false);
        o.finish()?;
        Ok(Self {
            timeout_ms,
            silent_start_ms,
            silent_start_peak,
            silent_start_retries,
            allow_voice_urls,
        })
    }
}

impl PocketHttpConfig {
    pub(crate) fn from_tts_config(cfg: &TtsConfig) -> Result<Self, anyhow::Error> {
        let PocketOptions {
            timeout_ms,
            silent_start_ms,
            silent_start_peak,
            silent_start_retries,
            allow_voice_urls,
        } = PocketOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let endpoint = cfg
            .endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());

        let voice = cfg
            .voice
            .clone()
            .unwrap_or_else(|| DEFAULT_VOICE.to_string());

        let silent_start = (silent_start_ms > 0).then(|| SilentStartGuard {
            window_samples: usize::try_from(u64::from(SAMPLE_RATE) * silent_start_ms / 1000)
                .unwrap_or(usize::MAX),
            peak_threshold: silent_start_peak,
            retries: silent_start_retries,
        });

        Ok(Self {
            endpoint,
            voice: voice_pipes_sdk::tts::voice_cell(voice),
            request_timeout: Duration::from_millis(timeout_ms),
            silent_start,
            allow_voice_urls,
        })
    }

    /// Snapshot the current default voice (cheap clone of an `Arc<String>`).
    pub(crate) fn current_voice(&self) -> std::sync::Arc<String> {
        self.voice.load_full()
    }
}
