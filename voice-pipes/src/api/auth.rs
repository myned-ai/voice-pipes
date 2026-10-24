//! Runtime authentication layer for every network surface.
//!
//! Design: auth is a
//! **runtime flag, not a Cargo feature** — the same binary runs open in
//! dev and enforced in production. Enablement is by presence: each
//! surface names a token env var, and setting that env var turns
//! enforcement on for that surface. Nothing set → the middleware passes
//! everything through (dev mode) and a startup warning fires if the
//! listener binds a non-loopback address. Deploy configs set
//! `required = true`, which turns a *missing* token into a startup
//! failure so production can never silently run open.
//!
//! Three credentials, three trust domains:
//! - **client** (`VP_CLIENT_TOKEN`): end-user WS transport (`/ws`).
//! - **admin** (`VP_ADMIN_TOKEN`): orchestrator admin API, including
//!   host extensions (benchmarking).
//! - **internal** (`VP_INTERNAL_TOKEN`): machine-to-machine — worker
//!   registration/heartbeat/events, the worker debug API (the
//!   orchestrator proxies to it), and gRPC component servers.
//!
//! The admin and internal routers share one listener today; giving them
//! distinct tokens means an admin credential does not grant internal
//! access.
//!
//! Health probes (`/health`, `/healthz`, `/readyz`) are always open so
//! Kubernetes probes and load-balancer checks need no credentials.
//!
//! This supersedes the handshake-time sketch in `transport/auth.rs`;
//! that module's `Authenticator` trait remains for future per-user
//! token → identity mapping.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

// ── Settings ([auth] config section) ────────────────────────

fn default_client_env() -> String {
    "VP_CLIENT_TOKEN".to_string()
}
fn default_admin_env() -> String {
    "VP_ADMIN_TOKEN".to_string()
}
fn default_internal_env() -> String {
    "VP_INTERNAL_TOKEN".to_string()
}

/// The optional `[auth]` config section. Absent section == all defaults
/// == enforcement driven purely by which env vars are set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AuthSettings {
    /// Env var holding the WS-client token.
    pub client_token_env: String,
    /// Env var holding the admin-API token.
    pub admin_token_env: String,
    /// Env var holding the internal (machine-to-machine) token.
    pub internal_token_env: String,
    /// When true, a surface whose token env var is unset aborts startup
    /// instead of running open. Set this in deploy configs.
    pub required: bool,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            client_token_env: default_client_env(),
            admin_token_env: default_admin_env(),
            internal_token_env: default_internal_env(),
            required: false,
        }
    }
}

// ── Resolved per-surface auth ───────────────────────────────

/// A resolved credential for one surface: `None` = open (dev mode).
#[derive(Clone)]
pub struct SurfaceAuth {
    token: Option<Arc<str>>,
    surface: &'static str,
    env_name: String,
}

impl std::fmt::Debug for SurfaceAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the token value.
        f.debug_struct("SurfaceAuth")
            .field("surface", &self.surface)
            .field("env", &self.env_name)
            .field("enabled", &self.token.is_some())
            .finish()
    }
}

impl SurfaceAuth {
    /// Read the token from `env_name`. Empty/whitespace values count as
    /// unset so `VP_ADMIN_TOKEN=` in a compose file doesn't enable
    /// enforcement with an empty credential. Silent — call
    /// [`log_status`](Self::log_status) once at startup for the
    /// "authentication enabled" line.
    pub fn resolve(env_name: &str, surface: &'static str) -> Self {
        let token = std::env::var(env_name)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(Arc::from);
        Self {
            token,
            surface,
            env_name: env_name.to_string(),
        }
    }

    /// One startup log line stating whether this surface enforces.
    pub fn log_status(&self) {
        if self.token.is_some() {
            tracing::info!(surface = self.surface, env = %self.env_name, "authentication enabled");
        }
    }

