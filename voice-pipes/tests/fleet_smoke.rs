//! End-to-end smoke test over the fleet seam: register → list → dispatch.
//!
//! The suite tests components thoroughly -- sessions, transport, pipeline --
//! but nothing exercised a worker registering with an orchestrator and a
//! browser being pointed at the result. That is the seam where a pipeline
//! stops being reachable, and its failures are quiet: an empty fleet list
//! looks exactly like a working fleet with nothing in it, and a dispatch that
//! hands out the wrong address looks like a successful dispatch.
//!
//! Everything runs in-process on ephemeral ports, so this is a normal `cargo
//! test` with no Docker, no models and no network.

#![expect(
    clippy::disallowed_methods,
    reason = "plain clients calling the in-process fleet under test"
)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use dashmap::DashMap;
use voice_pipes::api::auth::{SurfaceAuth, protect_router};
use voice_pipes::api::types::{
    OrchestratorConfig, OrchestratorState, WorkerCapacity, WorkerRegistration,
};
use voice_pipes::audit::{InMemorySessionStore, LocalFsAuditStore};

/// An orchestrator on a loopback port, with the real routers and the real
/// auth middleware in front of them.
struct Fleet {
    addr: SocketAddr,
    state: Arc<OrchestratorState>,
    admin_token: String,
    internal_token: String,
}

impl Fleet {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

/// Start an orchestrator. `tag` keeps each test's auth env vars distinct, so
/// tests that enable enforcement cannot race each other through the process
/// environment.
async fn start_orchestrator(tag: &str) -> Fleet {
    let admin_env = format!("VP_SMOKE_ADMIN_{tag}");
    let internal_env = format!("VP_SMOKE_INTERNAL_{tag}");
    let admin_token = format!("admin-{tag}");
    let internal_token = format!("internal-{tag}");
    // SAFETY: single-threaded setup before any task reads these, and the
    // names are unique per test.
    unsafe {
        std::env::set_var(&admin_env, &admin_token);
        std::env::set_var(&internal_env, &internal_token);
    };

    let state = Arc::new(OrchestratorState {
        config: OrchestratorConfig::default(),
        workers: Arc::new(DashMap::new()),
        sessions: Arc::new(InMemorySessionStore::new(16)),
        audit_store: Arc::new(LocalFsAuditStore::new(
            std::env::temp_dir().join(format!("vp_fleet_smoke_{tag}")),
        )),
        tap_store: None,
        start_time: std::time::Instant::now(),
    });

    let admin_auth = SurfaceAuth::resolve(&admin_env, "orchestrator admin API");
    let internal_auth = SurfaceAuth::resolve(&internal_env, "orchestrator internal API");
    let app: Router = protect_router(
        voice_pipes::api::orchestrator::router(Arc::clone(&state), vec![]),
        &admin_auth,
    )
    .merge(protect_router(
        voice_pipes::api::internal::router(Arc::clone(&state)),
        &internal_auth,
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Fleet {
        addr,
        state,
        admin_token,
        internal_token,
    }
}

/// A registration payload as a worker would send it.
///
/// `transport_address` is what the worker bound; `public_transport_address`
/// is what a browser can reach. They differ whenever the worker is
/// containerised or shares a host with others, which is exactly when getting
/// it wrong stops being obvious.
fn registration(id: &str, pipeline: &str, bound: &str, public: Option<&str>) -> WorkerRegistration {
    WorkerRegistration {
        worker_id: id.into(),
        address: format!("{id}:8085"),
        capacity: WorkerCapacity {
            max_concurrent: 4,
            active_sessions: 0,
        },
        pipeline_config_hash: format!("hash-of-{pipeline}"),
        pipeline_name: Some(pipeline.into()),
        pipeline_config: None,
        version: "0.0.0-test".into(),
        tags: vec!["smoke".into()],
        transport_address: Some(bound.into()),
        public_transport_address: public.map(Into::into),
        available_configs: Vec::new(),
    }
}

async fn register(fleet: &Fleet, reg: &WorkerRegistration) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(fleet.url("/internal/workers/register"))
        .bearer_auth(&fleet.internal_token)
        .json(reg)
        .send()
        .await
        .expect("register request")
        .status()
}

async fn get_json(fleet: &Fleet, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(fleet.url(path))
        .bearer_auth(&fleet.admin_token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, body)
}

// ── The path a pipeline takes to become reachable ───────────

#[tokio::test]
async fn a_registered_worker_appears_in_the_fleet_and_can_be_dispatched_to() {
    let fleet = start_orchestrator("basic").await;

    let (status, body) = get_json(&fleet, "/api/v1/workers").await;
    assert_eq!(status, 200);
    assert_eq!(
        body["data"]["total"], 0,
        "the fleet should start empty, got {body}"
    );

    // 201 Created in practice; assert the class rather than pinning the
    // exact code, which is the handler's business and not this test's.
    let status = register(&fleet, &registration("w1", "demo", "0.0.0.0:8800", None)).await;
    assert!(status.is_success(), "registration failed with {status}");

    let (_, body) = get_json(&fleet, "/api/v1/workers").await;
    assert_eq!(
        body["data"]["total"], 1,
        "a registered worker must be listed, got {body}"
    );

    let (status, body) = get_json(&fleet, "/api/v1/dispatch?pipeline_name=demo").await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["worker_id"], "w1", "dispatch body: {body}");
    assert!(
        body["data"]["ws_url"]
            .as_str()
            .is_some_and(|u| u.starts_with("ws://")),
        "dispatch must hand out a usable ws url, got {body}"
    );
}

#[tokio::test]
async fn dispatch_hands_out_the_browser_address_not_the_bind_address() {
    // The failure this guards: several workers on one host each bind
    // 0.0.0.0:8800 inside their own container and publish different host
    // ports. Inferring the URL from the bind address sends every browser to
    // the same port, and the session that opens is some other pipeline --
    // which looks like a working connection to the wrong agent.
    let fleet = start_orchestrator("public").await;
    register(
        &fleet,
        &registration("w-a", "alpha", "0.0.0.0:8800", Some("localhost:8801")),
    )
    .await;
    register(
        &fleet,
        &registration("w-b", "beta", "0.0.0.0:8800", Some("localhost:8802")),
    )
    .await;

    let (_, a) = get_json(&fleet, "/api/v1/dispatch?pipeline_name=alpha").await;
    let (_, b) = get_json(&fleet, "/api/v1/dispatch?pipeline_name=beta").await;
    assert_eq!(a["data"]["ws_url"], "ws://localhost:8801/ws", "got {a}");
    assert_eq!(b["data"]["ws_url"], "ws://localhost:8802/ws", "got {b}");
    assert_ne!(
        a["data"]["ws_url"], b["data"]["ws_url"],
        "two pipelines must not dispatch to the same address"
    );
}

#[tokio::test]
async fn dispatch_filters_by_pipeline_rather_than_returning_any_worker() {
    let fleet = start_orchestrator("filter").await;
    register(
        &fleet,
        &registration("w-hr", "croatian", "0.0.0.0:8800", Some("localhost:8801")),
    )
    .await;
    register(
        &fleet,
        &registration("w-en", "english", "0.0.0.0:8800", Some("localhost:8803")),
    )
    .await;

    let (_, body) = get_json(&fleet, "/api/v1/dispatch?pipeline_name=english").await;
    assert_eq!(
        body["data"]["worker_id"], "w-en",
        "asking for one pipeline must not return another: {body}"
    );
}

// ── Authentication failures must be loud ────────────────────

#[tokio::test]
async fn an_unauthenticated_fleet_read_is_rejected_not_answered_with_an_empty_list() {
    // The failure this guards: a console whose credential is missing gets a
    // 401 and renders "no workers", which is indistinguishable from a
    // healthy orchestrator that has none. Hours went into that ambiguity.
    let fleet = start_orchestrator("noauth").await;
    register(&fleet, &registration("w1", "demo", "0.0.0.0:8800", None)).await;

    let resp = reqwest::Client::new()
        .get(fleet.url("/api/v1/workers"))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        401,
        "an unauthenticated read must fail, not return an empty fleet"
    );
}

