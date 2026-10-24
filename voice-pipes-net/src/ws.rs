//! WebSocket client settings, and the one way to open a client connection.

use std::time::Duration;

use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A client config that refuses any message or frame over
/// `max_message_bytes`. A provider's frames are parsed whole, so this is the
/// bound on one allocation; tungstenite's default (64 MiB) is far above
/// anything a provider legitimately sends.
pub fn websocket_config(max_message_bytes: usize) -> WebSocketConfig {
    crate::install_crypto_provider();
    WebSocketConfig::default()
        .max_message_size(Some(max_message_bytes))
        .max_frame_size(Some(max_message_bytes))
}

/// A client connection [`connect_websocket`] opened.
pub type WebSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Why [`connect_websocket`] failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    /// The TCP, TLS and upgrade handshake did not finish in time.
    #[error("websocket connect timed out after {0:?}")]
    Timeout(Duration),
    /// The handshake failed: refused, reset, a TLS error, or an HTTP status
    /// instead of the upgrade (the response carries it).
    #[error(transparent)]
    Handshake(#[from] tokio_tungstenite::tungstenite::Error),
}

/// Open a WebSocket client connection to `request` (a URL, or a request
/// carrying headers): the TCP, TLS and upgrade handshake bounded by
/// `timeout`, and every message and frame after it by `max_message_bytes`
/// ([`websocket_config`]). A black-holed endpoint otherwise holds the
/// caller for the operating system's connect timeout, and an unbounded
/// frame is one allocation of whatever size the other side names.
pub async fn connect_websocket<R>(
    request: R,
    max_message_bytes: usize,
    timeout: Duration,
) -> Result<(WebSocket, Response), ConnectError>
where
    R: IntoClientRequest + Unpin,
{
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place the workspace opens a websocket, with its bounds applied"
    )]
    let connect = tokio_tungstenite::connect_async_with_config(
        request,
        Some(websocket_config(max_message_bytes)),
        // TCP_NODELAY: audio goes out in 20 ms frames, and Nagle's
        // algorithm would hold each small frame until the previous one is
        // acknowledged, a round trip of added latency.
        true,
    );
    tokio::time::timeout(timeout, connect)
        .await
        .map_err(|_elapsed| ConnectError::Timeout(timeout))?
        .map_err(ConnectError::from)
}

#[cfg(test)]
#[path = "tests/ws.rs"]
mod tests;
