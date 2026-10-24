use super::*;
use crate::api::types::{ComponentSnapshot, OrchestratorConfig, WorkerCapacity};
use crate::component::{ComponentKind, ComponentTelemetry, HealthState, HealthStatus};
use std::time::SystemTime;

/// Self-reported addresses must be plain `host:port` —
/// anything else is an SSRF/dispatch-poisoning primitive.
#[test]
fn worker_address_validation() {
    let any: Vec<String> = Vec::new();
    for ok in ["10.0.0.5:8085", "worker-1:8085", "localhost:9000"] {
        assert!(valid_worker_address(ok, &any), "{ok} should pass");
    }
    for bad in [
        "",
        "no-port",
        "host:notaport",
        "host:99999",
        "http://evil/",
        "evil.example/path:80",
        "user@host:80",
        "host:80\\x",
    ] {
        assert!(!valid_worker_address(bad, &any), "{bad} should fail");
    }
    // Allowlist restricts hosts.
    let allow = vec!["worker-1".to_string()];
    assert!(valid_worker_address("worker-1:8085", &allow));
    assert!(!valid_worker_address("evil:8085", &allow));
}

// ── Registry lifecycle ──────────────────────────────────────

fn record(id: &str, address: &str, last_heartbeat: chrono::DateTime<chrono::Utc>) -> WorkerRecord {
    WorkerRecord {
        id: id.into(),
        address: address.into(),
        registered_at: last_heartbeat,
        last_heartbeat,
        status: WorkerStatus::Active,
        capacity: WorkerCapacity {
            max_concurrent: 4,
            active_sessions: 2,
        },
        pipeline_config_hash: "h".into(),
        pipeline_name: None,
        pipeline_config: None,
        version: "test".into(),
        tags: Vec::new(),
        transport_address: None,
        public_transport_address: None,
        sessions_processed: 0,
        errors_total: 0,
        available_configs: Vec::new(),
        components: Vec::new(),
        aggregate_health: None,
    }
}

fn state_with(workers: Vec<WorkerRecord>, config: OrchestratorConfig) -> Arc<OrchestratorState> {
    let map = dashmap::DashMap::new();
    for w in workers {
        map.insert(w.id.clone(), w);
    }
    Arc::new(OrchestratorState {
        config,
        workers: Arc::new(map),
        sessions: Arc::new(crate::audit::InMemorySessionStore::new(10)),
        audit_store: Arc::new(crate::audit::LocalFsAuditStore::new(
            std::env::temp_dir().join("vp_internal_test"),
        )),
        tap_store: None,
        start_time: std::time::Instant::now(),
    })
}

fn registration(id: &str, address: &str) -> WorkerRegistration {
    WorkerRegistration {
        worker_id: id.into(),
        address: address.into(),
        capacity: WorkerCapacity {
            max_concurrent: 4,
            active_sessions: 0,
        },
        pipeline_config_hash: "h".into(),
        pipeline_name: None,
        pipeline_config: None,
        version: "test".into(),
        tags: Vec::new(),
        transport_address: None,
        public_transport_address: None,
        available_configs: Vec::new(),
    }
}

/// A silent worker is marked dead after the heartbeat timeout and removed
/// after the grace period, so the registry does not keep ghosts forever.
#[test]
fn reaper_marks_dead_then_evicts_after_the_grace_period() {
    let t0 = chrono::Utc::now();
    let timeout = chrono::Duration::seconds(30);
    let grace = chrono::Duration::seconds(600);
    let workers = dashmap::DashMap::new();
    workers.insert("fresh".to_string(), record("fresh", "a:1", t0));
    workers.insert(
        "silent".to_string(),
        record("silent", "b:1", t0 - chrono::Duration::seconds(45)),
    );
    workers.insert(
        "ancient".to_string(),
        record("ancient", "c:1", t0 - chrono::Duration::seconds(700)),
    );

    // One pass: the recently silent worker is marked dead and kept; the
    // one silent for longer than timeout + grace is removed outright.
    let evicted = reap_workers(&workers, t0, timeout, grace);
    assert_eq!(evicted, 1);
    assert_eq!(workers.get("fresh").unwrap().status, WorkerStatus::Active);
    assert_eq!(workers.get("silent").unwrap().status, WorkerStatus::Dead);
    assert!(workers.get("ancient").is_none());
    assert_eq!(live_worker_count(&workers), 1);

    // The dead record survives until its grace period is over; the fresh
    // worker keeps heartbeating meanwhile.
    workers.get_mut("fresh").unwrap().last_heartbeat = t0 + chrono::Duration::seconds(500);
    let evicted = reap_workers(
        &workers,
        t0 + chrono::Duration::seconds(500),
        timeout,
        grace,
    );
    assert_eq!(evicted, 0);
    assert!(workers.get("silent").is_some());
    workers.get_mut("fresh").unwrap().last_heartbeat = t0 + chrono::Duration::seconds(700);
    let evicted = reap_workers(
        &workers,
        t0 + chrono::Duration::seconds(700),
        timeout,
        grace,
    );
    assert_eq!(evicted, 1);
    assert!(workers.get("silent").is_none());
    assert_eq!(workers.len(), 1);
    assert_eq!(workers.get("fresh").unwrap().status, WorkerStatus::Active);
}

