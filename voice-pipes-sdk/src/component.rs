//! Component health, telemetry, and error policy for plugin providers.
//!
//! Plugin crates that want to participate in the health dashboard and the
//! auto-degrade loop implement [`Component`] and return it from
//! `as_component()` on their provider trait.
//!
//! The easiest path: embed an [`Arc<ComponentCore>`], call
//! `core.record_ok(latency)` / `core.record_error(...)` on each request, and
//! return a [`CoreComponent`] from `as_component()`.
//!
//! # Example
//!
//! Compile-checked; `synthesize` etc. omitted (see the crate-level docs for a
//! full provider):
//!
//! ```no_run
//! use std::sync::Arc;
//! use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
//!
//! pub struct MyTtsProvider {
//!     core: Arc<ComponentCore>,
//! }
//!
//! impl MyTtsProvider {
//!     pub fn new() -> Self {
//!         Self {
//!             core: Arc::new(ComponentCore::new("tts:my-provider", ComponentKind::Tts)),
//!         }
//!     }
//!
//!     fn as_component(&self) -> Option<Arc<dyn Component>> {
//!         Some(Arc::new(CoreComponent::new(self.core.clone())))
//!     }
//! }
//! ```
//!
//! # Error policy
//!
//! Override [`Component::error_policy`] to request automatic retry on
//! transient failures (fragment of the impl above):
//!
//! ```ignore
//! fn error_policy(&self) -> voice_pipes_sdk::component::ErrorPolicy {
//!     voice_pipes_sdk::component::ErrorPolicy::RetryWithBackoff { max: 3 }
//! }
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use parking_lot::{Mutex, RwLock};

// ── ComponentKind ─────────────────────────────────────────────

/// Coarse classification for grouping in dashboards and health checks. The
/// framework uses this type itself, for built-in and plugin components alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ComponentKind {
    /// Speech-to-text.
    Stt,
    /// Text-to-speech.
    Tts,
    /// Text processing (LLM, agents, filters).
    Text,
    /// Voice activity detection.
    Vad,
    /// Audio capture / playback / resampling.
    Audio,
    /// Client transport (WebSocket, …).
    Transport,
    /// Anything that doesn't fit the above.
    Other,
}

impl fmt::Display for ComponentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ComponentKind::Stt => "stt",
            ComponentKind::Tts => "tts",
            ComponentKind::Text => "text",
            ComponentKind::Vad => "vad",
            ComponentKind::Audio => "audio",
            ComponentKind::Transport => "transport",
            ComponentKind::Other => "other",
        };
        f.write_str(s)
    }
}

// ── Health ────────────────────────────────────────────────────

/// Coarse health classification for a single component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HealthState {
    /// Fully operational.
    Healthy,
    /// Operational but degraded (e.g. high latency, fallback in use).
    Degraded,
    /// Cannot serve requests right now.
    Unhealthy,
    /// Not configured / disabled in this deployment.
    NotConfigured,
}

impl HealthState {
    /// The worse of two states, for rolling health up across components:
    /// `NotConfigured` < `Healthy` < `Degraded` < `Unhealthy`. A stage that
    /// is not configured is simply not part of the pipeline.
    pub fn worst(a: HealthState, b: HealthState) -> HealthState {
        fn rank(s: HealthState) -> u8 {
            match s {
                HealthState::NotConfigured => 0,
                HealthState::Healthy => 1,
                HealthState::Degraded => 2,
                HealthState::Unhealthy => 3,
            }
        }
        if rank(a) >= rank(b) { a } else { b }
    }
}

/// Point-in-time health snapshot with a human-readable reason and timestamp.
///
/// `#[non_exhaustive]`: build one with [`healthy`](Self::healthy) and its
/// siblings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct HealthStatus {
    /// The coarse health classification.
    pub state: HealthState,
    /// Short user-facing reason. Empty when `Healthy`.
    pub reason: String,
    /// Wall-clock time of the last state transition.
    pub since: SystemTime,
}

impl HealthStatus {
    /// `Healthy` with an empty reason, stamped now.
    pub fn healthy() -> Self {
        Self {
            state: HealthState::Healthy,
            reason: String::new(),
            since: SystemTime::now(),
        }
    }

