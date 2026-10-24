//! Shared state for all transport sessions.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::api::types::{SessionRecord, TurnRecord};
use crate::config::PipelineConfig;
use crate::pipeline::AudioPostProcessor;
use crate::pipeline::events::PipelineEvent;
use crate::stt::SttProvider;
use crate::text::TextProcessor;
use crate::transport::session_store::SessionStore;
use crate::tts::TtsProvider;

/// Shared state for all transport sessions.
///
/// Built once by the server and shared across all accepted connections.
pub struct TransportState {
    pub config: Arc<PipelineConfig>,
    /// Local STT provider. `None` when the session is driven by a
    /// `[realtime_provider]` config (see `crate::realtime`), in which
    /// case the realtime runner owns the STT half of the round-trip.
    pub stt: Option<Arc<dyn SttProvider>>,
    /// Local TTS provider. `None` for realtime sessions (see `stt`).
    pub tts: Option<Arc<dyn TtsProvider>>,
    pub processors: Arc<Vec<Box<dyn TextProcessor>>>,
    /// Post-TTS audio processors.  Run on every audio chunk between TTS
    /// output and the WebSocket transport.  Empty by default.
    pub audio_post_processors: Arc<Vec<Box<dyn AudioPostProcessor>>>,
    /// Annotate-mode post-processors: one
    /// per-session [`AnnotatorHubs`](crate::pipeline::AnnotatorHubs) is
    /// spawned from these at session start. Empty by default.
    pub annotators: Arc<Vec<crate::pipeline::AnnotatorSpec>>,
    /// Current number of live sessions, maintained by an RAII guard in
    /// `run_session` so it is decremented on every exit path including the
    /// early-error ones.
    ///
    /// Shared with [`WorkerState::live_sessions`](crate::api::types::WorkerState)
    /// so the orchestrator heartbeat reports the same number `/health` does.
    /// The count cannot be derived from the worker's turn-history store,
    /// which only ever holds completed turns and so would always yield 0.
    pub active_sessions: Arc<AtomicUsize>,
    /// Maximum concurrent sessions.
    pub max_sessions: usize,
    /// Set to `true` once all models are loaded and the server is ready to
    /// accept work.  The `/readyz` probe checks this.
    pub ready: AtomicBool,
    /// Cancelled when the worker is shutting down: every session ends
    /// with a `Closing` frame, and `/readyz` turns away new work.
    pub shutdown: tokio_util::sync::CancellationToken,
    /// Optional channel for reporting completed turns to the orchestrator.
    /// Bounded: a stalled orchestrator forwarder drops records with a
    /// warning rather than growing memory without bound.
    pub turn_complete_tx: Option<mpsc::Sender<TurnRecord>>,
    /// Optional channel for reporting session lifecycle events (start/end).
    /// Bounded; see `turn_complete_tx`.
    pub session_event_tx: Option<mpsc::Sender<SessionRecord>>,
    /// Transport session store used for Configure/Resume handshakes.
    pub session_store: Arc<SessionStore>,
    /// Every session's pipeline events, for something observing the worker as
    /// a whole (agentic integration, tests). Subscribers call `.subscribe()`.
    /// Zero-cost when nobody is listening.
    ///
    /// Observation only. Sessions do not publish here directly and nothing may
    /// *record* from it: events carry no session id, so a capture written from
    /// this channel holds every session on the worker. Each session publishes
    /// to its own channel, which relays a copy here (see `session_events`).
    pub pipeline_event_tx: tokio::sync::broadcast::Sender<PipelineEvent>,
    /// Worker-wide parameter registry. Sessions consult it to fetch
    /// shared atomic cells for runtime-mutable parameters (e.g. VAD
    /// thresholds) so PATCH /debug/params updates take effect on the
    /// next chunk processed by every active session.
    pub param_registry: Arc<crate::api::param_store::ParamRegistry>,
    /// Worker-wide component registry. Shared with the worker debug API so
    /// the transport `/health` endpoint can report per-component health
    /// (e.g. a TTS backend flipped Unhealthy by a failed warm-up) on the
    /// same port the client connects to.
    pub component_registry: Arc<crate::component::ComponentRegistry>,
    /// Worker-declared client-side tools (inline `[[client_tool]]` merged
    /// with `client_tools_path` catalogs — resolved once at boot via
    /// [`crate::config::load_client_tools`]). Sessions merge per-session
    /// `Configure.options.client_tools` on top of this list.
    pub client_tools: Arc<Vec<crate::agent::client_tools::ClientToolConfig>>,
}

use tokio::sync::mpsc;

impl std::fmt::Debug for TransportState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportState")
            .field("max_sessions", &self.max_sessions)
            .field(
                "active_sessions",
                &self.active_sessions.load(Ordering::Relaxed),
            )
            .field("ready", &self.ready.load(Ordering::Relaxed))
            .field("shutting_down", &self.shutdown.is_cancelled())
            .finish_non_exhaustive()
    }
}
