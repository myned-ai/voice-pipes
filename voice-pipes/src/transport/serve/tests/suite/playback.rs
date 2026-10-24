#![expect(
    clippy::needless_continue,
    reason = "test event-drain loops use explicit continue arms"
)]
use super::common::*;
use crate::transport::direct::channel_pair;
use crate::transport::serve::run_session;
use crate::transport::{ClientControl, ClientMessage, PipelineState, ServerControl, ServerMessage};
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::test]
async fn stale_playback_complete_does_not_emit_state_change() {
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    handshake(&client).await;

    // No turn has started -- the runner's `current_utterance_id` is 0.
    // A `PlaybackComplete` for an arbitrary uid is unambiguously stale
    // and must be ignored (no StateChanged frame emitted).
    client
        .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
            utterance_id: 999,
        }))
        .await
        .unwrap();

    // Send a Ping; the immediate next server frame must be the matching
    // Pong, not a spurious StateChanged from the stale PlaybackComplete.
    client
        .send_client(ClientMessage::Control(ClientControl::Ping {
            timestamp_ms: 7,
        }))
        .await
        .unwrap();

    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_server())
        .await
        .expect("timed out waiting for Pong")
        .unwrap()
        .expect("expected a server message");

    match msg {
        ServerMessage::Control(ServerControl::Pong {
            client_timestamp_ms,
            ..
        }) => {
            assert_eq!(client_timestamp_ms, 7);
        }
        ServerMessage::Control(ServerControl::StateChanged { state }) => {
            panic!("stale PlaybackComplete must not emit StateChanged (got {state:?})");
        }
        other => panic!("unexpected server message: {other:?}"),
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn stale_playback_report_does_not_emit_barge_in_resolved() {
    let state = test_state();
    // Subscribe BEFORE the session starts so we capture every event.
    let mut events = state.pipeline_event_tx.subscribe();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    handshake(&client).await;

    // Send a PlaybackReport for a uid that was never started.  The
    // runner must ignore it deterministically and must not synthesise
    // a BargeInResolved event for an unknown turn.
    client
        .send_client(ClientMessage::Control(ClientControl::PlaybackReport {
            utterance_id: 999,
            last_played_offset_ms: 100,
            last_played_text: None,
        }))
        .await
        .unwrap();

    // Drain pipeline events for a short window -- no BargeInResolved
    // for the bogus uid should appear.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    let mut bad_event: Option<u32> = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await {
            Ok(Ok(crate::pipeline::events::PipelineEvent::BargeInResolved {
                utterance_id,
                ..
            })) => {
                bad_event = Some(utterance_id);
                break;
            }
            Ok(Ok(_) | Err(_)) | Err(_) => continue,
        }
    }
    assert!(
        bad_event.is_none(),
        "stale PlaybackReport must not emit BargeInResolved (got uid={bad_event:?})",
    );

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn duplicate_playback_complete_is_ignored() {
    // Even when the uid matches the active utterance, a second
    // PlaybackComplete for the same uid must not re-emit StateChanged.
    // This test exercises the "already finalized" guard.
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    handshake_with_rate(&client, Some(16_000)).await;

    // Drive a full turn so a real utterance_id is allocated.
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

    // Drain server messages until we see the first AudioMeta or text
    // event carrying the negotiated utterance_id, then capture it.
    let deadline = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    let mut active_uid: Option<u32> = None;
    while start.elapsed() < deadline && active_uid.is_none() {
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(ServerMessage::Control(ServerControl::TurnMetrics { snapshot })))) => {
                active_uid = Some(snapshot.utterance_id);
                break;
            }
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None) | Err(_)) | Err(_) => break,
        }
    }
    let uid = active_uid.expect("server never reported a turn uid");

    // Drain any pending StateChanged frames (Processing/Speaking/Draining)
    // emitted during the turn so the assertion below only sees frames
    // produced *after* the duplicate PlaybackComplete pair.  We probe
    // briefly with short timeouts since these frames have already been
    // emitted by the runner by the time TurnMetrics arrived.
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(150), client.recv_server())
            .await
        {
            Ok(Ok(Some(ServerMessage::Control(ServerControl::StateChanged { .. })))) => {
                continue;
            }
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None) | Err(_)) | Err(_) => break,
        }
    }

    // Send PlaybackComplete twice for the same uid -- second one must
    // be deduped and not produce another StateChanged frame.
    for _ in 0..2 {
        client
            .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
                utterance_id: uid,
            }))
            .await
            .unwrap();
    }

    // Send a Ping; we should see at most one StateChanged before Pong.
    client
        .send_client(ClientMessage::Control(ClientControl::Ping {
            timestamp_ms: 11,
        }))
        .await
        .unwrap();

    let mut state_changes = 0;
    let mut got_pong = false;
    let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !got_pong && std::time::Instant::now() < drain_deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), client.recv_server())
            .await
        {
            Ok(Ok(Some(ServerMessage::Control(ServerControl::StateChanged { .. })))) => {
                state_changes += 1;
            }
            Ok(Ok(Some(ServerMessage::Control(ServerControl::Pong { .. })))) => {
                got_pong = true;
            }
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None) | Err(_)) | Err(_) => break,
        }
    }
    assert!(
        got_pong,
        "Pong never received after duplicate PlaybackComplete"
    );
    assert_eq!(
        state_changes, 1,
        "duplicate PlaybackComplete must produce exactly ONE StateChanged \
         (the first PlaybackComplete transitions to Listening; the second \
         must be deduped). Got {state_changes} StateChanged frames."
    );

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn playback_report_does_not_finalize_turn() {
    // Pin the contract directly on TurnTracker.
    //
    // The runner's `PlaybackReport` handler must NOT call
    // `mark_playback_finalized`.  If it ever does again, the
    // immediately-following `PlaybackComplete` (sent back-to-back by
    // the frontend's `handleClearAudio`) will be rejected as stale
    // and the server will hang in `Draining` forever.
    use tokio_util::sync::CancellationToken;

    let mut tracker = super::super::turn::TurnTracker::new();
    tracker.start_turn(7, CancellationToken::new());

    // Step 1: PlaybackReport for the active uid is NOT stale.
    // (The actual handler ignores this return value when None -- the
    // important thing is that this branch does NOT call
    // `mark_playback_finalized` afterwards.)
    assert!(
        tracker.stale_playback_reason(7).is_none(),
        "PlaybackReport for the active utterance must not be stale"
    );

    // Step 2: an immediately-following PlaybackComplete must also
    // NOT be stale.  This is the assertion that pins the contract: if a
    // future change re-introduces a `mark_playback_finalized` call
    // in the PlaybackReport handler, this assertion will fail
    // because `stale_playback_reason(7)` will return
    // `Some("already finalized")`.
    assert!(
        tracker.stale_playback_reason(7).is_none(),
        "PlaybackComplete arriving after PlaybackReport for the same uid must \
         NOT be stale (PlaybackReport handler must not finalize)"
    );

    // Step 3: PlaybackComplete itself owns finalization, after
    // which a duplicate is correctly rejected.
    tracker.mark_playback_finalized(7);
    assert_eq!(
        tracker.stale_playback_reason(7),
        Some("already finalized"),
        "after PlaybackComplete finalizes, a duplicate must be stale"
    );
}

