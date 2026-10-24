//! Orchestrator node command: manage worker fleet and route sessions.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use tracing::{info, warn};

use crate::api;
use crate::api::types::{OrchestratorConfig, OrchestratorState};
#[cfg(feature = "session-sqlite")]
use crate::audit::store::SqliteSessionStore;
use crate::audit::store::{
    FsSessionStore, InMemorySessionStore, LocalFsAuditStore, reconcile_sessions_from_captures,
};

// ── Orchestrate: orchestrator node ──────────────────────────

/// Build the session/turn store from config. Unknown provider strings
/// are a hard error (falling through to in-memory on a typo would silently
/// lose history on restart), and a `SQLite` open failure is fatal rather than a
/// silent degrade to in-memory.
fn build_session_store(
    store_cfg: &crate::api::types::SessionPersistenceConfig,
    max_sessions: usize,
) -> Result<Arc<dyn crate::audit::store::SessionStore>, anyhow::Error> {
    let store: Arc<dyn crate::audit::store::SessionStore> = match store_cfg.provider.as_str() {
        #[cfg(not(feature = "session-sqlite"))]
        "sqlite" => {
            return Err(anyhow::anyhow!(
                "sessions.store.provider \"sqlite\" needs a build with the `session-sqlite` feature"
            ));
        }
        #[cfg(feature = "session-sqlite")]
        "sqlite" => {
            let path = store_cfg.path.as_deref().unwrap_or("sessions.db");
            let store =
                SqliteSessionStore::new(PathBuf::from(path), max_sessions).map_err(|e| {
                    anyhow::anyhow!("failed to open sqlite session store at \"{path}\": {e}")
                })?;
            Arc::new(store)
        }
        "fs" => {
            let dir = store_cfg.path.as_deref().unwrap_or("sessions_store/");
            Arc::new(FsSessionStore::new(PathBuf::from(dir), max_sessions))
        }
        "memory" => Arc::new(InMemorySessionStore::new(max_sessions)),
        other => {
            return Err(anyhow::anyhow!(
                "unknown sessions.store.provider \"{other}\"; expected \"memory\", \"fs\", or \"sqlite\""
            ));
        }
    };
    Ok(store)
}

