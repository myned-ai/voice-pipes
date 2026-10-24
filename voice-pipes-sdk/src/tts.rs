//! TTS types, traits, and errors.
//!
//! Implement [`TtsProvider`] and [`TtsFactory`] here and register your
//! provider with `voice_pipes::tts::register_tts_factory()` before the
//! engine serves.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

use crate::audio::AudioFrame;
use crate::config::{ConfigError, TtsConfig};
use crate::schema::ProviderSchema;

// ── Error ─────────────────────────────────────────────────────

/// Errors returned by TTS provider operations.
///
/// # Stability
/// `#[non_exhaustive]` — new variants can be added in minor SDK versions.
/// Match with a `_ => {}` wildcard arm to stay forward-compatible.
#[derive(Debug)]
#[non_exhaustive]
pub enum TtsError {
    /// Text → phoneme conversion failed.
    PhonemizationFailed(String),
    /// A single synthesis pass failed; retrying may succeed.
    InferenceFailed(String),
    /// Model weights could not be loaded (bad path, corrupt file, …).
    ModelLoadFailed(String),
    /// Provider configuration is invalid.
    ConfigError(String),
    /// Remote API returned an error (HTTP status, message).
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
}

impl TtsError {
    /// A saturated pool of engines: transient, so the turn fails fast
    /// instead of waiting on an engine another session holds.
    pub fn busy(reason: impl fmt::Display) -> Self {
        Self::InferenceFailed(reason.to_string())
    }

    /// The error for a hosted API's non-success HTTP `status`, with the
    /// vendor's `message` (already extracted from its error body, or the body
    /// itself; kept to 512 characters). `vendor` names the service in the
    /// message.
    ///
    /// 401/403 are [`AuthError`](Self::AuthError), 429
    /// [`RateLimited`](Self::RateLimited), 400/404/422 a request the config
    /// made wrong, such as an unknown voice or model
    /// ([`ConfigError`](Self::ConfigError)), 5xx a failed synthesis that may
    /// succeed on retry ([`InferenceFailed`](Self::InferenceFailed)),
    /// anything else [`ApiError`](Self::ApiError), which is transient only
    /// for 408 (see [`is_transient`](Self::is_transient)).
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
            500..=599 => {
                Self::InferenceFailed(format!("{vendor} server error ({status}): {message}"))
            }
            _ => Self::ApiError { status, message },
        }
    }

    /// Returns `true` for transient errors that may succeed on retry.
    ///
    /// An [`ApiError`](Self::ApiError) is an HTTP status: a timeout (408) or
    /// a server error (5xx) may pass, any other 4xx is the request itself
    /// and fails the same way again.
    pub fn is_transient(&self) -> bool {
        match self {
            TtsError::InferenceFailed(_)
            | TtsError::RateLimited { .. }
            | TtsError::NetworkError(_) => true,
            TtsError::ApiError { status, .. } => *status == 408 || *status >= 500,
            _ => false,
        }
    }
}

impl fmt::Display for TtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TtsError::PhonemizationFailed(m) => write!(f, "phonemization failed: {m}"),
            TtsError::InferenceFailed(m) => write!(f, "inference failed: {m}"),
            TtsError::ModelLoadFailed(m) => write!(f, "model load failed: {m}"),
            TtsError::ConfigError(m) => write!(f, "config error: {m}"),
            TtsError::ApiError { status, message } => write!(f, "API error ({status}): {message}"),
            TtsError::AuthError(m) => write!(f, "auth error: {m}"),
            TtsError::RateLimited { retry_after: None } => write!(f, "rate limited"),
            TtsError::RateLimited {
                retry_after: Some(d),
            } => write!(f, "rate limited; retry after {d:?}"),
            TtsError::NetworkError(m) => write!(f, "network error: {m}"),
        }
    }
}

impl std::error::Error for TtsError {}

// ── Data types ────────────────────────────────────────────────

/// A word-level timing boundary reported by TTS and speech-to-speech
/// providers that support it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WordBoundary {
    /// The word (or token) text.
    pub word: String,
    /// Offset from fragment start in milliseconds.
    pub start_ms: u32,
    /// Duration of the word in milliseconds.
    pub duration_ms: u32,
}