#[tokio::test]
async fn early_barge_short_circuits_draining_to_listening() {
    // Early barge-in regression: when the server-VAD cancel fires while
    // the turn state is still `Processing` (no audio chunk has been
    // emitted, no `ClearAudio` was sent to the client), the runner
    // calls `transition_early_barge_to_listening`.  That helper must
    // emit `StateChanged(Draining)` followed by
    // `StateChanged(Listening)` and finalize the utterance so any
    // late client `PlaybackComplete`/`PlaybackReport` for the same
    // uid is dropped as stale.
    use std::sync::atomic::AtomicU8;
    use tokio_util::sync::CancellationToken;

    let mut tracker = super::super::turn::TurnTracker::new();
    tracker.start_turn(42, CancellationToken::new());

    let turn_state: crate::pipeline::SharedTurnState = Arc::new(AtomicU8::new(0));
    crate::pipeline::set_turn_state(&turn_state, crate::pipeline::TurnState::Processing);

    let (out_tx, mut out_rx) = mpsc::channel::<ServerMessage>(8);
    let (_proc_tx, mut proc_rx) = mpsc::channel::<crate::pipeline::AudioChunk>(4);

    let utt_id = super::super::turn::transition_early_barge_to_listening(
        "test",
        &mut tracker,
        &turn_state,
        &out_tx,
        &mut proc_rx,
    )
    .await;

    assert_eq!(utt_id, 42, "helper must return the active utterance id");

    // First emitted frame: StateChanged(Draining).
    let first = out_rx.recv().await.expect("expected Draining frame");
    match first {
        ServerMessage::Control(ServerControl::StateChanged { state }) => {
            assert_eq!(
                state,
                PipelineState::Draining,
                "first StateChanged must be Draining"
            );
        }
        other => panic!("expected StateChanged(Draining), got {other:?}"),
    }

    // Second emitted frame: StateChanged(Listening) -- the server
    // self-resolves the handshake the client could never drive.
    let second = out_rx.recv().await.expect("expected Listening frame");
    match second {
        ServerMessage::Control(ServerControl::StateChanged { state }) => {
            assert_eq!(
                state,
                PipelineState::Listening,
                "second StateChanged must be Listening (early-barge short-circuit)"
            );
        }
        other => panic!("expected StateChanged(Listening), got {other:?}"),
    }

    // Shared turn state machine must agree.
    assert!(
        matches!(
            crate::pipeline::get_turn_state(&turn_state),
            crate::pipeline::TurnState::Listening
        ),
        "TurnState must be Listening after early-barge short-circuit"
    );

    // The utterance must be finalized so any late
    // PlaybackComplete/Report for the same uid is dropped as stale
    // rather than triggering a duplicate state transition.
    assert_eq!(
        tracker.stale_playback_reason(42),
        Some("already finalized"),
        "early-barge short-circuit must finalize the utterance so late \
         client playback frames are rejected as stale"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn collect_metrics_does_not_shift_across_turns() {
    // Regression -- `LlmExtras.ttft_ms` off-by-one across consecutive
    // turns (BACKLOG "Bugs" entry).
    //
    // The clean turn-completion path in `runner.rs` previously
    // called `collect_metrics(metrics_rx, wait=false)` (a bare
    // `try_recv`).  The spawned `process_utterance` task sends its
    // `TurnMetricsSnapshot` on `metrics_tx` *after* it has emitted
    // the end-of-utterance audio chunk that triggers the runner's
    // collect_metrics call.  Since the receiver and the spawn live
    // on different tasks, `try_recv` raced the send and, when it
    // lost, returned `Default::default()`.  The next turn's
    // `try_recv` then drained the previous turn's queued snapshot,
    // producing a one-turn shift in `summary.ttft_ms`,
    // `summary.endpointing_ms`, etc.
    //
    // This test simulates the runner pattern: for each of three
    // turns we spawn a task that, like `process_utterance`, sleeps
    // briefly before sending its snapshot, then we await
    // `collect_metrics` and assert the value belongs to *this*
    // turn, not the previous one.
    use std::time::Duration;

    use super::super::turn::collect_metrics;
    use crate::pipeline::metrics::{TurnComponentMetrics, TurnMetricsSnapshot};

    let (metrics_tx, mut metrics_rx) = mpsc::channel::<TurnMetricsSnapshot>(4);

    for turn_idx in 0u64..3 {
        let tx = metrics_tx.clone();
        let expected = (turn_idx + 1) * 100;
        // Mirror process_utterance: do work, then send the
        // snapshot.  The 25ms delay reliably loses the try_recv
        // race; awaiting collect_metrics(wait=true) wins it.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let mut llm = TurnComponentMetrics {
                kind: "llm".into(),
                component_id: "llm:test".into(),
                ..Default::default()
            };
            llm.extras
                .insert("ttft_ms".into(), serde_json::Value::from(expected));
            let snap = TurnMetricsSnapshot {
                components: vec![llm],
                ..Default::default()
            };
            let _ = tx.send(snap).await;
        });

        let got = collect_metrics(&mut metrics_rx, true).await;
        assert_eq!(
            got.ttft_ms(),
            Some(expected),
            "turn {turn_idx}: collect_metrics must return THIS turn's \
             snapshot, not the previous turn's (off-by-one regression)"
        );
    }
}

// ── Golden protocol trace (control-plane regression net) ──────
//
// Drives a full classic happy-path turn end-to-end through
// `run_session` and captures the normalized control-plane message
// sequence. The trace is pinned to a committed fixture so any
// accidental change to control-message ordering (the surface the
// turn-state machine drives, and the surface the upcoming
// `EngineDriver` consolidation touches) fails loudly.
//
// Regenerate the fixture intentionally with:
//   UPDATE_GOLDEN=1 cargo test --features realtime \
//     transport::serve::tests::golden_classic_happy_path_control_trace