#[tokio::test]
async fn registration_without_the_internal_token_is_refused() {
    let fleet = start_orchestrator("regauth").await;

    let status = reqwest::Client::new()
        .post(fleet.url("/internal/workers/register"))
        .json(&registration("w1", "demo", "0.0.0.0:8800", None))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(status, 401, "registration must present the internal token");

    assert!(
        fleet.state.workers.is_empty(),
        "a refused registration must not leave a worker in the fleet"
    );
}

// ── Nothing to dispatch to ──────────────────────────────────

#[tokio::test]
async fn dispatching_with_no_workers_fails_rather_than_returning_a_dead_url() {
    let fleet = start_orchestrator("empty").await;
    let (status, body) = get_json(&fleet, "/api/v1/dispatch?pipeline_name=demo").await;
    assert!(
        status.is_client_error()
            || status.is_server_error()
            || body["error"] != serde_json::Value::Null,
        "dispatch with an empty fleet must say so, got {status} {body}"
    );
}

#[tokio::test]
async fn dispatching_for_an_unknown_pipeline_does_not_fall_back_to_another() {
    let fleet = start_orchestrator("unknown").await;
    register(
        &fleet,
        &registration("w-hr", "croatian", "0.0.0.0:8800", Some("localhost:8801")),
    )
    .await;

    let (status, body) = get_json(&fleet, "/api/v1/dispatch?pipeline_name=does-not-exist").await;
    let dispatched = body["data"]["worker_id"].as_str().unwrap_or("");
    assert!(
        dispatched.is_empty() || status.is_client_error(),
        "an unknown pipeline must not quietly dispatch to an unrelated worker, got {body}"
    );
}
