//! Transport-agnostic agent-link vocabulary.
//!
//! These enums mirror `proto/agent.proto` one-to-one — the proto file
//! is the single protocol definition, and a future WebSocket adapter
//! carries the same messages via the canonical proto3 JSON mapping.
//! The session runner and the [`gateway`](super::gateway) processor
//! talk only to the [`AgentLink`] trait, never to a concrete
//! transport (same pattern as `RealtimeProtocol`).

use async_trait::async_trait;

use super::Scheduling;

/// Protocol version stamped into `SessionStart` — bump on breaking
/// wire changes; agents reject versions they don't understand via a
/// failed handshake.
pub const AGENT_PROTOCOL_VERSION: u32 = 1;

/// Pipeline → agent frames.
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineToAgent {
    /// Handshake; always the first frame on a (re)opened link.
    SessionStart {
        protocol_version: u32,
        session_id: String,
        pipeline_name: String,
        /// True when re-opening the link for a live session after a
        /// transport drop (reconnect-and-resume).
        resumed: bool,
    },
    /// A committed user turn — generate the reply as `TurnFragment`s
    /// carrying the same `turn_id`. `is_context` marks agent
    /// `Generate` input (injected background information, not
    /// something the user said).
    UserTurn {
        turn_id: u32,
        text: String,
        is_context: bool,
    },
    /// Barge-in ground truth: the user actually heard only
    /// `played_text` of this turn's reply — truncate agent-side
    /// history accordingly.
    Interrupted { turn_id: u32, played_text: String },
    /// Non-turn history fact. `kind` is `"context"` or
    /// `"assistant_message"` (an utterance spoken outside this agent,
    /// e.g. a static idle prompt).
    ContextNote { text: String, kind: String },
    /// The user has been silent; the agent decides whether/how to
    /// re-engage (typically by replying with `Say`).
    UserIdle { idle_ms: u64, prompt_count: u32 },
    /// S2S only: the provider model called one of the agent's tools.
    ToolCall {
        call_id: String,
        name: String,
        arguments_json: String,
    },
    /// The session is over; the link closes after this frame.
    SessionEnd { reason: String },
}

/// Agent → pipeline frames.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentToPipeline {
    /// Handshake reply; must be the agent's first frame.
    SessionAck {
        protocol_version: u32,
        agent_name: String,
    },
    /// Streamed reply to a `UserTurn`; `is_end_of_response` closes
    /// the turn.
    TurnFragment {
        turn_id: u32,
        text: String,
        is_end_of_response: bool,
        first_token_ms: Option<f64>,
        token_count: Option<u32>,
    },
    /// Out-of-turn speech (spoken via TTS as an agent-initiated turn).
    Say {
        text: String,
        scheduling: Scheduling,
    },
    /// Out-of-turn client message (rich content / commands / tool
    /// transparency) — no turn, no TTS, no state change.
    Message {
        kind: String,
        text: Option<String>,
        payload_json: Option<String>,
        record_in_history: bool,
    },
    /// Cancel the in-flight turn/response, if any.
    Cancel,
    /// S2S only: reply to a `ToolCall`. `pending` means `output` is an
    /// immediate ack and the real result follows as `CompleteTool`.
    ToolResult {
        call_id: String,
        pending: bool,
        output: String,
    },
    /// S2S only: late result of a pending `ToolCall`.
    CompleteTool {
        call_id: String,
        result: String,
        scheduling: Scheduling,
    },
}

/// Longest text field accepted on one agent frame (`TurnFragment.text`,
/// `Say.text`, `CompleteTool.result`, `ToolResult.output`,
/// `Message.text` / `payload_json`). Agent text reaches synthesis and
/// the model's context verbatim, so it is bounded like client input.
pub const MAX_AGENT_TEXT_BYTES: usize = 64 * 1024;

/// `Message.kind` values the engine emits itself to drive the
/// client-tool protocol. An agent frame carrying one would bypass the
/// client-tool allowlist, in-flight cap and audit trail, so it is
/// refused; transparency kinds (`tool_call`, `card`, ...) stay open.
pub const RESERVED_MESSAGE_KINDS: &[&str] = &["trigger_action", "client_tool_result"];

