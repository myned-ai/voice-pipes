use super::{connectable_addr, probe_json_response};
use std::sync::Arc;

use axum::extract::{Query, State};

use crate::api::types::{
    DispatchQuery, OrchestratorConfig, OrchestratorState, WorkerCapacity, WorkerRecord,
    WorkerStatus,
};

fn worker(id: &str, pipeline: &str, transport_address: &str) -> WorkerRecord {
    worker_with_public(id, pipeline, transport_address, None)
}

fn worker_with_public(
    id: &str,
    pipeline: &str,
    transport_address: &str,
    public_transport_address: Option<&str>,
) -> WorkerRecord {
    let now = chrono::Utc::now();
    WorkerRecord {
        id: id.into(),
        address: format!("{id}:8085"),
        public_transport_address: public_transport_address.map(String::from),
        registered_at: now,
        last_heartbeat: now,
        status: WorkerStatus::Active,
        capacity: WorkerCapacity {
            max_concurrent: 10,
            active_sessions: 0,
        },
        pipeline_config_hash: format!("hash-{pipeline}"),
        pipeline_name: Some(pipeline.into()),
        version: "test".into(),
        sessions_processed: 0,
        errors_total: 0,
        tags: Vec::new(),
        transport_address: Some(transport_address.into()),
        pipeline_config: None,
        available_configs: Vec::new(),
        components: Vec::new(),
        aggregate_health: None,
    }
}

fn state_with(workers: Vec<WorkerRecord>) -> Arc<OrchestratorState> {
    let map = dashmap::DashMap::new();
    for w in workers {
        map.insert(w.id.clone(), w);
    }
    Arc::new(OrchestratorState {
        config: OrchestratorConfig::default(),
        workers: Arc::new(map),
        sessions: Arc::new(crate::audit::InMemorySessionStore::new(10)),
        audit_store: Arc::new(crate::audit::LocalFsAuditStore::new(
            std::env::temp_dir().join("vp_dispatch_test"),
        )),
        tap_store: None,
        start_time: std::time::Instant::now(),
    })
}

async fn dispatch_ws_url(state: &Arc<OrchestratorState>, pipeline: &str) -> (String, String) {
    let resp = super::dispatch(
        State(Arc::clone(state)),
        Query(DispatchQuery {
            pipeline_name: Some(pipeline.into()),
            pipeline_hash: None,
            tags: None,
            policy: None,
        }),
    )
    .await;
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    (
        json["data"]["worker_id"].as_str().unwrap_or("").to_string(),
        json["data"]["ws_url"].as_str().unwrap_or("").to_string(),
    )
}

/// Two containerised workers on different host ports: the selected
/// pipeline must dispatch to *its* worker's public transport, not to
/// whatever `0.0.0.0` rewrites to (the bug: the Croatian worker's dispatch
/// returned `ws://127.0.0.1:8800/ws`, the Gemini worker's port). The
/// internal `transport_address` stays `0.0.0.0:8800` for the
/// orchestrator's own consumers (benchmark runner).
#[tokio::test]
async fn dispatch_returns_the_selected_workers_advertised_transport() {
    let state = state_with(vec![
        worker("nikola_tesla", "nikola_tesla", "0.0.0.0:8800"),
        worker_with_public(
            "tesla_hr",
            "tesla_hr",
            "0.0.0.0:8800",
            Some("localhost:8801"),
        ),
    ]);

    let (id, url) = dispatch_ws_url(&state, "tesla_hr").await;
    assert_eq!(id, "tesla_hr");
    assert_eq!(url, "ws://localhost:8801/ws");

    let (id, url) = dispatch_ws_url(&state, "nikola_tesla").await;
    assert_eq!(id, "nikola_tesla");
    assert_eq!(url, "ws://127.0.0.1:8800/ws");
}

