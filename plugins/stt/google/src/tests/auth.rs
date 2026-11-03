use super::*;

#[tokio::test]
async fn static_token_is_used_verbatim_and_needs_a_project() {
    let auth = AuthSource::new(Some("tok".into()), None, None);
    assert_eq!(auth.bearer().await.unwrap().expose(), "tok");
    let err = auth.project_id().await.unwrap_err();
    assert!(matches!(err, SttError::ConfigError(_)), "{err}");

    let auth = AuthSource::new(Some("tok".into()), None, Some("p".into()));
    assert_eq!(auth.project_id().await.unwrap(), "p");
}

#[tokio::test]
async fn unreadable_key_file_is_a_permanent_auth_error() {
    let auth = AuthSource::new(None, Some("/nope/key.json".into()), None);
    let err = auth.bearer().await.unwrap_err();
    assert!(matches!(err, SttError::AuthError(_)), "{err}");
    assert!(!err.is_transient());
}

#[test]
fn debug_redacts_token() {
    let auth = AuthSource::new(Some("ya29.secret".into()), None, None);
    let s = format!("{auth:?}");
    assert!(!s.contains("secret"), "{s}");
}
