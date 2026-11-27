//! HTTP router, health endpoints, and WebSocket upgrade handler.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::body::Bytes;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use tracing::info;
use voice_pipes_sdk::TenantId;
use voice_pipes_sdk::media_stream::{
    MediaStreamEncoding, WebhookError, WebhookRequest, get_media_stream_factory,
};

use crate::transport::health::{ConnectionHealthMonitor, HealthConfig};
use crate::transport::media_stream::{MediaStreamSession, RawPcmDialect, extract_correlation_id};
use crate::transport::ws::WsSession;

use super::runner::run_session;
use super::state::TransportState;

/// Build the transport router with WebSocket, raw media stream, telephony
/// dialect, and health endpoints.
pub fn transport_router(state: Arc<TransportState>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/ws/raw", get(raw_ws_handler))
        .route("/telephony/{provider}/ws", get(telephony_ws_handler))
        .route(
            "/telephony/{provider}/voice",
            post(telephony_webhook_handler),
        )
        .route("/wt", get(webtransport_probe_handler))
        .route("/webtransport", get(webtransport_probe_handler))
        .route("/health", get(health_handler))
        .route("/healthz", get(liveness_handler))
        .route("/readyz", get(readiness_handler))
        .with_state(state)
}

/// `GET /wt` and `GET /webtransport` — experimental WebTransport probe route.
///
/// The full QUIC/WebTransport data path is not yet enabled in this worker.
/// Returning a structured 501 here gives clients an explicit capability
/// signal instead of a generic 404.
async fn webtransport_probe_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        axum::Json(serde_json::json!({
            "status": "not_implemented",
            "transport": "webtransport",
            "message": "WebTransport endpoint is experimental and not enabled in this build",
            "fallback": "/ws"
        })),
    )
}

/// `GET /health` -- informational health snapshot on the transport port.
///
/// Beyond session counts, this reports the worst-case `aggregate_health`
/// across every registered component plus a per-component breakdown (id, kind,
/// state, reason). This makes a single `curl <worker>/health` enough to answer
/// "is this the worker I think it is, and is its TTS/STT/LLM backend up?" --
/// the same registry the `/debug/components` endpoint reads, but on the port
/// the client actually connects to. Components flip to Unhealthy via failed
/// warm-ups (see the runner) and the periodic auto-degrader.
async fn health_handler(State(state): State<Arc<TransportState>>) -> impl IntoResponse {
    use crate::component::HealthState;

    let active = state.active_sessions.load(Ordering::Relaxed);
    let max = state.max_sessions;

    let aggregate = state.component_registry.aggregate_health();
    let aggregate_str = match aggregate {
        HealthState::Healthy | HealthState::NotConfigured => "healthy",
        HealthState::Unhealthy => "unhealthy",
        // Degraded, or a state this build does not know: never "healthy".
        _ => "degraded",
    };

    // This path is open (probes cannot carry credentials), so it reports
    // each component's state but not its free-text `reason`, which is
    // filled from provider and warm-up errors and can carry upstream
    // detail. The reasons are on the authenticated `/debug/components`.
    let components: Vec<_> = state
        .component_registry
        .snapshot()
        .into_iter()
        .map(|c| {
            let h = c.health();
            serde_json::json!({
                "id": c.id(),
                "kind": c.kind(),
                "state": h.state,
            })
        })
        .collect();

    axum::Json(serde_json::json!({
        "status": "ok",
        "aggregate_health": aggregate_str,
        "active_sessions": active,
        "max_sessions": max,
        "components": components,
    }))
}

/// `GET /healthz` -- Kubernetes liveness probe: 200 whenever the transport
/// answers. A worker whose transport listener fails exits instead of
/// staying up without it, so an answer here is the liveness signal; what
/// the worker depends on is reported by `/readyz` and `/health`.
async fn liveness_handler() -> impl IntoResponse {
    (
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "status": "alive" })),
    )
}

/// `GET /readyz` -- Kubernetes readiness probe.
///
/// Returns 200 when the server is ready to accept new work (models loaded and
/// capacity available).  Returns 503 when still initialising, at capacity, or
/// shutting down.
async fn readiness_handler(State(state): State<Arc<TransportState>>) -> impl IntoResponse {
    if state.shutdown.is_cancelled() {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({ "status": "shutting_down" })),
        );
    }
    if !state.ready.load(Ordering::Relaxed) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({ "status": "not_ready" })),
        );
    }
    let active = state.active_sessions.load(Ordering::Relaxed);
    if active >= state.max_sessions {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "status": "at_capacity",
                "active_sessions": active,
                "max_sessions": state.max_sessions,
            })),
        );
    }
    (
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({
            "status": "ready",
            "active_sessions": active,
            "max_sessions": state.max_sessions,
        })),
    )
}

