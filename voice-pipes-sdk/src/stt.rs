//! STT types, traits, and errors.
//!
//! Implement [`SttProvider`] and [`SttFactory`] and register your provider
//! with `voice_pipes::stt::register_stt_factory()`.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

use crate::audio::AudioFrame;
use crate::config::{ConfigError, SttConfig};
use crate::schema::ProviderSchema;

// ── Capabilities ──────────────────────────────────────────────

/// Feature flags describing what a given STT provider supports.
///
/// # Stability
/// `#[non_exhaustive]` — new variants can be added in minor SDK versions.
/// Match with a `_ => {}` wildcard arm to stay forward-compatible.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SttCapability {
    /// Native real-time streaming (not pseudo/buffer-and-recompute).
    Streaming,
    /// Partial/interim transcripts before the final result.
    InterimResults,
    /// Per-word start/end timestamps and optional confidence.
    WordTimestamps,
    /// Provider performs its own voice-activity detection.
    BuiltinVad,
    /// Provider emits semantic end-of-turn signals.
    EndOfTurn,
    /// Speaker diarization (multi-speaker identification).
    Diarization,
    /// Automatic spoken-language detection.
    LanguageDetection,
    /// Key-term / phrase boosting / custom vocabulary.
    CustomVocabulary,
    /// Successive final results within one segment supersede each other:
    /// the provider re-sends the whole segment so far (same
    /// `segment_index`) rather than only the words added since the last
    /// final. The pipeline then keeps the latest final of a segment
    /// instead of appending it. Finals with a new `segment_index` are
    /// still appended, so a pause inside one utterance loses nothing.
    /// Providers that emit each final exactly once leave this unset.
    CumulativeFinals,
}

// ── Data types ────────────────────────────────────────────────

/// Timing and confidence for a single recognised word.
///
/// `#[non_exhaustive]`, so a new per-word detail (a speaker label, say) is
/// a field rather than a breaking change; build one with [`new`](Self::new)
/// and the `with_*` methods.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WordInfo {
    /// The recognised word text.
    pub word: String,
    /// Offset of the word start from the beginning of the utterance.
    pub start: Duration,
    /// Offset of the word end from the beginning of the utterance.
    pub end: Duration,
    /// Per-word confidence (0.0–1.0), when the provider reports one.
    pub confidence: Option<f32>,
}

impl WordInfo {
    /// `word` from `start` to `end`, with no confidence.
    pub fn new(word: impl Into<String>, start: Duration, end: Duration) -> Self {
        Self {
            word: word.into(),
            start,
            end,
            confidence: None,
        }
    }

    with_setters! {
        with_confidence => confidence: Option<f32>,
    }
}

/// Per-utterance streaming-inference metrics reported by the STT provider.
///
/// Field semantics follow Microsoft, *Pushing the Limits of On-Device
/// Streaming ASR* (arXiv:2604.14493v2, §3.2). Reported on the **final**
/// `TranscriptionResult` of a streaming utterance.
///
/// `#[non_exhaustive]`: build one with [`new`](Self::new) and the `with_*`
/// setters, so a metric added later does not break your crate.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct InferenceMetrics {
    /// Total audio duration fed to the provider for this utterance (ms).
    pub audio_duration_ms: Option<u64>,
    /// Number of encoder/decoder invocations performed across the utterance.
    pub inference_count: Option<u32>,
    /// Real-time factor: `audio_duration / total_inference_wall_time` (higher = faster).
    pub rtfx: Option<f32>,
    /// Static algorithmic delay floor (ms). For chunked transducers: chunk + right context.
    pub algorithmic_delay_ms: Option<u64>,
    /// Effective per-step latency: `algorithmic_delay × (1 + 1/RTFx)` (ms).
    pub effective_latency_ms: Option<u64>,
    /// Bound on the audio window re-encoded per step (ms). `None` = unbounded.
    pub streaming_window_ms: Option<u32>,
}

impl InferenceMetrics {
    /// No metrics yet; add the ones the provider measures.
    pub fn new() -> Self {
        Self::default()
    }

