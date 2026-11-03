//! `AssemblyAI` V3 Streaming API message types.
//!
//! Covers both server->client messages (`Begin`, `Turn`, `Termination`)
//! and client->server messages (`Terminate`, `ForceEndpoint`,
//! `UpdateConfiguration`).

use serde::Deserialize;

// ── Server -> Client ──────────────────────────────────────────

/// Top-level server message, dispatched by the `"type"` tag.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Session successfully established.
    Begin(BeginMessage),
    /// Speech activity detected in the audio stream.
    SpeechStarted(SpeechStartedMessage),
    /// Streaming transcription update (partial or final).
    Turn(TurnMessage),
    /// Session ended -- no further messages will follow.
    Termination(TerminationMessage),
    /// Server-reported error; usually followed by a Close frame carrying
    /// the same code.
    Error(ErrorMessage),
}

impl ServerMessage {
    /// The message's type, for logs: a `Turn` carries the user's words, so
    /// a log line names the message instead of printing it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Begin(_) => "Begin",
            Self::SpeechStarted(_) => "SpeechStarted",
            Self::Turn(_) => "Turn",
            Self::Termination(_) => "Termination",
            Self::Error(_) => "Error",
        }
    }
}

/// Sent once when the WebSocket session is established.
#[derive(Debug, Deserialize)]
pub struct BeginMessage {
    /// Unique session identifier.
    pub id: String,
    /// Unix timestamp when this session will expire.
    pub expires_at: u64,
}

/// Sent when speech activity is first detected in the audio stream.
#[derive(Debug, Deserialize)]
pub struct SpeechStartedMessage {
    /// Timestamp (ms) in the audio where speech was detected.
    pub timestamp: u64,
    /// Confidence that speech has started.
    pub confidence: f64,
}

/// A streaming transcription update for one turn.
#[derive(Debug, Deserialize)]
pub struct TurnMessage {
    /// Zero-based incrementing turn counter within the session.
    pub turn_order: u32,
    /// `true` when the model considers the speaker's turn finished.
    pub end_of_turn: bool,
    /// Accumulated transcript for the current turn.
    pub transcript: String,
    /// Model confidence (0.0-1.0) that the turn has ended.
    pub end_of_turn_confidence: f32,
    /// Per-word details (timing, confidence, finality).
    pub words: Vec<TurnWord>,
    /// Populated when an utterance boundary is detected within a turn.
    /// Empty string when no utterance boundary in this message.
    pub utterance: String,
    /// Detected language code (present when `language_detection=true`).
    #[serde(default)]
    pub language_code: Option<String>,
}

/// Per-word timing and confidence from a `Turn` message.
#[derive(Debug, Deserialize)]
pub struct TurnWord {
    /// Word start time in milliseconds from session start.
    pub start: u64,
    /// Word end time in milliseconds from session start.
    pub end: u64,
    /// The recognised word text.
    pub text: String,
    /// Word-level confidence score.
    pub confidence: f32,
    /// `true` once the word is finalised (will not change).
    pub word_is_final: bool,
}

/// Server-reported error, e.g.
/// `{"type":"Error","error_code":1008,"error":"Unauthorized Connection: Too many concurrent sessions"}`.
#[derive(Debug, Deserialize)]
pub struct ErrorMessage {
    /// Numeric error code (mirrors the subsequent WS close code when present).
    #[serde(default)]
    pub error_code: Option<u32>,
    /// Human-readable error description.
    #[serde(default)]
    pub error: String,
}

/// Sent when the session terminates (client-initiated or server timeout).
#[derive(Debug, Deserialize)]
pub struct TerminationMessage {
    /// Total seconds of audio processed in this session.
    pub audio_duration_seconds: f64,
    /// Total wall-clock seconds the session was active.
    pub session_duration_seconds: f64,
}

// ── Client -> Server ──────────────────────────────────────────

/// Request a graceful session shutdown.
pub(crate) const TERMINATE: &str = r#"{"type":"Terminate"}"#;

/// Force the current turn to end immediately.
pub(crate) const FORCE_ENDPOINT: &str = r#"{"type":"ForceEndpoint"}"#;

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/messages.rs"]
mod tests;