    /// `NotConfigured` (component disabled in this deployment), stamped now.
    pub fn not_configured() -> Self {
        Self {
            state: HealthState::NotConfigured,
            reason: String::new(),
            since: SystemTime::now(),
        }
    }

    /// `Degraded` with a user-facing reason, stamped now.
    pub fn degraded(reason: impl Into<String>) -> Self {
        Self {
            state: HealthState::Degraded,
            reason: reason.into(),
            since: SystemTime::now(),
        }
    }

    /// `Unhealthy` with a user-facing reason, stamped now.
    pub fn unhealthy(reason: impl Into<String>) -> Self {
        Self {
            state: HealthState::Unhealthy,
            reason: reason.into(),
            since: SystemTime::now(),
        }
    }
}

// ── Latency histogram ─────────────────────────────────────────

/// Summary statistics for a rolling latency window (all values in ms), as
/// component snapshots carry them over the worker heartbeat.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LatencyStats {
    /// 50th percentile (median) latency, ms.
    pub p50: u64,
    /// 90th percentile latency, ms.
    pub p90: u64,
    /// 99th percentile latency, ms.
    pub p99: u64,
    /// Arithmetic mean latency, ms.
    pub mean: u64,
    /// Fastest sample in the window, ms.
    pub min: u64,
    /// Slowest sample in the window, ms.
    pub max: u64,
}

// ── Telemetry ─────────────────────────────────────────────────

/// Rolling telemetry snapshot for a component.
///
/// `#[non_exhaustive]`: start from [`Default`] and set the counters.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComponentTelemetry {
    /// Successful operations since startup.
    pub processed_total: u64,
    /// Failed operations since startup (includes timeouts).
    pub errors_total: u64,
    /// Timed-out operations since startup.
    pub timeouts_total: u64,
    /// Rolling latency window summary.
    pub latency_ms: LatencyStats,
    /// Message of the most recent error, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Provider-specific extras (e.g. tokens/s for LLMs, chars/s for TTS).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

// ── Error policy ──────────────────────────────────────────────

/// How the orchestrator should handle failures from this component.
///
/// The default is [`ErrorPolicy::Propagate`] which preserves the existing
/// behaviour of surfacing the error to the client.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum ErrorPolicy {
    /// Bubble the error to the client — current behaviour everywhere.
    #[default]
    Propagate,
    /// Retry the same component up to `max` times with exponential backoff.
    ///
    /// For TTS and LLM providers the framework gates retries on no output
    /// having been sent to the client yet, so the user never hears
    /// duplicated audio or sees duplicated text.
    RetryWithBackoff {
        /// Maximum retry attempts before the error propagates.
        max: u8,
    },
    /// Swallow the error: log it and continue with a default / empty result.
    LogAndContinue,
}

// ── Component trait ───────────────────────────────────────────

/// Common interface every pipeline stage can implement to participate in
/// health dashboards, telemetry rollups, and automatic degrade detection.
///
/// All methods have cheap, lock-free or briefly-locked implementations when
/// backed by [`ComponentCore`]; they are called from HTTP handlers and the
/// periodic auto-degrade task.
pub trait Component: Send + Sync {
    /// Stable unique identifier, e.g. `"tts:piper"` or `"stt:assemblyai"`.
    fn id(&self) -> &str;

    /// Coarse classification for dashboard grouping.
    fn kind(&self) -> ComponentKind;

    /// Current health snapshot.
    fn health(&self) -> HealthStatus;

    /// Rolling telemetry snapshot.
    fn telemetry(&self) -> ComponentTelemetry;

    /// Update this component's health state.
    ///
    /// The auto-degrade task calls this periodically. Default is a no-op;
    /// components backed by [`ComponentCore`] forward to the core.
    fn set_health(&self, _status: HealthStatus) {}

    /// Error handling policy. Defaults to `Propagate`.
    fn error_policy(&self) -> ErrorPolicy {
        ErrorPolicy::Propagate
    }

