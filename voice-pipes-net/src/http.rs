//! HTTP clients built to one policy.

use std::time::Duration;

/// Connect timeout when none is given: an unreachable host fails fast.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an idle pooled connection is kept: longer than a typical pause
/// between turns, so the next turn skips the TLS handshake.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Idle connections kept per host. A provider talks to one vendor host, and a
/// few overlapping turns at most are in flight on it.
const POOL_MAX_IDLE_PER_HOST: usize = 4;

/// TCP keep-alive on pooled connections, so a NAT or load balancer does not
/// drop one silently between turns.
const TCP_KEEPALIVE: Duration = Duration::from_mins(1);

/// HTTP/2 pings on idle connections, and how long to wait for the answer: a
/// dead connection is found before a turn needs it, not during.
const HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a client may wait, in the two shapes providers need, and the
/// headers every request carries.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    connect: Duration,
    limit: Limit,
    headers: reqwest::header::HeaderMap,
}

/// Which wait a client bounds after connecting.
#[derive(Debug, Clone, Copy)]
enum Limit {
    /// The whole request, body included.
    Total(Duration),
    /// Each read: the response may last any time while bytes keep coming.
    Idle(Duration),
}

impl HttpOptions {
    /// A request-response API: the whole request, body included, must finish
    /// within `total`. Not for a body read as it arrives, which a slow
    /// consumer (audio played in real time) makes last longer than any fixed
    /// total.
    pub fn request(total: Duration) -> Self {
        Self {
            connect: DEFAULT_CONNECT_TIMEOUT,
            limit: Limit::Total(total),
            headers: reqwest::header::HeaderMap::new(),
        }
    }

    /// A streaming API whose response lasts as long as it keeps arriving (an
    /// LLM's tokens, streamed audio). No read, the response head included,
    /// may wait longer than `idle`; the response as a whole has no bound.
    pub fn streaming(idle: Duration) -> Self {
        Self {
            connect: DEFAULT_CONNECT_TIMEOUT,
            limit: Limit::Idle(idle),
            headers: reqwest::header::HeaderMap::new(),
        }
    }

    /// Override the connect timeout (default 5 s).
    #[must_use]
    pub fn connect_timeout(mut self, connect: Duration) -> Self {
        self.connect = connect;
        self
    }

    /// Headers sent with every request. Mark a credential's value
    /// `set_sensitive(true)` so it stays out of the client's debug output.
    #[must_use]
    pub fn default_headers(mut self, headers: reqwest::header::HeaderMap) -> Self {
        self.headers = headers;
        self
    }
}

/// A client with the policy applied: the process's TLS stack, the connect
/// timeout and the request or idle bound, no redirects, a fixed `User-Agent`, and pooled
/// connections kept warm between turns. The client comes back finished, so
/// nothing downstream can loosen the policy.
///
/// # Errors
/// When the TLS backend cannot be initialised.
pub fn http_client(opts: &HttpOptions) -> Result<reqwest::Client, reqwest::Error> {
    crate::install_crypto_provider();
    #[expect(
        clippy::disallowed_methods,
        reason = "the one place a provider's HTTP client is built"
    )]
    let builder = reqwest::Client::builder();
    let builder = builder
        .connect_timeout(opts.connect)
        .pool_idle_timeout(Some(POOL_IDLE_TIMEOUT))
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(Some(TCP_KEEPALIVE))
        .http2_keep_alive_interval(Some(HTTP2_KEEPALIVE_INTERVAL))
        .http2_keep_alive_timeout(HTTP2_KEEPALIVE_TIMEOUT)
        .http2_keep_alive_while_idle(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("voice-pipes/", env!("CARGO_PKG_VERSION")))
        .default_headers(opts.headers.clone());
    match opts.limit {
        Limit::Total(total) => builder.timeout(total),
        Limit::Idle(idle) => builder.read_timeout(idle),
    }
    .build()
}

/// A header value carrying a credential, marked sensitive: it is left out
/// of the client's debug output and never added to HTTP/2's header
/// compression table, where later requests could learn it.
///
/// # Errors
/// When `value` holds a byte a header cannot carry.
pub fn sensitive_header(
    value: &str,
) -> Result<reqwest::header::HeaderValue, reqwest::header::InvalidHeaderValue> {
    let mut header = reqwest::header::HeaderValue::from_str(value)?;
    header.set_sensitive(true);
    Ok(header)
}

/// How much of an error response's body is read: enough for a vendor's
/// error message, not enough for a hostile or broken server to matter.
pub const ERROR_BODY_LIMIT: usize = 8 * 1024;

/// The start of an error response's body as text, at most
/// [`ERROR_BODY_LIMIT`] bytes (invalid UTF-8 replaced); the rest is never
/// read. Empty when the body cannot be read.
pub async fn error_body(mut response: reqwest::Response) -> String {
    let mut body = Vec::new();
    while body.len() < ERROR_BODY_LIMIT {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(ERROR_BODY_LIMIT - body.len());
                body.extend_from_slice(&chunk[..take]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// Why a whole response body could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BodyError {
    /// The body passed the limit its reader set; the server sent more than
    /// any valid answer holds.
    #[error("response body exceeded {limit} bytes")]
    TooLarge {
        /// The limit that was exceeded, in bytes.
        limit: usize,
    },
    /// The connection failed part way through the body: a network fault,
    /// not a malformed answer.
    #[error("response body read failed: {0}")]
    Read(#[source] reqwest::Error),
}

/// A whole response body, refused once it passes `limit` bytes: for a small
/// success response (a model listing, one synthesis) parsed as a unit.
///
/// # Errors
/// [`BodyError::TooLarge`] past the limit; [`BodyError::Read`] when the
/// connection fails before the body ends, so a cut-off body is never handed
/// to a parser as if it were whole.
pub async fn limited_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, BodyError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(BodyError::Read)? {
        if body.len() + chunk.len() > limit {
            return Err(BodyError::TooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
#[path = "tests/http.rs"]
mod tests;
