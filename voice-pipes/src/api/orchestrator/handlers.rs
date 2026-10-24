use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;

use crate::api::types::{
    ApiError, ApiResponse, AuditFileItem, AuditFilesQuery, AuditMetrics, AuditTapItem, AuditTapsQuery,
    ComponentMetrics, DiffQuery, DiffResponse, DispatchQuery, DispatchResponse, DrainResponse,
    MetricsQuery, MetricsResponse, OrchestratorHealthResponse, OrchestratorState,
    PaginatedResponse, PipelineGraphResponse, PipelineGroup, PipelineGroupWorker,
    PipelineGroupsResponse, PipelineMetrics, PipelineQuery, SessionRecord, SessionStatus,
    SessionSummary, SessionWithTurns, SessionsQuery, TurnRecord, WorkerComponentsRollup,
    WorkerDetailResponse, WorkerFleetSummary, WorkerListItem, WorkerStatus, WorkersListResponse,
};

use super::helpers::{
    collect_diffs, compute_histogram, parse_audit_blobs, parse_tap_blobs, serve_blob, worker_counts,
};
use super::pipeline_graph::build_pipeline_graph;

// ── GET /api/v1/health ──────────────────────────────────────

pub async fn health(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<OrchestratorHealthResponse>> {
    let uptime = state.start_time.elapsed().as_secs();
    let (total, active, draining, dead) = worker_counts(&state);

    // COUNT-only query (limit 0): we need the number, not the records.
    let active_sessions = state
        .sessions
        .sessions_page(
            &crate::audit::SessionFilter {
                status: Some(SessionStatus::InProgress),
                ..Default::default()
            },
            0,
            0,
        )
        .await
        .1 as u64;

    let status = if dead > 0 || active == 0 {
        "degraded"
    } else {
        "healthy"
    };

    let resp = OrchestratorHealthResponse {
        status: status.to_string(),
        uptime_s: uptime,
        version: env!("CARGO_PKG_VERSION").to_string(),
        workers: WorkerFleetSummary {
            total,
            active,
            draining,
            dead,
        },
        sessions: SessionSummary {
            total_processed: state.sessions.total_processed().await,
            active: active_sessions,
            errors_total: state.sessions.errors_total().await,
        },
    };

    Json(ApiResponse::ok(resp))
}

// ── GET /healthz (liveness) ─────────────────────────────────

/// Kubernetes liveness probe -- returns 200 if the orchestrator process is
/// alive, 503 otherwise.
pub async fn orch_liveness() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "alive" })),
    )
}

// ── GET /readyz (readiness) ─────────────────────────────────

/// Kubernetes readiness probe -- returns 200 when at least one active worker
/// is registered and the orchestrator can route traffic.  Returns 503 when
/// no workers are available.
pub async fn orch_readiness(State(state): State<Arc<OrchestratorState>>) -> impl IntoResponse {
    let (_total, active, _draining, _dead) = worker_counts(&state);
    if active == 0 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "not_ready",
                "reason": "no active workers",
            })),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ready",
            "active_workers": active,
        })),
    )
}

// ── GET /api/v1/config ──────────────────────────────────────

pub async fn orch_config(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let val = serde_json::json!({ "orchestrator": state.config });
    Json(ApiResponse::ok(val))
}

// ── GET /api/v1/paths ──────────────────────────────────────────────

pub async fn paths_config(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let mut captures_dir = None;
    for entry in state.workers.iter() {
        if let Some(ref cfg) = entry.value().pipeline_config
            && let Some(dir) = cfg
                .get("pipeline")
                .and_then(|p| p.get("audit"))
                .and_then(|a| a.get("captures_dir"))
                .and_then(|d| d.as_str())
        {
            captures_dir = Some(dir.to_string());
            break;
        }
    }

    // Fallback if no worker is connected, guess based on cwd + config default (captures)
    let captures_dir = captures_dir.unwrap_or_else(|| {
        if let Ok(cwd) = std::env::current_dir() {
            cwd.join("captures").to_string_lossy().to_string()
        } else {
            "captures".to_string()
        }
    });

    let val = serde_json::json!({ "captures_dir": captures_dir });
    Json(ApiResponse::ok(val))
}

// ── GET /api/v1/config/pipeline ─────────────────────────────

pub async fn pipeline_config(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    // Return pipeline config from first registered worker
    let mut config_val = None;
    for entry in state.workers.iter() {
        if let Some(ref cfg) = entry.value().pipeline_config {
            config_val = Some(cfg.clone());
            break;
        }
    }

    match config_val {
        Some(mut val) => {
            // Defense-in-depth: workers register redacted
            // configs, but re-redact in case an older worker sent raw
            // keys.
            crate::config::redact::redact_secrets(&mut val);
            Json(ApiResponse::ok(val))
        }
        None => Json(ApiResponse::ok(serde_json::json!({
            "warning": "No workers registered yet"
        }))),
    }
}

