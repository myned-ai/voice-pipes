use super::*;

#[tokio::test]
async fn noop_always_succeeds() {
    let auth = NoopAuthenticator;
    let result = auth.authenticate(&AuthCredentials::Anonymous).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().user_id, "anonymous");
}

#[tokio::test]
async fn noop_succeeds_with_token() {
    let auth = NoopAuthenticator;
    let result = auth
        .authenticate(&AuthCredentials::Token("anything".into()))
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn token_auth_valid() {
    let mut tokens = HashMap::new();
    tokens.insert("secret-key".into(), "user-42".into());
    let auth = TokenAuthenticator::new(tokens);

    let result = auth
        .authenticate(&AuthCredentials::Token("secret-key".into()))
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().user_id, "user-42");
}

#[tokio::test]
async fn token_auth_invalid() {
    let mut tokens = HashMap::new();
    tokens.insert("secret-key".into(), "user-42".into());
    let auth = TokenAuthenticator::new(tokens);

    let result = auth
        .authenticate(&AuthCredentials::Token("wrong-key".into()))
        .await;
    assert!(matches!(result, Err(TransportError::AuthFailed(_))));
}

#[tokio::test]
async fn token_auth_anonymous_rejected() {
    let tokens = HashMap::new();
    let auth = TokenAuthenticator::new(tokens);

    let result = auth.authenticate(&AuthCredentials::Anonymous).await;
    assert!(matches!(result, Err(TransportError::AuthFailed(_))));
}