/// Run the orchestrator node.
///
/// `extension_routers` is called with the fully-resolved [`OrchestratorConfig`]
/// and returns additional routers to merge into the admin API via the router's
/// neutral `extensions` seam. This is how optional host extensions (e.g. the
/// `voice-pipes-bench` benchmarking crate) attach without the core engine
/// depending on them. Core-only callers pass `|_| vec![]`.
pub async fn run_orchestrate<F>(
    config_path: Option<PathBuf>,
    extension_routers: F,
) -> Result<(), anyhow::Error>
where
    F: FnOnce(&OrchestratorConfig) -> Vec<Router<Arc<OrchestratorState>>>,
{
    let mut config: OrchestratorConfig = if let Some(path) = config_path {
        let text = std::fs::read_to_string(&path)?;
        let cfg: OrchestratorConfig = toml::from_str(&text)?;
        cfg
    } else {
        OrchestratorConfig::default()
    };

    if let Ok(cwd) = std::env::current_dir() {
        if config.captures_dir.is_relative() {
            config.captures_dir = cwd.join(&config.captures_dir);
        }
        if config.benchmarks_dir.is_relative() {
            config.benchmarks_dir = cwd.join(&config.benchmarks_dir);
        }
        if config.audit.shared_dir.is_relative() {
            config.audit.shared_dir = cwd.join(&config.audit.shared_dir);
        }
        if let Some(store_path) = config.sessions.store.path.as_ref()
            && std::path::Path::new(store_path).is_relative()
        {
            config.sessions.store.path = Some(cwd.join(store_path).to_string_lossy().into_owned());
        }
    }

    let max_sessions = config.sessions.max_entries;
    let reap_interval = config.workers.reap_interval_s;
    let heartbeat_timeout = config.workers.heartbeat_timeout_s;
    let dead_grace = config.workers.dead_grace_s;
    let orch_bind = config.bind.clone();
    let orch_port = config.port;

    // Resolve optional host-extension routers (e.g. benchmarking) from the
    // fully-merged config before it is moved into the shared state.
    let extension_routers = extension_routers(&config);

    let audit_store: Arc<dyn crate::audit::store::AuditStore> =
        Arc::new(LocalFsAuditStore::new(config.audit.shared_dir.clone()));

    let sessions = build_session_store(&config.sessions.store, max_sessions)?;

    // Recover sessions whose captures exist on disk but never made it into the
    // index (lost events, prior in-memory store wiped on restart). Best-effort
    // and non-blocking so a large captures dir doesn't delay startup.
    {
        let store = Arc::clone(&sessions);
        let captures_dir = config.captures_dir.clone();
        tokio::spawn(async move {
            reconcile_sessions_from_captures(store.as_ref(), &captures_dir).await;
        })
    };

    let state = Arc::new(OrchestratorState {
        config,
        workers: Arc::new(dashmap::DashMap::new()),
        sessions,
        audit_store,
        tap_store: None,
        start_time: Instant::now(),
    });

    // Background task: mark silent workers dead, then remove them.
    let reaper_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(reap_interval));
        loop {
            interval.tick().await;
            api::internal::reap_workers(
                &reaper_state.workers,
                chrono::Utc::now(),
                chrono::Duration::seconds(heartbeat_timeout as i64),
                chrono::Duration::seconds(dead_grace as i64),
            );
        }
    });

    // Runtime auth: distinct credentials for the admin and
    // internal surfaces — they share this listener, so separate tokens
    // are what keeps an admin credential from granting worker-internal
    // access. Unset env vars leave a surface open (dev mode);
    // `[auth] required = true` makes that a startup error instead.
    let auth_cfg = state.config.auth.clone();
    let admin_auth =
        api::auth::SurfaceAuth::resolve(&auth_cfg.admin_token_env, "orchestrator admin API");
    let internal_auth =
        api::auth::SurfaceAuth::resolve(&auth_cfg.internal_token_env, "orchestrator internal API");
    admin_auth.enforce_required(auth_cfg.required)?;
    internal_auth.enforce_required(auth_cfg.required)?;
    admin_auth.log_status();
    internal_auth.log_status();

    // Merge admin + internal routers. Host extensions (e.g. benchmarking)
    // attach through the admin router's neutral `extensions` seam, supplied by
    // the composition root.
    let admin_router = api::auth::protect_router(
        api::orchestrator::router(Arc::clone(&state), extension_routers),
        &admin_auth,
    );
    let internal_router =
        api::auth::protect_router(api::internal::router(Arc::clone(&state)), &internal_auth);
    // CORS sits outside auth so preflights (which carry no token) are
    // answered; the internal surface is never called from a browser and is
    // unaffected by it.
    let cors = api::orchestrator::build_cors(&state.config.cors_origins);
    let app = admin_router.merge(internal_router).layer(cors);

    let (listener, actual_port) =
        super::serve::try_bind(&orch_bind, orch_port, "orchestrator API").await?;
    admin_auth.warn_if_open(&format!("{orch_bind}:{actual_port}"));
    internal_auth.warn_if_open(&format!("{orch_bind}:{actual_port}"));
    if actual_port != orch_port {
        warn!(
            configured = orch_port,
            actual = actual_port,
            "orchestrator bound to a different port -- workers must target port {actual_port}"
        );
    }
    info!(bind = %format!("{orch_bind}:{actual_port}"), "orchestrator API listening");
    // Ctrl-C or SIGTERM stops accepting and lets in-flight requests finish.
    axum::serve(listener, app)
        .with_graceful_shutdown(crate::commands::serve::shutdown_signal())
        .await?;
    info!("orchestrator stopped");

    Ok(())
}

#[cfg(test)]
#[path = "tests/orchestrate.rs"]
mod tests;