// ── GET /api/v1/pipeline ────────────────────────────────────

/// Parse a worker's registered pipeline config, saying why when it fails.
///
/// A worker and the orchestrator are separate binaries that can be built from
/// different revisions, and [`crate::config::PipelineConfig`] is
/// `deny_unknown_fields` — so a config field added on the worker side *after*
/// the orchestrator was built makes the entire config unparseable here.
///
/// An empty graph would read in the console as "no workers" for a worker
/// that is registered and healthy, so the failure is logged and its reason
/// returned for the response's error. `Ok(None)` when the worker registered
/// no config.
pub(super) fn parse_worker_pipeline_config(
    worker_id: &str,
    raw: Option<&serde_json::Value>,
) -> Result<Option<crate::config::PipelineConfig>, String> {
    let Some(raw) = raw else { return Ok(None) };
    serde_json::from_value(raw.clone()).map(Some).map_err(|e| {
        tracing::warn!(
            worker_id,
            error = %e,
            "worker pipeline config did not parse — the live graph cannot be drawn \
             for this worker. Usually a version skew: the worker sends a field \
             this orchestrator build does not know. Rebuild the orchestrator."
        );
        format!(
            "worker {worker_id} registered a pipeline config this orchestrator cannot read ({e}). \
             This is usually a version skew: the worker was built from a newer revision \
             than the orchestrator. Rebuild and restart the orchestrator"
        )
    })
}

pub async fn pipeline_graph(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<PipelineQuery>,
) -> Json<ApiResponse<PipelineGraphResponse>> {
    // Build graph from a specific worker's config, or fall back to first worker
    let entry = match &q.worker_id {
        Some(wid) => state.workers.get(wid).map(|e| e.value().clone()),
        None => state.workers.iter().next().map(|e| e.value().clone()),
    };
    let (cfg, worker) = match entry {
        Some(v) => match parse_worker_pipeline_config(&v.id, v.pipeline_config.as_ref()) {
            Ok(cfg) => (cfg, Some(v)),
            Err(message) => {
                return Json(ApiResponse {
                    data: None,
                    error: Some(ApiError {
                        code: "WORKER_CONFIG_UNREADABLE".to_string(),
                        message,
                    }),
                });
            }
        },
        None => (None, None),
    };

    let mut graph = build_pipeline_graph(cfg.as_ref());

    if let Some(w) = worker {
        for node in &mut graph.nodes {
            if let Some(comp) = w.components.iter().find(|c| {
                c.kind.to_string().to_lowercase() == node.node_type || c.id.starts_with(&node.id)
            }) && let Some(schema) = &comp.param_schema
            {
                node.param_schema = schema.clone();
                for param in schema {
                    if let Some(obj) = node.metadata.as_object_mut()
                        && !obj.contains_key(&param.key)
                    {
                        // The admin UI skips rendering params missing from metadata.
                        // Insert a placeholder to force it to render the input control.
                        let placeholder = match param.value_type {
                            crate::api::param_schema::ParamValueType::Int => {
                                serde_json::json!(0)
                            }
                            crate::api::param_schema::ParamValueType::Float => {
                                serde_json::json!(0.0)
                            }
                            crate::api::param_schema::ParamValueType::Bool => {
                                serde_json::json!(false)
                            }
                            crate::api::param_schema::ParamValueType::Enum => {
                                if let Some(c) = &param.constraints {
                                    if let Some(opts) = &c.options {
                                        if opts.is_empty() {
                                            serde_json::json!("<remote>")
                                        } else {
                                            serde_json::json!(opts[0].clone())
                                        }
                                    } else {
                                        serde_json::json!("<remote>")
                                    }
                                } else {
                                    serde_json::json!("<remote>")
                                }
                            }
                            // Str, Path, and any type added later: a
                            // text input.
                            _ => serde_json::json!("<remote>"),
                        };
                        obj.insert(param.key.clone(), placeholder);
                    }
                }
            }
        }
    }

    Json(ApiResponse::ok(graph))
}

// ── GET /api/v1/metrics ─────────────────────────────────────