/// Out-of-turn speech frames (`Say`, and `CompleteTool` on the classic
/// gateway) accepted per [`OUT_OF_TURN_WINDOW`]; a looping agent is
/// throttled instead of monopolising the session.
pub const OUT_OF_TURN_BURST: u32 = 4;
/// Window over which [`OUT_OF_TURN_BURST`] refills.
pub const OUT_OF_TURN_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Why an inbound agent frame was refused (see [`frame_violation`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameViolation {
    /// A text field exceeds [`MAX_AGENT_TEXT_BYTES`].
    Oversized { field: &'static str, bytes: usize },
    /// `Message.kind` is one of [`RESERVED_MESSAGE_KINDS`].
    ReservedKind,
}

impl std::fmt::Display for FrameViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Oversized { field, bytes } => {
                write!(f, "{field} is {bytes} bytes; limit {MAX_AGENT_TEXT_BYTES}")
            }
            Self::ReservedKind => f.write_str("message kind is reserved for the engine"),
        }
    }
}

/// Check one inbound frame against the size and kind bounds. Returns
/// the first violation; a frame with one is dropped by the pumps.
pub fn frame_violation(frame: &AgentToPipeline) -> Option<FrameViolation> {
    let over = |field: &'static str, s: &str| {
        (s.len() > MAX_AGENT_TEXT_BYTES).then_some(FrameViolation::Oversized {
            field,
            bytes: s.len(),
        })
    };
    match frame {
        AgentToPipeline::SessionAck { agent_name, .. } => over("agent_name", agent_name),
        AgentToPipeline::TurnFragment { text, .. } => over("TurnFragment.text", text),
        AgentToPipeline::Say { text, .. } => over("Say.text", text),
        AgentToPipeline::Message {
            kind,
            text,
            payload_json,
            ..
        } => {
            if RESERVED_MESSAGE_KINDS.contains(&kind.as_str()) {
                return Some(FrameViolation::ReservedKind);
            }
            over("Message.kind", kind)
                .or_else(|| text.as_deref().and_then(|t| over("Message.text", t)))
                .or_else(|| {
                    payload_json
                        .as_deref()
                        .and_then(|p| over("Message.payload_json", p))
                })
        }
        AgentToPipeline::Cancel => None,
        AgentToPipeline::ToolResult { output, .. } => over("ToolResult.output", output),
        AgentToPipeline::CompleteTool { result, .. } => over("CompleteTool.result", result),
    }
}

/// Token bucket for out-of-turn speech frames: [`OUT_OF_TURN_BURST`]
/// tokens, refilled evenly over [`OUT_OF_TURN_WINDOW`].
#[derive(Debug)]
pub struct OutOfTurnLimiter {
    tokens: f64,
    last: std::time::Instant,
}

impl Default for OutOfTurnLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl OutOfTurnLimiter {
    pub fn new() -> Self {
        Self {
            tokens: f64::from(OUT_OF_TURN_BURST),
            last: std::time::Instant::now(),
        }
    }

    /// Take one token if available.
    pub fn allow(&mut self) -> bool {
        let now = std::time::Instant::now();
        let refill_per_sec = f64::from(OUT_OF_TURN_BURST) / OUT_OF_TURN_WINDOW.as_secs_f64();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * refill_per_sec)
            .min(f64::from(OUT_OF_TURN_BURST));
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A tool the agent exposes to an S2S provider model (transport-level
/// mirror of the realtime `ToolSpec`, kept feature-independent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the parameters, as a JSON string.
    pub parameters_json_schema: String,
}

/// One live agent link (per session). Implementations own the
/// transport; the gateway/runner never see tonic or tungstenite types.
#[async_trait]
pub trait AgentLink: Send + Sync + 'static {
    /// Push one frame to the agent. Errors mean the link is down —
    /// callers surface that through the component error policy.
    async fn send(&self, event: PipelineToAgent) -> Result<(), AgentLinkError>;

    /// Receive the next frame from the agent; `None` when the link
    /// closed. Single consumer (the gateway pump).
    async fn recv(&self) -> Option<AgentToPipeline>;

    /// Tools the agent exposes to an S2S provider model. Default:
    /// none (classic-only agents don't implement it).
    async fn list_tools(&self) -> Result<Vec<LinkToolSpec>, AgentLinkError> {
        Ok(Vec::new())
    }

    /// Best-effort synchronous teardown notice (callable from `Drop`).
    fn close(&self, _reason: &str) {}
}

/// The link is down; the frame was not delivered.
#[derive(Debug)]
pub struct AgentLinkError {
    pub message: String,
}

impl AgentLinkError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AgentLinkError {}
