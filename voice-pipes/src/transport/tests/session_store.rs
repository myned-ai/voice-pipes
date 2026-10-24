use super::*;

fn test_config() -> SessionStoreConfig {
    SessionStoreConfig {
        reconnect_grace_period: Duration::from_millis(500),
        replay_buffer_size: 5,
        max_sessions: 10,
        reap_interval: Duration::from_millis(100),
    }
}

#[test]
fn create_session() {
    let store = SessionStore::new(test_config());
    let (id, token) = store.create_session(SessionConfig::default()).unwrap();
    assert!(!id.is_empty());
    assert!(!token.is_empty());
    assert_eq!(store.total_count(), 1);
    assert!(store.is_connected(&id));
}

#[test]
fn capacity_limit_with_all_connected() {
    let store = SessionStore::new(SessionStoreConfig {
        max_sessions: 2,
        ..test_config()
    });
    store.create_session(SessionConfig::default()).unwrap();
    store.create_session(SessionConfig::default()).unwrap();
    // Both slots hold *connected* sessions -> genuinely full.
    let result = store.create_session(SessionConfig::default());
    assert!(matches!(result, Err(TransportError::CapacityExhausted)));
}

// A finished session is `disconnect`ed, not removed; the store must
// reclaim that slot for a new connection instead of bricking the worker.
#[test]
fn disconnected_slot_is_reused_at_capacity() {
    let store = SessionStore::new(SessionStoreConfig {
        max_sessions: 1,
        ..test_config()
    });
    let (id1, _) = store.create_session(SessionConfig::default()).unwrap();
    store.disconnect(&id1); // session ended, now in grace

    // A new connection must succeed by evicting the disconnected session.
    let (id2, _) = store.create_session(SessionConfig::default()).unwrap();
    assert_ne!(id1, id2);
    assert_eq!(store.total_count(), 1);
    assert!(store.is_connected(&id2));
}

#[test]
fn many_sequential_connect_disconnect_never_bricks() {
    // Regression for the leak: opening and closing far more than
    // `max_sessions` sessions sequentially must all succeed.
    let store = SessionStore::new(SessionStoreConfig {
        max_sessions: 4,
        ..test_config()
    });
    for _ in 0..(4 + 10) {
        let (id, _) = store
            .create_session(SessionConfig::default())
            .expect("connection should not be refused after prior ones closed");
        store.disconnect(&id);
    }
    // Store never exceeded capacity.
    assert!(store.total_count() <= 4);
}

#[test]
fn eviction_prefers_oldest_disconnected_and_spares_connected() {
    let store = SessionStore::new(SessionStoreConfig {
        max_sessions: 2,
        ..test_config()
    });
    let (id_old, _) = store.create_session(SessionConfig::default()).unwrap();
    store.disconnect(&id_old); // oldest disconnected
    let (id_live, _) = store.create_session(SessionConfig::default()).unwrap();
    // id_live stays connected; creating a third must evict id_old, not id_live.
    let (id_new, _) = store.create_session(SessionConfig::default()).unwrap();
    assert!(
        store.is_connected(&id_live),
        "connected session must be spared"
    );
    assert!(store.is_connected(&id_new));
    assert_eq!(store.total_count(), 2);
    // The evicted session's token is gone.
    assert!(store.get_config(&id_old).is_none());
}

#[test]
fn disconnect_and_reconnect() {
    let store = SessionStore::new(test_config());
    let (id, token) = store.create_session(SessionConfig::default()).unwrap();

    store.disconnect(&id);
    assert!(!store.is_connected(&id));

    let (recovered_id, new_token, replay) = store.reconnect(&token).unwrap();
    assert_eq!(recovered_id, id);
    assert_ne!(new_token, token); // new token issued
    assert!(replay.is_empty());
    assert!(store.is_connected(&id));
}

