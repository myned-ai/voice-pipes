//! Bounds every gRPC component client shares: connecting, the first call
//! that proves the service is there, and the wait between streamed
//! messages.

use std::time::Duration;

use tonic::transport::{Channel, Endpoint};

/// Bound on opening the channel, and on the `GetInfo` call that follows.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest wait for the next message of a streamed answer. A service that
/// answers and then goes silent fails the turn after this instead of
/// holding it until the session ends.
pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Open a channel to `endpoint` within `timeout`: a black-holed address
/// fails here rather than at the operating system's connect timeout.
///
/// # Errors
/// An invalid endpoint, a refused connection, or the bound passing.
pub async fn connect(
    endpoint: &str,
    timeout: Duration,
    what: &str,
) -> Result<Channel, anyhow::Error> {
    let endpoint = Endpoint::from_shared(endpoint.to_string())?.connect_timeout(timeout);
    tokio::time::timeout(timeout, endpoint.connect())
        .await
        .map_err(|_elapsed| anyhow::anyhow!("gRPC {what} connect timed out after {timeout:?}"))?
        .map_err(|e| anyhow::anyhow!("gRPC {what} connect failed: {e}"))
}

/// Run one call within `timeout`.
///
/// # Errors
/// The call's own error, or the bound passing.
pub async fn within<T>(
    timeout: Duration,
    what: &str,
    call: impl Future<Output = Result<T, tonic::Status>>,
) -> Result<T, anyhow::Error> {
    tokio::time::timeout(timeout, call)
        .await
        .map_err(|_elapsed| anyhow::anyhow!("gRPC {what} timed out after {timeout:?}"))?
        .map_err(|e| anyhow::anyhow!("gRPC {what} failed: {e}"))
}
