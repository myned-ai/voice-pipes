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

/// Metadata header carrying the caller's session ID so a shared gRPC
/// text-processor container can isolate per-session conversation memory.
pub const SESSION_ID_HEADER: &str = "x-vp-session-id";

/// Interval between HTTP/2 PING frames and TCP keep-alive probes so idle
/// container channels are not silently dropped by cloud NATs/LBs.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Timeout waiting for an HTTP/2 PING ACK before marking the connection dead.
pub const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(5);

fn configure_endpoint(raw: &str, timeout: Duration) -> Result<Endpoint, anyhow::Error> {
    Ok(Endpoint::from_shared(raw.trim().to_string())?
        .connect_timeout(timeout)
        .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
        .keep_alive_while_idle(true)
        .tcp_keepalive(Some(KEEP_ALIVE_INTERVAL)))
}

/// Open a channel to `endpoint` within `timeout`: a black-holed address
/// fails here rather than at the operating system's connect timeout.
///
/// Accepts either a single URL (`"http://stt:50051"`) or a comma-separated
/// list of replica URLs (`"http://stt-1:50051,http://stt-2:50051"`) balanced
/// round-robin across HTTP/2 subchannels.
///
/// # Errors
/// An invalid endpoint, a refused connection, or the bound passing.
pub async fn connect(
    endpoint: &str,
    timeout: Duration,
    what: &str,
) -> Result<Channel, anyhow::Error> {
    let parts: Vec<&str> = endpoint
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        anyhow::bail!("gRPC {what} endpoint must not be empty");
    }
    if parts.len() == 1 {
        let ep = configure_endpoint(parts[0], timeout)?;
        return tokio::time::timeout(timeout, ep.connect())
            .await
            .map_err(|_elapsed| anyhow::anyhow!("gRPC {what} connect timed out after {timeout:?}"))?
            .map_err(|e| anyhow::anyhow!("gRPC {what} connect failed: {e}"));
    }

    let mut endpoints = Vec::with_capacity(parts.len());
    for raw in parts {
        endpoints.push(configure_endpoint(raw, timeout)?);
    }
    // Probe the first endpoint eagerly so an unreachable list still fails at startup.
    tokio::time::timeout(timeout, endpoints[0].connect())
        .await
        .map_err(|_elapsed| anyhow::anyhow!("gRPC {what} connect timed out after {timeout:?}"))?
        .map_err(|e| anyhow::anyhow!("gRPC {what} connect failed: {e}"))?;

    Ok(Channel::balance_list(endpoints.into_iter()))
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
