//! Agent integration: the entry points for an agentic system to drive
//! a live session.
//!
//! - [`AgentEvent`] / [`AgentSessionHandle`] — the in-process channel a
//!   session runner exposes; events are applied on the runner's select
//!   loop, preserving the single-writer turn-state rule.
//! - [`link`] — the transport-agnostic wire vocabulary + [`AgentLink`]
//!   trait for remote agents (gRPC today, WebSocket-ready by design).
//! - [`gateway`] — the `TextProcessor` that fronts a remote agent over
//!   an [`AgentLink`].
//! - [`grpc_link`] — the tonic implementation of [`AgentLink`].
//!
//! [`AgentLink`]: link::AgentLink

pub mod client_tools;
pub mod gateway;
pub mod link;

#[cfg(feature = "text-grpc")]
pub mod grpc_link;

#[cfg(feature = "realtime")]
pub mod tool_backend;

use tokio::sync::mpsc;

#[cfg(feature = "realtime")]
use crate::realtime::{ToolError, ToolOutcome};
use crate::transport::PipelineMessage;

/// When agent-initiated output is delivered relative to an in-flight
/// response. Vocabulary follows Gemini Live's async function calling
/// (`INTERRUPT` / `WHEN_IDLE` / `SILENT`) — the industry precedent for
/// late tool-result delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheduling {
    /// Queue until the session is idle (no active response). Default.
    #[default]
    WhenIdle,
    /// Cancel the in-flight response and deliver now.
    Interrupt,
    /// Inject into conversation context only; the model speaks about it
    /// only if a later turn calls for it.
    Silent,
}

/// Events an agentic system (or the runner's own spawned tool
/// dispatches) can push into a live session.
#[derive(Debug)]
pub enum AgentEvent {
    /// Ask the session to speak. On the realtime path this injects a
    /// context instruction and kicks a response (the model phrases it
    /// in its own voice); the classic path synthesizes a TTS-only turn.
    Say {
        text: String,
        scheduling: Scheduling,
    },
    /// Inject `context` (a tool result, an external event) and have the
    /// model/LLM generate the spoken reply from it. On the classic path
    /// this runs the LLM chain via
    /// `TextProcessor::process_context_stream` as an agent-initiated
    /// turn; on the realtime path it injects a context message and
    /// kicks a response.
    Generate {
        context: String,
        scheduling: Scheduling,
    },
    /// Deliver the real result of a tool that earlier returned
    /// [`ToolOutcome::Pending`]. Injected as a context message (never a
    /// second function output — providers accept one output per
    /// `call_id`), then delivered per `scheduling`.
    CompleteTool {
        call_id: String,
        result: String,
        scheduling: Scheduling,
    },
    /// Send the client a textual / rich / structured message with no
    /// turn, no TTS, and no state change. `record_in_history` also
    /// injects it into the model's context (Silent-style) so the agent
    /// can refer to it later.
    Message {
        message: PipelineMessage,
        record_in_history: bool,
    },
    /// Cancel the in-flight response, if any.
    Cancel,
    /// The driving LLM (cascade path) recognized a **client-side tool**
    /// call. The runner validates the name
    /// against the session's client-tool allowlist, registers the call
    /// in its bounded in-flight map, and routes the command to the
    /// client as a `trigger_action` message — with no turn, no TTS, and
    /// no spoken filler. Realtime sessions handle client tools inline
    /// on the provider `FunctionCall` path instead, so this event is
    /// dropped there.
    ClientToolCall {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// Runner-internal: completion of a spawned [`ToolHandler`]
    /// invocation. Sent by the runner's own dispatch task — agentic
    /// systems should never send this.
    ///
    /// [`ToolHandler`]: crate::realtime::ToolHandler
    #[cfg(feature = "realtime")]
    ToolDispatchDone {
        call_id: String,
        name: String,
        outcome: Result<ToolOutcome, ToolError>,
    },
}

/// Most agent events a runner holds back while a turn or response is
/// active. Each queued event opens a spoken turn once the session is
/// idle, so a deeper queue would only keep the user waiting longer.
pub const MAX_PENDING_AGENT_EVENTS: usize = 16;

/// Queue an agent event behind the active turn. Past the cap the
/// oldest queued event is dropped with a warning; `front` places the
/// event at the head (an interrupt that re-queues as the next thing to
/// say).
pub fn queue_pending(
    queue: &mut std::collections::VecDeque<AgentEvent>,
    event: AgentEvent,
    front: bool,
    session_id: &str,
) {
    if queue.len() >= MAX_PENDING_AGENT_EVENTS {
        let dropped = queue.pop_front();
        tracing::warn!(
            session_id = %session_id,
            cap = MAX_PENDING_AGENT_EVENTS,
            dropped = ?dropped.as_ref().map(std::mem::discriminant),
            "pending agent event queue full; oldest event dropped"
        );
    }
    if front {
        queue.push_front(event);
    } else {
        queue.push_back(event);
    }
}

/// Something that can take over re-engaging a user who has gone quiet: a
/// processor whose agent phrases the prompt itself, registered on the
/// session's [`AgentSessionHandle`] when the processor is bound.
#[async_trait::async_trait]
pub trait UserIdleHandler: Send + Sync {
    /// The user has been silent past the idle timeout. `true` when this
    /// handler's agent will re-engage in its own words; `false` leaves it
    /// to the next handler, then to the configured static message.
    async fn on_user_idle(&self, idle_ms: u64, prompt_count: u32) -> bool;
}

type IdleHandlers = std::sync::Arc<parking_lot::Mutex<Vec<std::sync::Arc<dyn UserIdleHandler>>>>;

/// Cloneable sender half handed to the agentic system.
#[derive(Clone)]
pub struct AgentSessionHandle {
    tx: mpsc::Sender<AgentEvent>,
    session_id: std::sync::Arc<str>,
    idle_handlers: IdleHandlers,
}

impl std::fmt::Debug for AgentSessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSessionHandle")
            .field("session_id", &self.session_id)
            .field("idle_handlers", &self.idle_handlers.lock().len())
            .finish_non_exhaustive()
    }
}

