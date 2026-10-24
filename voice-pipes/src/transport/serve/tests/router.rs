use super::webtransport_probe_handler;
use axum::response::IntoResponse;

#[tokio::test]
async fn webtransport_probe_is_explicitly_not_implemented() {
    let response = webtransport_probe_handler().await.into_response();
    assert_eq!(response.status(), axum::http::StatusCode::NOT_IMPLEMENTED);
}
