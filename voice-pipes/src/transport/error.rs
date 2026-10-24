//! Transport error types.
//!
//! Provides a unified error enum for all transport-layer failures,
//! with transient vs. permanent classification for retry decisions.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Semantic numeric error codes representing the standard voice-pipes error namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    // 1000 - 1999: Transport and Connection errors
    HandshakeTimeout = 1000,
    SessionCreateFailed = 1001,
    SessionResumeFailed = 1002,
    InvalidHandshake = 1003,
    RealtimeConnectFailed = 1004,
    RealtimeConfigureFailed = 1005,
    RealtimeProviderError = 1006,
    /// A client-declared tool was refused because its name belongs to
    /// a server-side tool (warning; the session continues without it).
    ClientToolRefused = 1007,

    // 2000 - 2999: Acoustic, VAD, and STT errors
    VadInitializationFailed = 2000,
    SttWarmupFailed = 2001,

    // 3000 - 3999: LLM/Reasoning errors
    LlmRetriesExhausted = 3000,
    LlmWarmupFailed = 3001,

    // 4000 - 4999: Synthesis and TTS errors
    InjectionFailed = 4000,
    TtsSynthesisFailed = 4001,
    TtsWarmupFailed = 4002,
}

impl ErrorCode {
    /// Returns the raw u16 code representation.
    pub fn as_u16(&self) -> u16 {
        *self as u16
    }

    /// Resolves high-level error code enum from numeric value.
    pub fn from_u16(val: u16) -> Option<Self> {
        match val {
            1000 => Some(Self::HandshakeTimeout),
            1001 => Some(Self::SessionCreateFailed),
            1002 => Some(Self::SessionResumeFailed),
            1003 => Some(Self::InvalidHandshake),
            1004 => Some(Self::RealtimeConnectFailed),
            1005 => Some(Self::RealtimeConfigureFailed),
            1006 => Some(Self::RealtimeProviderError),
            1007 => Some(Self::ClientToolRefused),
            2000 => Some(Self::VadInitializationFailed),
            2001 => Some(Self::SttWarmupFailed),
            3000 => Some(Self::LlmRetriesExhausted),
            3001 => Some(Self::LlmWarmupFailed),
            4000 => Some(Self::InjectionFailed),
            4001 => Some(Self::TtsSynthesisFailed),
            4002 => Some(Self::TtsWarmupFailed),
            _ => None,
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u16(self.as_u16())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let val = u16::deserialize(deserializer)?;
        Self::from_u16(val)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown system error code: {val}")))
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u16())
    }
}

/// Errors originating from the transport layer.
#[derive(Debug, Clone)]
pub enum TransportError {
    /// Connection was closed normally by the remote side.
    ConnectionClosed,
    /// Connection was lost unexpectedly (network failure, timeout).
    ConnectionLost(String),
    /// Message could not be serialized or deserialized.
    Codec(String),
    /// Message exceeds the configured maximum size.
    MessageTooLarge {
        /// Actual size in bytes.
        size: usize,
        /// Maximum allowed size in bytes.
        max: usize,
    },
    /// Authentication failed or credentials expired.
    AuthFailed(String),
    /// The session token for reconnection is invalid or expired.
    InvalidSession(String),
    /// The server has reached its connection capacity.
    CapacityExhausted,
    /// A protocol-level violation (unexpected frame type, bad handshake).
    Protocol(String),
    /// An internal error (bug, channel closed unexpectedly).
    Internal(String),
}

impl TransportError {
    /// Whether this error is transient and the operation may be retried.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::ConnectionLost(_) | Self::CapacityExhausted)
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionClosed => write!(f, "connection closed"),
            Self::ConnectionLost(reason) => write!(f, "connection lost: {reason}"),
            Self::Codec(detail) => write!(f, "codec error: {detail}"),
            Self::MessageTooLarge { size, max } => {
                write!(f, "message too large: {size} bytes (max {max})")
            }
            Self::AuthFailed(reason) => write!(f, "authentication failed: {reason}"),
            Self::InvalidSession(reason) => write!(f, "invalid session: {reason}"),
            Self::CapacityExhausted => write!(f, "server capacity exhausted"),
            Self::Protocol(detail) => write!(f, "protocol error: {detail}"),
            Self::Internal(detail) => write!(f, "internal error: {detail}"),
        }
    }
}

impl std::error::Error for TransportError {}

// ── Error-message helpers ────────────────────────────────────

/// Heuristic: does this error string look like a failure to *establish a
/// network connection* to an external backend (refused / unreachable / connect
/// timeout / DNS), as opposed to a protocol- or application-level error?
///
/// Used to decide whether a user-facing error frame should carry an actionable
/// "is the dependency running?" hint. Intentionally substring-based and
/// platform-aware (Windows `10061` / Linux `111` for connection refused) so it
/// catches the common shapes without parsing provider-specific error types.
pub fn looks_like_connection_failure(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "connection refused",
        "actively refused",            // Windows phrasing
        "no connection could be made", // Windows phrasing
        "tcp connect error",
        "error trying to connect",
        "connection reset",
        "os error 10061", // WSAECONNREFUSED (Windows)
        "os error 111",   // ECONNREFUSED (Linux)
        "connect timeout",
        "timed out",
        "dns error",
        "failed to lookup address",
        "name or service not known",
        "could not resolve",
        "network is unreachable",
    ];
    NEEDLES.iter().any(|n| m.contains(n))
}

/// Append an actionable "is the backend running?" hint to `msg` when it looks
/// like a connection failure ([`looks_like_connection_failure`]); otherwise
/// return `msg` unchanged. `dependency` is a short human label for the missing
/// service (e.g. `"TTS"`, `"STT"`).
///
/// This turns an opaque `tcp connect error: ... actively refused` into a
/// message that tells the operator what to actually do — the most common cause
/// in local/dev setups is simply that the provider's sidecar process is not
/// running.
pub fn with_backend_hint(msg: String, dependency: &str) -> String {
    if looks_like_connection_failure(&msg) {
        format!(
            "{msg} — the {dependency} backend appears to be unreachable. \
             Check that the {dependency} server/sidecar is running and reachable \
             at its configured endpoint."
        )
    } else {
        msg
    }
}

#[cfg(test)]
#[path = "tests/error.rs"]
mod tests;
