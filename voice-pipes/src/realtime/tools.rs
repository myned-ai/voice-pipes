//! Tool / function-call dispatch for realtime S2S sessions.
//!
//! Mirrors the [`SttFactory`](crate::stt::SttFactory) registry shape:
//! each tool exposes a small unit struct implementing [`ToolHandler`]
//! and the runner collects them into a [`ToolRegistry`] which is
//! consulted on every [`RealtimeEvent::FunctionCall`](super::event::RealtimeEvent::FunctionCall).
//!
//! ## Dispatch
//!
//! Dispatch is **non-blocking**: the runner spawns each invocation and
//! keeps processing provider/client events while the tool runs, so a
//! slow tool does not freeze audio forwarding or barge-in. Handlers
//! are `async`; synchronous I/O inside a handler should be wrapped in
//! `tokio::task::spawn_blocking` by the handler itself.
//!
//! Long-running tools return [`ToolOutcome::Pending`] with an `ack`
//! payload: the ack goes back to the model immediately (so it can
//! speak a filler in its own voice) and the real result is delivered
//! later via [`AgentEvent::CompleteTool`](crate::agent::AgentEvent) on
//! the [`AgentSessionHandle`](crate::agent::AgentSessionHandle) the
//! registry factory receives.
//!
//! To expose tools, register a factory once at startup (same idiom as
//! [`register_stt_factory`](crate::stt::register_stt_factory)):
//!
//! ```ignore
//! voice_pipes::realtime::register_tool_registry_factory(|handle| {
//!     let mut reg = ToolRegistry::new();
//!     reg.register(Arc::new(MyTool::new(handle)));
//!     reg
//! });
//! ```
//!
//! Tool definitions are surfaced to the model via
//! [`ToolRegistry::tool_specs`], which the realtime runner stamps into
//! [`SessionConfig.tools`](super::SessionConfig::tools).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use crate::agent::AgentSessionHandle;

use super::ToolSpec;

/// Error returned by a tool invocation. The realtime runner forwards
/// the message back to the model as the function-output payload so
/// the model can decide whether to retry or surface the error.
#[derive(Debug, Clone)]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

/// Result of a tool invocation.
#[derive(Debug, Clone)]
pub enum ToolOutcome {
    /// Result available now — forwarded as the function output, then
    /// the runner kicks a `response.create` so the model speaks it.
    Done(String),
    /// Long-running: `ack` is returned to the model immediately (so it
    /// can speak a filler); the real result arrives later on the agent
    /// channel as [`AgentEvent::CompleteTool`](crate::agent::AgentEvent).
    /// Providers accept only one output per `call_id`, so the late
    /// result is injected as a context message, never a second output.
    Pending { ack: String },
}

/// A locally-dispatched tool exposed to the realtime model.
///
/// Invocations are spawned off the runner's select loop, so a slow
/// handler never blocks audio forwarding or barge-in. Handlers doing
/// synchronous I/O (file reads, blocking DB calls) should wrap it in
/// `tokio::task::spawn_blocking` themselves.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Stable function name the model invokes.
    fn name(&self) -> &str;
    /// Human-readable description shown to the model.
    fn description(&self) -> &str;
    /// JSON Schema for the function's parameters.
    fn parameters_schema(&self) -> serde_json::Value;
    /// Execute the tool. `arguments` is the raw JSON string emitted by
    /// the model. Returns the outcome sent back as the function
    /// output, or an error message forwarded verbatim.
    async fn invoke(&self, arguments: &str) -> Result<ToolOutcome, ToolError>;
}

/// Collection of [`ToolHandler`]s queried by the realtime runner on
/// every [`RealtimeEvent::FunctionCall`](super::event::RealtimeEvent::FunctionCall).
#[derive(Default, Clone)]
pub struct ToolRegistry {
    handlers: Vec<Arc<dyn ToolHandler>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field(
                "handlers",
                &self.handlers.iter().map(|h| h.name()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ToolRegistry {
    /// Empty registry. Equivalent to [`Default::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a handler. Last-registered wins on name collision so
    /// callers can override built-ins from operator code.
    pub fn register(&mut self, handler: Arc<dyn ToolHandler>) {
        self.handlers.push(handler);
    }

    /// Look up a handler by name. Returns the *last* matching handler
    /// to honour the override semantics documented on [`register`].
    pub fn lookup(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        self.handlers
            .iter()
            .rev()
            .find(|h| h.name() == name)
            .cloned()
    }

    /// Materialise the [`ToolSpec`] list to hand to the provider via
    /// [`SessionConfig.tools`](super::SessionConfig::tools).
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.handlers
            .iter()
            .map(|h| ToolSpec::new(h.name(), h.description(), h.parameters_schema()))
            .collect()
    }

    /// `true` when no handlers are registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

/// Default registry for compiled-in tools. Empty -- operators expose
/// tools via [`register_tool_registry_factory`].
pub fn default_tool_registry() -> ToolRegistry {
    ToolRegistry::new()
}

// ── External factory registry ─────────────────────────────────

/// Constructor for a per-session [`ToolRegistry`]. Receives the
/// session's [`AgentSessionHandle`] so tool handlers can push
/// [`AgentEvent`](crate::agent::AgentEvent)s (late tool results,
/// out-of-turn speech, rich messages) back into the running session.
pub type ToolRegistryFactory = fn(AgentSessionHandle) -> ToolRegistry;

static TOOL_REGISTRY_FACTORY: OnceLock<parking_lot::Mutex<Option<ToolRegistryFactory>>> =
    OnceLock::new();

/// Register the tool-registry factory before calling `voice_pipes::run()`.
/// Called once per session at connect time. Last registration wins.
///
/// Thread-safe. Same idiom as
/// [`register_stt_factory`](crate::stt::register_stt_factory).
pub fn register_tool_registry_factory(factory: ToolRegistryFactory) {
    *TOOL_REGISTRY_FACTORY
        .get_or_init(|| parking_lot::Mutex::new(None))
        .lock() = Some(factory);
}

/// Build the per-session registry: the registered factory if any,
/// otherwise [`default_tool_registry`] (empty).
pub fn build_tool_registry(handle: AgentSessionHandle) -> ToolRegistry {
    let factory = *TOOL_REGISTRY_FACTORY
        .get_or_init(|| parking_lot::Mutex::new(None))
        .lock();
    match factory {
        Some(f) => f(handle),
        None => default_tool_registry(),
    }
}

#[cfg(test)]
#[path = "tests/tools.rs"]
mod tests;