/// The window string is split at the last character, so a multi-byte
/// final character is a parse failure rather than a panic.
#[test]
fn parse_window_handles_multibyte_and_negative_input() {
    assert_eq!(
        super::parse_window("30s"),
        Some(chrono::Duration::seconds(30))
    );
    assert_eq!(
        super::parse_window("5m"),
        Some(chrono::Duration::minutes(5))
    );
    assert_eq!(super::parse_window("1h"), Some(chrono::Duration::hours(1)));
    for bad in ["5é", "é", "", "-5s", "5x", "s"] {
        assert_eq!(super::parse_window(bad), None, "{bad:?}");
    }
}

/// An absurd page number yields an empty page, not an overflow.
#[tokio::test]
async fn huge_page_numbers_yield_an_empty_page() {
    let state = state_with(vec![]);
    let resp = super::list_turns(
        State(Arc::clone(&state)),
        Query(crate::api::types::SessionsQuery {
            page: Some(usize::MAX),
            per_page: Some(200),
            tenant_id: None,
            status: None,
            since: None,
            until: None,
            search: None,
            worker_id: None,
            pipeline_name: None,
            pipeline_hash: None,
            tag: None,
            exclude_tag: None,
            min_turns: None,
        }),
    )
    .await;
    assert!(resp.0.data.as_ref().is_some_and(|p| p.items.is_empty()));
    let resp = super::audit_files(
        State(state),
        Query(crate::api::types::AuditFilesQuery {
            page: Some(usize::MAX),
            per_page: Some(200),
            session_id: None,
            file_type: None,
        }),
    )
    .await;
    assert!(resp.0.data.as_ref().is_some_and(|p| p.items.is_empty()));
}

