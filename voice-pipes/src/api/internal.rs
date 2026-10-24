//! Internal API handlers -- worker-to-orchestrator communication.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono::Utc;

use super::types::{
    ApiResponse, OrchestratorState, SessionCompleteEvent, SessionRecord, SessionStatus,
    WorkerDeregistration, WorkerHeartbeat, WorkerRecord, WorkerRegistration, WorkerStatus,
};

// ── Router ──────────────────────────────────────────────────

/// Build the internal (worker-to-orchestrator) HTTP router.
pub fn router(state: Arc<OrchestratorState>) -> Router {
    Router::new()
        .route("/internal/workers/register", post(register_worker))
        .route("/internal/workers/heartbeat", post(heartbeat))
        .route("/internal/workers/deregister", post(deregister_worker))
        // Session lifecycle (start/end) from workers.
        .route("/internal/events/session", post(session_lifecycle_event))
        // Turn completion events from workers.
        .route("/internal/events/turn", post(turn_event))
        .with_state(state)
}

// ── POST /internal/workers/register ─────────────────────────

/// True when `addr` looks like a plain `host:port` a worker may
/// legitimately advertise: no scheme, no path/query, a valid
/// port, and — when the orchestrator configures an allowlist — a host
/// on it. The dispatch endpoint and the params proxy connect to these
/// addresses, so accepting arbitrary strings here is an SSRF primitive.
fn valid_worker_address(addr: &str, allowed_hosts: &[String]) -> bool {
    if addr.is_empty() || addr.contains('/') || addr.contains('\\') || addr.contains('@') {
        return false;
    }
    let Some((host, port)) = addr.rsplit_once(':') else {
        return false;
    };
    if host.is_empty() || port.parse::<u16>().is_err() {
        return false;
    }
    allowed_hosts.is_empty() || allowed_hosts.iter().any(|h| h == host)
}

/// The host part of a validated `host:port` address.
fn host_of(addr: &str) -> &str {
    addr.rsplit_once(':').map_or(addr, |(host, _)| host)
}

/// Registered workers the reaper has not marked dead.
pub fn live_worker_count(workers: &dashmap::DashMap<String, WorkerRecord>) -> usize {
    workers
        .iter()
        .filter(|w| w.status != WorkerStatus::Dead)
        .count()
}

/// One pass of the registry reaper: a worker whose heartbeat is older
/// than `heartbeat_timeout` is marked dead, and a dead record older than
/// `heartbeat_timeout + dead_grace` is removed. Without the removal the
/// registry only grows: a fleet whose workers mint a fresh id per process
/// would eventually hit `max_registered` and refuse every new worker.
pub fn reap_workers(
    workers: &dashmap::DashMap<String, WorkerRecord>,
    now: chrono::DateTime<chrono::Utc>,
    heartbeat_timeout: chrono::Duration,
    dead_grace: chrono::Duration,
) -> usize {
    workers.iter_mut().for_each(|mut entry| {
        let w = entry.value_mut();
        if w.status != WorkerStatus::Dead && now - w.last_heartbeat > heartbeat_timeout {
            tracing::info!(worker_id = %w.id, "marking worker as dead (heartbeat timeout)");
            w.status = WorkerStatus::Dead;
        }
    });
    let before = workers.len();
    workers.retain(|_, w| {
        let evict = w.status == WorkerStatus::Dead
            && now - w.last_heartbeat > heartbeat_timeout + dead_grace;
        if evict {
            tracing::info!(worker_id = %w.id, "removing dead worker from the registry");
        }
        !evict
    });
    before - workers.len()
}