    /// What a local model measured over one utterance: `audio_secs` of
    /// audio run through `inference_count` passes that took
    /// `inference_wall` in all. Sets the audio duration, the pass count and
    /// the real-time factor `rtfx` (audio time over wall time; `None` when no wall time was
    /// measured). A negative or NaN duration counts as 0 ms.
    ///
    /// ```
    /// use std::time::Duration;
    /// use voice_pipes_sdk::InferenceMetrics;
    ///
    /// let m = InferenceMetrics::measured(2.0, Duration::from_millis(500), 3);
    /// assert_eq!(m.audio_duration_ms, Some(2_000));
    /// assert_eq!(m.inference_count, Some(3));
    /// assert_eq!(m.rtfx, Some(4.0));
    /// ```
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "float to int and f64 to f32 casts saturate (NaN to 0); they cannot panic"
    )]
    pub fn measured(audio_secs: f64, inference_wall: Duration, inference_count: u32) -> Self {
        let wall_secs = inference_wall.as_secs_f64();
        let rtfx = (wall_secs > 0.0).then(|| (audio_secs / wall_secs) as f32);
        Self::new()
            .with_audio_duration_ms(Some((audio_secs * 1000.0) as u64))
            .with_inference_count(Some(inference_count))
            .with_rtfx(rtfx)
    }

    /// With `delay_ms` as the algorithmic delay floor, and the effective
    /// latency it implies at this `rtfx`: `delay × (1 + 1/RTFx)`. Without a
    /// positive `rtfx` there is no effective latency.
    ///
    /// ```
    /// use std::time::Duration;
    /// use voice_pipes_sdk::InferenceMetrics;
    ///
    /// let m = InferenceMetrics::measured(2.0, Duration::from_millis(500), 3)
    ///     .with_delay_floor_ms(160);
    /// assert_eq!(m.algorithmic_delay_ms, Some(160));
    /// assert_eq!(m.effective_latency_ms, Some(200));
    /// ```
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "milliseconds far inside f64's exact range; a float to int cast saturates"
    )]
    pub fn with_delay_floor_ms(self, delay_ms: u64) -> Self {
        let effective = self
            .rtfx
            .filter(|&r| r > 0.0)
            .map(|r| (delay_ms as f64 * (1.0 + 1.0 / f64::from(r))) as u64);
        self.with_algorithmic_delay_ms(Some(delay_ms))
            .with_effective_latency_ms(effective)
    }

    with_setters! {
        with_audio_duration_ms => audio_duration_ms: Option<u64>,
        with_inference_count => inference_count: Option<u32>,
        with_rtfx => rtfx: Option<f32>,
        with_algorithmic_delay_ms => algorithmic_delay_ms: Option<u64>,
        with_effective_latency_ms => effective_latency_ms: Option<u64>,
        with_streaming_window_ms => streaming_window_ms: Option<u32>,
    }
}

/// Result of transcription — a single recognised segment.
///
/// # Stability
/// `#[non_exhaustive]`: build one with [`new`](Self::new) and the `with_*`
/// setters (or set the public fields on a value you hold), so a field added
/// in a minor SDK version does not break your crate.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TranscriptionResult {
    /// The recognised text for this segment.
    pub text: String,
    /// `true` for the final result of an utterance; `false` for interim partials.
    pub is_final: bool,
    /// Segment confidence (0.0–1.0), when the provider reports one.
    pub confidence: Option<f32>,
    /// Detected or configured language (BCP-47), when known.
    pub language: Option<String>,
    /// Monotonic index of this segment within the stream.
    pub segment_index: u32,
    /// Segment start offset from the beginning of the audio, when known.
    pub start_time: Option<Duration>,
    /// Segment end offset from the beginning of the audio, when known.
    pub end_time: Option<Duration>,
    /// Per-word timestamps (empty when provider does not support them).
    pub words: Vec<WordInfo>,
    /// Semantic end-of-turn signal (`None` = not supported by this provider).
    pub is_end_of_turn: Option<bool>,
    /// `true` when the provider detected that speech has started in the stream.
    pub speech_started: bool,
    /// Speaker ID from diarization (`None` = not supported / single speaker).
    pub speaker_id: Option<u32>,
    /// Streaming-inference metrics, populated on the final result by providers
    /// that measure them. `None` for non-final partials and batch providers.
    pub inference_metrics: Option<InferenceMetrics>,
}

