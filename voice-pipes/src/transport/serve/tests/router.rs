use super::{resolve_media_stream_config_with, webtransport_probe_handler};
use axum::response::IntoResponse;

#[tokio::test]
async fn webtransport_probe_is_explicitly_not_implemented() {
    let response = webtransport_probe_handler().await.into_response();
    assert_eq!(response.status(), axum::http::StatusCode::NOT_IMPLEMENTED);
}

#[test]
fn media_stream_config_resolves_inline_custom_env_and_default_env() {
    let env = |k: &str| match k {
        "TWILIO_AUTH_TOKEN" => Some("default-env-token".to_string()),
        "CUSTOM_TWILIO_KEY" => Some("custom-env-token".to_string()),
        "CUSTOM_SIGN_KEY" => Some("custom-sign-secret".to_string()),
        _ => None,
    };

    // 1. Inline auth_token wins over both custom and default env vars.
    let inline = serde_json::json!({
        "auth_token": "inline-token",
        "auth_token_env": "CUSTOM_TWILIO_KEY"
    });
    let resolved = resolve_media_stream_config_with(&inline, Some("TWILIO_AUTH_TOKEN"), env);
    assert_eq!(resolved["auth_token"], "inline-token");

    // 2. Explicit `auth_token_env` and `stream_signing_secret_env` override default env.
    let custom = serde_json::json!({
        "auth_token_env": "CUSTOM_TWILIO_KEY",
        "stream_signing_secret_env": "CUSTOM_SIGN_KEY"
    });
    let resolved = resolve_media_stream_config_with(&custom, Some("TWILIO_AUTH_TOKEN"), env);
    assert_eq!(resolved["auth_token"], "custom-env-token");
    assert_eq!(resolved["stream_signing_secret"], "custom-sign-secret");

    // 3. Omitting both falls back to dialect's default env var (TWILIO_AUTH_TOKEN).
    let fallback = serde_json::json!({ "require_auth": true });
    let resolved = resolve_media_stream_config_with(&fallback, Some("TWILIO_AUTH_TOKEN"), env);
    assert_eq!(resolved["auth_token"], "default-env-token");

    // 4. Invalid env var names are ignored safely.
    let invalid = serde_json::json!({ "auth_token_env": "BAD NAME;rm" });
    let resolved = resolve_media_stream_config_with(&invalid, None, env);
    assert!(resolved.get("auth_token").is_none());
}