/// A worker that accepts the connection and never answers must not hold
/// the admin's params request: the proxy gives up at `proxy_timeout_s`.
#[tokio::test]
async fn params_proxy_gives_up_on_a_silent_worker() {
    // Accept TCP and read nothing: the request is sent, the response never comes.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    let mut w = worker("silent", "p", "0.0.0.0:8800");
    w.address = format!("127.0.0.1:{}", addr.port());
    let mut config = OrchestratorConfig::default();
    config.workers.proxy_timeout_s = 1;
    let map = dashmap::DashMap::new();
    map.insert(w.id.clone(), w);
    let state = Arc::new(OrchestratorState {
        config,
        workers: Arc::new(map),
        sessions: Arc::new(crate::audit::InMemorySessionStore::new(10)),
        audit_store: Arc::new(crate::audit::LocalFsAuditStore::new(
            std::env::temp_dir().join("vp_proxy_test"),
        )),
        tap_store: None,
        start_time: std::time::Instant::now(),
    });

    let started = std::time::Instant::now();
    let resp = super::proxy_worker_params(
        State(state),
        axum::extract::Path("silent".to_string()),
        axum::Json(serde_json::json!({"component": "x", "param": "y", "value": 1})),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn dispatch_rejects_unknown_pipeline() {
    let state = state_with(vec![worker("a", "alpha", "0.0.0.0:8800")]);
    let resp = super::dispatch(
        State(Arc::clone(&state)),
        Query(DispatchQuery {
            pipeline_name: Some("nope".into()),
            pipeline_hash: None,
            tags: None,
            policy: None,
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn wildcard_listen_addr_becomes_loopback() {
    assert_eq!(connectable_addr("0.0.0.0:8800"), "127.0.0.1:8800");
    assert_eq!(connectable_addr("0.0.0.0:8085"), "127.0.0.1:8085");
}

#[test]
fn concrete_hosts_pass_through() {
    assert_eq!(connectable_addr("127.0.0.1:8800"), "127.0.0.1:8800");
    assert_eq!(connectable_addr("192.168.1.10:8800"), "192.168.1.10:8800");
    assert_eq!(connectable_addr("worker-a:8800"), "worker-a:8800");
}

#[test]
fn a_host_that_merely_contains_the_wildcard_is_left_alone() {
    assert_eq!(connectable_addr("10.0.0.0:8081"), "10.0.0.0:8081");
    assert_eq!(connectable_addr("100.0.0.0:8081"), "100.0.0.0:8081");
    assert_eq!(connectable_addr("0.0.0.0.example:80"), "0.0.0.0.example:80");
}

#[test]
fn the_ipv6_wildcard_becomes_ipv6_loopback() {
    assert_eq!(connectable_addr("[::]:8800"), "[::1]:8800");
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).expect("the relay must always answer with JSON")
}

#[tokio::test]
async fn a_worker_without_probe_routes_is_named_not_relayed_as_a_parse_failure() {
    // What a worker built before the probe endpoints answers: a bare 404.
    let resp = probe_json_response(
        "worker-old",
        axum::http::StatusCode::NOT_FOUND,
        axum::body::Bytes::new(),
    );
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_IMPLEMENTED);
    let json = body_json(resp).await;
    assert_eq!(json["error"]["code"], "PROBE_UNSUPPORTED");
    let message = json["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("worker-old") && message.contains("Rebuild"),
        "{message}"
    );
}

#[tokio::test]
async fn a_json_answer_is_relayed_untouched_whatever_its_status() {
    let body = axum::body::Bytes::from_static(
        br#"{"data":null,"error":{"code":"BUSY","message":"one at a time"}}"#,
    );
    let resp = probe_json_response("w", axum::http::StatusCode::TOO_MANY_REQUESTS, body);
    assert_eq!(resp.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body_json(resp).await["error"]["code"], "BUSY");
}

#[tokio::test]
async fn any_other_non_json_answer_becomes_a_gateway_error_that_says_so() {
    let resp = probe_json_response(
        "w",
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::body::Bytes::from_static(b"<html>oops</html>"),
    );
    assert_eq!(resp.status(), axum::http::StatusCode::BAD_GATEWAY);
    let json = body_json(resp).await;
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not JSON")
    );
}

#[tokio::test]
async fn burst_dispatch_least_connections_balances_and_respects_max_concurrent() {
    let mut w1 = worker("worker-1", "cpu-voice", "10.0.0.1:8800");
    w1.capacity.max_concurrent = 3;
    let mut w2 = worker("worker-2", "cpu-voice", "10.0.0.2:8800");
    w2.capacity.max_concurrent = 3;
    let state = state_with(vec![w1, w2]);

    let mut counts = std::collections::HashMap::<String, usize>::new();
    for _ in 0..6 {
        let (wid, _) = dispatch_ws_url(&state, "cpu-voice").await;
        *counts.entry(wid).or_insert(0) += 1;
    }
    assert_eq!(counts.get("worker-1"), Some(&3));
    assert_eq!(counts.get("worker-2"), Some(&3));

    // 7th dispatch before any heartbeat frees slots must return 503.
    let resp = super::dispatch(
        State(Arc::clone(&state)),
        Query(DispatchQuery {
            pipeline_name: Some("cpu-voice".into()),
            pipeline_hash: None,
            tags: None,
            policy: None,
        }),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn burst_dispatch_round_robin_cycles_across_workers() {
    let w1 = worker("worker-a", "rr-voice", "10.0.0.1:8800");
    let w2 = worker("worker-b", "rr-voice", "10.0.0.2:8800");
    let state = state_with(vec![w2, w1]);

    let mut chosen = Vec::new();
    for _ in 0..4 {
        let resp = super::dispatch(
            State(Arc::clone(&state)),
            Query(DispatchQuery {
                pipeline_name: Some("rr-voice".into()),
                pipeline_hash: None,
                tags: None,
                policy: Some("round-robin".into()),
            }),
        )
        .await;
        let json = body_json(resp).await;
        chosen.push(json["data"]["worker_id"].as_str().unwrap().to_string());
    }
    assert_ne!(chosen[0], chosen[1]);
    assert_eq!(chosen[0], chosen[2]);
    assert_eq!(chosen[1], chosen[3]);
}