impl TranscriptionResult {
    /// A segment with `text`, final or interim; every other field at its
    /// default (segment 0, no timing, no words).
    pub fn new(text: impl Into<String>, is_final: bool) -> Self {
        Self {
            text: text.into(),
            is_final,
            ..Self::default()
        }
    }

    with_setters! {
        with_confidence => confidence: Option<f32>,
        with_language => language: Option<String>,
        with_segment_index => segment_index: u32,
        with_start_time => start_time: Option<Duration>,
        with_end_time => end_time: Option<Duration>,
        with_words => words: Vec<WordInfo>,
        with_is_end_of_turn => is_end_of_turn: Option<bool>,
        with_speech_started => speech_started: bool,
        with_speaker_id => speaker_id: Option<u32>,
        with_inference_metrics => inference_metrics: Option<InferenceMetrics>,
    }
}

/// Audio encoding format for the input stream.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AudioEncoding {
    /// Signed 16-bit integer PCM.
    PcmI16,
    /// 32-bit float PCM.
    PcmF32,
}

/// A chunk of audio for streaming transcription.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// PCM i16 samples.
    pub samples: Vec<i16>,
    /// `true` signals end-of-utterance — the final chunk.
    pub is_last: bool,
}

/// Request parameters for speech recognition.
///
/// `#[non_exhaustive]`: start from [`Default`] and the `with_*` methods.
#[derive(Debug)]
#[non_exhaustive]
pub struct RecognitionRequest {
    /// Caller-chosen id correlating results and audit records.
    pub request_id: String,
    /// Requested recognition language (BCP-47); `None` = provider default / auto.
    pub language: Option<String>,
    /// Input sample rate in Hz.
    pub sample_rate: u32,
    /// Input channel count (the pipeline sends mono).
    pub channels: u16,
    /// Input PCM encoding.
    pub encoding: AudioEncoding,
    /// Emit interim (non-final) partial results while decoding.
    pub interim_results: bool,
    /// Ask the provider for punctuated output, where supported.
    pub punctuation: bool,
    /// Ask the provider for per-word timestamps, where supported.
    pub word_timestamps: bool,
    /// Provider-specific options.
    pub options: HashMap<String, serde_json::Value>,
    /// How long the provider may wait for a free inference slot before
    /// refusing the stream; `None` is the provider's own bound. The engine
    /// sets it when it starts a stream to replace one it gave up on: the
    /// replaced stream's last run may still hold the slot until the model
    /// returns, and the replacement is to take over the moment it does.
    pub slot_wait: Option<Duration>,
}

impl Default for RecognitionRequest {
    fn default() -> Self {
        Self {
            request_id: String::new(),
            language: None,
            sample_rate: 16_000,
            channels: 1,
            encoding: AudioEncoding::PcmI16,
            interim_results: false,
            punctuation: true,
            word_timestamps: false,
            options: HashMap::new(),
            slot_wait: None,
        }
    }
}

impl RecognitionRequest {
    with_setters! {
        with_request_id => request_id: String,
        with_language => language: Option<String>,
        with_sample_rate => sample_rate: u32,
        with_channels => channels: u16,
        with_encoding => encoding: AudioEncoding,
        with_interim_results => interim_results: bool,
        with_punctuation => punctuation: bool,
        with_word_timestamps => word_timestamps: bool,
        with_options => options: HashMap<String, serde_json::Value>,
        with_slot_wait => slot_wait: Option<Duration>,
    }
}

// ── Error type ────────────────────────────────────────────────

