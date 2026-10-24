//! Transport session trait -- protocol-agnostic bidirectional connection.
//!
//! Each transport implementation (WebSocket, WebTransport, Direct)
//! provides a concrete type implementing [`TransportSession`].  The
//! session manager and pipeline bridge interact only through this trait.

use async_trait::async_trait;

use super::error::TransportError;
use super::{ClientMessage, ConnectionQuality, ServerMessage, SessionMetadata};

/// A single bidirectional transport connection.
///
/// Implementations handle protocol-specific framing, serialization,
/// and connection management.  The session manager interacts only
/// through this trait.
#[async_trait]
pub trait TransportSession: Send + Sync {
    /// Receive the next message from the client.
    ///
    /// Returns `None` when the connection is cleanly closed.
    /// Returns `Err` on protocol errors or unexpected disconnects.
    async fn recv(&mut self) -> Result<Option<ClientMessage>, TransportError>;

    /// Send a message to the client.
    async fn send(&self, msg: ServerMessage) -> Result<(), TransportError>;

    /// Close the connection with a human-readable reason.
    async fn close(&self, reason: &str) -> Result<(), TransportError>;

    /// Current connection quality assessment.
    fn quality(&self) -> ConnectionQuality;

    /// Session metadata (remote address, transport type, timing).
    fn metadata(&self) -> &SessionMetadata;

    /// Shared handle the runner uses to publish the session's negotiated
    /// input sample rate after the handshake (the session object itself is
    /// moved into its transport pump before negotiation completes, so a
    /// direct setter cannot be called). Transport-level policies — the
    /// inbound audio rate limiter — size themselves from this
    /// instead of the codec's default stamp. `None` for transports without
    /// such policies.
    fn negotiated_input_rate(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicU32>> {
        None
    }

    /// The ping/pong health monitor behind [`quality`](Self::quality), for
    /// consumers that want to follow it (the adaptive-quality controller)
    /// after the session has moved into its transport pump. `None` for
    /// transports without a heartbeat.
    fn health_monitor(&self) -> Option<std::sync::Arc<super::health::ConnectionHealthMonitor>> {
        None
    }
}