impl AgentSessionHandle {
    pub fn new(tx: mpsc::Sender<AgentEvent>) -> Self {
        Self {
            tx,
            session_id: std::sync::Arc::from(""),
            idle_handlers: IdleHandlers::default(),
        }
    }

    /// Offer to re-engage the user when they go quiet. Handlers are asked
    /// in the order they registered, which is the processor order.
    pub fn register_idle_handler(&self, handler: std::sync::Arc<dyn UserIdleHandler>) {
        self.idle_handlers.lock().push(handler);
    }

    /// Ask the registered handlers, in order, to re-engage an idle user;
    /// `true` once one takes it on.
    pub async fn offer_user_idle(&self, idle_ms: u64, prompt_count: u32) -> bool {
        let handlers = self.idle_handlers.lock().clone();
        for h in handlers {
            if h.on_user_idle(idle_ms, prompt_count).await {
                return true;
            }
        }
        false
    }

    /// Offer the idle prompt to the registered handlers on a task of its
    /// own, and fall back to `fallback` (when not empty) if none takes it
    /// on within `within`. A handler waits on its agent, which may never
    /// answer; the session loop must keep carrying audio and barge-ins
    /// meanwhile, so it never awaits this. `stalled` is the session's
    /// once-only flag for logging a handler that ran out the bound.
    pub(crate) fn spawn_idle_offer(
        &self,
        idle_ms: u64,
        prompt_count: u32,
        within: std::time::Duration,
        fallback: String,
        stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        let handle = self.clone();
        tokio::spawn(async move {
            let offer = tokio::time::timeout(within, handle.offer_user_idle(idle_ms, prompt_count));
            let handled = if let Ok(handled) = offer.await {
                handled
            } else {
                if !stalled.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::warn!(
                        session_id = %handle.session_id,
                        within_ms = within.as_millis() as u64,
                        "idle handler did not answer in time; using the configured message"
                    );
                }
                false
            };
            if !handled && !fallback.is_empty() {
                let _ = handle.tx.try_send(AgentEvent::Say {
                    text: fallback,
                    scheduling: Scheduling::WhenIdle,
                });
            }
        })
    }

    /// The session as a text processor sees it. What any processor reports
    /// (a client-tool call) arrives on this handle's channel. The engine
    /// handle rides along wrapped in a type private to this module, so only
    /// the engine's own session-driving processors (the agent gateway) can
    /// read it back, with [`from_session`](Self::from_session): a plugin
    /// that links this crate still gets only the SDK handle's powers, not
    /// `Say`, `Cancel` or idle-handler registration.
    pub fn session_handle(&self) -> voice_pipes_sdk::agent::SessionHandle {
        voice_pipes_sdk::agent::SessionHandle::new(
            std::sync::Arc::new(AgentChannelSink(self.clone())),
            &self.session_id,
        )
        .with_host_data(std::sync::Arc::new(EngineSession(self.clone())))
    }

    /// The engine handle behind a processor's session handle, when the
    /// engine built it. Crate-private, like the type it is stored as.
    pub(crate) fn from_session(session: &voice_pipes_sdk::agent::SessionHandle) -> Option<Self> {
        session
            .host_data::<EngineSession>()
            .map(|EngineSession(handle)| handle.clone())
    }

    /// Attach the canonical session id (used by remote agent links for
    /// their `SessionStart` handshake and reconnect correlation).
    #[must_use]
    pub fn with_session_id(mut self, session_id: &str) -> Self {
        self.session_id = std::sync::Arc::from(session_id);
        self
    }

    /// The canonical session id, or "" when not attached.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Push an event into the session. Fails only when the session has
    /// ended (receiver dropped); pending events are then meaningless,
    /// so callers may ignore the error.
    pub async fn send(&self, event: AgentEvent) -> Result<(), AgentSendError> {
        self.tx
            .send(event)
            .await
            .map_err(|_undelivered_event| AgentSendError)
    }
}

/// The engine handle as a processor's session carries it. Private, so the
/// host data is opaque to everything outside this module and
/// [`AgentSessionHandle::from_session`].
struct EngineSession(AgentSessionHandle);

/// Delivers what a processor reports through its SDK session handle onto
/// the session's agent channel.
#[derive(Debug)]
struct AgentChannelSink(AgentSessionHandle);

#[async_trait::async_trait]
impl voice_pipes_sdk::agent::SessionSink for AgentChannelSink {
    async fn client_tool_call(
        &self,
        call: voice_pipes_sdk::agent::ClientToolCall,
    ) -> Result<(), voice_pipes_sdk::agent::SessionEnded> {
        self.0
            .send(AgentEvent::ClientToolCall {
                call_id: call.call_id,
                name: call.name,
                arguments: call.arguments,
            })
            .await
            .map_err(|_ended: AgentSendError| voice_pipes_sdk::agent::SessionEnded)
    }
}

/// The session ended; the event was not delivered.
#[derive(Debug, Clone, Copy)]
pub struct AgentSendError;

impl std::fmt::Display for AgentSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session ended; agent event not delivered")
    }
}

impl std::error::Error for AgentSendError {}