pub async fn metrics(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<MetricsQuery>,
) -> Json<ApiResponse<MetricsResponse>> {
    let window = q.window.unwrap_or_else(|| "1m".to_string());
    let window_dur = parse_window(&window).unwrap_or(chrono::Duration::minutes(1));
    let cutoff = Utc::now() - window_dur;
    let (_, active, _, _) = worker_counts(&state);

    // Only the turns inside the rolling window (and optionally one
    // worker) leave the store: the admin UI polls this, and the full table
    // is up to `max_entries` records with their events.
    let in_window = state
        .sessions
        .turns_since(cutoff, q.worker_id.as_deref())
        .await;
    let filtered: Vec<&TurnRecord> = in_window.iter().collect();

    // Compute latency stats from recent sessions
    let latencies: Vec<u64> = filtered
        .iter()
        .filter(|s| s.status == SessionStatus::Completed)
        .filter_map(|s| s.total_ms)
        .collect();

    let pipeline_metrics = PipelineMetrics {
        requests_total: filtered
            .iter()
            .filter(|s| s.status == SessionStatus::Completed)
            .count() as u64,
        requests_active: filtered
            .iter()
            .filter(|s| s.status == SessionStatus::InProgress)
            .count() as u64,
        errors_total: filtered
            .iter()
            .filter(|s| s.status == SessionStatus::Error)
            .count() as u64,
        latency_ms: compute_histogram(&latencies),
    };

    // Extract per-component latencies, skipping `None` so missing samples
    // don't pollute percentiles with `0` placeholders.
    let stt_latencies: Vec<u64> = filtered
        .iter()
        .filter_map(|s| s.detail.as_ref())
        .filter_map(|d| d.stt.as_ref())
        .filter_map(|stt| stt.latency_ms)
        .collect();

    let tts_latencies: Vec<u64> = filtered
        .iter()
        .filter_map(|s| s.detail.as_ref())
        .filter_map(|d| d.tts.as_ref())
        .filter_map(|tts| tts.latency_ms)
        .collect();

    let text_latencies: Vec<u64> = filtered
        .iter()
        .filter_map(|s| s.detail.as_ref())
        .filter_map(|d| d.text_latency_ms)
        .collect();

    let resp = MetricsResponse {
        timestamp: Utc::now(),
        window,
        workers_active: active,
        pipeline: pipeline_metrics,
        stt: ComponentMetrics {
            latency_ms: compute_histogram(&stt_latencies),
        },
        text: ComponentMetrics {
            latency_ms: compute_histogram(&text_latencies),
        },
        tts: ComponentMetrics {
            latency_ms: compute_histogram(&tts_latencies),
        },
        audit: AuditMetrics {
            records_total: 0,
            records_dropped: 0,
        },
        components: collect_components_rollup(&state, q.worker_id.as_deref()),
    };

    Json(ApiResponse::ok(resp))
}

/// Parse a rolling-window string like `"30s"`, `"5m"`, `"1h"` into a
/// `chrono::Duration`.  Returns `None` when the input is malformed so the
/// caller can fall back to a sensible default.
fn parse_window(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Split off the last *character*, not the last byte: a multi-byte
    // final character is not a char boundary one byte from the end.
    let unit = s.chars().last()?;
    let num = &s[..s.len() - unit.len_utf8()];
    let n: i64 = num.parse().ok()?;
    if n < 0 {
        return None;
    }
    match unit {
        's' => Some(chrono::Duration::seconds(n)),
        'm' => Some(chrono::Duration::minutes(n)),
        'h' => Some(chrono::Duration::hours(n)),
        _ => None,
    }
}

/// Offset of a 1-based page, saturating so an absurd `page` from the
/// query yields an empty page instead of an overflow.
fn page_offset(page: usize, per_page: usize) -> usize {
    page.saturating_sub(1).saturating_mul(per_page)
}

// ── GET /api/v1/workers ─────────────────────────────────────

pub async fn list_workers(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<WorkersListResponse>> {
    let items: Vec<WorkerListItem> = state
        .workers
        .iter()
        .map(|entry| {
            let w = entry.value();
            WorkerListItem {
                id: w.id.clone(),
                address: w.address.clone(),
                registered_at: w.registered_at,
                last_heartbeat: w.last_heartbeat,
                status: w.status,
                capacity: w.capacity.clone(),
                pipeline_config_hash: w.pipeline_config_hash.clone(),
                pipeline_name: w.pipeline_name.clone(),
                version: w.version.clone(),
                tags: w.tags.clone(),
            }
        })
        .collect();

    let total = items.len();
    Json(ApiResponse::ok(WorkersListResponse { total, items }))
}

// ── GET /api/v1/workers/{id} ────────────────────────────────

pub async fn get_worker(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match state.workers.get(&id) {
        Some(entry) => {
            let w = entry.value();
            let resp = WorkerDetailResponse {
                id: w.id.clone(),
                address: w.address.clone(),
                registered_at: w.registered_at,
                last_heartbeat: w.last_heartbeat,
                status: w.status,
                capacity: w.capacity.clone(),
                pipeline_config_hash: w.pipeline_config_hash.clone(),
                pipeline_name: w.pipeline_name.clone(),
                // Defense-in-depth: re-redact for configs
                // registered by older workers that sent raw keys.
                pipeline_config: w.pipeline_config.clone().map(|mut c| {
                    crate::config::redact::redact_secrets(&mut c);
                    c
                }),
                version: w.version.clone(),
                sessions_processed: w.sessions_processed,
                errors_total: w.errors_total,
                tags: w.tags.clone(),
                transport_address: w.transport_address.clone(),
                public_transport_address: w.public_transport_address.clone(),
                components: w.components.clone(),
                aggregate_health: w.aggregate_health,
            };
            Json(ApiResponse::ok(resp)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Worker {id} not found"
            ))),
        )
            .into_response(),
    }
}

