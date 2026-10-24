//! What a text processor can do in a live session beyond answering a turn.
//!
//! A session hands its text processors a [`SessionHandle`] when it is built
//! ([`TextProcessor::bind_session`](crate::text::TextProcessor::bind_session))
//! and the client-side tools the session allows
//! ([`TextProcessor::configure_client_tools`](crate::text::TextProcessor::configure_client_tools)).
//! A function-calling processor advertises those tools to its model and,
//! when the model calls one, reports it through the handle; the framework
//! checks the name against the session's allowlist and sends the command to
//! the client. The processor never executes a tool itself.
//!
//! **Speculative runs.** With speculative execution on, the framework may
//! run the processor on a partial transcript that it later discards. A tool
//! call reported during such a run reaches the framework like any other, so
//! a processor should report a call only once the model has committed to it
//! (the end of the tool-call message), never from a partial one.
//!
//! The handle is implemented by the framework through [`SessionSink`]; a
//! plugin only receives one, and a test can build one around its own sink.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;

// ── Client-side tools ─────────────────────────────────────────

/// A client-side tool as the model should see it: a command the model may
/// invoke that the client fulfils. What happens with the client's result is
/// the framework's business, not the processor's.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ClientToolSpec {
    /// Stable function name the model invokes.
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema for the tool's parameters (an object schema).
    pub parameters: serde_json::Value,
}

impl ClientToolSpec {
    /// A tool the model can call by `name`, described by `description`, with
    /// arguments matching the JSON Schema `parameters`.
    pub fn new(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A client-side tool call the model made, as the processor parsed it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ClientToolCall {
    /// The model's id for the call, echoed back with its result.
    pub call_id: String,
    /// The tool's name; the framework drops a call whose name is not on the
    /// session's allowlist.
    pub name: String,
    /// The arguments, as the model sent them.
    pub arguments: serde_json::Value,
}

impl ClientToolCall {
    /// The call `call_id` to the tool `name` with `arguments`.
    pub fn new(call_id: &str, name: &str, arguments: serde_json::Value) -> Self {
        Self {
            call_id: call_id.into(),
            name: name.into(),
            arguments,
        }
    }
}

// ── Session handle ────────────────────────────────────────────

/// The session ended; what was sent was not delivered. Pending output is
/// meaningless by then, so a caller may ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEnded;

impl std::fmt::Display for SessionEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session ended; not delivered")
    }
}

impl std::error::Error for SessionEnded {}

/// Where a [`SessionHandle`] delivers what a processor reports. Implemented
/// by the framework for each session.
#[async_trait]
pub trait SessionSink: Send + Sync + std::fmt::Debug {
    /// Deliver a client-side tool call to the session.
    async fn client_tool_call(&self, call: ClientToolCall) -> Result<(), SessionEnded>;
}

/// A live session, as a text processor sees it. Cheap to clone; a clone
/// reaches the same session.
#[derive(Clone, Debug)]
pub struct SessionHandle {
    sink: Arc<dyn SessionSink>,
    session_id: Arc<str>,
    host: Option<Arc<dyn Any + Send + Sync>>,
}

impl SessionHandle {
    /// A handle delivering to `sink`, for the session `session_id` (empty
    /// when the session has no id yet).
    pub fn new(sink: Arc<dyn SessionSink>, session_id: &str) -> Self {
        Self {
            sink,
            session_id: Arc::from(session_id),
            host: None,
        }
    }

    /// Attach data the host keeps for its own built-in processors, beside
    /// what every processor gets. Every processor's handle carries it, so a
    /// host stores it as a type private to itself: only the host can then
    /// read it back with [`host_data`](Self::host_data), and a plugin gets
    /// nothing from it even when it links the host crate.
    #[must_use]
    pub fn with_host_data(mut self, data: Arc<dyn Any + Send + Sync>) -> Self {
        self.host = Some(data);
        self
    }

    /// The host's attachment, when there is one and it is a `T`.
    pub fn host_data<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.host.as_deref()?.downcast_ref::<T>()
    }

    /// The session's id, or `""` when it has none.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Report a client-side tool call the model made.
    pub async fn client_tool_call(&self, call: ClientToolCall) -> Result<(), SessionEnded> {
        self.sink.client_tool_call(call).await
    }
}

#[cfg(test)]
#[path = "tests/agent.rs"]
mod tests;