/// Errors returned by STT provider operations.
///
/// # Stability
/// `#[non_exhaustive]` — new variants can be added in minor SDK versions.
/// Match with a `_ => {}` wildcard arm to stay forward-compatible.
#[derive(Debug)]
#[non_exhaustive]
pub enum SttError {
    // Permanent
    /// Model weights could not be loaded (bad path, corrupt file, …).
    ModelLoadFailed(String),
    /// Provider configuration is invalid.
    ConfigError(String),
    /// Input audio was rejected (wrong format, empty, …).
    InvalidAudio(String),
    // Transient
    /// A single inference pass failed; retrying may succeed.
    InferenceFailed(String),
    /// The operation exceeded the given deadline.
    Timeout(Duration),
    // Streaming-specific
    /// The streaming channel closed before the utterance completed.
    ChannelClosed,
    // API / hosted provider errors
    /// Remote API returned an error.
    ApiError {
        /// HTTP status code.
        status: u16,
        /// Provider-supplied error message.
        message: String,
    },
    /// Authentication failed (bad or missing API key).
    AuthError(String),
    /// Rate-limited by the remote provider; `retry_after` is how long it
    /// asked the caller to wait, when it said.
    RateLimited {
        /// The provider's `Retry-After`, when it sent one.
        retry_after: Option<Duration>,
    },
    /// Network / connection failure.
    NetworkError(String),
    /// The streaming client gave up reconnecting: too many consecutive
    /// failures without a healthy session in between, or too many
    /// reconnects in a short window. Permanent for this session; `reason`
    /// carries the last underlying error.
    ReconnectExhausted {
        /// Failed attempts (or reconnects in the window) at the point of
        /// giving up.
        attempts: u32,
        /// The last error that triggered a reconnect.
        reason: String,
    },
}

impl SttError {
    /// A saturated pool of engines: transient, so the turn fails fast
    /// instead of waiting on an engine another session holds.
    pub fn busy(reason: impl fmt::Display) -> Self {
        Self::InferenceFailed(reason.to_string())
    }

    /// Returns `true` for transient errors that may succeed on retry.
    ///
    /// An [`ApiError`](Self::ApiError) is an HTTP status: a timeout (408) or
    /// a server error (5xx) may pass, any other 4xx is the request itself
    /// and fails the same way again.
    pub fn is_transient(&self) -> bool {
        match self {
            SttError::InferenceFailed(_)
            | SttError::Timeout(_)
            | SttError::RateLimited { .. }
            | SttError::NetworkError(_) => true,
            SttError::ApiError { status, .. } => *status == 408 || *status >= 500,
            _ => false,
        }
    }

    /// The error for a hosted API's non-success HTTP `status`, with the
    /// vendor's `message` (kept to 512 characters). `vendor` names the
    /// service in the message.
    ///
    /// 401/403 are [`AuthError`](Self::AuthError), 429
    /// [`RateLimited`](Self::RateLimited), 400/404/422 a request the config
    /// made wrong ([`ConfigError`](Self::ConfigError)), anything else
    /// [`ApiError`](Self::ApiError), transient for 408 and 5xx.
    pub fn from_http_status(vendor: &str, status: u16, message: &str) -> Self {
        let message: String = if message.trim().is_empty() {
            format!("{vendor} HTTP {status}")
        } else {
            message.chars().take(512).collect()
        };
        match status {
            401 | 403 => Self::AuthError(format!("{vendor}: {message}")),
            429 => Self::RateLimited { retry_after: None },
            400 | 404 | 422 => Self::ConfigError(format!("{vendor}: {message}")),
            _ => Self::ApiError { status, message },
        }
    }
}

impl fmt::Display for SttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SttError::ModelLoadFailed(m) => write!(f, "model load failed: {m}"),
            SttError::ConfigError(m) => write!(f, "config error: {m}"),
            SttError::InvalidAudio(m) => write!(f, "invalid audio: {m}"),
            SttError::InferenceFailed(m) => write!(f, "inference failed: {m}"),
            SttError::Timeout(d) => write!(f, "timeout after {d:?}"),
            SttError::ChannelClosed => write!(f, "channel closed"),
            SttError::ApiError { status, message } => write!(f, "API error ({status}): {message}"),
            SttError::AuthError(m) => write!(f, "auth error: {m}"),
            SttError::RateLimited { retry_after: None } => write!(f, "rate limited"),
            SttError::RateLimited {
                retry_after: Some(d),
            } => write!(f, "rate limited; retry after {d:?}"),
            SttError::NetworkError(m) => write!(f, "network error: {m}"),
            SttError::ReconnectExhausted { attempts, reason } => {
                write!(f, "gave up after {attempts} reconnect attempts: {reason}")
            }
        }
    }
}