// ── GET /api/v1/workers/{id}/components ─────────────────────

/// Per-worker component snapshot, sourced from the most recent heartbeat.
/// Empty `components` + `aggregate_health: None` means the worker has not
/// yet sent component data (e.g. an old binary, or a worker that just
/// registered and hasn't ticked the heartbeat loop yet).
pub async fn get_worker_components(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match state.workers.get(&id) {
        Some(entry) => {
            let w = entry.value();
            let resp = WorkerComponentsRollup {
                worker_id: w.id.clone(),
                aggregate_health: w.aggregate_health,
                components: w.components.clone(),
            };
            Json(ApiResponse::ok(resp)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Worker {id} not found"
            ))),
        )
            .into_response(),
    }
}

// ── GET /api/v1/components ──────────────────────────────────

/// Rolled-up view of every worker's component snapshots. Drives the admin
/// UI's fleet-wide health panel without N follow-up requests. Workers that
/// have not yet reported components are still included with empty data so
/// the UI can show "no data yet" instead of dropping the row.
pub async fn list_components(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<Vec<WorkerComponentsRollup>>> {
    let mut items: Vec<WorkerComponentsRollup> = state
        .workers
        .iter()
        .map(|entry| {
            let w = entry.value();
            WorkerComponentsRollup {
                worker_id: w.id.clone(),
                aggregate_health: w.aggregate_health,
                components: w.components.clone(),
            }
        })
        .collect();
    // Stable order so polling consumers don't see rows shuffle.
    items.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
    Json(ApiResponse::ok(items))
}

/// Build the per-worker component rollup used by `MetricsResponse`. When
/// `worker_filter` is `Some`, only that worker is included (matches the
/// `?worker_id=` query already supported by `/api/v1/metrics`).
fn collect_components_rollup(
    state: &Arc<OrchestratorState>,
    worker_filter: Option<&str>,
) -> Vec<WorkerComponentsRollup> {
    let mut items: Vec<WorkerComponentsRollup> = state
        .workers
        .iter()
        .filter(|entry| match worker_filter {
            Some(wid) => entry.value().id == wid,
            None => true,
        })
        // Skip workers that have not reported any components yet so the
        // metrics payload doesn't carry empty noise.
        .filter(|entry| !entry.value().components.is_empty())
        .map(|entry| {
            let w = entry.value();
            WorkerComponentsRollup {
                worker_id: w.id.clone(),
                aggregate_health: w.aggregate_health,
                components: w.components.clone(),
            }
        })
        .collect();
    items.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
    items
}

// ── POST /api/v1/workers/{id}/drain ─────────────────────────

pub async fn drain_worker(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match state.workers.get_mut(&id) {
        Some(mut entry) => {
            let w = entry.value_mut();
            w.status = WorkerStatus::Draining;
            let resp = DrainResponse {
                worker_id: w.id.clone(),
                status: WorkerStatus::Draining,
                active_sessions_remaining: w.capacity.active_sessions,
            };
            Json(ApiResponse::ok(resp)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Worker {id} not found"
            ))),
        )
            .into_response(),
    }
}

// ── PATCH /api/v1/workers/{id}/params ───────────────────────

/// Transparent proxy: forwards the body to the worker's `PATCH /debug/params`.
///
/// The orchestrator does not validate or transform the body -- Layer 2's
/// schema-driven validation runs inside the worker (and on the component
/// server in the gRPC case).
pub async fn proxy_worker_params(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let address = match state.workers.get(&id) {
        Some(entry) => entry.value().address.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::not_found(format!(
                    "Worker {id} not found"
                ))),
            )
                .into_response();
        }
    };

    // Worker debug API listens on `address`; rewrite to a connectable host.
    let connect_host = connectable_addr(&address);
    let url = format!("http://{connect_host}/debug/params");

    // The worker debug surface enforces the shared internal credential;
    // attach it so proxying keeps working when auth is on.
    let internal_auth = crate::api::auth::SurfaceAuth::resolve(
        &state.config.auth.internal_token_env,
        "worker debug proxy",
    );
    let client = match crate::api::auth::http_client_with_bearer_and_timeout(
        &internal_auth,
        std::time::Duration::from_secs(state.config.workers.proxy_timeout_s),
    ) {
        Ok(client) => client,
        Err(e) => return fleet_client_unavailable(&e),
    };
    let result = client.patch(&url).json(&body).send().await;
    match result {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            (status, [("content-type", "application/json")], bytes).into_response()
        }
        Err(e) => {
            // The error's Display carries the worker URL; the caller gets
            // the kind of failure, the log gets the detail.
            let kind = if e.is_timeout() {
                "timed out"
            } else if e.is_connect() {
                "connection refused"
            } else {
                "request failed"
            };
            tracing::warn!(worker_id = %id, error = %e, "params proxy failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(ApiResponse::<()>::internal_error(format!(
                    "worker {id} unreachable: {kind}"
                ))),
            )
                .into_response()
        }
    }
}

