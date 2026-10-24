use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::FutureExt;
use serde::Deserialize;

use crate::store::BenchmarkStore;
use voice_pipes::api::types::{ApiResponse, OrchestratorState};

// ── Extension seam ──────────────────────────────────────────
//
// The benchmark HTTP surface is grouped here as a self-contained router so it
// can be mounted through the orchestrator's neutral `extensions` merge point
// (see `router()`), and later lifted into a separate `voice-pipes-bench` crate
// without touching core routing.

/// All benchmark routes as one mergeable router (state applied by the caller).
///
/// The [`BenchmarkStore`] is attached as a request extension so the handlers
/// depend on it directly rather than on a field of [`OrchestratorState`] —
/// keeping the store out of core state and letting this whole surface move to
/// a separate crate later.
pub(crate) fn routes(store: BenchmarkStore) -> Router<Arc<OrchestratorState>> {
    Router::new()
        .route(
            "/api/v1/benchmark/experiments",
            get(list_experiments).post(create_experiment),
        )
        .route(
            "/api/v1/benchmark/experiments/{id}",
            get(get_experiment).delete(delete_experiment),
        )
        .route("/api/v1/benchmark/experiments/{id}/run", post(run_experiment))
        .route(
            "/api/v1/benchmark/experiments/{id}/results",
            get(get_experiment_results),
        )
        .route(
            "/api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id}/config",
            get(get_variant_config),
        )
        .route(
            "/api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id}/scenarios/{scenario_id}/entries/{entry_id}/audio",
            get(get_tts_entry_audio),
        )
        .route("/api/v1/benchmark/suites", get(list_suites).post(create_suite))
        .route(
            "/api/v1/benchmark/suites/{id}",
            get(get_suite).put(update_suite).delete(delete_suite),
        )
        .route("/api/v1/benchmark/configs", get(list_configs))
        .layer(Extension(store))
}

// ── Types ───────────────────────────────────────────────────

/// Request body for creating an experiment.
#[derive(Debug, Deserialize)]
pub(super) struct CreateExperimentRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    test_suite_id: String,
    #[serde(default)]
    variants: Vec<crate::types::Variant>,
    #[serde(default = "default_repetitions")]
    repetitions: u32,
}

fn default_repetitions() -> u32 {
    1
}

/// Request body for creating a test suite. Accepts either the full
/// `TestSuite` shape (with client-supplied `id` / `created_at`) or
/// the legacy minimal shape -- missing fields are server-generated so
/// older clients keep working.
#[derive(Debug, Deserialize)]
pub(super) struct CreateSuiteRequest {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    scenarios: Vec<crate::types::TestScenario>,
}

/// Optional query parameters for the run endpoint.
#[derive(Debug, Deserialize)]
pub(super) struct RunExperimentQuery {
    /// Injection speed override (default: 0.0 = AFAP).
    #[serde(default)]
    speed: Option<f32>,
    /// Per-scenario timeout in seconds (default: 120).
    #[serde(default)]
    timeout_s: Option<u64>,
    /// Comma-separated variant IDs to run. If omitted, all variants run.
    #[serde(default)]
    variants: Option<String>,
    /// Comma-separated scenario IDs to run. If omitted, all scenarios run.
    #[serde(default)]
    scenarios: Option<String>,
    /// Pin this run to a specific worker (by `worker_id`). When set, the
    /// runner skips tag-based selection and uses this worker if it is
    /// Active and has spare capacity; otherwise the run fails fast.
    #[serde(default)]
    worker_id: Option<String>,
    /// When `true`, allow re-running a `Complete` or `Failed` experiment
    /// without a `variants` filter. The runner will overwrite the existing
    /// `results.json` for the experiment. Has no effect on `Draft` or
    /// `Running` experiments.
    #[serde(default)]
    force: bool,
}

// ── Handlers ────────────────────────────────────────────────

pub async fn create_experiment(
    Extension(store): Extension<BenchmarkStore>,
    Json(body): Json<CreateExperimentRequest>,
) -> Response {
    let exp = crate::types::Experiment {
        id: uuid::Uuid::new_v4().to_string(),
        name: body.name,
        description: body.description,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        status: crate::types::ExperimentStatus::Draft,
        test_suite_id: body.test_suite_id,
        variants: body.variants,
        repetitions: body.repetitions,
    };
    match store.save_experiment(&exp).await {
        Ok(()) => Json(ApiResponse::ok(exp)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to save experiment: {e}"
            ))),
        )
            .into_response(),
    }
}

pub async fn list_experiments(Extension(store): Extension<BenchmarkStore>) -> Response {
    match store.list_experiments().await {
        Ok(exps) => Json(ApiResponse::ok(exps)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to list experiments: {e}"
            ))),
        )
            .into_response(),
    }
}

