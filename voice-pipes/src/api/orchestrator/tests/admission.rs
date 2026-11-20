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
        resp_json(cancel_ticket(State(Arc::clone(&state)), Path(ticket_c)).await).await;
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
