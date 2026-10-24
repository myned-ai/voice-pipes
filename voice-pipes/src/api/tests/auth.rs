#![expect(
    clippy::disallowed_methods,
    reason = "plain clients calling the local test server; the policy under test is the server's"
)]

use super::*;
use axum::routing::get;

fn authed(token: Option<&str>) -> SurfaceAuth {
    SurfaceAuth {
        token: token.map(Arc::from),
        surface: "test",
        env_name: "VP_TEST_TOKEN".to_string(),
    }
}

fn app(auth: &SurfaceAuth) -> Router {
    let router = Router::new()
        .route("/secret", get(|| async { "ok" }))
        .route("/healthz", get(|| async { "alive" }));
    protect_router(router, auth)
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn open_surface_passes_everything() {
    let base = serve(app(&authed(None))).await;
    let resp = reqwest::get(format!("{base}/secret")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn enforced_surface_rejects_missing_and_wrong_tokens() {
    let base = serve(app(&authed(Some("s3cret")))).await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{base}/secret")).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "UNAUTHENTICATED");

    let resp = client
        .get(format!("{base}/secret"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn enforced_surface_accepts_bearer_api_key_and_query() {
    let base = serve(app(&authed(Some("s3cret")))).await;
    let client = reqwest::Client::new();

    for req in [
        client.get(format!("{base}/secret")).bearer_auth("s3cret"),
        client
            .get(format!("{base}/secret"))
            .header("x-api-key", "s3cret"),
        client.get(format!("{base}/secret?token=s3cret")),
    ] {
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}

/// A flag-style query pair before the token does not hide the token.
#[test]
fn query_token_is_found_after_a_pair_without_a_value() {
    let headers = HeaderMap::new();
    assert_eq!(
        presented_token(&headers, Some("debug&token=abc")).as_deref(),
        Some("abc")
    );
    assert_eq!(
        presented_token(&headers, Some("a=1&flag&api_key=k")).as_deref(),
        Some("k")
    );
    assert_eq!(presented_token(&headers, Some("flag")), None);
}

/// With CORS applied outside the auth layer, a browser preflight (no
/// token) is answered, and it advertises the headers and methods the
/// admin API takes.
#[tokio::test]
async fn cors_preflight_is_answered_on_an_enforced_surface() {
    let cors = crate::api::orchestrator::build_cors(&["http://ui.example".to_string()]);
    let router = app(&authed(Some("s3cret"))).layer(cors);
    let base = serve(router).await;
    let client = reqwest::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("{base}/secret"))
        .header("Origin", "http://ui.example")
        .header("Access-Control-Request-Method", "PATCH")
        .header(
            "Access-Control-Request-Headers",
            "authorization,content-type",
        )
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let allow_methods = resp
        .headers()
        .get("access-control-allow-methods")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_uppercase();
    assert!(allow_methods.contains("PATCH"), "{allow_methods}");
    let allow_headers = resp
        .headers()
        .get("access-control-allow-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(allow_headers.contains("authorization"), "{allow_headers}");
    // The actual request still needs the token.
    let resp = client
        .get(format!("{base}/secret"))
        .header("Origin", "http://ui.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn health_probes_stay_open_on_enforced_surface() {
    let base = serve(app(&authed(Some("s3cret")))).await;
    let resp = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

/// The admin and internal routers share one listener, so
/// the separation guarantee is per-surface credentials — an admin
/// token must not unlock `/internal/*`.
#[tokio::test]
async fn admin_token_cannot_call_internal_endpoints() {
    let state = Arc::new(crate::api::types::OrchestratorState {
        config: crate::api::types::OrchestratorConfig::default(),
        workers: Arc::new(dashmap::DashMap::new()),
        sessions: Arc::new(crate::audit::InMemorySessionStore::new(10)),
        audit_store: Arc::new(crate::audit::LocalFsAuditStore::new(
            std::env::temp_dir().join("vp_auth_sep_test"),
        )),
        tap_store: None,
        start_time: std::time::Instant::now(),
    });
    let admin = protect_router(
        crate::api::orchestrator::router(Arc::clone(&state), Vec::new()),
        &authed(Some("admin-tok")),
    );
    let internal = protect_router(
        crate::api::internal::router(state),
        &authed(Some("internal-tok")),
    );
    let base = serve(admin.merge(internal)).await;
    let client = reqwest::Client::new();
    let hb = serde_json::json!({
        "worker_id": "w1",
        "capacity": { "max_concurrent": 1, "active_sessions": 0 },
    });

    // Admin token unlocks the admin surface…
    let resp = client
        .get(format!("{base}/api/v1/workers"))
        .bearer_auth("admin-tok")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // …but not the internal surface.
    let resp = client
        .post(format!("{base}/internal/workers/heartbeat"))
        .bearer_auth("admin-tok")
        .json(&hb)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // The internal token authenticates (404 = unknown worker, i.e.
    // the handler ran).
    let resp = client
        .post(format!("{base}/internal/workers/heartbeat"))
        .bearer_auth("internal-tok")
        .json(&hb)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // No token at all: both surfaces reject.
    let resp = client
        .get(format!("{base}/api/v1/workers"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[test]
fn required_without_token_aborts_startup() {
    let auth = authed(None);
    assert!(auth.enforce_required(false).is_ok());
    let err = auth.enforce_required(true).unwrap_err().to_string();
    assert!(err.contains("VP_TEST_TOKEN"), "{err}");
    assert!(authed(Some("t")).enforce_required(true).is_ok());
}

#[test]
fn resolve_treats_empty_env_as_unset() {
    // SAFETY: test-local env var name; tests touching distinct vars.
    unsafe { std::env::set_var("VP_TEST_EMPTY_TOKEN", "   ") };
    let auth = SurfaceAuth::resolve("VP_TEST_EMPTY_TOKEN", "test");
    assert!(!auth.enabled());
}

#[test]
fn debug_never_prints_the_token() {
    let auth = authed(Some("super-secret-value"));
    let s = format!("{auth:?}");
    assert!(!s.contains("super-secret-value"), "{s}");
}

#[test]
fn settings_default_and_toml_roundtrip() {
    let s: AuthSettings = toml::from_str("").unwrap();
    assert_eq!(s, AuthSettings::default());
    assert!(!s.required);
    let s: AuthSettings =
        toml::from_str("required = true\nadmin_token_env = \"MY_ADMIN\"").unwrap();
    assert!(s.required);
    assert_eq!(s.admin_token_env, "MY_ADMIN");
}

#[test]
fn fleet_client_refuses_a_token_no_header_can_carry() {
    let auth = authed(Some("line\nbreak"));
    let err = http_client_with_bearer(&auth).expect_err("a token with a newline");
    let message = err.to_string();
    assert!(matches!(err, FleetClientError::InvalidToken { .. }));
    assert!(message.contains("VP_TEST_TOKEN"), "{message}");
    assert!(!message.contains("line"), "the token leaked: {message}");
}

#[test]
fn fleet_client_builds_with_a_valid_token_and_without_one() {
    assert!(http_client_with_bearer(&authed(Some("abc123"))).is_ok());
    assert!(http_client_with_bearer(&authed(None)).is_ok());
}