pub async fn get_experiment(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match store.load_experiment(&id).await {
        Ok(Some(exp)) => Json(ApiResponse::ok(exp)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "experiment {id} not found"
            ))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to load experiment: {e}"
            ))),
        )
            .into_response(),
    }
}

pub async fn delete_experiment(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match store.delete_experiment(&id).await {
        Ok(true) => Json(ApiResponse::ok(())).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "experiment {id} not found"
            ))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to delete experiment: {e}"
            ))),
        )
            .into_response(),
    }
}

/// Single TOML config entry returned by `list_configs`.
#[derive(Debug, serde::Serialize)]
pub(super) struct ConfigListEntry {
    /// Path relative to the worker's working directory -- suitable
    /// for use as a `Variant.config_path`.
    pub path: String,
    /// Display name (file stem) for UI dropdowns.
    pub name: String,
    /// Worker IDs that have this config file available locally.
    /// Empty when the entry came from the orchestrator's own local
    /// scan (fallback when no workers are registered).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worker_ids: Vec<String>,
}

/// Query params for `GET /api/v1/benchmark/configs`.
#[derive(Debug, Deserialize, Default)]
pub(super) struct ListConfigsQuery {
    /// Comma-separated list of worker tags. When present, only configs
    /// available on workers whose tag set ⊇ the requested set are
    /// returned. Empty / missing returns the union across all workers.
    #[serde(default)]
    tags: Option<String>,
}

/// GET /api/v1/benchmark/configs
///
/// Returns pipeline-config files available on registered workers
/// (aggregated from their `WorkerRegistration.available_configs`),
/// optionally filtered to workers carrying every tag in `?tags=a,b`.
///
/// Falls back to scanning the orchestrator's own CWD `examples/`
/// directory when no workers are registered, so single-host dev
/// workflows keep working.
pub async fn list_configs(
    State(state): State<Arc<OrchestratorState>>,
    Query(q): Query<ListConfigsQuery>,
) -> Response {
    let requested_tags: Vec<String> = q
        .tags
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // Aggregate configs across matching workers, deduped by path.
    // Map: path -> (ConfigListEntry, BTreeSet<worker_id>)
    use std::collections::{BTreeMap, BTreeSet};
    let mut agg: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    let mut any_worker = false;

    for entry in state.workers.iter() {
        any_worker = true;
        let worker = entry.value();
        // Tag filter: worker's tags must be a superset of requested.
        if !requested_tags.is_empty() {
            let worker_tags: BTreeSet<&str> = worker.tags.iter().map(String::as_str).collect();
            let all_present = requested_tags
                .iter()
                .all(|t| worker_tags.contains(t.as_str()));
            if !all_present {
                continue;
            }
        }
        for cfg in &worker.available_configs {
            let slot = agg
                .entry(cfg.path.clone())
                .or_insert_with(|| (cfg.name.clone(), BTreeSet::new()));
            slot.1.insert(worker.id.clone());
        }
    }

    let entries: Vec<ConfigListEntry> = if any_worker {
        agg.into_iter()
            .map(|(path, (name, ids))| ConfigListEntry {
                path,
                name,
                worker_ids: ids.into_iter().collect(),
            })
            .collect()
    } else {
        // No worker has reported: scan the local `examples/` directory
        // (single-host dev).
        voice_pipes::config::discover::scan_configs(std::path::Path::new("examples"), 2)
            .into_iter()
            .map(|e| ConfigListEntry {
                path: e.path,
                name: e.name,
                worker_ids: Vec::new(),
            })
            .collect()
    };

    Json(ApiResponse::ok(entries)).into_response()
}

pub async fn create_suite(
    Extension(store): Extension<BenchmarkStore>,
    Json(body): Json<CreateSuiteRequest>,
) -> Response {
    let suite = crate::types::TestSuite {
        id: body.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: body.name,
        description: body.description,
        created_at: body.created_at.unwrap_or_else(chrono::Utc::now),
        scenarios: body.scenarios,
    };
    match store.save_suite(&suite).await {
        Ok(()) => Json(ApiResponse::ok(suite)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to save test suite: {e}"
            ))),
        )
            .into_response(),
    }
}

pub async fn list_suites(Extension(store): Extension<BenchmarkStore>) -> Response {
    match store.list_suites().await {
        Ok(suites) => Json(ApiResponse::ok(suites)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to list test suites: {e}"
            ))),
        )
            .into_response(),
    }
}

pub async fn get_suite(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match store.load_suite(&id).await {
        Ok(Some(suite)) => Json(ApiResponse::ok(suite)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "test suite {id} not found"
            ))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to load test suite: {e}"
            ))),
        )
            .into_response(),
    }
}