/// Relay a worker's answer to a probe request, which is JSON by contract.
///
/// A worker built before the probe endpoints existed answers them with its
/// router's bare 404: no body, not JSON. Passed through under a JSON content
/// type, that reaches the console as a parse failure that says nothing about
/// the worker. Anything that is not JSON is replaced here with an error that
/// names the worker and the cause.
fn probe_json_response(worker_id: &str, status: StatusCode, body: axum::body::Bytes) -> Response {
    if serde_json::from_slice::<serde_json::Value>(&body).is_ok() {
        return (status, [("content-type", "application/json")], body).into_response();
    }
    if status == StatusCode::NOT_FOUND {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(ApiResponse::<()>::error(
                "PROBE_UNSUPPORTED",
                format!(
                    "Worker {worker_id} has no probe endpoints: it is running a build from before they were added. Rebuild and restart that worker."
                ),
            )),
        )
            .into_response();
    }
    (
        StatusCode::BAD_GATEWAY,
        Json(ApiResponse::<()>::internal_error(format!(
            "worker {worker_id} answered the probe with HTTP {} and a body that is not JSON",
            status.as_u16()
        ))),
    )
        .into_response()
}

// ── GET /api/v1/workers/{id}/probe/fixtures ─────────────────

/// Transparent proxy to a worker's `GET /debug/probe/fixtures`.
///
/// Separate from the POST proxy below because it takes no body and reads the
/// worker's committed manifest, which is what stops a caller's fixture list
/// drifting from the fixtures that actually exist.
pub async fn proxy_worker_probe_fixtures(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let address = match state.workers.get(&id) {
        Some(entry) => entry.value().address.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::not_found(format!(
                    "Worker {id} not found"
                ))),
            )
                .into_response();
        }
    };
    let url = format!("http://{}/debug/probe/fixtures", connectable_addr(&address));
    let internal_auth = crate::api::auth::SurfaceAuth::resolve(
        &state.config.auth.internal_token_env,
        "worker probe proxy",
    );
    let client = match crate::api::auth::http_client_with_bearer_and_timeout(
        &internal_auth,
        std::time::Duration::from_secs(state.config.workers.proxy_timeout_s),
    ) {
        Ok(client) => client,
        Err(e) => return fleet_client_unavailable(&e),
    };
    match client.get(&url).send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            probe_json_response(&id, status, bytes)
        }
        Err(e) => {
            tracing::warn!(worker_id = %id, error = %e, "fixture list proxy failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(ApiResponse::<()>::internal_error(format!(
                    "worker {id} unreachable"
                ))),
            )
                .into_response()
        }
    }
}

// ── GET /api/v1/workers/{id}/probe/audio/{name} ─────────────

/// Transparent proxy for audio a worker's TTS probe synthesised.
///
/// Unlike the JSON proxies above, this passes the worker's content type
/// through rather than stamping `application/json` on it: the body is a WAV,
/// and relabelling it would make a browser refuse to play it.
pub async fn proxy_worker_probe_audio(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path((id, name)): axum::extract::Path<(String, String)>,
) -> Response {
    let address = match state.workers.get(&id) {
        Some(entry) => entry.value().address.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::not_found(format!(
                    "Worker {id} not found"
                ))),
            )
                .into_response();
        }
    };
    // The worker validates the name against the one shape its probe writes;
    // this layer only refuses separators so the name cannot become a path.
    if name.contains(['/', '\\']) || name.contains("..") {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::bad_request("invalid audio name")),
        )
            .into_response();
    }
    let url = format!(
        "http://{}/debug/probe/audio/{name}",
        connectable_addr(&address)
    );
    let internal_auth = crate::api::auth::SurfaceAuth::resolve(
        &state.config.auth.internal_token_env,
        "worker probe proxy",
    );
    let client = match crate::api::auth::http_client_with_bearer_and_timeout(
        &internal_auth,
        std::time::Duration::from_secs(state.config.workers.proxy_timeout_s),
    ) {
        Ok(client) => client,
        Err(e) => return fleet_client_unavailable(&e),
    };
    match client.get(&url).send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();
            let bytes = resp.bytes().await.unwrap_or_default();
            (status, [("content-type", content_type)], bytes).into_response()
        }
        Err(e) => {
            tracing::warn!(worker_id = %id, error = %e, "probe audio proxy failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(ApiResponse::<()>::internal_error(format!(
                    "worker {id} unreachable"
                ))),
            )
                .into_response()
        }
    }
}

// ── POST /api/v1/workers/{id}/probe/{kind} ──────────────────