    /// A surface that never enforces (used where a caller needs a
    /// `SurfaceAuth` but the config disabled the surface entirely).
    pub fn open(surface: &'static str) -> Self {
        Self {
            token: None,
            surface,
            env_name: String::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.token.is_some()
    }

    /// The token value, for attaching to *outbound* requests to a peer
    /// that enforces the same credential.
    pub fn bearer(&self) -> Option<Arc<str>> {
        self.token.clone()
    }

    /// Fail startup when `required` is set but no token is configured.
    pub fn enforce_required(&self, required: bool) -> anyhow::Result<()> {
        if required && self.token.is_none() {
            anyhow::bail!(
                "[auth] required = true but {} is not set — refusing to serve the {} unauthenticated \
                 (set the env var, or remove `required` for local development)",
                self.env_name,
                self.surface
            );
        }
        Ok(())
    }

    /// Loud startup warning when an open surface binds a non-loopback
    /// address — the "you are exposed" signal for dev mode.
    pub fn warn_if_open(&self, bind: &str) {
        if self.token.is_none() && !is_loopback(bind) {
            tracing::warn!(
                surface = self.surface,
                bind,
                env = %self.env_name,
                "surface is reachable from the network WITHOUT authentication — set the token \
                 env var to enable it (dev mode is fine on localhost)"
            );
        }
    }

    /// Constant-time check of a presented token.
    pub fn check(&self, presented: Option<&str>) -> bool {
        match (&self.token, presented) {
            (None, _) => true,
            (Some(expected), Some(got)) => ct_eq(expected.as_bytes(), got.as_bytes()),
            (Some(_), None) => false,
        }
    }
}

fn is_loopback(bind: &str) -> bool {
    let host = bind.rsplit_once(':').map_or(bind, |(h, _)| h);
    matches!(host, "127.0.0.1" | "::1" | "localhost" | "[::1]")
}

/// Constant-time byte comparison (length leak is acceptable).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ── Axum middleware ─────────────────────────────────────────

/// Paths that stay open even on an enforced surface: Kubernetes /
/// load-balancer probes cannot carry credentials, and none of these
/// serve secrets.
const OPEN_PATHS: &[&str] = &["/health", "/healthz", "/readyz"];

/// Extract a presented token: `Authorization: Bearer …`, `X-Api-Key`,
/// or a `token`/`api_key` query parameter (browser `WebSocket` cannot
/// set headers, so `/ws?token=…` is the only option there).
fn presented_token(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(v.trim().to_string());
    }
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return Some(v.trim().to_string());
    }
    let query = query?;
    for pair in query.split('&') {
        // A flag-style pair (`?debug&token=…`) has no `=`; skip it rather
        // than giving up on the pairs after it.
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == "token" || k == "api_key" {
            // Percent-decode the minimum that tokens realistically
            // contain; tokens we mint are URL-safe anyway.
            return Some(v.replace("%3D", "=").replace("%2B", "+"));
        }
    }
    None
}

async fn auth_middleware(State(auth): State<SurfaceAuth>, req: Request, next: Next) -> Response {
    if OPEN_PATHS.contains(&req.uri().path()) {
        return next.run(req).await;
    }
    let presented = presented_token(req.headers(), req.uri().query());
    if auth.check(presented.as_deref()) {
        return next.run(req).await;
    }
    tracing::debug!(
        surface = auth.surface,
        path = %req.uri().path(),
        "rejected unauthenticated request"
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        axum::Json(serde_json::json!({
            "data": null,
            "error": { "code": "UNAUTHENTICATED", "message": "missing or invalid token" }
        })),
    )
        .into_response()
}

/// Wrap `router` with token enforcement. A disabled `SurfaceAuth` adds
/// no layer at all, so dev mode has zero per-request overhead.
pub fn protect_router<S>(router: Router<S>, auth: &SurfaceAuth) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    if auth.enabled() {
        router.layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            auth_middleware,
        ))
    } else {
        router
    }
}

// ── Outbound helpers ────────────────────────────────────────

/// Bound on establishing a TCP connection to a peer of the fleet.
pub const FLEET_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// Default bound on one request to a peer of the fleet (registration,
/// heartbeat, event forwarding, the params proxy).
pub const FLEET_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why a client for the fleet could not be built.
#[derive(Debug, thiserror::Error)]
pub enum FleetClientError {
    /// The credential holds a byte an HTTP header cannot carry; the
    /// value itself is never part of the message.
    #[error("the {surface} token in {env_name} holds a byte an HTTP header cannot carry")]
    InvalidToken {
        surface: &'static str,
        env_name: String,
        #[source]
        source: reqwest::header::InvalidHeaderValue,
    },
    /// The HTTP client itself could not be built (TLS backend, resolver).
    #[error("could not build the HTTP client for the fleet: {0}")]
    Build(#[source] reqwest::Error),
}

/// A `reqwest` client that attaches `Authorization: Bearer <token>` to
/// every request when the credential is enabled — used by workers
/// calling the orchestrator's internal API and by the orchestrator
/// proxying to worker debug APIs. Every request is bounded: a peer that
/// accepts the connection and never answers must not hold a heartbeat
/// loop or an admin request open.
///
/// # Errors
/// When the token cannot be a header value or the client cannot be
/// built. Neither falls back to a client without the bearer or the
/// bounds: such a client would be refused by every peer, or hang on one.
pub fn http_client_with_bearer(auth: &SurfaceAuth) -> Result<reqwest::Client, FleetClientError> {
    http_client_with_bearer_and_timeout(auth, FLEET_REQUEST_TIMEOUT)
}

/// [`http_client_with_bearer`] with an explicit per-request bound.
///
/// # Errors
/// As [`http_client_with_bearer`].
pub fn http_client_with_bearer_and_timeout(
    auth: &SurfaceAuth,
    request_timeout: std::time::Duration,
) -> Result<reqwest::Client, FleetClientError> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(token) = auth.bearer() {
        let value =
            voice_pipes_net::sensitive_header(&format!("Bearer {token}")).map_err(|source| {
                FleetClientError::InvalidToken {
                    surface: auth.surface,
                    env_name: auth.env_name.clone(),
                    source,
                }
            })?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    voice_pipes_net::http_client(
        &voice_pipes_net::HttpOptions::request(request_timeout)
            .connect_timeout(FLEET_CONNECT_TIMEOUT)
            .default_headers(headers),
    )
    .map_err(FleetClientError::Build)
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/auth.rs"]
mod tests;