async fn register_worker(
    State(state): State<Arc<OrchestratorState>>,
    Json(reg): Json<WorkerRegistration>,
) -> Response {
    let id = reg.worker_id.clone();

    // Validate self-reported addresses before they can feed
    // `GET /api/v1/dispatch` or the params proxy.
    let allowed = &state.config.workers.allowed_hosts;
    let mut bad_address = (!valid_worker_address(&reg.address, allowed))
        .then(|| format!("invalid worker address {:?}", reg.address));
    if bad_address.is_none()
        && let Some(t) = reg.transport_address.as_deref()
        && !valid_worker_address(t, allowed)
    {
        bad_address = Some(format!("invalid transport address {t:?}"));
    }
    if bad_address.is_none()
        && let Some(t) = reg.public_transport_address.as_deref()
        && !valid_worker_address(t, allowed)
    {
        bad_address = Some(format!("invalid public transport address {t:?}"));
    }
    if let Some(message) = bad_address {
        tracing::warn!(worker_id = %id, %message, "rejected worker registration");
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::bad_request(message)),
        )
            .into_response();
    }

    // A live worker's id cannot be re-pointed at another host by a
    // second registration: the internal token is shared by the whole
    // fleet, so this is the only thing keeping one worker (or anything
    // else holding the token) from taking over another's dispatch and
    // params proxy. A dead record may be claimed by a worker coming back
    // on a new address.
    if let Some(existing) = state.workers.get(&id)
        && existing.status != WorkerStatus::Dead
        && host_of(&existing.address) != host_of(&reg.address)
    {
        tracing::warn!(
            worker_id = %id,
            existing = %existing.address,
            requested = %reg.address,
            "rejected re-registration of a live worker from a different host"
        );
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::bad_request(format!(
                "worker {id} is live at another address; wait for it to be reaped or use a different id"
            ))),
        )
            .into_response();
    }

    // Bound the registry — attacker-chosen ids must not grow
    // the map without limit. Known ids re-register freely; records the
    // reaper has marked dead do not count against the cap.
    let is_new = !state.workers.contains_key(&id);
    if is_new && live_worker_count(&state.workers) >= state.config.workers.max_registered {
        tracing::warn!(
            worker_id = %id,
            cap = state.config.workers.max_registered,
            "rejected worker registration: registry at capacity"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<()>::internal_error(
                "worker registry at capacity".to_string(),
            )),
        )
            .into_response();
    }

    let record = WorkerRecord {
        id: id.clone(),
        address: reg.address,
        registered_at: Utc::now(),
        last_heartbeat: Utc::now(),
        status: WorkerStatus::Active,
        capacity: reg.capacity,
        pipeline_config_hash: reg.pipeline_config_hash,
        pipeline_name: reg.pipeline_name,
        pipeline_config: reg.pipeline_config,
        version: reg.version,
        tags: reg.tags,
        transport_address: reg.transport_address,
        public_transport_address: reg.public_transport_address,
        sessions_processed: 0,
        errors_total: 0,
        available_configs: reg.available_configs,
        components: Vec::new(),
        aggregate_health: None,
    };

    state.workers.insert(id.clone(), record);

    let status = if is_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(ApiResponse::ok(serde_json::json!({
            "worker_id": id,
            "registered": is_new,
        }))),
    )
        .into_response()
}

// ── POST /internal/workers/heartbeat ────────────────────────

async fn heartbeat(
    State(state): State<Arc<OrchestratorState>>,
    Json(hb): Json<WorkerHeartbeat>,
) -> Response {
    match state.workers.get_mut(&hb.worker_id) {
        Some(mut entry) => {
            let w = entry.value_mut();
            w.last_heartbeat = Utc::now();
            w.capacity = hb.capacity;
            if w.status == WorkerStatus::Dead {
                tracing::info!(worker_id = %w.id, "worker recovered from dead status via heartbeat");
                w.status = WorkerStatus::Active;
            }
            // Component snapshots are optional in the heartbeat (older
            // workers omit them). Only overwrite when the heartbeat carried
            // data, so a worker that briefly stops sending snapshots does
            // not appear to lose its components in the admin UI.
            if !hb.components.is_empty() {
                w.components = hb.components;
                w.aggregate_health = hb.aggregate_health;
            }
            Json(ApiResponse::ok(serde_json::json!({
                "worker_id": hb.worker_id,
                "acknowledged": true,
            })))
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Worker {} not registered. Call /internal/workers/register first.",
                hb.worker_id
            ))),
        )
            .into_response(),
    }
}

// ── POST /internal/workers/deregister ───────────────────────

