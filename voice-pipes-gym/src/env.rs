//! Core abstractions for native in-memory RL task environments.
//!
//! A [`TaskEnvironment`] exposes deterministic client-side tool specifications,
//! handles in-flight tool calls, maintains in-memory domain state, and evaluates
//! scenario completion without external network or database dependencies.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use voice_pipes_sdk::agent::{ClientToolCall, ClientToolSpec};

/// The outcome of evaluating an agent's actions against a scenario goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskEvaluation {
    /// Whether the core scenario goal was achieved.
    pub success: bool,
    /// Quantitative score between 0.0 (complete failure) and 1.0 (perfect task completion).
    pub score: f32,
    /// Detailed diagnostic report explaining the outcome.
    pub message: String,
}

impl TaskEvaluation {
    /// Create a successful evaluation with a perfect 1.0 score.
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            success: true,
            score: 1.0,
            message: message.into(),
        }
    }

    /// Create a failed evaluation with a 0.0 score.
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            score: 0.0,
            message: message.into(),
        }
    }

    /// Create a partial success evaluation with a custom score in [0.0, 1.0].
    pub fn partial(score: f32, message: impl Into<String>) -> Self {
        let clamped = score.clamp(0.0, 1.0);
        Self {
            success: clamped >= 0.99,
            score: clamped,
            message: message.into(),
        }
    }
}

/// A native, self-contained task environment.
pub trait TaskEnvironment: Send + Sync {
    /// Identifier for this environment (e.g. `"banking"`).
    fn name(&self) -> &'static str;

    /// The client-side tools exposed to the agent during this scenario.
    fn tools(&self) -> Vec<ClientToolSpec>;

    /// Execute a tool call against the internal in-memory state.
    ///
    /// Returns the JSON value result to feed back into the agent context,
    /// or an error message string if execution failed.
    fn execute(&mut self, call: &ClientToolCall) -> Result<Value, String>;

    /// Reset internal state to its initial baseline snapshot in `< 100µs`.
    fn reset(&mut self);

    /// Downcast helper for concrete task environment inspection.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Mutable downcast helper.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}