    /// The parameters this component defines at run time, when they are not
    /// known from its config (a remote service that describes its own).
    /// Default: `None`, and the factory's `provider_schema` stands.
    fn dynamic_schema(&self) -> Option<Vec<crate::schema::ParamDescriptor>> {
        None
    }
}

// ── ComponentCore ─────────────────────────────────────────────

const LATENCY_WINDOW: usize = 512;

/// Reusable component state: request counters, a bounded latency ring buffer,
/// health state, and a provider-specific extras map.
///
/// All hot-path writes use `AtomicU64`; health and latency updates use
/// lightweight `parking_lot` mutexes.
pub struct ComponentCore {
    id: String,
    kind: ComponentKind,
    processed: AtomicU64,
    errors: AtomicU64,
    timeouts: AtomicU64,
    last_error: Mutex<Option<String>>,
    latencies: Mutex<LatencyWindow>,
    health: RwLock<HealthStatus>,
    extras: Mutex<serde_json::Map<String, serde_json::Value>>,
    dynamic_schema: Mutex<Option<Vec<crate::schema::ParamDescriptor>>>,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "Debug intentionally omits lock/ring internals"
)]
impl fmt::Debug for ComponentCore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ComponentCore")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("processed", &self.processed.load(Ordering::Relaxed))
            .field("errors", &self.errors.load(Ordering::Relaxed))
            .field("timeouts", &self.timeouts.load(Ordering::Relaxed))
            .finish()
    }
}

impl ComponentCore {
    /// Create a new healthy component.
    pub fn new(id: impl Into<String>, kind: ComponentKind) -> Self {
        Self {
            id: id.into(),
            kind,
            processed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            last_error: Mutex::new(None),
            latencies: Mutex::new(LatencyWindow::new(LATENCY_WINDOW)),
            health: RwLock::new(HealthStatus::healthy()),
            extras: Mutex::new(serde_json::Map::new()),
            dynamic_schema: Mutex::new(None),
        }
    }

    /// Create in `NotConfigured` state.
    pub fn new_not_configured(id: impl Into<String>, kind: ComponentKind) -> Self {
        let core = Self::new(id, kind);
        *core.health.write() = HealthStatus::not_configured();
        core
    }

    /// Stable unique identifier, e.g. `"tts:piper"`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Coarse classification for dashboard grouping.
    pub fn kind(&self) -> ComponentKind {
        self.kind
    }

    /// Record a successful operation with its measured latency.
    pub fn record_ok(&self, latency: Duration) {
        self.processed.fetch_add(1, Ordering::Relaxed);
        let ms = (latency.as_micros() / 1000) as u64;
        self.latencies.lock().push(ms);
    }

