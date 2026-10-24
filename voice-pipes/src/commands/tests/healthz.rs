use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;

use super::run_healthz;

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_success_answer_passes_and_anything_else_fails() {
    let base = serve(
        Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route("/down", get(|| async { StatusCode::SERVICE_UNAVAILABLE })),
    )
    .await;
    let bound = Duration::from_secs(2);
    run_healthz(&format!("{base}/healthz"), bound)
        .await
        .expect("200 passes");
    let err = run_healthz(&format!("{base}/down"), bound)
        .await
        .expect_err("503 fails");
    assert!(err.to_string().contains("503"), "{err}");
}

#[tokio::test]
async fn nothing_listening_fails_within_the_bound() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let started = std::time::Instant::now();
    run_healthz(&format!("http://{addr}/healthz"), Duration::from_secs(2))
        .await
        .expect_err("refused");
    assert!(started.elapsed() < Duration::from_secs(5));
}
