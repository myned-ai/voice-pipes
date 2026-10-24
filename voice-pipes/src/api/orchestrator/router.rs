use std::sync::Arc;

use axum::Router;
use axum::routing::{get, patch, post};
use tower_http::cors::{Any, CorsLayer};

use crate::api::types::OrchestratorState;

use super::handlers::{
    audit_file_download, audit_files, audit_tap_download, audit_taps, dispatch, drain_worker,
    get_session, get_turn, get_worker, get_worker_components, health, list_components,
    list_pipelines, list_sessions, list_turns, list_workers, metrics, orch_config, orch_liveness,
    orch_readiness, paths_config, pipeline_config, pipeline_diff, pipeline_graph,
    proxy_worker_params, proxy_worker_probe, proxy_worker_probe_audio, proxy_worker_probe_fixtures,
    session_turns,
};

/// Build the orchestrator HTTP router.
///
/// `extensions` are additional routers merged into the admin surface before the
/// shared [`OrchestratorState`] is applied — the neutral seam through which
/// optional host extensions (e.g. benchmarking) attach their routes. Core
/// callers pass an empty vec.
pub fn router(
    state: Arc<OrchestratorState>,
    extensions: Vec<Router<Arc<OrchestratorState>>>,
) -> Router {
    let admin = Router::new()
        .route("/api/v1/health", get(health))
        .route("/healthz", get(orch_liveness))
        .route("/readyz", get(orch_readiness))
        .route("/api/v1/config", get(orch_config))
        .route("/api/v1/paths", get(paths_config))
        .route("/api/v1/config/pipeline", get(pipeline_config))
        .route("/api/v1/pipeline", get(pipeline_graph))
        .route("/api/v1/metrics", get(metrics))
        .route("/api/v1/workers", get(list_workers))
        .route("/api/v1/workers/{id}", get(get_worker))
        .route(
            "/api/v1/workers/{id}/components",
            get(get_worker_components),
        )
        .route("/api/v1/workers/{id}/drain", post(drain_worker))
        .route("/api/v1/workers/{id}/params", patch(proxy_worker_params))
        .route(
            "/api/v1/workers/{id}/probe/fixtures",
            get(proxy_worker_probe_fixtures),
        )
        .route(
            "/api/v1/workers/{id}/probe/audio/{name}",
            get(proxy_worker_probe_audio),
        )
        // Component probes, proxied to the worker that holds the components.
        .route(
            "/api/v1/workers/{id}/probe/{kind}",
            post(proxy_worker_probe),
        )
        .route("/api/v1/components", get(list_components))
        .route("/api/v1/sessions", get(list_sessions))
        .route("/api/v1/sessions/{id}", get(get_session))
        .route("/api/v1/sessions/{id}/turns", get(session_turns))
        .route("/api/v1/turns", get(list_turns))
        .route("/api/v1/turns/{id}", get(get_turn))
        .route("/api/v1/dispatch", get(dispatch))
        .route("/api/v1/pipelines", get(list_pipelines))
        .route("/api/v1/pipelines/diff", get(pipeline_diff))
        .route("/api/v1/audit/files", get(audit_files))
        .route("/api/v1/audit/files/{filename}", get(audit_file_download))
        .route("/api/v1/audit/taps", get(audit_taps))
        .route("/api/v1/audit/taps/{filename}", get(audit_tap_download));

    // Merge optional host-extension routers (benchmarking, …) while the state
    // type is still open, so they share `Arc<OrchestratorState>`.
    let admin = extensions
        .into_iter()
        .fold(admin, |admin, ext| admin.merge(ext));

    admin.with_state(state)
}

/// CORS for the admin surface. Applied *outside* the auth layer by the
/// composition root: a browser preflight carries no bearer token, so
/// inside the auth layer it would be answered with 401 and no
/// cross-origin admin call could ever succeed. The allowed methods and
/// headers cover what the admin API actually takes (`PATCH` for params,
/// `PUT`/`DELETE` for the benchmark extension, `Authorization` for the
/// token).
pub fn build_cors(origins: &[String]) -> CorsLayer {
    use axum::http::{Method, header};
    let methods = [
        Method::GET,
        Method::POST,
        Method::PATCH,
        Method::PUT,
        Method::DELETE,
        Method::OPTIONS,
    ];
    let headers = [
        header::CONTENT_TYPE,
        header::ACCEPT,
        header::AUTHORIZATION,
        header::HeaderName::from_static("x-api-key"),
    ];
    if origins.is_empty() || origins.iter().any(|o| o == "*") {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(methods)
            .allow_headers(headers)
    } else {
        let origins: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(methods)
            .allow_headers(headers)
    }
}