pub async fn delete_suite(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match store.delete_suite(&id).await {
        Ok(true) => Json(ApiResponse::ok(())).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "test suite {id} not found"
            ))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to delete test suite: {e}"
            ))),
        )
            .into_response(),
    }
}

/// PUT /api/v1/benchmark/suites/{id}
///
/// Replaces an existing test suite with the supplied definition. The body
/// must include the full `TestSuite` shape (id, name, `created_at`, scenarios).
/// The `id` in the body must match the id in the path.
pub async fn update_suite(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(suite): Json<crate::types::TestSuite>,
) -> Response {
    if suite.id != id {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::bad_request(format!(
                "suite id in body ({}) does not match URL id ({id})",
                suite.id
            ))),
        )
            .into_response();
    }

    // 404 unless the suite already exists. `save_suite` is upsert, so we
    // explicitly probe first to keep PUT and POST semantics distinct.
    match store.load_suite(&id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::not_found(format!(
                    "test suite {id} not found"
                ))),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::internal_error(format!(
                    "failed to load existing suite: {e}"
                ))),
            )
                .into_response();
        }
    }

    match store.save_suite(&suite).await {
        Ok(()) => Json(ApiResponse::ok(suite)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to update test suite: {e}"
            ))),
        )
            .into_response(),
    }
}

/// POST /api/v1/benchmark/experiments/{id}/run
///
/// Spawns the benchmark runner as a background task and returns immediately
/// with the experiment ID.  Poll `GET .../results` to check completion.
pub async fn run_experiment(
    Extension(store): Extension<BenchmarkStore>,
    State(state): State<Arc<OrchestratorState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(q): Query<RunExperimentQuery>,
) -> Response {
    // Quick validation: experiment must exist and be runnable.
    // With a variant filter OR ?force=true, we allow re-running
    // Complete/Failed experiments. Without either, a finished experiment
    // is locked to prevent accidental result clobbering.
    let has_variant_filter = q.variants.is_some();
    let force = q.force;
    match store.load_experiment(&id).await {
        Ok(Some(exp)) => {
            let allowed = match exp.status {
                crate::types::ExperimentStatus::Draft => true,
                crate::types::ExperimentStatus::Complete
                | crate::types::ExperimentStatus::Failed => has_variant_filter || force,
                crate::types::ExperimentStatus::Running => false,
            };
            if !allowed {
                return (
                    StatusCode::CONFLICT,
                    Json(ApiResponse::<()>::error("CONFLICT", format!(
                        "experiment {id} is {:?}; must be Draft to run all variants, \
                         or Complete/Failed with ?variants= (subset) or ?force=true (overwrite all)",
                        exp.status
                    ))),
                )
                    .into_response();
            }
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::not_found(format!(
                    "experiment {id} not found"
                ))),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::internal_error(format!(
                    "failed to load experiment: {e}"
                ))),
            )
                .into_response();
        }
    }

    let variant_filter = q.variants.map(|v| {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let scenario_filter = q.scenarios.map(|v| {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });

    let config = crate::runner::RunnerConfig {
        inject_speed: q.speed.unwrap_or(0.0),
        scenario_timeout: std::time::Duration::from_secs(q.timeout_s.unwrap_or(120)),
        variant_filter,
        scenario_filter,
        target_worker_id: q.worker_id.filter(|s| !s.is_empty()),
        ..Default::default()
    };

    let store = store.clone();
    let workers = Arc::clone(&state.workers);
    let experiment_id = id.clone();

    // Spawn as a background task so the HTTP response returns immediately.
    //
    // IMPORTANT: `run_experiment` flips the experiment status to `Running`
    // and persists it before doing any real work. If the runner then
    // errors out (or panics -- e.g. a stale variant config that the
    // synthetic caller can't construct), the in-store status would
    // otherwise stay pinned to `Running` forever and the UI would show a
    // permanent spinner. We catch both outcomes here and forcibly
    // transition the experiment to `Failed` so the UI unblocks.
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(crate::runner::run_experiment(
            &store,
            &workers,
            &experiment_id,
            &config,
        ))
        .catch_unwind()
        .await;

        let err_msg: Option<String> = match result {
            Ok(Ok(report)) => {
                tracing::info!(
                    experiment_id = %report.experiment_id,
                    status = ?report.status,
                    "benchmark run completed"
                );
                None
            }
            Ok(Err(e)) => {
                tracing::error!(experiment_id = %experiment_id, error = %e, "benchmark run failed");
                Some(format!("{e:#}"))
            }
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&'static str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                tracing::error!(
                    experiment_id = %experiment_id,
                    panic = %msg,
                    "benchmark runner panicked"
                );
                Some(format!("runner panicked: {msg}"))
            }
        };

        if let Some(reason) = err_msg {
            // Best-effort: load the experiment, mark it Failed, save it
            // back. If any step fails we log but don't retry -- at worst
            // the user sees a Running spinner, but that's the existing
            // behaviour and we've already logged the underlying error.
            match store.load_experiment(&experiment_id).await {
                Ok(Some(mut exp)) => {
                    use crate::types::ExperimentStatus;
                    match exp.status {
                        ExperimentStatus::Running => {
                            exp.status = ExperimentStatus::Failed;
                            exp.completed_at = Some(chrono::Utc::now());
                            if let Err(e) = store.save_experiment(&exp).await {
                                tracing::error!(
                                    experiment_id = %experiment_id,
                                    error = %e,
                                    "failed to persist Failed status after runner error"
                                );
                            } else {
                                tracing::warn!(
                                    experiment_id = %experiment_id,
                                    reason = %reason,
                                    "transitioned stuck-Running experiment to Failed"
                                );
                            }
                        }
                        ExperimentStatus::Draft
                        | ExperimentStatus::Complete
                        | ExperimentStatus::Failed => {
                            // Runner already moved it to a terminal state (or
                            // never made it past Draft); nothing to clean up.
                        }
                    }
                }
                Ok(None) => {
                    tracing::warn!(
                        experiment_id = %experiment_id,
                        "experiment vanished from store before failure cleanup"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        experiment_id = %experiment_id,
                        error = %e,
                        "failed to reload experiment during failure cleanup"
                    );
                }
            }
        }
    });

    // Return 202 Accepted -- caller should poll results.
    (
        StatusCode::ACCEPTED,
        Json(ApiResponse::ok(serde_json::json!({
            "experiment_id": id,
            "message": "benchmark run started; poll GET .../results for completion"
        }))),
    )
        .into_response()
}

