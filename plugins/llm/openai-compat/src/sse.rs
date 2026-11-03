//! Server-Sent Events (SSE) parser for OpenAI-compatible streaming responses.
//!
//! Reads `text/event-stream` byte chunks from a `voice_pipes_net::reqwest::Response` as lines
//! ([`SseReader`]) and parses them into typed [`ChatCompletionChunk`] values.

use super::types::ChatCompletionChunk;

/// A `data:` payload that is neither a chunk nor an error. It says what
/// kind of failure and where, never what the payload held: the payload can
/// carry the reply, and serde's own message quotes the values it rejects.
#[derive(Debug)]
pub struct SseParseError {
    /// The JSON decode failure.
    error: serde_json::Error,
}

impl std::fmt::Display for SseParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SSE payload: {}",
            voice_pipes_sdk::secrets::json_error(&self.error)
        )
    }
}

// No `source`: serde's error quotes the payload, and a chain printer would
// show it.
impl std::error::Error for SseParseError {}

/// One parsed line of an event stream.
#[derive(Debug)]
pub enum SseLine {
    /// A `data:` payload that decoded as a completion chunk.
    Chunk(ChatCompletionChunk),
    /// The terminal `data: [DONE]` sentinel.
    Done,
    /// An error the server sent in place of a chunk; the turn has failed.
    Error(StreamError),
    /// An event separator, comment or metadata line.
    Skip,
}

impl SseLine {
    /// The chunk carried by a `Chunk` line.
    #[cfg(test)]
    pub fn into_chunk(self) -> Option<ChatCompletionChunk> {
        match self {
            Self::Chunk(c) => Some(c),
            Self::Done | Self::Error(_) | Self::Skip => None,
        }
    }
}

/// Parse a single line from an SSE stream.
///
/// Malformed JSON in a `data:` payload is an error; every other line
/// classifies as one of the [`SseLine`] variants.
pub fn parse_sse_line(line: &str) -> Result<SseLine, SseParseError> {
    let line = line.trim();

    // Empty lines are SSE event separators.
    if line.is_empty() {
        return Ok(SseLine::Skip);
    }

    // Lines without the `data:` prefix are comments or event metadata.
    let payload = match line.strip_prefix("data:") {
        Some(p) => p.trim(),
        None => return Ok(SseLine::Skip),
    };

    if payload == "[DONE]" {
        return Ok(SseLine::Done);
    }

    match serde_json::from_str::<ChatCompletionChunk>(payload) {
        Ok(chunk) => Ok(SseLine::Chunk(chunk)),
        Err(error) => match serde_json::from_str::<ErrorEnvelope>(payload) {
            Ok(envelope) => Ok(SseLine::Error(envelope.error)),
            Err(_) => Err(SseParseError { error }),
        },
    }
}

/// The error object OpenAI-compatible servers send in a stream:
/// `{"error": {"message": "...", "type": "..."}}`.
#[derive(Debug, serde::Deserialize)]
struct ErrorEnvelope {
    error: StreamError,
}

/// An error the server reported inside the stream.
#[derive(Debug, serde::Deserialize)]
pub struct StreamError {
    /// The server's description.
    #[serde(default)]
    pub message: String,
    /// Its classification (`server_error`, `rate_limit_exceeded`, ...),
    /// when given.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

/// The lines of an SSE byte stream, however it arrives in chunks: each
/// line whole once its newline arrives, decoded then (a newline byte never
/// falls inside a UTF-8 character, so a character split across chunks is
/// never split here), and refused past `max_line_bytes`.
pub struct SseReader {
    lines: voice_pipes_net::BoundedLines,
}

impl SseReader {
    pub fn new(max_line_bytes: usize) -> Self {
        Self {
            lines: voice_pipes_net::BoundedLines::new(max_line_bytes),
        }
    }

    /// The lines `bytes` completes, without their line ends. Invalid UTF-8
    /// in a line decodes as U+FFFD.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, voice_pipes_net::LineTooLong> {
        Ok(self
            .lines
            .push(bytes)?
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect())
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/sse.rs"]
mod tests;
