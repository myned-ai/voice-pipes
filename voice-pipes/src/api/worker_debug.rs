//! Worker debug API -- health, metrics, and diagnostic endpoints.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};

use super::types::{
    ApiResponse, AuditFileItem, AuditFilesQuery, ComponentSnapshot, ComponentStatus,
    ComponentsResponse, PaginatedResponse, ProvidersResponse, SessionsQuery, SttProviderInfo,
    TtsProviderInfo, TurnRecord, WorkerComponents, WorkerHealthResponse, WorkerState,
};
use crate::api::orchestrator::helpers::{parse_audit_blobs, serve_blob};
use crate::api::param_schema::ParamError;

// ── Router ──────────────────────────────────────────────────

/// Build the worker debug HTTP router.
pub fn router(state: Arc<WorkerState>) -> Router {
    Router::new()
        .route("/debug/health", get(health))
        .route("/debug/config", get(config))
        .route("/debug/providers", get(providers))
        .route("/debug/components", get(components))
        .route("/debug/sessions", get(sessions))
        .route("/debug/audit/files", get(audit_files))
        .route("/debug/audit/files/{filename}", get(audit_file_download))
        .route("/debug/params", patch(patch_params))
        // Component probes: run one stage against a known input and report
        // what came out. They accept caller-supplied audio and text and run
        // real models on it, which is why they live here behind the internal
        // token and never on the public transport port.
        .route(
            "/debug/probe/fixtures",
            get(crate::api::probe::probe_fixtures),
        )
        .route(
            "/debug/probe/audio/{name}",
            get(crate::api::probe::probe_audio),
        )
        .route("/debug/probe/vad", post(crate::api::probe::probe_vad))
        .route("/debug/probe/stt", post(crate::api::probe::probe_stt))
        .route("/debug/probe/tts", post(crate::api::probe::probe_tts))
        .route("/debug/probe/llm", post(crate::api::probe::probe_llm))
        .with_state(state)
}

// ── GET /debug/health ───────────────────────────────────────

async fn health(State(state): State<Arc<WorkerState>>) -> Json<ApiResponse<WorkerHealthResponse>> {
    use crate::component::{ComponentKind, HealthState};

    let uptime = state.start_time.elapsed().as_secs();

    // Pull live snapshots from the component registry. For each pipeline kind
    // we surface the first registered component (there is at most one per
    // worker for stt/tts/audio today).
    let snapshot = state.component_registry.snapshot();
    let find = |kind: ComponentKind| {
        snapshot
            .iter()
            .find(|c| c.kind() == kind)
            .map(|c| (c.id().to_string(), c.health(), c.telemetry()))
    };

    fn state_str(s: HealthState) -> &'static str {
        match s {
            HealthState::Healthy => "healthy",
            HealthState::Degraded => "degraded",
            HealthState::Unhealthy => "unhealthy",
            HealthState::NotConfigured => "not_configured",
            _ => "unknown",
        }
    }

    let to_status =
        |kind: ComponentKind, configured: bool, fallback_provider: Option<String>| match find(kind)
        {
            Some((id, h, t)) => ComponentStatus {
                status: state_str(h.state).to_string(),
                provider: Some(id),
                records_written: Some(t.processed_total),
                records_dropped: Some(t.errors_total),
            },
            None => ComponentStatus {
                status: if configured {
                    "ready"
                } else {
                    "not_configured"
                }
                .to_string(),
                provider: fallback_provider,
                records_written: None,
                records_dropped: None,
            },
        };

    let stt = to_status(
        ComponentKind::Stt,
        state.config.stt.is_some(),
        state.config.stt.as_ref().map(|s| s.provider.clone()),
    );
    let tts = to_status(
        ComponentKind::Tts,
        state.config.tts.is_some(),
        state.config.tts.as_ref().map(|t| t.provider.clone()),
    );
    let vad = to_status(ComponentKind::Vad, true, Some(state.vad_backend.clone()));

    let audit_status = if state.config.pipeline.audit.enabled {
        "active"
    } else {
        "not_configured"
    };

    let aggregate = state.component_registry.aggregate_health();
    let top_status = match aggregate {
        HealthState::Unhealthy => "unhealthy",
        HealthState::Healthy | HealthState::NotConfigured => "healthy",
        // Degraded, or a state this build does not know: never "healthy".
        _ => "degraded",
    };

    let resp = WorkerHealthResponse {
        status: top_status.to_string(),
        worker_id: state.worker_id.clone(),
        uptime_s: uptime,
        components: WorkerComponents {
            stt,
            tts,
            vad,
            audit: ComponentStatus {
                status: audit_status.to_string(),
                provider: None,
                records_written: None,
                records_dropped: None,
            },
            otel: ComponentStatus {
                status: "unknown".to_string(),
                provider: None,
                records_written: None,
                records_dropped: None,
            },
        },
    };

    Json(ApiResponse::ok(resp))
}

// ── GET /debug/config ───────────────────────────────────────

async fn config(State(state): State<Arc<WorkerState>>) -> Response {
    // Blank secret-like values — this endpoint serves config
    // *shape* for the admin UI, never credentials.
    match crate::config::redact::to_redacted_json(&state.config) {
        Ok(val) => Json(ApiResponse::ok(val)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "config serialisation failed: {e}"
            ))),
        )
            .into_response(),
    }
}

// ── GET /debug/providers ───────────────────────────────────