/// Transparent proxy to a worker's `POST /debug/probe/{kind}`.
///
/// Same shape as the params proxy above and for the same reason: the worker
/// holds the components and the credential, and a browser has neither. The
/// body passes through untouched -- it is a WAV for the audio probes and JSON
/// for the text ones, and the orchestrator has no business reading either.
///
/// `kind` is restricted here rather than forwarded blindly: it is caller input
/// that becomes part of a URL, and an open proxy into a worker's debug surface
/// is a larger thing than a probe.
pub async fn proxy_worker_probe(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path((id, kind)): axum::extract::Path<(String, String)>,
    query: Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    const KINDS: [&str; 4] = ["vad", "stt", "tts", "llm"];
    if !KINDS.contains(&kind.as_str()) {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "no probe named {kind:?}; expected one of {}",
                KINDS.join(", ")
            ))),
        )
            .into_response();
    }

    let address = match state.workers.get(&id) {
        Some(entry) => entry.value().address.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::not_found(format!(
                    "Worker {id} not found"
                ))),
            )
                .into_response();
        }
    };

    let connect_host = connectable_addr(&address);
    let url = format!("http://{connect_host}/debug/probe/{kind}");

    let internal_auth = crate::api::auth::SurfaceAuth::resolve(
        &state.config.auth.internal_token_env,
        "worker probe proxy",
    );
    // A probe may load a model on its first call, which outlasts the ordinary
    // proxy timeout; the worker caps its own run and frees the permit.
    let client = match crate::api::auth::http_client_with_bearer_and_timeout(
        &internal_auth,
        std::time::Duration::from_mins(3),
    ) {
        Ok(client) => client,
        Err(e) => return fleet_client_unavailable(&e),
    };

    let content_type = if kind == "vad" || kind == "stt" {
        "audio/wav"
    } else {
        "application/json"
    };
    let result = client
        .post(&url)
        .query(&query.0)
        .header("content-type", content_type)
        .body(body)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            probe_json_response(&id, status, bytes)
        }
        Err(e) => {
            let kind_of_failure = if e.is_timeout() {
                "timed out"
            } else if e.is_connect() {
                "connection refused"
            } else {
                "request failed"
            };
            tracing::warn!(worker_id = %id, probe = %kind, error = %e, "probe proxy failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(ApiResponse::<()>::internal_error(format!(
                    "worker {id} unreachable: {kind_of_failure}"
                ))),
            )
                .into_response()
        }
    }
}

// ── GET /api/v1/sessions ────────────────────────────────────

pub async fn list_sessions(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<SessionsQuery>,
) -> Json<ApiResponse<PaginatedResponse<SessionRecord>>> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(50).min(200);

    let status_filter = q.status.as_deref().and_then(|s| match s {
        "completed" => Some(SessionStatus::Completed),
        "error" => Some(SessionStatus::Error),
        "in_progress" => Some(SessionStatus::InProgress),
        _ => None,
    });

    // `since`/`until` accept RFC3339 timestamps; unparseable values are
    // ignored (a filter that errors would otherwise blank the whole list).
    let parse_ts = |s: Option<&str>| {
        s.and_then(|v| {
            chrono::DateTime::parse_from_rfc3339(v)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|e| tracing::warn!(value = v, error = %e, "ignoring bad time filter"))
                .ok()
        })
    };

    // Filters and paging are pushed down to the store (SQL for the
    // sqlite store), so a page view never loads the whole table.
    let filter = crate::audit::SessionFilter {
        status: status_filter,
        worker_id: q.worker_id,
        pipeline_name: q.pipeline_name,
        pipeline_hash_prefix: q.pipeline_hash,
        tag: q.tag,
        exclude_tag: q.exclude_tag,
        min_turns: q.min_turns,
        started_after: parse_ts(q.since.as_deref()),
        started_before: parse_ts(q.until.as_deref()),
    };
    let (items, total) = state
        .sessions
        .sessions_page(&filter, per_page, page_offset(page, per_page))
        .await;

    Json(ApiResponse::ok(PaginatedResponse {
        total,
        page,
        per_page,
        items,
    }))
}

// ── GET /api/v1/sessions/{id} ───────────────────────────────

pub async fn get_session(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match state.sessions.find_session(&id).await {
        Some(session) => {
            let turns = state.sessions.turns_for_session(&id).await;
            Json(ApiResponse::ok(SessionWithTurns { session, turns })).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Session {id} not found"
            ))),
        )
            .into_response(),
    }
}

// ── GET /api/v1/sessions/{id}/turns ─────────────────────────

pub async fn session_turns(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match state.sessions.find_session(&id).await {
        Some(_) => {
            let turns = state.sessions.turns_for_session(&id).await;
            Json(ApiResponse::ok(turns)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Session {id} not found"
            ))),
        )
            .into_response(),
    }
}

// ── GET /api/v1/turns ───────────────────────────────────────

