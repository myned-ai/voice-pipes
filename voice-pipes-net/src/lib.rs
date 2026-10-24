//! The one place a voice-pipes provider gets a network client.
//!
//! Every hosted provider talks to a vendor over HTTPS or a WebSocket and
//! sends it a credential. Left to each provider, the TLS stack, the timeouts,
//! the redirect policy and what counts as a safe endpoint drift apart; here
//! they are decided once:
//!
//! - **One TLS stack.** `rustls` on the `ring` provider, platform root
//!   certificates. [`install_crypto_provider`] makes that the process-wide
//!   default; call it at startup (the binary does), and anything that builds
//!   a rustls client after it uses `ring`.
//! - **No unbounded waits.** An [`HttpOptions`] always carries a connect
//!   timeout, and either a whole-request timeout or, for a response read as
//!   it arrives, a bound on each read.
//! - **No redirects.** A vendor API that answers with a redirect gets the
//!   redirect back, not followed: `reqwest` strips `Authorization` when a
//!   redirect changes host, but not a custom key header (`xi-api-key`,
//!   `x-goog-api-key`), so following one could hand the key to another host.
//! - **No credentials in plain text** to a host on the internet:
//!   [`check_credential_endpoint`] refuses `http://`/`ws://` for a request
//!   that carries a key unless the host is local (loopback, a private
//!   address, a single-label name such as a container) or the operator opted
//!   in.
//! - **Bounded input.** Error bodies are read to a limit ([`error_body`]),
//!   newline-delimited streams split with one ([`BoundedLines`]), and
//!   WebSocket messages capped (feature `ws`: [`websocket_config`]).

#![forbid(unsafe_code)]

mod endpoint;
mod http;
mod lines;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "ws")]
mod ws;

pub use endpoint::{
    ALLOW_INSECURE_OPTION, EndpointError, check_credential_endpoint, credential_endpoint_error,
};
pub use http::{
    BodyError, ERROR_BODY_LIMIT, HttpOptions, error_body, http_client, limited_body,
    sensitive_header,
};
pub use lines::{BoundedLines, LineTooLong};
#[cfg(feature = "ws")]
pub use ws::{ConnectError, WebSocket, connect_websocket, websocket_config};

/// The HTTP client crate, so a provider names its types (`Client`,
/// `Response`, `StatusCode`) without depending on it and building clients
/// outside [`http_client`].
pub use reqwest;
/// The WebSocket client crate (feature `ws`), for the same reason: connect
/// with [`connect_websocket`], never with its defaults.
#[cfg(feature = "ws")]
pub use tokio_tungstenite;

/// Make `ring` the process-wide rustls crypto provider. Idempotent: the
/// first call installs it and later calls do nothing, so a library may call
/// it defensively and a binary should call it first thing in `main`.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
