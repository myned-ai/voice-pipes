//! HTTP router, health endpoints, and WebSocket upgrade handler.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::extract::State;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, Uri, header};
use axum::response::IntoResponse;
use axum::routing::get;
use tracing::info;
use voice_pipes_sdk::TenantId;

use crate::transport::health::{ConnectionHealthMonitor, HealthConfig};
use crate::transport::ws::WsSession;

use super::runner::run_session;
use super::state::TransportState;

/// Build the transport router with WebSocket and health endpoints.
pub fn transport_router(state: Arc<TransportState>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
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

    // Cap inbound message/frame size at the protocol layer so an
    // oversized message is refused before it is ever buffered (tungstenite's
    // default is ~64 MiB per message). The codec enforces the same limit
    // with a typed `MessageTooLarge` error.
    ws.max_message_size(crate::transport::ws::MAX_CLIENT_MESSAGE_BYTES)
        .max_frame_size(crate::transport::ws::MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_ws_connection(socket, addr, tenant_id, state))
        .into_response()
}

async fn handle_ws_connection(
    socket: WebSocket,
    addr: SocketAddr,
    tenant_id: Option<TenantId>,
    state: Arc<TransportState>,
) {
    let session_id = uuid::Uuid::new_v4().to_string();
    info!(session_id = %session_id, ?tenant_id, remote = %addr, "WS connection accepted");

    let health = ConnectionHealthMonitor::new(HealthConfig::default());
    let write_timeout = std::time::Duration::from_millis(
        state
            .config
            .transport
            .as_ref()
            .map_or(5_000, |t| t.write_timeout_ms),
    );
    let session = WsSession::new(
        socket,
        session_id,
        Some(addr),
        health,
        write_timeout,
    )
    .with_tenant(tenant_id);

    run_session(Box::new(session), state).await;
}

#[cfg(test)]
#[path = "tests/router.rs"]
mod tests;