impl std::error::Error for SttError {}

// ── Provider trait ────────────────────────────────────────────

/// The plugin-facing STT provider interface.
///
/// Implement this trait in your plugin crate, then expose a [`SttFactory`]
/// and register it with `voice_pipes::stt::register_stt_factory()`.
///
/// # Component health
/// Override [`as_component`](SttProvider::as_component) to register this
/// provider in the health dashboard and the auto-degrade loop. The easiest
/// path: embed an `Arc<ComponentCore>` and return a `CoreComponent`:
///
/// ```ignore
/// fn as_component(&self) -> Option<std::sync::Arc<dyn voice_pipes_sdk::Component>> {
///     Some(std::sync::Arc::new(
///         voice_pipes_sdk::component::CoreComponent::new(self.core.clone())))
/// }
/// ```
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Batch: transcribe a complete audio buffer.
    async fn transcribe(&self, audio: &AudioFrame) -> Result<TranscriptionResult, SttError>;

    /// Streaming: receive audio chunks, produce results incrementally.
    ///
    /// # Contract
    ///
    /// The pipeline makes **one call per session** and keeps it running for
    /// the session's lifetime:
    ///
    /// - A chunk with `is_last = true` marks an **utterance boundary**, not
    ///   the end of the stream. Emit the final result for that utterance
    ///   (with `is_final = true`), reset any per-utterance state, and keep
    ///   reading: the next chunk belongs to the next utterance.
    /// - Number utterances with `segment_index`: `0` for the first, `1` for
    ///   the next, and so on. Interim and final results of one utterance
    ///   share its index. A provider whose finals re-send the growing text
    ///   of an utterance also declares [`SttCapability::CumulativeFinals`].
    /// - Return only when `cancel` fires, when `audio_rx` closes (the
    ///   session ended), when `tx` has no receiver, or on an error the
    ///   provider cannot recover from. Recoverable transport errors are the
    ///   provider's to retry; the pipeline treats a return as the end of the
    ///   worker and restarts it only as failure recovery.
    /// - For native streaming providers, emit interim results as audio
    ///   arrives. A buffering provider accumulates chunks until `is_last`
    ///   and emits one final per utterance.
    async fn transcribe_stream(
        &self,
        request: RecognitionRequest,
        audio_rx: tokio::sync::mpsc::Receiver<AudioChunk>,
        tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError>;

    /// Provider's expected input sample rate (Hz). Typically `16_000`.
    fn expected_sample_rate(&self) -> u32;

    /// Human-readable name for logging and metrics (e.g. `"nemo-ctc"`).
    fn name(&self) -> &str;

    /// Optional warm-up. Default: no-op.
    ///
    /// Pre-establish connections, touch the GPU, run one decode. Errors are
    /// logged but don't block the session. When it runs is decided by
    /// [`warm_up_scope`](Self::warm_up_scope): per session by default, or
    /// once at worker start for a local model that holds an inference set
    /// while it warms.
    async fn warm_up(&self) -> Result<(), SttError> {
        Ok(())
    }

    /// Whether the cost this warm-up pays is per connection or per process.
    ///
    /// A hosted recogniser pays a TLS handshake and a provider-side session
    /// per connection, so it is warmed at every session start, in the
    /// background, racing the caller's first words. A local model pays the
    /// runtime's lazy start-up once per process, and it holds one of a
    /// bounded number of inference sets while it does: warmed per session
    /// on a pool of one it took the only set exactly when the caller's
    /// first utterance needed it, and that turn failed for want of a set.
    /// Such a provider says so, and is warmed once at worker start instead,
    /// before any session exists to contend with it.
    fn warm_up_scope(&self) -> WarmUpScope {
        WarmUpScope::PerSession
    }

    /// How many sessions this provider can serve at once, when that is
    /// bounded: the size of a local model's inference pool. `None` when it
    /// is not (a hosted service, a streaming model with per-session state).
    ///
    /// The worker compares it with its own session cap at start. A batch
    /// recogniser holds one pool set for the whole of a decode, so two
    /// callers who stop speaking within the same second contend for it; a
    /// cap above the pool is a configuration that fails under ordinary load,
    /// and the failure reads as the recogniser being down.
    fn concurrent_capacity(&self) -> Option<usize> {
        None
    }

    /// Identity of the model actually serving this provider, resolved once at
    /// session setup. Override to resolve more than the local config; default
    /// `None` → setup falls back to the configured model name.
    async fn model_identity(&self) -> Option<crate::component::ModelIdentity> {
        None
    }

    /// Compute device this provider ran on (gpu/cpu), resolved once at session
    /// setup and recorded in `metadata.json` for the session diagnostic.
    /// Default `None` → device not reported (no badge shown).
    async fn device_info(&self) -> Option<crate::component::DeviceInfo> {
        None
    }

    /// Whether the pipeline should append ~500ms of synthetic silence after
    /// the final audio chunk at end-of-utterance.
    ///
    /// Return `false` for modern transducer/CTC models that already receive
    /// VAD-trailing silence and don't need the flush hack. Default: `true`.
    fn needs_flush_silence(&self) -> bool {
        true
    }

    /// Declare which capabilities this provider supports.
    fn capabilities(&self) -> HashSet<SttCapability>;

    /// Convenience: check for a single capability.
    fn supports(&self, cap: &SttCapability) -> bool {
        self.capabilities().contains(cap)
    }

    /// Optional [`Component`](crate::component::Component) adapter for health
    /// registration and auto-degrade.
    ///
    /// Return `Some(Arc::new(CoreComponent::new(self.core.clone())))` to opt
    /// in. Default: `None` (no health registration).
    fn as_component(&self) -> Option<std::sync::Arc<dyn crate::component::Component>> {
        None
    }
}