async fn providers(State(state): State<Arc<WorkerState>>) -> Json<ApiResponse<ProvidersResponse>> {
    let facts = state.provider_facts.get().cloned().unwrap_or_default();
    let stt_info = state.config.stt.as_ref().map(|s| SttProviderInfo {
        name: s.provider.clone(),
        expected_sample_rate: facts.stt_sample_rate.unwrap_or(16000),
        capabilities: facts.stt_capabilities.clone(),
    });

    let tts_info = state.config.tts.as_ref().map(|t| TtsProviderInfo {
        name: t.provider.clone(),
        native_sample_rate: facts.tts_sample_rate.unwrap_or(22050),
    });

    Json(ApiResponse::ok(ProvidersResponse {
        stt: stt_info,
        tts: tts_info,
        vad: state.vad_info.clone(),
    }))
}

// ── GET /debug/components ───────────────────────────────────

async fn components(
    State(state): State<Arc<WorkerState>>,
) -> Json<ApiResponse<ComponentsResponse>> {
    let snap = state.component_registry.snapshot();
    let aggregate_health = state.component_registry.aggregate_health();
    let components = snap
        .into_iter()
        .map(|c| ComponentSnapshot {
            id: c.id().to_string(),
            kind: c.kind(),
            health: c.health(),
            telemetry: c.telemetry(),
            param_schema: c.dynamic_schema(),
        })
        .collect();
    Json(ApiResponse::ok(ComponentsResponse {
        aggregate_health,
        components,
    }))
}

// ── GET /debug/sessions ─────────────────────────────────────

async fn sessions(
    State(state): State<Arc<WorkerState>>,
    Query(q): Query<SessionsQuery>,
) -> Json<ApiResponse<PaginatedResponse<TurnRecord>>> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(50).min(200);

    let store = state.local_sessions.read();
    let filtered: Vec<_> = store.iter().cloned().collect();
    let total = filtered.len();
    let start = page.saturating_sub(1).saturating_mul(per_page);
    let items: Vec<_> = filtered.into_iter().skip(start).take(per_page).collect();

    Json(ApiResponse::ok(PaginatedResponse {
        total,
        page,
        per_page,
        items,
    }))
}

// ── GET /debug/audit/files ──────────────────────────────────

async fn audit_files(
    State(state): State<Arc<WorkerState>>,
    Query(q): Query<AuditFilesQuery>,
) -> Json<ApiResponse<PaginatedResponse<AuditFileItem>>> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(50).min(200);

    let Some(store) = &state.audit_store else {
        return Json(ApiResponse::ok(PaginatedResponse {
            total: 0,
            page,
            per_page,
            items: vec![],
        }));
    };

    let blobs = store.list("").await.unwrap_or_default();
    let items = parse_audit_blobs(&blobs, q.session_id.as_deref(), q.file_type.as_deref());
    let total = items.len();
    let start = page.saturating_sub(1).saturating_mul(per_page);
    let paged: Vec<_> = items.into_iter().skip(start).take(per_page).collect();

    Json(ApiResponse::ok(PaginatedResponse {
        total,
        page,
        per_page,
        items: paged,
    }))
}

// ── GET /debug/audit/files/{filename} ───────────────────────

async fn audit_file_download(
    State(state): State<Arc<WorkerState>>,
    axum::extract::Path(filename): axum::extract::Path<String>,
) -> Response {
    let Some(store) = &state.audit_store else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found("Audit not configured")),
        )
            .into_response();
    };

    serve_blob(&**store, &filename).await
}

// ── PATCH /debug/params ──────────────────────────────────────

/// Generic parameter PATCH request body.
#[derive(Debug, serde::Deserialize)]
struct PatchParamsReq {
    /// Component id (matches `PipelineNode.id`, e.g. "vad", "tts", "`llm_0`").
    component: String,
    /// Parameter key (matches `ParamDescriptor.key`).
    key: String,
    /// New value (typed at the boundary by `ParamDescriptor::validate`).
    value: serde_json::Value,
}

/// Generic parameter PATCH response body.
#[derive(Debug, serde::Serialize)]
struct PatchParamsResp {
    applied: bool,
    component: String,
    key: String,
    value: serde_json::Value,
}

/// Apply a runtime parameter change.
///
/// This handler is the entire dispatch surface for Layer 2: it does not branch
/// on component name. All component-specific knowledge lives in the registered
/// component's [`ComponentParamSchema`] and [`MutableParamStore`] impls.
async fn patch_params(
    State(state): State<Arc<WorkerState>>,
    Json(req): Json<PatchParamsReq>,
) -> Response {
    match state
        .param_registry
        .apply(&req.component, &req.key, req.value.clone())
        .await
    {
        Ok(stored) => Json(ApiResponse::ok(PatchParamsResp {
            applied: true,
            component: req.component,
            key: req.key,
            value: stored.to_json(),
        }))
        .into_response(),
        Err(e) => param_error_to_response(&e),
    }
}

/// Map a [`ParamError`] to an HTTP response with the appropriate status code
/// and a structured error body. The error type is the single source of truth
/// for which conditions exist; this function only chooses the status code.
fn param_error_to_response(e: &ParamError) -> Response {
    let msg = e.to_string();
    let (status, body) = match e {
        ParamError::UnknownComponent(_) | ParamError::UnknownKey(_) => {
            (StatusCode::NOT_FOUND, ApiResponse::<()>::not_found(msg))
        }
        ParamError::Immutable
        | ParamError::TypeMismatch { .. }
        | ParamError::OutOfRange { .. }
        | ParamError::NotInOptions(_)
        | ParamError::TooLong { .. } => {
            (StatusCode::BAD_REQUEST, ApiResponse::<()>::bad_request(msg))
        }
        ParamError::Remote(_) => (
            StatusCode::BAD_GATEWAY,
            ApiResponse::<()>::internal_error(msg),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiResponse::<()>::internal_error(msg),
        ),
    };
    (status, Json(body)).into_response()
}