/// A fleet whose workers mint a fresh id per process never hits the
/// registry cap: dead records do not count and are eventually removed.
#[tokio::test]
async fn restarting_workers_do_not_exhaust_the_registry_cap() {
    let mut config = OrchestratorConfig::default();
    config.workers.max_registered = 3;
    let t0 = chrono::Utc::now();
    let stale = t0 - chrono::Duration::seconds(45);
    let state = state_with(
        vec![
            record("old-1", "w:1", stale),
            record("old-2", "w:1", stale),
            record("old-3", "w:1", stale),
        ],
        config,
    );
    // Before the reaper runs, the three stale records are live and the cap holds.
    let resp = register_worker(
        State(Arc::clone(&state)),
        Json(registration("new-1", "w:1")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    reap_workers(
        &state.workers,
        t0,
        chrono::Duration::seconds(30),
        chrono::Duration::seconds(600),
    );
    let resp = register_worker(
        State(Arc::clone(&state)),
        Json(registration("new-1", "w:1")),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "dead records must not count against the cap"
    );
}

/// A live worker's id cannot be moved to another host by a second
/// registration; a dead one can be claimed from a new address.
#[tokio::test]
async fn re_registration_from_another_host_is_refused_while_live() {
    let t0 = chrono::Utc::now();
    let state = state_with(
        vec![record("w1", "host-a:8085", t0)],
        OrchestratorConfig::default(),
    );

    let resp = register_worker(
        State(Arc::clone(&state)),
        Json(registration("w1", "host-b:8085")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(state.workers.get("w1").unwrap().address, "host-a:8085");

    // Same host, new port: a restart on the same machine is fine.
    let resp = register_worker(
        State(Arc::clone(&state)),
        Json(registration("w1", "host-a:9000")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Once dead, the id may come back from anywhere.
    state.workers.get_mut("w1").unwrap().status = WorkerStatus::Dead;
    let resp = register_worker(
        State(Arc::clone(&state)),
        Json(registration("w1", "host-b:8085")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(state.workers.get("w1").unwrap().address, "host-b:8085");
}

/// A turn is not a session: reporting one leaves the worker's live
/// session count alone (the heartbeat carries the real value).
#[tokio::test]
async fn turn_event_does_not_touch_active_sessions() {
    let t0 = chrono::Utc::now();
    let state = state_with(
        vec![record("w1", "host-a:8085", t0)],
        OrchestratorConfig::default(),
    );
    let event = SessionCompleteEvent {
        worker_id: "w1".into(),
        session: crate::api::types::TurnRecord {
            id: "turn-1".into(),
            session_id: String::new(),
            worker_id: "w1".into(),
            started_at: t0,
            completed_at: Some(t0),
            status: SessionStatus::Completed,
            total_ms: Some(10),
            stt_provider: None,
            tts_provider: None,
            text_preview: None,
            otel_trace_id: None,
            error: None,
            detail: None,
            pipeline_name: None,
            pipeline_config_hash: None,
            tags: Vec::new(),
            events: Vec::new(),
        },
    };
    let resp = turn_event(State(Arc::clone(&state)), Json(event)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let w = state.workers.get("w1").unwrap();
    assert_eq!(w.capacity.active_sessions, 2);
    assert_eq!(w.sessions_processed, 1);
}

fn sample_snapshot() -> ComponentSnapshot {
    let mut health = HealthStatus::healthy();
    health.since = SystemTime::UNIX_EPOCH;
    let mut telemetry = ComponentTelemetry::default();
    telemetry.processed_total = 12;
    ComponentSnapshot {
        id: "stt:parakeet".to_string(),
        kind: ComponentKind::Stt,
        health,
        telemetry,
        param_schema: None,
    }
}

#[test]
fn worker_heartbeat_roundtrips_without_components() {
    // Back-compat: an old worker that omits the `components` field
    // still deserializes and produces an empty Vec.
    let json = serde_json::json!({
        "worker_id": "w1",
        "capacity": {"max_concurrent": 4, "active_sessions": 1},
    });
    let hb: WorkerHeartbeat = serde_json::from_value(json).expect("deserialize");
    assert_eq!(hb.worker_id, "w1");
    assert!(hb.components.is_empty());
    assert!(hb.aggregate_health.is_none());
}

#[test]
fn worker_heartbeat_roundtrips_with_components() {
    let original = WorkerHeartbeat {
        worker_id: "w2".to_string(),
        capacity: WorkerCapacity {
            max_concurrent: 2,
            active_sessions: 0,
        },
        components: vec![sample_snapshot()],
        aggregate_health: Some(HealthState::Healthy),
    };
    let s = serde_json::to_string(&original).expect("serialize");
    let back: WorkerHeartbeat = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(back.worker_id, "w2");
    assert_eq!(back.components.len(), 1);
    assert_eq!(back.components[0].id, "stt:parakeet");
    // Verify enum tags survive the round-trip (snake_case from serde attr).
    match back.components[0].kind {
        ComponentKind::Stt => {}
        other => panic!("expected Stt, got {other:?}"),
    }
    assert_eq!(back.aggregate_health, Some(HealthState::Healthy));
}

#[test]
fn worker_heartbeat_serialised_skips_empty_components() {
    // Old orchestrators must not see a noisy `"components": []` key.
    let hb = WorkerHeartbeat {
        worker_id: "w3".to_string(),
        capacity: WorkerCapacity {
            max_concurrent: 1,
            active_sessions: 0,
        },
        components: Vec::new(),
        aggregate_health: None,
    };
    let s = serde_json::to_string(&hb).expect("serialize");
    assert!(
        !s.contains("components"),
        "components should be skipped: {s}"
    );
    assert!(
        !s.contains("aggregate_health"),
        "aggregate_health should be skipped: {s}"
    );
}

fn session_record(id: &str) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        worker_id: "w1".into(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: SessionStatus::InProgress,
        pipeline_name: Some("p".into()),
        pipeline_config_hash: None,
        stt_provider: None,
        tts_provider: None,
        input_sample_rate: None,
        output_sample_rate: None,
        tags: Vec::new(),
        turn_count: 0,
    }
}

/// The end-of-session record fills the config hash and sample rates when
/// the start record lacked them, and never overwrites values already
/// recorded.
#[tokio::test]
async fn session_end_fills_missing_hash_and_rates_without_overwriting() {
    let state = state_with(Vec::new(), OrchestratorConfig::default());

    let start = session_record("s-missing");
    session_lifecycle_event(State(Arc::clone(&state)), Json(start)).await;
    let mut end = session_record("s-missing");
    end.ended_at = Some(chrono::Utc::now());
    end.status = SessionStatus::Completed;
    end.pipeline_config_hash = Some("abc123".into());
    end.input_sample_rate = Some(16_000);
    end.output_sample_rate = Some(24_000);
    session_lifecycle_event(State(Arc::clone(&state)), Json(end)).await;
    let merged = state.sessions.find_session("s-missing").await.unwrap();
    assert_eq!(merged.pipeline_config_hash.as_deref(), Some("abc123"));
    assert_eq!(merged.input_sample_rate, Some(16_000));
    assert_eq!(merged.output_sample_rate, Some(24_000));
    assert_eq!(merged.status, SessionStatus::Completed);

    let mut start = session_record("s-kept");
    start.pipeline_config_hash = Some("from-start".into());
    start.input_sample_rate = Some(48_000);
    session_lifecycle_event(State(Arc::clone(&state)), Json(start)).await;
    let mut end = session_record("s-kept");
    end.ended_at = Some(chrono::Utc::now());
    end.pipeline_config_hash = Some("from-end".into());
    end.input_sample_rate = Some(8_000);
    session_lifecycle_event(State(Arc::clone(&state)), Json(end)).await;
    let merged = state.sessions.find_session("s-kept").await.unwrap();
    assert_eq!(merged.pipeline_config_hash.as_deref(), Some("from-start"));
    assert_eq!(merged.input_sample_rate, Some(48_000));
}

/// A worker that deregisters is dead at once, not after the heartbeat
/// timeout, and an unknown id is a 404.
#[tokio::test]
async fn a_deregistered_worker_is_dead_at_once() {
    let state = state_with(
        vec![record("w1", "10.0.0.5:8085", chrono::Utc::now())],
        OrchestratorConfig::default(),
    );
    let resp = deregister_worker(
        State(Arc::clone(&state)),
        Json(WorkerDeregistration {
            worker_id: "w1".into(),
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let w = state.workers.get("w1").expect("kept until the reaper");
    assert_eq!(w.status, WorkerStatus::Dead);
    assert_eq!(w.capacity.active_sessions, 0);
    drop(w);

    let resp = deregister_worker(
        State(state),
        Json(WorkerDeregistration {
            worker_id: "nobody".into(),
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
