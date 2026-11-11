//! Direct (in-process) transport -- channel-based, no network.
//!
//! Used for CLI commands, integration tests, and any scenario
//! where the client and server live in the same process.
//! Communication happens through `tokio::sync::mpsc` channels.

#![expect(
    clippy::map_err_ignore,
    reason = "transport sink errors carry no signal beyond peer-gone; mapped to ConnectionClosed"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};

use super::error::TransportError;
use super::session::TransportSession;
use super::{ClientMessage, ConnectionQuality, ServerMessage, SessionMetadata, TransportType};

/// Channel buffer size for direct transport.
const CHANNEL_BUFFER: usize = 256;

/// Create a matched pair of direct transport sessions.
///
/// Returns `(client_side, server_side)`.  Messages sent by the client
/// arrive at the server's `recv()`, and vice versa.
pub fn channel_pair() -> (DirectTransport, DirectTransport) {
    let (client_to_server_tx, client_to_server_rx) = mpsc::channel(CHANNEL_BUFFER);
    let (server_to_client_tx, server_to_client_rx) = mpsc::channel(CHANNEL_BUFFER);

    let now = Instant::now();
    let session_id = uuid::Uuid::new_v4().to_string();

    let client = DirectTransport {
        outgoing: client_to_server_tx,
        incoming: Arc::new(Mutex::new(server_to_client_rx)),
        metadata: SessionMetadata {
            session_id: session_id.clone(),
            tenant_id: None,
            transport_type: TransportType::Direct,
            remote_addr: None,
            connected_at: now,
            quality: ConnectionQuality::Excellent,
            rtt_ms: Some(0),
            auth_identity: None,
        },
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let server = DirectTransport {
        outgoing: server_to_client_tx,
        incoming: Arc::new(Mutex::new(client_to_server_rx)),
        metadata: SessionMetadata {
            session_id,
            tenant_id: None,
            transport_type: TransportType::Direct,
            remote_addr: Some(SocketAddr::from(([127, 0, 0, 1], 0))),
            connected_at: now,
            quality: ConnectionQuality::Excellent,
            rtt_ms: Some(0),
            auth_identity: None,
        },
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    (client, server)
}

/// In-process transport over mpsc channels.
#[derive(Debug)]
pub struct DirectTransport {
    outgoing: mpsc::Sender<TransportPayload>,
    incoming: Arc<Mutex<mpsc::Receiver<TransportPayload>>>,
    metadata: SessionMetadata,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

/// Internal wrapper -- we send either real messages or a close signal.
#[derive(Debug)]
enum TransportPayload {
    Message(Box<MessageVariant>),
    Close(String),
}

/// Unified message variant for the channel -- avoids double-boxing.
#[expect(
    clippy::large_enum_variant,
    reason = "the large variant is the dominant case on the audio hot path; boxing adds indirection"
)]
#[derive(Debug)]
enum MessageVariant {
    Client(ClientMessage),
    Server(ServerMessage),
}

impl DirectTransport {
    /// Send a [`ClientMessage`] (for the client side).
    pub async fn send_client(&self, msg: ClientMessage) -> Result<(), TransportError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(TransportError::ConnectionClosed);
        }
        self.outgoing
            .send(TransportPayload::Message(Box::new(MessageVariant::Client(
                msg,
            ))))
            .await
            .map_err(|_| TransportError::ConnectionClosed)
    }

    /// Receive a [`ServerMessage`] (for the client side).
    pub async fn recv_server(&self) -> Result<Option<ServerMessage>, TransportError> {
        let mut rx = self.incoming.lock().await;
        match rx.recv().await {
            Some(TransportPayload::Message(m)) => match *m {
                MessageVariant::Server(msg) => Ok(Some(msg)),
                MessageVariant::Client(_) => Err(TransportError::Protocol(
                    "received client message on client side".into(),
                )),
            },
            Some(TransportPayload::Close(reason)) => {
                tracing::debug!(reason, "transport closed");
                self.closed
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(None)
            }
            None => Ok(None),
        }
    }
}

#[async_trait]
impl TransportSession for DirectTransport {
    async fn recv(&mut self) -> Result<Option<ClientMessage>, TransportError> {
        let mut rx = self.incoming.lock().await;
        match rx.recv().await {
            Some(TransportPayload::Message(m)) => match *m {
                MessageVariant::Client(msg) => Ok(Some(msg)),
                MessageVariant::Server(_) => Err(TransportError::Protocol(
                    "received server message on server side".into(),
                )),
            },
            Some(TransportPayload::Close(reason)) => {
                tracing::debug!(reason, "transport closed");
                self.closed
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(None)
            }
            None => Ok(None),
        }
    }

    async fn send(&self, msg: ServerMessage) -> Result<(), TransportError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(TransportError::ConnectionClosed);
        }
        self.outgoing
            .send(TransportPayload::Message(Box::new(MessageVariant::Server(
                msg,
            ))))
            .await
            .map_err(|_| TransportError::ConnectionClosed)
    }

    async fn close(&self, reason: &str) -> Result<(), TransportError> {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self
            .outgoing
            .send(TransportPayload::Close(reason.to_owned()))
            .await;
        Ok(())
    }

    fn quality(&self) -> ConnectionQuality {
        ConnectionQuality::Excellent
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/direct.rs"]
mod tests;