/// Groups character-level timings, as some vendors report speech, into
/// [`WordBoundary`]s: whitespace ends a word and punctuation stays attached
/// to it.
///
/// It keeps the open word between calls, so a word whose characters arrive
/// in two chunks becomes one boundary, as long as both chunks' times count
/// from the same origin. A whitespace entry (or an empty one) ends the open
/// word; [`flush`](Self::flush) ends the last.
///
/// ```
/// use voice_pipes_sdk::tts::WordGrouper;
///
/// let mut words = WordGrouper::new();
/// let mut out = Vec::new();
/// for (ch, start, end) in [("H", 0, 50), ("i", 50, 100), ("!", 100, 150), (" ", 150, 200)] {
///     words.push(ch, start, end, &mut out);
/// }
/// words.push("y", 200, 260, &mut out);
/// words.flush(&mut out);
/// assert_eq!(out[0].word, "Hi!");
/// assert_eq!((out[0].start_ms, out[0].duration_ms), (0, 150));
/// assert_eq!(out[1].word, "y");
/// ```
#[derive(Debug, Clone, Default)]
pub struct WordGrouper {
    word: String,
    start_ms: u64,
    end_ms: u64,
}

impl WordGrouper {
    /// No word open yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// One entry of the vendor's timing (a character, or a run it sends as
    /// one), spoken from `start_ms` to `end_ms`. Whitespace ends the open
    /// word and pushes it to `out`; anything else extends it.
    pub fn push(&mut self, ch: &str, start_ms: u64, end_ms: u64, out: &mut Vec<WordBoundary>) {
        if ch.chars().all(char::is_whitespace) {
            self.flush(out);
            return;
        }
        if self.word.is_empty() {
            self.start_ms = start_ms;
        }
        self.word.push_str(ch);
        self.end_ms = end_ms;
    }

    /// End the open word, if there is one, and push it to `out`. Times past
    /// `u32` milliseconds saturate.
    pub fn flush(&mut self, out: &mut Vec<WordBoundary>) {
        if self.word.is_empty() {
            return;
        }
        out.push(WordBoundary {
            word: std::mem::take(&mut self.word),
            start_ms: u32::try_from(self.start_ms).unwrap_or(u32::MAX),
            duration_ms: u32::try_from(self.end_ms.saturating_sub(self.start_ms))
                .unwrap_or(u32::MAX),
        });
    }
}

/// The `tts_detail` audit event for one synthesis, so every voice reports
/// its timing in one shape: `elapsed_ms` (whole milliseconds), `chars` (the
/// text's length in bytes, never the text), `audio_duration_s`,
/// `sample_rate`, `rtf` (synthesis time over audio time) and `rtfx` (its
/// inverse), a ratio being 0 where its divisor is; then each field of
/// `extra`, an object such as `{"voice": .., "provider": .., "model": ..}`,
/// which adds to these or replaces one. `component` is the emitter, as in
/// [`TraceEvent::new`](crate::TraceEvent::new).
///
/// ```
/// use std::time::Duration;
/// use voice_pipes_sdk::tts::detail_event;
///
/// let e = detail_event(
///     "tts:mine",
///     11,
///     48_000,
///     24_000,
///     Duration::from_millis(500),
///     serde_json::json!({ "voice": "alba" }),
/// );
/// assert_eq!(e.event_type, "tts_detail");
/// assert_eq!(e.payload["audio_duration_s"], 2.0);
/// assert_eq!(e.payload["rtfx"], 4.0);
/// assert_eq!(e.payload["voice"], "alba");
/// ```
#[expect(
    clippy::cast_precision_loss,
    reason = "a sample count far inside f64's exact range"
)]
pub fn detail_event(
    component: &str,
    chars: usize,
    samples: usize,
    sample_rate: u32,
    elapsed: Duration,
    extra: serde_json::Value,
) -> crate::TraceEvent {
    let ratio = |num: f64, den: f64| if den > 0.0 { num / den } else { 0.0 };
    let audio_s = ratio(samples as f64, f64::from(sample_rate));
    let elapsed_s = elapsed.as_secs_f64();
    let mut payload = serde_json::json!({
        "elapsed_ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        "chars": chars,
        "audio_duration_s": audio_s,
        "sample_rate": sample_rate,
        "rtf": ratio(elapsed_s, audio_s),
        "rtfx": ratio(audio_s, elapsed_s),
    });
    if let (Some(fields), serde_json::Value::Object(extra)) = (payload.as_object_mut(), extra) {
        fields.extend(extra);
    }
    crate::TraceEvent::new(component, "tts_detail", payload)
}