    /// Record a failed operation. Stores the last error message.
    pub fn record_error(&self, err: impl fmt::Display) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self.last_error.lock() = Some(err.to_string());
    }

    /// Record a timeout. Counts as both an error and a timeout.
    pub fn record_timeout(&self) {
        self.timeouts.fetch_add(1, Ordering::Relaxed);
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self.last_error.lock() = Some("timeout".to_string());
    }

    /// Transition to a new health state. No-op when state and reason are
    /// unchanged (preserves the original `since` timestamp).
    pub fn set_health(&self, status: HealthStatus) {
        let mut h = self.health.write();
        if h.state == status.state && h.reason == status.reason {
            return;
        }
        *h = status;
    }

    /// Shorthand for `set_health(HealthStatus::healthy())`.
    pub fn mark_healthy(&self) {
        self.set_health(HealthStatus::healthy());
    }

    /// Shorthand for `set_health(HealthStatus::degraded(reason))`.
    pub fn mark_degraded(&self, reason: impl Into<String>) {
        self.set_health(HealthStatus::degraded(reason));
    }

    /// Shorthand for `set_health(HealthStatus::unhealthy(reason))`.
    pub fn mark_unhealthy(&self, reason: impl Into<String>) {
        self.set_health(HealthStatus::unhealthy(reason));
    }

    /// Current health snapshot.
    pub fn health(&self) -> HealthStatus {
        self.health.read().clone()
    }

    /// Rolling telemetry snapshot (counters + latency summary + extras).
    pub fn telemetry(&self) -> ComponentTelemetry {
        ComponentTelemetry {
            processed_total: self.processed.load(Ordering::Relaxed),
            errors_total: self.errors.load(Ordering::Relaxed),
            timeouts_total: self.timeouts.load(Ordering::Relaxed),
            latency_ms: self.latencies.lock().summary(),
            last_error: self.last_error.lock().clone(),
            extras: self.extras.lock().clone(),
        }
    }

    /// Record the parameters this component defines at run time (a remote
    /// service describing its own); [`CoreComponent`] reports them as its
    /// [`Component::dynamic_schema`].
    pub fn set_dynamic_schema(&self, schema: Vec<crate::schema::ParamDescriptor>) {
        *self.dynamic_schema.lock() = Some(schema);
    }

    /// The run-time parameters recorded with
    /// [`set_dynamic_schema`](Self::set_dynamic_schema), if any.
    pub fn dynamic_schema(&self) -> Option<Vec<crate::schema::ParamDescriptor>> {
        self.dynamic_schema.lock().clone()
    }

    /// Set a single provider-specific telemetry field. Cheap per-request.
    pub fn set_extra(&self, key: impl Into<String>, value: serde_json::Value) {
        self.extras.lock().insert(key.into(), value);
    }

    /// Update multiple extras atomically under one lock.
    pub fn extras_with<F>(&self, f: F)
    where
        F: FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    {
        f(&mut self.extras.lock());
    }
}

// ── CoreComponent ─────────────────────────────────────────────

/// Minimal [`Component`] implementation that delegates everything to an
/// [`Arc<ComponentCore>`].
///
/// The typical usage:
/// ```ignore
/// fn as_component(&self) -> Option<Arc<dyn Component>> {
///     Some(Arc::new(CoreComponent::new(self.core.clone())))
/// }
/// ```
#[derive(Debug)]
pub struct CoreComponent {
    core: Arc<ComponentCore>,
    error_policy: ErrorPolicy,
}

impl CoreComponent {
    /// Wrap a shared [`ComponentCore`] with the default `Propagate` policy.
    pub fn new(core: Arc<ComponentCore>) -> Self {
        Self {
            core,
            error_policy: ErrorPolicy::Propagate,
        }
    }

    /// Set a custom error policy (e.g. retry).
    #[must_use]
    pub fn with_error_policy(mut self, policy: ErrorPolicy) -> Self {
        self.error_policy = policy;
        self
    }
}

impl Component for CoreComponent {
    fn id(&self) -> &str {
        self.core.id()
    }

    fn kind(&self) -> ComponentKind {
        self.core.kind()
    }

    fn health(&self) -> HealthStatus {
        self.core.health()
    }

    fn telemetry(&self) -> ComponentTelemetry {
        self.core.telemetry()
    }

    fn set_health(&self, status: HealthStatus) {
        self.core.set_health(status);
    }

    fn dynamic_schema(&self) -> Option<Vec<crate::schema::ParamDescriptor>> {
        self.core.dynamic_schema()
    }

    fn error_policy(&self) -> ErrorPolicy {
        self.error_policy.clone()
    }
}

// ── LatencyWindow (private ring buffer) ───────────────────────

struct LatencyWindow {
    cap: usize,
    buf: Vec<u64>,
    next: usize,
    full: bool,
}

