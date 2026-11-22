use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;

use crate::api::types::{
    AcquireDispatchRequest, AdmissionConfig, OrchestratorConfig, OrchestratorState,
    TicketPollQuery, WorkerCapacity, WorkerRecord, WorkerStatus,
};

use super::{
    acquire_dispatch, build_capacity_snapshot, cancel_ticket, get_ticket_status, on_session_end,
    on_session_start, prometheus_metrics,
};

fn make_worker(id: &str, max_concurrent: u32) -> WorkerRecord {
    WorkerRecord {
        id: id.into(),
        address: "127.0.0.1:8085".into(),
        transport_address: Some("127.0.0.1:8800".into()),
        public_transport_address: None,
        registered_at: Utc::now(),
        last_heartbeat: Utc::now(),
        status: WorkerStatus::Active,
        capacity: WorkerCapacity {
            active_sessions: 0,
            max_concurrent,
        },
        pipeline_config_hash: "hash-cpu".into(),
        pipeline_name: Some("hf-cpu-assistant".into()),
        version: "0.3.0".into(),
        tags: vec!["device:cpu".into()],
        sessions_processed: 0,
        errors_total: 0,
        pipeline_config: None,
        available_configs: Vec::new(),
        components: Vec::new(),
        aggregate_health: None,
    }
}

fn state_with_admission(
    workers: Vec<WorkerRecord>,
    admission: AdmissionConfig,
) -> Arc<OrchestratorState> {
    let map = dashmap::DashMap::new();
    for w in workers {
        map.insert(w.id.clone(), w);
    }
    let mut cfg = OrchestratorConfig::default();
    cfg.dispatch.admission = admission;
    Arc::new(OrchestratorState {
        config: cfg,
        workers: Arc::new(map),
        sessions: Arc::new(crate::audit::InMemorySessionStore::new(32)),
        audit_store: Arc::new(crate::audit::LocalFsAuditStore::new(
            std::env::temp_dir().join("vp_admission_test"),
        )),
        tap_store: None,
        start_time: std::time::Instant::now(),
        admission: Arc::default(),
    })
}

async fn resp_json(resp: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let val: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    (status, val)
}

