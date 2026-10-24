use super::common::*;
use crate::transport::direct::channel_pair;
use crate::transport::serve::run_session;
use crate::transport::{ClientControl, ClientMessage, ServerMessage};
use std::sync::Arc;

/// Drive handshake -> one voice turn -> playback-complete and return the
/// normalized control-plane golden trace.
async fn classic_happy_path_control_trace() -> Vec<String> {
    use crate::transport::serve::gateway::golden::{control_trace, normalize_control};

    let state = test_state();
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    handshake_with_rate(&client, Some(16_000)).await;

    // Speech: 20 x 320 samples @ 16 kHz, then an end-of-utterance flush.
    for _ in 0..20 {
        client
            .send_client(ClientMessage::Audio(crate::pipeline::AudioChunk {
                samples: vec![500; 320],
                sample_rate: 16_000,
                channels: 1,
                is_end_of_utterance: false,
                caption: None,
                utterance_id: 0,
                full_text: None,
                extras: None,
                last_speech_at: None,
                synth_duration: None,
            }))
            .await
            .unwrap();
    }
    client
        .send_client(ClientMessage::Audio(crate::pipeline::AudioChunk {
            samples: vec![],
            sample_rate: 16_000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        }))
        .await
        .unwrap();

    // The handshake already consumed SessionReady; we prepend its token
    // below so the trace starts from the true beginning of the session.
    let mut messages: Vec<ServerMessage> = Vec::new();

    // Collect until the server enters Draining (turn done and
    // waiting for the client's PlaybackComplete), capturing the active
    // utterance id from emitted audio frames.
    let mut utterance_id = 0u32;
    let deadline = std::time::Duration::from_secs(15);
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() >= deadline {
            break;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(msg))) => {
                if let ServerMessage::Audio(ref c) = msg
                    && c.utterance_id != 0
                {
                    utterance_id = c.utterance_id;
                }
                let is_draining = matches!(
                    normalize_control(&msg).as_deref(),
                    Some("state_changed:draining")
                );
                messages.push(msg);
                if is_draining {
                    break;
                }
            }
            Ok(Ok(None) | Err(_)) | Err(_) => break,
        }
    }

    // Ack playback so the server completes Draining -> Listening.
    let _ = client
        .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
            utterance_id,
        }))
        .await;

    loop {
        if start.elapsed() >= deadline {
            break;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(msg))) => {
                let is_listening = matches!(
                    normalize_control(&msg).as_deref(),
                    Some("state_changed:listening")
                );
                messages.push(msg);
                if is_listening {
                    break;
                }
            }
            Ok(Ok(None) | Err(_)) | Err(_) => break,
        }
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

    // Prepend the SessionReady token consumed during the handshake.
    let mut trace = vec!["session_ready".to_string()];
    trace.extend(control_trace(&messages));
    trace
}

#[tokio::test]
async fn golden_classic_happy_path_control_trace() {
    let trace = classic_happy_path_control_trace().await;
    let actual = trace.join("\n");

    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/transport/serve/gateway/tests/fixtures/classic_happy_path.trace");

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = fixture_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&fixture_path, format!("{actual}\n")).unwrap();
        eprintln!("updated golden fixture: {}", fixture_path.display());
        return;
    }

    let expected = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|_| {
            panic!(
                "golden fixture missing: {}\n\
                 Generate it once with UPDATE_GOLDEN=1, review, and commit.",
                fixture_path.display()
            )
        })
        // Normalize line endings: the fixture may be checked out
        // with CRLF on Windows (git autocrlf), but `actual` joins with '\n'.
        .replace('\r', "");

    assert_eq!(
        actual,
        expected.trim_end(),
        "classic happy-path control trace drifted from the golden fixture.\n\
         If this change is intentional, regenerate with UPDATE_GOLDEN=1 and \
         review the diff."
    );
}

/// Drive an agent-initiated (idle-prompt) turn with NO user input
/// and capture its control-plane trace — the agent-turn counterpart
/// of `classic_happy_path_control_trace`.
async fn agent_initiated_control_trace() -> Vec<String> {
    use crate::transport::serve::gateway::golden::{control_trace, normalize_control};

    let state = test_state_full(true, true, Some(idle_settings(150, 1)));
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    let mut messages: Vec<ServerMessage> = Vec::new();

    // No user input at all — collect until Draining,
    // capturing the agent turn's utterance id from its audio.
    let mut utterance_id = 0u32;
    let deadline = std::time::Duration::from_secs(15);
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() >= deadline {
            break;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(msg))) => {
                if let ServerMessage::Audio(ref c) = msg
                    && c.utterance_id != 0
                {
                    utterance_id = c.utterance_id;
                }
                let is_draining = matches!(
                    normalize_control(&msg).as_deref(),
                    Some("state_changed:draining")
                );
                messages.push(msg);
                if is_draining {
                    break;
                }
            }
            Ok(Ok(None) | Err(_)) | Err(_) => break,
        }
    }

    // Ack playback so the server completes Draining -> Listening.
    let _ = client
        .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
            utterance_id,
        }))
        .await;
    loop {
        if start.elapsed() >= deadline {
            break;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(msg))) => {
                let is_listening = matches!(
                    normalize_control(&msg).as_deref(),
                    Some("state_changed:listening")
                );
                messages.push(msg);
                if is_listening {
                    break;
                }
            }
            Ok(Ok(None) | Err(_)) | Err(_) => break,
        }
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

    let mut trace = vec!["session_ready".to_string()];
    trace.extend(control_trace(&messages));
    trace
}

// ── Client-side tools ──

/// Golden fixture for the agent-initiated turn: pins the
/// control-plane sequence an out-of-turn agent utterance drives,
/// which must look exactly like a user turn minus the user input.
#[tokio::test]
async fn golden_agent_initiated_turn_control_trace() {
    let trace = agent_initiated_control_trace().await;
    let actual = trace.join("\n");

    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/transport/serve/gateway/tests/fixtures/agent_initiated_turn.trace");

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = fixture_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&fixture_path, format!("{actual}\n")).unwrap();
        eprintln!("updated golden fixture: {}", fixture_path.display());
        return;
    }

    let expected = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|_| {
            panic!(
                "golden fixture missing: {}\n\
                 Generate it once with UPDATE_GOLDEN=1, review, and commit.",
                fixture_path.display()
            )
        })
        .replace('\r', "");

    assert_eq!(
        actual,
        expected.trim_end(),
        "agent-initiated turn control trace drifted from the golden fixture.\n\
         If this change is intentional, regenerate with UPDATE_GOLDEN=1 and \
         review the diff."
    );
}