pub async fn list_turns(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<SessionsQuery>,
) -> Json<ApiResponse<PaginatedResponse<TurnRecord>>> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(50).min(200);

    let status = q.status.as_deref().and_then(|s| match s {
        "completed" => Some(SessionStatus::Completed),
        "error" => Some(SessionStatus::Error),
        "in_progress" => Some(SessionStatus::InProgress),
        _ => None,
    });
    // Filters and paging are the store's job, so a page view never clones
    // the whole turn table.
    let filter = crate::audit::TurnFilter {
        status,
        worker_id: q.worker_id,
        search: q.search,
        pipeline_name: q.pipeline_name,
        pipeline_hash_prefix: q.pipeline_hash,
        tag: q.tag,
        exclude_tag: q.exclude_tag,
    };
    let (items, total) = state
        .sessions
        .turns_page(&filter, per_page, page_offset(page, per_page))
        .await;

    Json(ApiResponse::ok(PaginatedResponse {
        total,
        page,
        per_page,
        items,
    }))
}

// ── GET /api/v1/turns/{id} ──────────────────────────────────

pub async fn get_turn(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match state.sessions.find(&id).await {
        Some(turn) => Json(ApiResponse::ok(turn)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!("Turn {id} not found"))),
        )
            .into_response(),
    }
}

// ── GET /api/v1/dispatch ────────────────────────────────────

pub async fn dispatch(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<DispatchQuery>,
) -> Response {
    let has_filter = q.pipeline_name.is_some() || q.pipeline_hash.is_some() || q.tags.is_some();
    if state.config.dispatch.require_pipeline_match && !has_filter {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::bad_request(
                "dispatch requires at least one pipeline filter (pipeline_name, pipeline_hash, or tags)"
            )),
        ).into_response();
    }

    let required_tags: Vec<String> = q
        .tags
        .as_deref()
        .map(|t| {
            t.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let policy = q.policy.as_deref().unwrap_or(&state.config.dispatch.policy);

    // Collect eligible workers
    #[expect(
        clippy::type_complexity,
        reason = "candidate tuple is consumed a few lines below; an alias would relocate, not simplify"
    )]
    let mut candidates: Vec<(String, u32, Option<String>, String, Option<String>)> = Vec::new();
    for entry in state.workers.iter() {
        let w = entry.value();
        if w.status != WorkerStatus::Active {
            continue;
        }
        if w.capacity.active_sessions >= w.capacity.max_concurrent {
            continue;
        }
        if let Some(ref name) = q.pipeline_name
            && w.pipeline_name.as_deref() != Some(name.as_str())
        {
            continue;
        }
        if let Some(ref hash) = q.pipeline_hash
            && !w.pipeline_config_hash.starts_with(hash.as_str())
        {
            continue;
        }
        if !required_tags.is_empty() && !required_tags.iter().all(|t| w.tags.contains(t)) {
            continue;
        }
        // Browsers get the public address when the worker registered one;
        // otherwise the internal bind address, made connectable.
        let raw_addr = w
            .public_transport_address
            .as_deref()
            .or(w.transport_address.as_deref())
            .unwrap_or(&w.address);
        let addr = connectable_addr(raw_addr);
        let ws_url = format!("ws://{addr}/ws");
        candidates.push((
            w.id.clone(),
            w.capacity.active_sessions,
            w.pipeline_name.clone(),
            w.pipeline_config_hash.clone(),
            Some(ws_url),
        ));
    }

    if candidates.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<()>::internal_error(
                "no available workers matching criteria",
            )),
        )
            .into_response();
    }

    // Apply routing policy
    let selected = match policy {
        "round-robin" | "random" => {
            // Simple random selection for both (true round-robin needs state)
            let idx = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as usize)
                % candidates.len();
            &candidates[idx]
        }
        _ => {
            // Default: least-connections
            candidates.iter().min_by_key(|c| c.1).unwrap()
        }
    };

    let resp = DispatchResponse {
        worker_id: selected.0.clone(),
        ws_url: selected.4.clone().unwrap_or_default(),
        pipeline_name: selected.2.clone(),
        pipeline_config_hash: selected.3.clone(),
    };
    Json(ApiResponse::ok(resp)).into_response()
}

// ── GET /api/v1/pipelines ───────────────────────────────────

