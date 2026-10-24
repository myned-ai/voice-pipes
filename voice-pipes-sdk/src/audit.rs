//! Audit trace event — emitted by providers for observability.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// An observability event emitted by a pipeline component.
///
/// A factory finds the sender in [`BuildContext::audit_tx`](crate::BuildContext)
/// when the host records audit events, and `None` when it does not; sending
/// is optional either way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// When the event occurred.
    pub timestamp: SystemTime,
    /// Which component emitted it (e.g. `"stt:my-nemo-ctc"`, `"llm:llama3"`).
    pub component: String,
    /// Event type tag (e.g. `"transcription"`, `"tool_call"`).
    pub event_type: String,
    /// Arbitrary JSON payload.
    pub payload: serde_json::Value,
}

impl TraceEvent {
    /// Create a new event timestamped to now.
    pub fn new(
        component: impl Into<String>,
        event_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            timestamp: SystemTime::now(),
            component: component.into(),
            event_type: event_type.into(),
            payload,
        }
    }
}