/// When a provider's [`SttProvider::warm_up`] should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WarmUpScope {
    /// The cost is paid per connection: warm at every session start.
    PerSession,
    /// The cost is paid once per process: warm once at worker start, never
    /// inside a session.
    PerProcess,
}

// ── Factory trait ─────────────────────────────────────────────

/// Registration metadata + construction for one STT provider.
///
/// Implement this on a unit struct in your plugin crate, then call
/// `voice_pipes::stt::register_stt_factory(|| Box::new(MyFactory))`.
pub trait SttFactory: Send + Sync {
    /// Name as it appears in TOML `provider = "..."`.
    fn name(&self) -> &str;

    /// Additional names that resolve to this factory (aliases).
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Returns `true` if `n` matches this factory's name or any alias.
    fn matches(&self, n: &str) -> bool {
        self.name() == n || self.aliases().contains(&n)
    }

    /// Local providers don't require an API key.
    fn is_local(&self) -> bool {
        false
    }

    /// The environment variable the host reads this provider's API key
    /// from when the config sets none. The host resolves the key and hands
    /// it over in the config's `api_key`; the provider never reads the
    /// environment for it. `None` for a provider without a key.
    fn default_api_key_env(&self) -> Option<&'static str> {
        None
    }

    /// Validate provider-specific config. Return one [`ConfigError`] per invalid field.
    fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError>;

    /// Construct the provider from its config. `ctx` carries what the
    /// framework supplies: an audit channel, this component's live
    /// parameters; see [`BuildContext`](crate::build::BuildContext).
    fn build(
        &self,
        cfg: &SttConfig,
        ctx: &crate::build::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, crate::BuildError>;

    /// Provider-owned parameter schema for the graph editor.
    ///
    /// Return the complete descriptor list (including common rows like `provider`)
    /// plus matching current values. The default implementation returns a minimal
    /// fallback so unmigrated providers still display their name.
    fn provider_schema(&self, cfg: &SttConfig) -> ProviderSchema {
        let mut s = ProviderSchema::fallback(self.name(), &format!("{} STT provider", self.name()));
        s.push(
            crate::schema::ParamDescriptor::static_str(
                "provider",
                "Provider",
                "The STT framework or service used for transcription.",
            ),
            cfg.provider.clone(),
        );
        s
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/stt.rs"]
mod tests;