/// GET /api/v1/benchmark/experiments/{id}/results
pub async fn get_experiment_results(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match store.load_results(&id).await {
        Ok(Some(results)) => Json(ApiResponse::ok(results)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "results for experiment {id} not found"
            ))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to load results: {e}"
            ))),
        )
            .into_response(),
    }
}

/// GET /`api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id}/config`
///
/// Returns the frozen TOML config snapshot for a variant as `text/plain`.
pub async fn get_variant_config(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path((experiment_id, variant_id)): axum::extract::Path<(String, String)>,
) -> Response {
    match store.load_variant_config(&experiment_id, &variant_id).await {
        Ok(Some(toml)) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            toml,
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "config for variant {variant_id} in experiment {experiment_id} not found"
            ))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to load variant config: {e}"
            ))),
        )
            .into_response(),
    }
}

/// GET /`api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id`}
///        /`scenarios/{scenario_id}/entries/{entry_id}/audio`
///
/// Streams the synthesised WAV for a single TTS-only corpus entry.
///
/// Path resolution is anchored to the persisted experiment results -- the
/// caller never supplies a filesystem path, only opaque ids. Defence in
/// depth on the persisted `audio_path` is limited to rejecting `..`
/// segments and non-files; the path is NOT anchored to the captures
/// directory; full path containment is not yet enforced here.
pub async fn get_tts_entry_audio(
    Extension(store): Extension<BenchmarkStore>,
    axum::extract::Path((experiment_id, variant_id, scenario_id, entry_id)): axum::extract::Path<(
        String,
        String,
        String,
        String,
    )>,
) -> Response {
    let results = match store.load_results(&experiment_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::not_found(format!(
                    "results for experiment {experiment_id} not found"
                ))),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::internal_error(format!(
                    "failed to load results: {e}"
                ))),
            )
                .into_response();
        }
    };

    let entry = results
        .variant_results
        .iter()
        .find(|v| v.variant_id == variant_id)
        .and_then(|v| v.scenarios.iter().find(|s| s.scenario_id == scenario_id))
        .and_then(|s| s.tts_entries.as_ref())
        .and_then(|entries| entries.iter().find(|e| e.entry_id == entry_id));

    let Some(path_str) = entry.and_then(|e| e.audio_path.as_deref()) else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "no audio recorded for entry {entry_id}"
            ))),
        )
            .into_response();
    };

    let path = std::path::PathBuf::from(path_str);
    // Defence in depth: refuse anything that does not resolve to an
    // existing regular file, and reject any traversal segments that
    // sneaked into the persisted record.
    if path_str.contains("..") || !path.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "audio file unavailable for entry {entry_id}"
            ))),
        )
            .into_response();
    }

    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "audio/wav"),
                (axum::http::header::CONTENT_DISPOSITION, "inline"),
                (axum::http::header::CACHE_CONTROL, "private, max-age=60"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error(format!(
                "failed to read audio file: {e}"
            ))),
        )
            .into_response(),
    }
}