// ── WebSocket upgrade ───────────────────────────────────────

fn extract_tenant_id(headers: &HeaderMap, query: Option<&str>) -> Option<TenantId> {
    headers
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| TenantId::new(v).ok())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .and_then(|v| {
                    let trimmed = v.trim();
                    trimmed
                        .strip_prefix("vp_live_")
                        .or_else(|| trimmed.strip_prefix("vp_test_"))
                })
                .and_then(|rest| rest.split_once('_'))
                .and_then(|(tenant, _)| TenantId::new(tenant).ok())
        })
        .or_else(|| {
            query.and_then(|q| {
                q.split('&').find_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    if (k == "tenant_id" || k == "tenant") && !v.is_empty() {
                        let unescaped = v.replace("%2D", "-").replace("%5F", "_");
                        TenantId::new(unescaped).ok()
                    } else {
                        None
                    }
                })
            })
        })
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    uri: Uri,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<TransportState>>,
) -> impl IntoResponse {
    let active = state.active_sessions.load(Ordering::Relaxed);
    if active >= state.max_sessions {
        tracing::warn!(
            active,
            max = state.max_sessions,
            "rejecting WS connection: max sessions reached"
        );
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "max sessions reached",
        )
            .into_response();
    }

    let tenant_id = extract_tenant_id(&headers, uri.query());
    let correlation_headers = state
        .config
        .transport
        .as_ref()
        .map_or(&[][..], |t| t.correlation_headers.as_slice());
    let external_id = extract_correlation_id(&headers, correlation_headers);

    // Cap inbound message/frame size at the protocol layer so an
    // oversized message is refused before it is ever buffered (tungstenite's
    // default is ~64 MiB per message). The codec enforces the same limit
    // with a typed `MessageTooLarge` error.
    ws.max_message_size(crate::transport::ws::MAX_CLIENT_MESSAGE_BYTES)
        .max_frame_size(crate::transport::ws::MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_ws_connection(socket, addr, tenant_id, external_id, state))
        .into_response()
}

async fn handle_ws_connection(
    socket: WebSocket,
    addr: SocketAddr,
    tenant_id: Option<TenantId>,
    external_id: Option<String>,
    state: Arc<TransportState>,
) {
    let session_id = uuid::Uuid::new_v4().to_string();
    info!(
        session_id = %session_id,
        ?tenant_id,
        ?external_id,
        remote = %addr,
        "WS connection accepted"
    );

    let health = ConnectionHealthMonitor::new(HealthConfig::default());
    let write_timeout = std::time::Duration::from_millis(
        state
            .config
            .transport
            .as_ref()
            .map_or(5_000, |t| t.write_timeout_ms),
    );
    let session = WsSession::new(socket, session_id, Some(addr), health, write_timeout)
        .with_tenant(tenant_id)
        .with_external_id(external_id);

    run_session(Box::new(session), state).await;
}

// ── Generic Raw PCM WebSocket (/ws/raw) ─────────────────────

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn parse_query_param<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key && !v.is_empty()).then_some(v)
    })
}

/// `GET /ws/raw` — generic handshake-free binary audio WebSocket (PCM16 / G.711).
///
/// Fail-closed: disabled unless `transport.raw_ws_enabled = true` AND
/// `VP_CLIENT_TOKEN` is set and matched via `Authorization: Bearer <token>`.
async fn raw_ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    uri: Uri,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<TransportState>>,
) -> impl IntoResponse {
    let enabled = state
        .config
        .transport
        .as_ref()
        .is_some_and(|t| t.raw_ws_enabled);
    if !enabled {
        return (StatusCode::NOT_FOUND, "raw websocket endpoint is disabled").into_response();
    }

    // Fail-closed bearer token gate before upgrading the WebSocket.
    let expected_token = match std::env::var("VP_CLIENT_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "raw websocket requires VP_CLIENT_TOKEN to be configured",
            )
                .into_response();
        }
    };
    let provided_token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    match provided_token {
        Some(tok) if constant_time_eq(tok.as_bytes(), expected_token.trim().as_bytes()) => {}
        _ => {
            return (StatusCode::UNAUTHORIZED, "invalid or missing bearer token").into_response();
        }
    }

    let active = state.active_sessions.load(Ordering::Relaxed);
    if active >= state.max_sessions {
        return (StatusCode::SERVICE_UNAVAILABLE, "max sessions reached").into_response();
    }

    let sample_rate = parse_query_param(uri.query(), "sample_rate")
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|r| voice_pipes_sdk::audio::resample::SUPPORTED_RATES.contains(r))
        .unwrap_or(16_000);
    let encoding = match parse_query_param(uri.query(), "encoding") {
        Some("mulaw" | "ulaw") => MediaStreamEncoding::Mulaw,
        Some("alaw") => MediaStreamEncoding::Alaw,
        _ => MediaStreamEncoding::Pcm16Le,
    };

    let tenant_id = extract_tenant_id(&headers, uri.query());
    let correlation_headers = state
        .config
        .transport
        .as_ref()
        .map_or(&[][..], |t| t.correlation_headers.as_slice());
    let external_id = extract_correlation_id(&headers, correlation_headers);
    let session_id = uuid::Uuid::new_v4().to_string();

    let dialect = RawPcmDialect::new(session_id.clone(), sample_rate, encoding)
        .with_external_id(external_id.clone())
        .with_tenant_id(tenant_id.as_ref().map(|t| t.as_str().to_string()));
    let max_bytes = voice_pipes_sdk::media_stream::MediaStreamDialect::max_frame_bytes(&dialect);

    ws.max_message_size(max_bytes)
        .max_frame_size(max_bytes)
        .on_upgrade(move |socket| async move {
            info!(
                session_id = %session_id,
                ?tenant_id,
                ?external_id,
                sample_rate,
                remote = %addr,
                "raw media stream WS accepted"
            );
            let health = ConnectionHealthMonitor::new(HealthConfig::default());
            let write_timeout = std::time::Duration::from_millis(
                state
                    .config
                    .transport
                    .as_ref()
                    .map_or(5_000, |t| t.write_timeout_ms),
            );
            let session = MediaStreamSession::new(
                socket,
                Box::new(dialect),
                session_id,
                Some(addr),
                Arc::new(health),
                write_timeout,
            )
            .with_tenant(tenant_id)
            .with_external_id(external_id)
            .with_auth_identity(Some("bearer".to_string()));
            run_session(Box::new(session), state).await;
        })
        .into_response()
}