#[test]
fn reconnect_with_invalid_token() {
    let store = SessionStore::new(test_config());
    let result = store.reconnect("bogus-token");
    assert!(matches!(result, Err(TransportError::InvalidSession(_))));
}

#[test]
fn replay_buffer_on_reconnect() {
    let store = SessionStore::new(test_config());
    let (id, token) = store.create_session(SessionConfig::default()).unwrap();

    store.disconnect(&id);

    // Buffer some messages.
    let msg1 = ServerMessage::Message(super::super::PipelineMessage {
        kind: "text".into(),
        text: Some("hello".into()),
        payload: None,
        binary: None,
        role: super::super::MessageRole::Assistant,
        turn_id: None,
        message_id: "1".into(),
    });
    let msg2 = ServerMessage::Message(super::super::PipelineMessage {
        kind: "text".into(),
        text: Some("world".into()),
        payload: None,
        binary: None,
        role: super::super::MessageRole::Assistant,
        turn_id: None,
        message_id: "2".into(),
    });
    store.buffer_message(&id, msg1);
    store.buffer_message(&id, msg2);

    let (_, _, replay) = store.reconnect(&token).unwrap();
    assert_eq!(replay.len(), 2);
}

#[test]
fn replay_buffer_overflow() {
    let store = SessionStore::new(SessionStoreConfig {
        replay_buffer_size: 2,
        ..test_config()
    });
    let (id, token) = store.create_session(SessionConfig::default()).unwrap();
    store.disconnect(&id);

    for i in 0..5 {
        store.buffer_message(
            &id,
            ServerMessage::Message(super::super::PipelineMessage {
                kind: "text".into(),
                text: Some(format!("msg-{i}")),
                payload: None,
                binary: None,
                role: super::super::MessageRole::Assistant,
                turn_id: None,
                message_id: format!("{i}"),
            }),
        );
    }

    let (_, _, replay) = store.reconnect(&token).unwrap();
    // Only last 2 kept.
    assert_eq!(replay.len(), 2);
}

#[test]
fn remove_session() {
    let store = SessionStore::new(test_config());
    let (id, _) = store.create_session(SessionConfig::default()).unwrap();
    assert_eq!(store.total_count(), 1);
    store.remove(&id);
    assert_eq!(store.total_count(), 0);
}

#[tokio::test]
async fn reap_expired() {
    let store = SessionStore::new(SessionStoreConfig {
        reconnect_grace_period: Duration::from_millis(50),
        ..test_config()
    });

    let (id, _) = store.create_session(SessionConfig::default()).unwrap();
    store.disconnect(&id);

    // Not yet expired.
    assert_eq!(store.reap_expired(), 0);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Now expired.
    assert_eq!(store.reap_expired(), 1);
    assert_eq!(store.total_count(), 0);
}

#[test]
fn reconnect_after_grace_fails() {
    let store = SessionStore::new(SessionStoreConfig {
        reconnect_grace_period: Duration::from_millis(1),
        ..test_config()
    });
    let (id, token) = store.create_session(SessionConfig::default()).unwrap();
    store.disconnect(&id);

    // Busy-wait past grace period.
    std::thread::sleep(Duration::from_millis(10));

    let result = store.reconnect(&token);
    assert!(matches!(result, Err(TransportError::InvalidSession(_))));
}

#[test]
fn active_count() {
    let store = SessionStore::new(test_config());
    let (id1, _) = store.create_session(SessionConfig::default()).unwrap();
    let (_id2, _) = store.create_session(SessionConfig::default()).unwrap();
    assert_eq!(store.active_count(), 2);

    store.disconnect(&id1);
    assert_eq!(store.active_count(), 1);
    assert_eq!(store.total_count(), 2);
}

#[test]
fn token_uniqueness() {
    // Tokens should be unique.
    let t1 = generate_token();
    let t2 = generate_token();
    assert_ne!(t1, t2);
    // 64 hex chars = 32 bytes = 256 bits.
    assert_eq!(t1.len(), 64);
}
