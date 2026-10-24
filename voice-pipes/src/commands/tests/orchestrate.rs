use super::*;
use crate::api::types::SessionPersistenceConfig;

fn cfg(provider: &str) -> SessionPersistenceConfig {
    SessionPersistenceConfig {
        provider: provider.into(),
        path: None,
    }
}

// An unknown provider string is rejected, not silently turned into
// an in-memory store that loses all history on the next restart.
#[test]
fn unknown_session_store_provider_is_rejected() {
    let err = build_session_store(&cfg("sqlit"), 100).unwrap_err();
    assert!(err.to_string().contains("unknown sessions.store.provider"));
}

#[test]
fn memory_provider_is_accepted() {
    assert!(build_session_store(&cfg("memory"), 100).is_ok());
}

#[test]
fn empty_provider_is_rejected() {
    // A blank/typo'd provider must not fall through to in-memory.
    assert!(build_session_store(&cfg(""), 100).is_err());
}