// ── Plugin Telephony Routes (/telephony/{provider}/*) ───────

fn is_valid_env_var_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(&first) = bytes.first() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Resolve host environment-variable credentials into a dialect's JSON config
/// without letting provider crates read `std::env` directly.
///
/// Precedence for any `<field>` / `<field>_env` pair (e.g. `auth_token` /
/// `auth_token_env`, `stream_signing_secret` / `stream_signing_secret_env`):
/// 1. Explicit non-empty `<field>` in TOML.
/// 2. Value of the env var named by `<field>_env` in TOML.
/// 3. For `auth_token` only: value of `default_auth_env` (e.g. `TWILIO_AUTH_TOKEN`).
fn resolve_media_stream_config_with<F>(
    raw: &serde_json::Value,
    default_auth_env: Option<&str>,
    mut lookup_env: F,
) -> serde_json::Value
where
    F: FnMut(&str) -> Option<String>,
{
    let mut obj = match raw.as_object() {
        Some(map) => map.clone(),
        None => serde_json::Map::new(),
    };

    // Collect `<field>_env` mappings first so we don't mutate while iterating.
    let env_refs: Vec<(String, String)> = obj
        .iter()
        .filter_map(|(k, v)| {
            let target = k.strip_suffix("_env")?;
            let var_name = v.as_str()?.trim();
            (!target.is_empty() && is_valid_env_var_name(var_name))
                .then(|| (target.to_string(), var_name.to_string()))
        })
        .collect();

    let has_non_empty = |map: &serde_json::Map<String, serde_json::Value>, key: &str| {
        map.get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };

    for (target, var_name) in env_refs {
        if !has_non_empty(&obj, &target)
            && let Some(val) = lookup_env(&var_name).filter(|s| !s.trim().is_empty())
        {
            obj.insert(target, serde_json::Value::String(val));
        }
    }

    // Fallback to the dialect's default auth env var (e.g. TWILIO_AUTH_TOKEN)
    // when neither `auth_token` nor `auth_token_env` provided a value.
    if !has_non_empty(&obj, "auth_token")
        && let Some(default_var) = default_auth_env
        && is_valid_env_var_name(default_var)
        && let Some(val) = lookup_env(default_var).filter(|s| !s.trim().is_empty())
    {
        obj.insert("auth_token".to_string(), serde_json::Value::String(val));
    }

    serde_json::Value::Object(obj)
}

fn resolve_media_stream_config(
    raw: &serde_json::Value,
    default_auth_env: Option<&str>,
) -> serde_json::Value {
    resolve_media_stream_config_with(raw, default_auth_env, |name| std::env::var(name).ok())
}