/// Optional per-request TTS parameter overrides.
///
/// Providers read the fields they support and silently ignore the rest.
/// `#[non_exhaustive]`, so a new hint is a field rather than a breaking
/// change: start from [`Default`] and the `with_*` methods.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SynthesisHints {
    /// Override voice/speaker for this fragment.
    pub voice: Option<String>,
    /// Override language code (BCP-47: `"en-us"`, `"ja"`, etc.).
    pub language: Option<String>,
    /// Override speed (0.5–2.0).
    pub speed: Option<f32>,
    /// Provider-specific key-value passthrough.
    pub options: HashMap<String, serde_json::Value>,
}

impl SynthesisHints {
    with_setters! {
        with_voice => voice: Option<String>,
        with_language => language: Option<String>,
        with_speed => speed: Option<f32>,
        with_options => options: HashMap<String, serde_json::Value>,
    }
}

/// One streaming synthesis: the text, and what goes with it.
///
/// `#[non_exhaustive]`, so a new input becomes a field rather than another
/// trait method; build one with [`new`](Self::new) and the `with_*` methods.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SynthesisRequest<'a> {
    /// The text to speak.
    pub text: &'a str,
    /// Per-request overrides (voice, language, speed); `None` for the
    /// provider's configured defaults.
    pub hints: Option<&'a SynthesisHints>,
    /// Where to report word timings, when the caller wants them. Set only
    /// for a provider whose
    /// [`supports_word_timestamps`](TtsProvider::supports_word_timestamps)
    /// is `true`; a provider that cannot time words ignores it.
    pub word_tx: Option<tokio::sync::mpsc::Sender<WordBoundary>>,
}

impl<'a> SynthesisRequest<'a> {
    /// A request to speak `text` with the provider's defaults.
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            hints: None,
            word_tx: None,
        }
    }

    /// With per-request overrides.
    #[must_use]
    pub fn with_hints(mut self, hints: Option<&'a SynthesisHints>) -> Self {
        self.hints = hints;
        self
    }

    /// With a channel for word timings.
    #[must_use]
    pub fn with_word_timing(
        mut self,
        word_tx: Option<tokio::sync::mpsc::Sender<WordBoundary>>,
    ) -> Self {
        self.word_tx = word_tx;
        self
    }
}

// ── Sessionful provider support ───────────────────────────────

/// Configuration handed to [`TtsProvider::open_tts_session`] when opening a
/// per-session upstream handle.
///
/// `#[non_exhaustive]`: build one with [`new`](Self::new) and the `with_*`
/// methods.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TtsSessionConfig {
    /// The voice-pipes session id this handle is bound to.
    pub session_id: String,
    /// Voice / speaker selection. `None` lets the provider pick its default.
    pub voice: Option<String>,
    /// BCP-47 language tag (`"en-us"`, `"ja"`, …). `None` defers to provider default.
    pub language: Option<String>,
    /// Sample rate of user audio that will be teed in via `push_user_audio`.
    pub input_sample_rate: u32,
    /// Sample rate the runner expects on output [`AudioFrame`]s.
    pub output_sample_rate: u32,
    /// Provider-specific key-value passthrough.
    pub options: HashMap<String, serde_json::Value>,
}

/// The pipeline's 16 kHz in both directions: a default with zero rates
/// would hand a provider a rate no audio can have.
impl Default for TtsSessionConfig {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            voice: None,
            language: None,
            input_sample_rate: 16_000,
            output_sample_rate: 16_000,
            options: HashMap::new(),
        }
    }
}

impl TtsSessionConfig {
    /// For the session `session_id`, everything else at its default.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Self::default()
        }
    }

    with_setters! {
        with_voice => voice: Option<String>,
        with_language => language: Option<String>,
        with_input_sample_rate => input_sample_rate: u32,
        with_output_sample_rate => output_sample_rate: u32,
        with_options => options: HashMap<String, serde_json::Value>,
    }
}

/// A per-session upstream handle for sessionful TTS providers.
///
/// The lifecycle is owned by the runner task — all methods take `&mut self`
/// so the borrow checker enforces serial access on a single owner.
///
/// Default: implement `TtsProvider::open_tts_session` to return `Ok(None)`
/// for stateless providers that don't need this interface.
#[async_trait]
pub trait TtsSession: Send + Sync {
    /// Push a chunk of user audio into the upstream context buffer.
    async fn push_user_audio(&mut self, frame: &AudioFrame) -> Result<(), TtsError>;

    /// Mark end of user turn.
    async fn commit_user_turn(&mut self) -> Result<(), TtsError>;