impl LatencyWindow {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            buf: Vec::with_capacity(cap),
            next: 0,
            full: false,
        }
    }

    fn push(&mut self, sample: u64) {
        if self.full {
            self.buf[self.next] = sample;
            self.next = (self.next + 1) % self.cap;
        } else {
            self.buf.push(sample);
            if self.buf.len() == self.cap {
                self.full = true;
                self.next = 0;
            }
        }
    }

    fn summary(&self) -> LatencyStats {
        if self.buf.is_empty() {
            return LatencyStats::default();
        }
        let mut sorted = self.buf.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let pick = |q: f64| -> u64 {
            let idx = ((q * n as f64).ceil() as usize)
                .saturating_sub(1)
                .min(n - 1);
            sorted[idx]
        };
        let sum: u128 = sorted.iter().map(|&v| u128::from(v)).sum();
        let mean = (sum / n as u128) as u64;
        LatencyStats {
            p50: pick(0.50),
            p90: pick(0.90),
            p99: pick(0.99),
            mean,
            min: sorted[0],
            max: sorted[n - 1],
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/component.rs"]
mod tests;

// ── Model identity and device ─────────────────────────────────

/// Identity of the model actually serving a provider, resolved once at session
/// setup and recorded on `NegotiatedConfig` + `metadata.json` so the session
/// report shows the real model/version — even for a provider running in a
/// separate process (an OpenAI-compatible TTS/LLM sidecar, etc.).
///
/// Providers override [`crate::stt::SttProvider::model_identity`] /
/// [`crate::tts::TtsProvider::model_identity`] / [`crate::text::TextProcessor::model_identity`] when they
/// can resolve more than the local config (e.g. probing `/v1/models`); when they
/// return `None`, session setup falls back to the configured model name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ModelIdentity {
    /// Stable model id, e.g. `"gpt-4o-mini"`, `"VibeVoice-Realtime-0.5B"`,
    /// `"parakeet-tdt-0.6b"`.
    pub id: String,
    /// Optional version / runtime descriptor (build, revision, or
    /// dtype/EP/steps for local models). `None` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// How the identity was sourced: `"endpoint"` (queried the running process,
    /// e.g. `/v1/models`), `"config"` (local configured name), or `"runtime"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl ModelIdentity {
    /// Build an identity from a configured model name (`source = "config"`).
    pub fn from_config(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: None,
            source: Some("config".into()),
        }
    }

    with_setters! {
        with_version => version: Option<String>,
        with_source => source: Option<String>,
    }
}

/// Compute device a provider actually ran on, resolved once at session setup
/// and recorded on `metadata.json` so the session diagnostic can show whether a
/// component used a GPU — even when it runs in a separate sidecar process.
///
/// Providers override [`crate::stt::SttProvider::device_info`] /
/// [`crate::tts::TtsProvider::device_info`] / [`crate::text::TextProcessor::device_info`] when they can
/// determine the device (e.g. the worker picks the ONNX execution provider
/// itself, or a sidecar exposes an offload count). Returning `None` means "not
/// reported" — the diagnostic shows no device badge rather than guessing.
///
/// `kind` deliberately carries `"unknown"` as a first-class value (not an
/// error): an opaque sidecar that won't reveal its device is honestly
/// `unknown`, which is more truthful than echoing a *requested* GPU setting as
/// if it were confirmed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct DeviceInfo {
    /// Compute device: `"cuda"`, `"cpu"`, `"metal"`, or `"unknown"`.
    pub kind: String,
    /// Human-readable detail, e.g. `"28/28 layers"`, `"VP_ORT_EP=cuda"`,
    /// `"by design"`. `None` when there's nothing useful to add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// How the device was determined: `"config"` (the worker decided it
    /// in-process and is authoritative), `"probe:/props"` (read from a running
    /// sidecar), `"env-requested"` (configured but unconfirmed — a request, not
    /// a confirmation), `"static"` (true by construction), or `"unknown"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl DeviceInfo {
    /// A device the worker decided in-process (authoritative). `source = "config"`.
    pub fn config(kind: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            kind: kind.into(),
            detail,
            source: Some("config".into()),
        }
    }

    /// A device that's true by construction (e.g. a CPU-only sidecar).
    /// `source = "static"`.
    pub fn static_(kind: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            detail: Some(detail.into()),
            source: Some("static".into()),
        }
    }

    /// A device the operator *requested* but that we could not confirm — e.g. a
    /// llama-server sidecar whose actual offload we can't read over HTTP (its
    /// `/props` is device-blind). `source = "env-requested"`; the UI renders
    /// this as an explicitly unconfirmed badge (amber `cuda?`) so a request is
    /// never mistaken for a confirmation.
    pub fn requested(kind: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            detail: Some(detail.into()),
            source: Some("env-requested".into()),
        }
    }
}