/// `GET /telephony/{provider}/ws` — WebSocket media stream dispatched to any
/// registered [`MediaStreamDialectFactory`](voice_pipes_sdk::media_stream::MediaStreamDialectFactory).
async fn telephony_ws_handler(
    Path(provider): Path<String>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    uri: Uri,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<TransportState>>,
) -> impl IntoResponse {
    let Some(factory) = get_media_stream_factory(&provider) else {
        return (
            StatusCode::NOT_FOUND,
            format!("media stream dialect \"{provider}\" is not registered"),
        )
            .into_response();
    };

    let active = state.active_sessions.load(Ordering::Relaxed);
    if active >= state.max_sessions {
        return (StatusCode::SERVICE_UNAVAILABLE, "max sessions reached").into_response();
    }

    let raw_cfg = state
        .config
        .transport
        .as_ref()
        .and_then(|t| t.media_streams.get(&provider))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let dialect_cfg = resolve_media_stream_config(&raw_cfg, factory.default_auth_token_env());

    let dialect = match factory.create_dialect(&dialect_cfg) {
        Ok(d) => d,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response();
        }
    };

    let tenant_id = extract_tenant_id(&headers, uri.query());
    let correlation_headers = state
        .config
        .transport
        .as_ref()
        .map_or(&[][..], |t| t.correlation_headers.as_slice());
    let external_id = extract_correlation_id(&headers, correlation_headers);
    let max_bytes = dialect.max_frame_bytes();

    ws.max_message_size(max_bytes)
        .max_frame_size(max_bytes)
        .on_upgrade(move |socket| async move {
            let session_id = uuid::Uuid::new_v4().to_string();
            info!(
                session_id = %session_id,
                dialect = %provider,
                ?tenant_id,
                ?external_id,
                remote = %addr,
                "telephony media stream WS accepted"
            );
            let health = ConnectionHealthMonitor::new(HealthConfig::default());
            let write_timeout = std::time::Duration::from_millis(
                state
                    .config
                    .transport
                    .as_ref()
                    .map_or(5_000, |t| t.write_timeout_ms),
            );
            let session = MediaStreamSession::new(
                socket,
                dialect,
                session_id,
                Some(addr),
                Arc::new(health),
                write_timeout,
            )
            .with_tenant(tenant_id)
            .with_external_id(external_id);
            run_session(Box::new(session), state).await;
        })
        .into_response()
}

fn percent_decode_form_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i).copied() {
            Some(b'+') => {
                out.push(b' ');
                i += 1;
            }
            Some(b'%') if i + 2 < bytes.len() => {
                if let (Some(&h1), Some(&h2)) = (bytes.get(i + 1), bytes.get(i + 2)) {
                    let hex = [h1, h2];
                    if let Ok(s) = std::str::from_utf8(&hex)
                        && let Ok(val) = u8::from_str_radix(s, 16)
                    {
                        out.push(val);
                        i += 3;
                        continue;
                    }
                }
                out.push(b'%');
                i += 1;
            }
            Some(b) => {
                out.push(b);
                i += 1;
            }
            None => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_form_urlencoded(body: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(body);
    let mut map = HashMap::new();
    for pair in text.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(
            percent_decode_form_component(k),
            percent_decode_form_component(v),
        );
    }
    map
}

/// `POST /telephony/{provider}/voice` — optional carrier HTTP call-setup webhook
/// (e.g., Twilio `TwiML` `<Connect><Stream>`).
async fn telephony_webhook_handler(
    Path(provider): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<TransportState>>,
    body: Bytes,
) -> impl IntoResponse {
    let Some(factory) = get_media_stream_factory(&provider) else {
        return (
            StatusCode::NOT_FOUND,
            format!("media stream dialect \"{provider}\" is not registered"),
        )
            .into_response();
    };

    let raw_cfg = state
        .config
        .transport
        .as_ref()
        .and_then(|t| t.media_streams.get(&provider))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let dialect_cfg = resolve_media_stream_config(&raw_cfg, factory.default_auth_token_env());

    let mut hdrs = HashMap::new();
    for (k, v) in &headers {
        if let Ok(s) = v.to_str() {
            hdrs.insert(k.as_str().to_ascii_lowercase(), s.to_string());
        }
    }

    let req = WebhookRequest::new()
        .with_headers(hdrs)
        .with_form_params(parse_form_urlencoded(&body))
        .with_raw_body(body.to_vec());

    match factory.handle_webhook(&dialect_cfg, &req) {
        Ok(Some(resp)) => {
            let status = StatusCode::from_u16(resp.status_code).unwrap_or(StatusCode::OK);
            (
                status,
                [(header::CONTENT_TYPE, resp.content_type)],
                resp.body,
            )
                .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "webhook not supported by dialect").into_response(),
        Err(WebhookError::Unauthorized(msg)) => (StatusCode::FORBIDDEN, msg).into_response(),
        Err(WebhookError::BadRequest(msg)) => (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(WebhookError::Misconfigured(msg)) => {
            (StatusCode::SERVICE_UNAVAILABLE, msg).into_response()
        }
        Err(other) => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()).into_response(),
    }
}

#[cfg(test)]
#[path = "tests/router.rs"]
mod tests;