#[tokio::test]
async fn immediate_grant_and_per_tenant_cap_enforcement() {
    let state = state_with_admission(
        vec![make_worker("w1", 2)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 2,
            max_per_tenant: 1,
            max_queue_size: 5,
            queue_timeout_s: 60,
            lease_ttl_s: 30,
            avg_session_duration_hint_s: 60,
        },
    );

    // First request from alice -> 200 Granted
    let (st1, j1) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("hf-alice".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st1, StatusCode::OK);
    assert_eq!(j1["data"]["status"], "granted");
    assert_eq!(j1["data"]["worker_id"], "w1");

    // Second concurrent request from alice -> 429 TENANT_LIMIT_REACHED
    let (st2, j2) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("hf-alice".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st2, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(j2["error"]["code"], "TENANT_LIMIT_REACHED");
}

#[tokio::test]
async fn fifo_queue_enqueues_and_promotes_in_order_on_session_end() {
    let state = state_with_admission(
        vec![make_worker("w1", 1)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 1,
            max_per_tenant: 1,
            max_queue_size: 2,
            queue_timeout_s: 60,
            lease_ttl_s: 30,
            avg_session_duration_hint_s: 60,
        },
    );

    // 1. Alice takes the only active slot
    let (st_a, _) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("hf-alice".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st_a, StatusCode::OK);
    on_session_start(&state, "sess-alice", "w1", Some("hf-alice"));

    // 2. Bob enters queue at position 1
    let (st_b, jb) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("hf-bob".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st_b, StatusCode::ACCEPTED);
    assert_eq!(jb["data"]["status"], "queued");
    assert_eq!(jb["data"]["position"], 1);
    let ticket_b = jb["data"]["ticket_id"]
        .as_str()
        .expect("ticket_b")
        .to_string();

    // 3. Carol enters queue at position 2
    let (st_c, jc) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("hf-carol".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st_c, StatusCode::ACCEPTED);
    assert_eq!(jc["data"]["position"], 2);
    let ticket_c = jc["data"]["ticket_id"]
        .as_str()
        .expect("ticket_c")
        .to_string();

    // 4. Dave rejected because max_queue_size == 2
    let (st_d, jd) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("hf-dave".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st_d, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(jd["error"]["code"], "QUEUE_FULL");

    // 5. Alice's session ends -> Bob (position 1) is immediately promoted to Granted, Carol shifts to position 1
    on_session_end(&state, "sess-alice", "w1");

    let (st_poll_b, j_poll_b) = resp_json(
        get_ticket_status(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(ticket_b),
            Query(TicketPollQuery { wait_ms: Some(0) }),
        )
        .await,
    )
    .await;
    assert_eq!(st_poll_b, StatusCode::OK);
    assert_eq!(j_poll_b["data"]["status"], "granted");
    assert_eq!(j_poll_b["data"]["worker_id"], "w1");

    let (_, j_poll_c) = resp_json(
        get_ticket_status(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(ticket_c.clone()),
            Query(TicketPollQuery { wait_ms: Some(0) }),
        )
        .await,
    )
    .await;
    assert_eq!(j_poll_c["data"]["status"], "queued");
    assert_eq!(j_poll_c["data"]["position"], 1);

    // 6. Cancel Carol's ticket -> queue depth drops to 0
    let (st_cancel, _) =
        resp_json(cancel_ticket(State(Arc::clone(&state)), HeaderMap::new(), Path(ticket_c)).await)
            .await;
    assert_eq!(st_cancel, StatusCode::OK);

    let snap = build_capacity_snapshot(&state);
    assert_eq!(snap.queue_depth, 0);
}

#[tokio::test]
async fn prometheus_metrics_exposes_keda_scaling_gauges() {
    let state = state_with_admission(
        vec![make_worker("w1", 4)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 3,
            max_per_tenant: 1,
            max_queue_size: 10,
            ..Default::default()
        },
    );
    on_session_start(&state, "s1", "w1", Some("hf-alice"));

    let resp = prometheus_metrics(State(Arc::clone(&state))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("metrics body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");
    assert!(text.contains("vp_fleet_active_sessions 1"));
    assert!(text.contains("vp_fleet_effective_limit_sessions 3"));
    assert!(text.contains("vp_dispatch_queue_depth 0"));
    assert!(text.contains("vp_active_tenants_total 1"));
}

#[tokio::test]
async fn worker_heartbeat_preserves_unredeemed_lease_count() {
    let state = state_with_admission(
        vec![make_worker("w1", 2)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 2,
            max_per_tenant: 1,
            max_queue_size: 5,
            lease_ttl_s: 30,
            ..Default::default()
        },
    );

    let (st, j) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("alice".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(j["data"]["status"], "granted");

    // Worker sends a heartbeat reporting 0 active sessions before WS connects.
    let merged = super::on_worker_heartbeat(
        &state,
        "w1",
        WorkerCapacity {
            active_sessions: 0,
            max_concurrent: 2,
        },
    );
    assert_eq!(merged.active_sessions, 1);
}

#[tokio::test]
async fn mismatched_tenant_cannot_poll_or_cancel_ticket() {
    let state = state_with_admission(
        vec![make_worker("w1", 1)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 1,
            max_per_tenant: 1,
            max_queue_size: 5,
            ..Default::default()
        },
    );

    // Fill the single slot
    let _ = acquire_dispatch(
        State(Arc::clone(&state)),
        HeaderMap::new(),
        Json(AcquireDispatchRequest {
            tenant_id: Some("alice".into()),
            ..Default::default()
        }),
    )
    .await;

    // Bob enters the queue
    let (st_b, jb) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("bob".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st_b, StatusCode::ACCEPTED);
    let ticket_b = jb["data"]["ticket_id"]
        .as_str()
        .expect("ticket_id")
        .to_string();

    let mut eve_headers = HeaderMap::new();
    eve_headers.insert("x-tenant-id", "eve".parse().expect("header"));

    // Eve cannot poll Bob's ticket
    let (st_poll, _) = resp_json(
        get_ticket_status(
            State(Arc::clone(&state)),
            eve_headers.clone(),
            Path(ticket_b.clone()),
            Query(TicketPollQuery { wait_ms: Some(0) }),
        )
        .await,
    )
    .await;
    assert_eq!(st_poll, StatusCode::NOT_FOUND);

    // Eve cannot cancel Bob's ticket
    let (_, j_cancel) = resp_json(
        cancel_ticket(
            State(Arc::clone(&state)),
            eve_headers,
            Path(ticket_b.clone()),
        )
        .await,
    )
    .await;
    assert_eq!(j_cancel["data"]["cancelled"], false);
    assert_eq!(build_capacity_snapshot(&state).queue_depth, 1);
}

#[tokio::test]
async fn expired_ticket_increments_timeout_rejection_counter() {
    let state = state_with_admission(
        vec![make_worker("w1", 1)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 1,
            max_per_tenant: 1,
            max_queue_size: 5,
            queue_timeout_s: 5,
            ..Default::default()
        },
    );
    let ctrl = super::for_state(&state);
    let mut inner = ctrl.inner.lock().expect("lock");
    let past = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(10))
        .expect("past instant");
    let initial = crate::api::types::AcquireDispatchResponse::Queued {
        ticket_id: "vp_ticket_expired".into(),
        position: 1,
        ahead: 0,
        queue_depth: 1,
        active_sessions: 1,
        capacity_sessions: 1,
        estimated_wait_s: 5,
    };
    let (tx, rx) = tokio::sync::watch::channel(initial);
    inner.queue.push_back(super::QueuedTicket {
        ticket_id: "vp_ticket_expired".into(),
        request: AcquireDispatchRequest::default(),
        enqueued_at: past
            .checked_sub(std::time::Duration::from_secs(5))
            .expect("enqueued instant"),
        expires_at: past,
        tx,
        rx,
    });
    drop(inner);

    let _ = build_capacity_snapshot(&state);
    assert_eq!(
        ctrl.counters
            .timeout
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(build_capacity_snapshot(&state).queue_depth, 0);
}

#[tokio::test]
async fn heartbeat_after_ws_connect_before_session_start_does_not_double_count() {
    let state = state_with_admission(
        vec![make_worker("w1", 2)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 2,
            max_per_tenant: 1,
            max_queue_size: 5,
            ..Default::default()
        },
    );

    let (st, j) = resp_json(
        acquire_dispatch(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Json(AcquireDispatchRequest {
                tenant_id: Some("alice".into()),
                ..Default::default()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(j["data"]["status"], "granted");

    // Client WebSocket connects to worker (`active_sessions` becomes 1 on the worker)
    // and triggers an immediate heartbeat BEFORE `Configure` / `on_session_start`
    // has redeemed the lease on the orchestrator.
    let merged = super::on_worker_heartbeat(
        &state,
        "w1",
        WorkerCapacity {
            active_sessions: 1,
            max_concurrent: 2,
        },
    );
    assert_eq!(
        merged.active_sessions, 1,
        "a single connected session whose lease is not yet redeemed must not be double-counted as 2"
    );
}

#[tokio::test]
async fn heartbeat_before_session_end_does_not_double_decrement() {
    let state = state_with_admission(
        vec![make_worker("w1", 2)],
        AdmissionConfig {
            enabled: true,
            max_active_sessions: 2,
            max_per_tenant: 1,
            max_queue_size: 5,
            ..Default::default()
        },
    );

    for (tenant, sid) in [("alice", "s1"), ("bob", "s2")] {
        let (st, _) = resp_json(
            acquire_dispatch(
                State(Arc::clone(&state)),
                HeaderMap::new(),
                Json(AcquireDispatchRequest {
                    tenant_id: Some(tenant.into()),
                    ..Default::default()
                }),
            )
            .await,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        on_session_start(&state, sid, "w1", Some(tenant));
    }
    assert_eq!(
        state
            .workers
            .get("w1")
            .expect("w1")
            .capacity
            .active_sessions,
        2
    );

    // Session s1 disconnects: worker's event-driven heartbeat (reporting 1 remaining
    // active session) arrives at the orchestrator right BEFORE `on_session_end("s1")`.
    let merged = super::on_worker_heartbeat(
        &state,
        "w1",
        WorkerCapacity {
            active_sessions: 1,
            max_concurrent: 2,
        },
    );
    state.workers.get_mut("w1").expect("w1").capacity = merged;
    assert_eq!(
        state
            .workers
            .get("w1")
            .expect("w1")
            .capacity
            .active_sessions,
        1
    );

    // Now `on_session_end` for s1 arrives; s2 is still active so active_sessions must remain 1.
    on_session_end(&state, "s1", "w1");
    assert_eq!(
        state
            .workers
            .get("w1")
            .expect("w1")
            .capacity
            .active_sessions,
        1,
        "on_session_end must not decrement below the remaining active session count when heartbeat arrived first"
    );
}