/// A worker shutting down says so: it is marked dead at once, so nothing is
/// dispatched to it while the heartbeat timeout would still count it live.
/// The reaper removes the record later as for any dead worker; a heartbeat
/// from the same id (a restart) brings it back.
async fn deregister_worker(
    State(state): State<Arc<OrchestratorState>>,
    Json(req): Json<WorkerDeregistration>,
) -> Response {
    match state.workers.get_mut(&req.worker_id) {
        Some(mut entry) => {
            let w = entry.value_mut();
            w.status = WorkerStatus::Dead;
            w.capacity.active_sessions = 0;
            tracing::info!(worker_id = %w.id, "worker deregistered (shutting down)");
            Json(ApiResponse::ok(serde_json::json!({
                "worker_id": req.worker_id,
                "status": WorkerStatus::Dead,
            })))
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Worker {} not registered",
                req.worker_id
            ))),
        )
            .into_response(),
    }
}

// ── POST /internal/events/session ────────────────────────────

/// Receives session lifecycle events (start / end) from workers.
async fn session_lifecycle_event(
    State(state): State<Arc<OrchestratorState>>,
    Json(record): Json<SessionRecord>,
) -> Response {
    let session_id = record.id.clone();

    // If this is a session-end event (ended_at is set), merge with the
    // existing session so we preserve started_at and accumulated turn_count.
    if record.ended_at.is_some() {
        if let Some(mut existing) = state.sessions.find_session(&session_id).await {
            existing.status = record.status;
            existing.ended_at = record.ended_at;
            // Keep existing.started_at (the real session start time).
            // Keep existing.turn_count (accumulated by turn events).
            // Update any other fields that the end event might carry.
            if existing.pipeline_name.is_none() {
                existing.pipeline_name = record.pipeline_name;
            }
            if existing.pipeline_config_hash.is_none() {
                existing.pipeline_config_hash = record.pipeline_config_hash;
            }
            if existing.input_sample_rate.is_none() {
                existing.input_sample_rate = record.input_sample_rate;
            }
            if existing.output_sample_rate.is_none() {
                existing.output_sample_rate = record.output_sample_rate;
            }
            if existing.stt_provider.is_none() {
                existing.stt_provider = record.stt_provider;
            }
            if existing.tts_provider.is_none() {
                existing.tts_provider = record.tts_provider;
            }
            if existing.worker_id.is_empty() {
                existing.worker_id = record.worker_id;
            }
            // Merge tags: the end-event carries the full configure tags.
            for tag in record.tags {
                if !existing.tags.contains(&tag) {
                    existing.tags.push(tag);
                }
            }
            state.sessions.push_session(existing).await;
        } else {
            // No existing session found -- just store whatever we got.
            state.sessions.push_session(record).await;
        }
    } else {
        state.sessions.push_session(record).await;
    }

    (
        StatusCode::CREATED,
        Json(ApiResponse::ok(serde_json::json!({
            "session_id": session_id,
            "ingested": true,
        }))),
    )
        .into_response()
}

// ── POST /internal/events/turn ──────────────────────────────

async fn turn_event(
    State(state): State<Arc<OrchestratorState>>,
    Json(event): Json<SessionCompleteEvent>,
) -> Response {
    let turn_id = event.session.id.clone();
    let worker_id = event.worker_id.clone();

    let is_new = state.sessions.find(&event.session.id).await.is_none();

    if is_new {
        // Update worker stats. Capacity is not touched here: a session has
        // many turns, and the live count arrives with every heartbeat.
        if let Some(mut entry) = state.workers.get_mut(&worker_id) {
            let w = entry.value_mut();
            w.sessions_processed += 1;
            if event.session.status == SessionStatus::Error {
                w.errors_total += 1;
            }
        }

        // Update session turn_count if session exists.
        if !event.session.session_id.is_empty()
            && let Some(mut session) = state.sessions.find_session(&event.session.session_id).await
        {
            session.turn_count += 1;
            state.sessions.push_session(session).await;
        }
    }

    // Push into turn store
    state.sessions.push(event.session).await;

    (
        StatusCode::CREATED,
        Json(ApiResponse::ok(serde_json::json!({
            "turn_id": turn_id,
            "ingested": true,
        }))),
    )
        .into_response()
}

#[cfg(test)]
#[path = "tests/internal.rs"]
mod tests;