pub async fn list_pipelines(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<PipelineGroupsResponse>> {
    let mut groups: std::collections::HashMap<String, PipelineGroup> =
        std::collections::HashMap::new();

    for entry in state.workers.iter() {
        let w = entry.value();
        let group = groups
            .entry(w.pipeline_config_hash.clone())
            .or_insert_with(|| PipelineGroup {
                pipeline_name: w.pipeline_name.clone(),
                pipeline_config_hash: w.pipeline_config_hash.clone(),
                workers: Vec::new(),
                total_capacity: 0,
                total_active: 0,
                sessions_processed: 0,
            });
        group.workers.push(PipelineGroupWorker {
            id: w.id.clone(),
            status: w.status,
            active_sessions: w.capacity.active_sessions,
            max_concurrent: w.capacity.max_concurrent,
        });
        group.total_capacity += w.capacity.max_concurrent;
        group.total_active += w.capacity.active_sessions;
        group.sessions_processed += w.sessions_processed;
    }

    let groups: Vec<PipelineGroup> = groups.into_values().collect();
    Json(ApiResponse::ok(PipelineGroupsResponse { groups }))
}

// ── GET /api/v1/pipelines/diff ──────────────────────────────

pub async fn pipeline_diff(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<DiffQuery>,
) -> Response {
    // Find configs for hash_a and hash_b
    let config_a = state
        .workers
        .iter()
        .find(|e| e.value().pipeline_config_hash.starts_with(&q.hash_a))
        .and_then(|e| e.value().pipeline_config.clone());
    let config_b = state
        .workers
        .iter()
        .find(|e| e.value().pipeline_config_hash.starts_with(&q.hash_b))
        .and_then(|e| e.value().pipeline_config.clone());

    let (Some(mut a), Some(mut b)) = (config_a, config_b) else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(
                "one or both pipeline hashes not found among registered workers",
            )),
        )
            .into_response();
    };

    // Never diff raw credentials — redact both sides first so
    // a key rotation shows as `***REDACTED*** == ***REDACTED***`
    // (no diff) rather than leaking either value.
    crate::config::redact::redact_secrets(&mut a);
    crate::config::redact::redact_secrets(&mut b);

    let mut diffs = Vec::new();
    collect_diffs("", &a, &b, &mut diffs);

    Json(ApiResponse::ok(DiffResponse {
        hash_a: q.hash_a,
        hash_b: q.hash_b,
        diffs,
    }))
    .into_response()
}

// ── GET /api/v1/audit/files ─────────────────────────────────

pub async fn audit_files(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<AuditFilesQuery>,
) -> Json<ApiResponse<PaginatedResponse<AuditFileItem>>> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(50).min(200);

    let blobs = state.audit_store.list("").await.unwrap_or_default();
    let items = parse_audit_blobs(&blobs, q.session_id.as_deref(), q.file_type.as_deref());
    let total = items.len();
    let start = page_offset(page, per_page);
    let paged: Vec<_> = items.into_iter().skip(start).take(per_page).collect();

    Json(ApiResponse::ok(PaginatedResponse {
        total,
        page,
        per_page,
        items: paged,
    }))
}

// ── GET /api/v1/audit/files/{filename} ──────────────────────

pub async fn audit_file_download(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(filename): axum::extract::Path<String>,
) -> Response {
    serve_blob(&*state.audit_store, &filename).await
}

// ── GET /api/v1/audit/taps ──────────────────────────────────

pub async fn audit_taps(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<AuditTapsQuery>,
) -> Json<ApiResponse<PaginatedResponse<AuditTapItem>>> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(50).min(200);

    // Use the orchestrator's tap store, falling back to the audit store.
    let store = state.tap_store.as_ref().unwrap_or(&state.audit_store);
    let blobs = store.list("").await.unwrap_or_default();
    let items = parse_tap_blobs(&blobs, q.label.as_deref(), q.tag.as_deref());
    let total = items.len();
    let start = page_offset(page, per_page);
    let paged: Vec<_> = items.into_iter().skip(start).take(per_page).collect();

    Json(ApiResponse::ok(PaginatedResponse {
        total,
        page,
        per_page,
        items: paged,
    }))
}

// ── GET /api/v1/audit/taps/{filename} ───────────────────────

pub async fn audit_tap_download(
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(filename): axum::extract::Path<String>,
) -> Response {
    let store = state.tap_store.as_ref().unwrap_or(&state.audit_store);
    serve_blob(&**store, &filename).await
}

// ── Address helpers ─────────────────────────────────────────

/// Rewrite a worker *listen* address into one the orchestrator can
/// *connect* to: `0.0.0.0` and `[::]` are bind wildcards, not routable hosts
/// (connecting to one fails on Windows and is implementation-defined
/// elsewhere), so each becomes loopback. Other hosts pass through
/// unchanged. Only the whole host is compared, so `10.0.0.0:8081` stays
/// what it is.
fn connectable_addr(listen_addr: &str) -> String {
    match listen_addr.rsplit_once(':') {
        Some(("0.0.0.0", port)) => format!("127.0.0.1:{port}"),
        Some(("[::]", port)) => format!("[::1]:{port}"),
        _ => listen_addr.to_string(),
    }
}

/// The answer when the orchestrator cannot build its client for a worker:
/// the reason goes to the log, the caller learns only that it is the
/// orchestrator's own configuration that failed.
fn fleet_client_unavailable(
    error: &crate::api::auth::FleetClientError,
) -> axum::response::Response {
    tracing::error!(error = %error, "cannot build the client for worker requests");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiResponse::<()>::internal_error(
            "the orchestrator cannot build its client for worker requests; see its log".to_string(),
        )),
    )
        .into_response()
}

#[cfg(test)]
#[path = "tests/handlers.rs"]
mod tests;