    /// Synthesize the next assistant turn.
    /// Frames MUST be sent at [`TtsSessionConfig::output_sample_rate`].
    async fn synthesize_turn(
        &mut self,
        text: &str,
        hints: Option<&SynthesisHints>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
    ) -> Result<(), TtsError>;

    /// Cancel an in-progress upstream response. Idempotent.
    async fn cancel(&mut self) -> Result<(), TtsError>;

    /// Close the upstream connection cleanly. Consumes the box.
    async fn close(self: Box<Self>) -> Result<(), TtsError>;
}

// ── Provider trait ────────────────────────────────────────────

/// The plugin-facing TTS provider interface.
///
/// Implement this trait in your plugin crate, then expose a [`TtsFactory`]
/// and register it with `voice_pipes::tts::register_tts_factory()`.
///
/// # Component health
/// Override [`as_component`](TtsProvider::as_component) to register this
/// provider in the health dashboard and the auto-degrade loop. Embed an
/// `Arc<ComponentCore>` and return a `CoreComponent`:
///
/// ```ignore
/// fn as_component(&self) -> Option<std::sync::Arc<dyn voice_pipes_sdk::Component>> {
///     Some(std::sync::Arc::new(
///         voice_pipes_sdk::component::CoreComponent::new(self.core.clone())))
/// }
/// ```
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize text into an `AudioFrame` of PCM i16 samples (batch).
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError>;

    /// Streaming synthesis: sends audio frames on `tx` as they become
    /// available. `req` carries the text and what goes with it (per-request
    /// hints, and a word-timing channel when the caller wants one).
    ///
    /// The default wraps batch [`synthesize`](Self::synthesize), ignores the
    /// hints and sends one frame.
    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let frame = self.synthesize(req.text).await?;
        if cancel.is_cancelled() {
            return Ok(());
        }
        let _ = tx.send(frame).await;
        Ok(())
    }

    /// Native sample rate of the provider (Hz).
    fn sample_rate(&self) -> u32;

    /// Human-readable name for logging / metrics.
    fn name(&self) -> &str;

    /// Optional session-start warm-up. Default: no-op. Errors are logged
    /// but must not block session readiness.
    async fn warm_up(&self) -> Result<(), TtsError> {
        Ok(())
    }

    /// The model actually serving this provider, for the component check
    /// and the session report. Default: not reported.
    async fn model_identity(&self) -> Option<crate::component::ModelIdentity> {
        None
    }

    /// The compute device the model runs on. Default: not reported.
    async fn device_info(&self) -> Option<crate::component::DeviceInfo> {
        None
    }

    /// Whether this provider can produce word-level timing boundaries. When
    /// `true`, the pipeline passes a word-timing channel in
    /// [`SynthesisRequest::word_tx`] and the provider reports on it.
    fn supports_word_timestamps(&self) -> bool {
        false
    }

    /// Open a session-scoped handle for providers that need a persistent
    /// upstream connection (conversational-awareness TTS, realtime S2S, etc.).
    ///
    /// Default returns `Ok(None)` — stateless providers do nothing and the
    /// runner falls back to per-call [`synthesize_stream`](TtsProvider::synthesize_stream).
    async fn open_tts_session(
        &self,
        _cfg: &TtsSessionConfig,
    ) -> Result<Option<Box<dyn TtsSession>>, TtsError> {
        Ok(None)
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

// ── Factory trait ─────────────────────────────────────────────

/// Registration metadata + construction for one TTS provider.
///
/// Implement this on a unit struct in your plugin crate, then call
/// `voice_pipes::tts::register_tts_factory(|| Box::new(MyFactory))`.
pub trait TtsFactory: Send + Sync {
    /// Name as it appears in TOML `provider = "..."`.
    fn name(&self) -> &str;

    /// Additional names that resolve to this factory (aliases).
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Whether `n` is this factory's name or one of its aliases.
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

    /// Validate provider-specific config. Return one [`ConfigError`] per
    /// invalid field.
    fn validate(&self, cfg: &TtsConfig) -> Vec<ConfigError>;

    /// Construct the provider from its config. `ctx` carries what the
    /// framework supplies: an audit channel, this component's live
    /// parameters, text chunking; see [`BuildContext`](crate::build::BuildContext).
    fn build(
        &self,
        cfg: &TtsConfig,
        ctx: &crate::build::BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, crate::BuildError>;

    /// The sample rate this provider's audio comes at for `cfg`, when it is
    /// known without building the provider (the pipeline graph shows it).
    /// Default: unknown.
    fn output_sample_rate(&self, cfg: &TtsConfig) -> Option<u32> {
        let _ = cfg;
        None
    }

    /// Provider-owned parameter schema for the graph editor.
    ///
    /// Return the complete descriptor list plus matching current values.
    /// The default implementation returns a minimal fallback so unmigrated
    /// providers still display their name.
    fn provider_schema(&self, cfg: &TtsConfig) -> ProviderSchema {
        let mut s = ProviderSchema::fallback(self.name(), &format!("{} TTS provider", self.name()));
        s.push(
            crate::schema::ParamDescriptor::static_str(
                "provider",
                "Provider",
                "The TTS framework or service used for speech synthesis.",
            ),
            cfg.provider.clone(),
        );
        s
    }
}

// ── Shared pieces for streaming providers ────────────────────
//
// What every hosted TTS provider needs around its own request: the voice
// an operator can change live, the voice one request asks for, the
// component's latency and error counts, and whole-utterance synthesis on
// top of streaming.

/// A provider's configured voice, shared with the parameter store so a
/// live edit applies on the next synthesis.
pub type VoiceCell = std::sync::Arc<arc_swap::ArcSwap<String>>;

/// A voice cell holding `voice`.
pub fn voice_cell(voice: String) -> VoiceCell {
    std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(voice))
}

/// `cell`, or the parameter store's `voice` cell when the store has one:
/// adopting it is what makes an edit through the store reach the provider.
pub fn adopt_voice(params: Option<&crate::params::AtomicParamStore>, cell: VoiceCell) -> VoiceCell {
    params.and_then(|p| p.string_cell("voice")).unwrap_or(cell)
}

/// The voice for one request: the hint's when it names one, else the
/// configured voice as it is now.
pub fn requested_voice(hints: Option<&SynthesisHints>, configured: &VoiceCell) -> String {
    hints
        .and_then(|h| h.voice.clone())
        .unwrap_or_else(|| (*configured.load_full()).clone())
}

/// `synthesis`'s outcome on `core`: its latency when it succeeds, the error
/// when it fails.
///
/// # Errors
/// `synthesis`'s own.
pub async fn recorded<T>(
    core: &crate::component::ComponentCore,
    synthesis: impl std::future::Future<Output = Result<T, TtsError>>,
) -> Result<T, TtsError> {
    let start = std::time::Instant::now();
    let result = synthesis.await;
    match &result {
        Ok(_) => core.record_ok(start.elapsed()),
        Err(e) => core.record_error(e),
    }
    result
}

/// [`recorded`] for a synthesis that `cancel` can stop: when `cancel` has
/// fired by the time it ends, nothing is recorded. A stream cut short by a
/// barge-in returns `Ok` with part of the audio; counting it would add a
/// success and a latency sample for synthesis that never finished.
///
/// # Errors
/// `synthesis`'s own.
pub async fn recorded_unless_cancelled<T>(
    core: &crate::component::ComponentCore,
    cancel: &tokio_util::sync::CancellationToken,
    synthesis: impl std::future::Future<Output = Result<T, TtsError>>,
) -> Result<T, TtsError> {
    let start = std::time::Instant::now();
    let result = synthesis.await;
    if cancel.is_cancelled() {
        return result;
    }
    match &result {
        Ok(_) => core.record_ok(start.elapsed()),
        Err(e) => core.record_error(e),
    }
    result
}

/// Whole-utterance synthesis from a streaming one: `stream` is given a
/// sender and streams the utterance into it, and the frames are collected
/// into one at `sample_rate`. The two run together, so a stream that sends
/// more than the channel holds does not wait on itself.
///
/// # Errors
/// The stream's own.
pub async fn collect_stream<F, Fut>(sample_rate: u32, stream: F) -> Result<AudioFrame, TtsError>
where
    F: FnOnce(tokio::sync::mpsc::Sender<AudioFrame>) -> Fut,
    Fut: std::future::Future<Output = Result<(), TtsError>>,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AudioFrame>(64);
    let collect = async {
        let mut samples: Vec<i16> = Vec::new();
        while let Some(frame) = rx.recv().await {
            samples.extend_from_slice(&frame.samples);
        }
        samples
    };
    let (streamed, samples) = tokio::join!(stream(tx), collect);
    streamed?;
    Ok(AudioFrame {
        samples,
        sample_rate,
        channels: 1,
    })
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/tts.rs"]
mod tests;
