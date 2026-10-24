#![expect(
    clippy::needless_continue,
    reason = "test event-drain loops use explicit continue arms"
)]
use super::common::*;
use crate::transport::direct::channel_pair;
use crate::transport::serve::{TransportState, run_session};
use crate::transport::{
    AudioFormat, ClientControl, ClientMessage, ErrorCode, ServerControl, ServerMessage,
    SessionConfig,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::mpsc;

/// A client that opts in gets the STT's native rate instead of its own
/// proposal — the 512 kbps uplink saving, since browsers capture at 48 kHz
/// and every STT here works at 16 kHz (the mock reports 16 kHz too).
#[tokio::test]
async fn input_rate_is_lowered_to_stt_native_when_the_client_opts_in() {
    let state = test_state();
    let rate = negotiated_input_rate(
        &state,
        SessionConfig {
            sample_rate: 48_000,
            accepts_negotiated_input_rate: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(rate, 16_000);
}

/// The safety gate. A client that never declared it can adapt keeps its
/// proposed rate — lowering it would make the server read 48 kHz audio at
/// 16 kHz with nothing able to detect the mismatch, because the input rate
/// is bound once at the handshake.
#[tokio::test]
async fn input_rate_is_honoured_verbatim_without_the_opt_in() {
    let state = test_state();
    let rate = negotiated_input_rate(
        &state,
        SessionConfig {
            sample_rate: 48_000,
            accepts_negotiated_input_rate: false,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(rate, 48_000, "old clients must be unaffected");
}

/// Never raise: a client proposing less than the STT wants presumably
/// means it, and upsampling invents no information.
#[tokio::test]
async fn input_rate_is_never_raised_above_the_client_proposal() {
    let state = test_state();
    let rate = negotiated_input_rate(
        &state,
        SessionConfig {
            sample_rate: 8_000,
            accepts_negotiated_input_rate: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(rate, 8_000);
}

#[tokio::test]
async fn session_sends_session_ready() {
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    // Send Configure, then SessionReady should arrive with negotiated config.
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig {
                sample_rate: 48_000,
                output_sample_rate: Some(48_000),
                ..Default::default()
            },
        )))
        .await
        .unwrap();

    let msg = client
        .recv_server()
        .await
        .unwrap()
        .expect("expected a message");
    match msg {
        ServerMessage::Control(ServerControl::SessionReady(info)) => {
            assert!(!info.session_id.is_empty());
            assert!(!info.session_token.is_empty());
            assert!(!info.resumed);
            assert_eq!(info.negotiated.input_sample_rate, 48_000);
            assert_eq!(info.negotiated.output_sample_rate, 48_000);
        }
        other => panic!("expected SessionReady, got {other:?}"),
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn session_configure_opus_downgrades_to_pcm_with_fallback_hint() {
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig {
                audio_format: AudioFormat::Opus,
                sample_rate: 48_000,
                output_sample_rate: Some(48_000),
                ..Default::default()
            },
        )))
        .await
        .unwrap();

    let msg = client
        .recv_server()
        .await
        .unwrap()
        .expect("expected a message");
    match msg {
        ServerMessage::Control(ServerControl::SessionReady(info)) => {
            assert_eq!(info.negotiated.audio_format, AudioFormat::PcmI16Le);
            assert_eq!(
                info.negotiated.options.get("audio_format_fallback"),
                Some(&serde_json::Value::String("opus_to_pcm_i16_le".to_string()))
            );
        }
        other => panic!("expected SessionReady, got {other:?}"),
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn session_errors_on_handshake_timeout() {
    // Per the explicit handshake contract, a client that does not send
    // Configure or Resume within the handshake window must receive an
    // Error(HANDSHAKE_TIMEOUT) and the session must close -- the server
    // no longer falls back to default SessionReady.
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    let msg = tokio::time::timeout(std::time::Duration::from_secs(10), client.recv_server())
        .await
        .expect("timed out waiting for handshake error")
        .unwrap()
        .expect("expected a message");

    match msg {
        ServerMessage::Control(ServerControl::Error { code, .. }) => {
            assert_eq!(code, ErrorCode::HandshakeTimeout);
        }
        other => panic!("expected Error(HANDSHAKE_TIMEOUT), got {other:?}"),
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn session_ping_pong() {
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    // Complete handshake first.
    handshake(&client).await;

    // Send ping.
    client
        .send_client(ClientMessage::Control(ClientControl::Ping {
            timestamp_ms: 42,
        }))
        .await
        .unwrap();

    // Expect pong.
    let msg = client.recv_server().await.unwrap().expect("expected pong");
    match msg {
        ServerMessage::Control(ServerControl::Pong {
            client_timestamp_ms,
            ..
        }) => {
            assert_eq!(client_timestamp_ms, 42);
        }
        other => panic!("expected Pong, got {other:?}"),
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

/// A `Cancel` with no turn in flight has nothing to interrupt: the
/// client is asked to flush whatever it may still be playing, and that
/// is all. No metrics snapshot is invented for a turn that never
/// existed and the state machine stays in Listening.
#[tokio::test]
async fn cancel_without_a_turn_only_flushes_the_client() {
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    handshake(&client).await;

    client
        .send_client(ClientMessage::Control(ClientControl::Cancel))
        .await
        .unwrap();
    client
        .send_client(ClientMessage::Control(ClientControl::Ping {
            timestamp_ms: 3,
        }))
        .await
        .unwrap();

    let msg = client
        .recv_server()
        .await
        .unwrap()
        .expect("expected ClearAudio");
    assert!(
        matches!(
            msg,
            ServerMessage::Control(ServerControl::ClearAudio { .. })
        ),
        "expected ClearAudio first, got {msg:?}"
    );
    // The very next frame is the Pong: nothing else was emitted for the
    // cancel.
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_server())
        .await
        .expect("timed out waiting for Pong")
        .unwrap()
        .expect("expected Pong");
    match msg {
        ServerMessage::Control(ServerControl::Pong {
            client_timestamp_ms,
            ..
        }) => assert_eq!(client_timestamp_ms, 3),
        other => panic!("cancel without a turn must emit only ClearAudio, got {other:?}"),
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn active_session_count_tracks() {
    let state = test_state();
    assert_eq!(
        state
            .active_sessions
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    // Complete handshake so the session moves into the main loop.
    handshake(&client).await;

    // Give the session a moment to start.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        state
            .active_sessions
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );

    // Close client.
    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

    // Session count should be back to 0.
    assert_eq!(
        state
            .active_sessions
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

/// A classic session whose config lacks a TTS provider (e.g. a
/// realtime config on a build without the `realtime` feature) must return a
/// clean `SESSION_CREATE_FAILED` error instead of panicking on the internal
/// `.expect()`s -- and the active-session counter must return to 0 via the
/// RAII guard even though the runner exits on the early-error path.
#[tokio::test]
async fn classic_runner_missing_tts_closes_cleanly() {
    let state = test_state_opts(true, false);
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    // Drive the handshake.
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig::default(),
        )))
        .await
        .unwrap();

    // The server must eventually deliver a SessionCreateFailed error (it may
    // send SessionReady first, before the precondition check fires).
    let mut saw_error = false;
    while let Ok(Some(msg)) = client.recv_server().await {
        if let ServerMessage::Control(ServerControl::Error { code, .. }) = msg {
            assert_eq!(code, ErrorCode::SessionCreateFailed);
            saw_error = true;
            break;
        }
    }
    assert!(
        saw_error,
        "expected a SessionCreateFailed error, none arrived"
    );

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

    // The guard decremented the counter despite the early error return.
    assert_eq!(
        state
            .active_sessions
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "active_sessions must return to 0 after a failed session"
    );
}

/// A worker with no STT must still serve `text_in_voice_out`: that mode
/// exists precisely for text-in/speech-out deployments, takes no audio
/// input, and `setup_audio_pipeline` already builds no VAD/STT chain for
/// it. Requiring an STT anyway made such a worker impossible to configure —
/// an absent `[stt]` fell back to the implicit "parakeet-tdt" default and
/// failed startup on a model the operator never asked for.
#[tokio::test]
async fn text_in_voice_out_session_runs_without_stt() {
    let state = test_state_opts(false, true);
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    let cfg = SessionConfig {
        mode: crate::transport::SessionMode::TextInVoiceOut,
        ..SessionConfig::default()
    };
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(cfg)))
        .await
        .unwrap();

    // SessionReady, and crucially no SessionCreateFailed behind it.
    let mut saw_ready = false;
    while let Ok(Some(msg)) = client.recv_server().await {
        match msg {
            ServerMessage::Control(ServerControl::SessionReady(_)) => {
                saw_ready = true;
                break;
            }
            ServerMessage::Control(ServerControl::Error { code, message, .. }) => {
                panic!("session failed without an STT: {code:?} {message}");
            }
            _ => {}
        }
    }
    assert!(saw_ready, "expected SessionReady for a text-input mode");

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

/// The precondition still bites for modes that *do* take audio: a `voice`
/// session on an STT-less worker fails cleanly rather than accepting audio
/// nothing will ever transcribe.
#[tokio::test]
async fn voice_session_without_stt_closes_cleanly() {
    let state = test_state_opts(false, true);
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig::default(), // mode defaults to Voice
        )))
        .await
        .unwrap();

    let mut saw_error = false;
    while let Ok(Some(msg)) = client.recv_server().await {
        if let ServerMessage::Control(ServerControl::Error { code, message, .. }) = msg {
            assert_eq!(code, ErrorCode::SessionCreateFailed);
            // Name the missing provider, the mode as it is actually spelled
            // in config/on the wire, and what to do about it -- an operator
            // should not have to read the source to act on this.
            assert!(
                message.contains("an STT provider"),
                "must name the missing provider, got: {message}"
            );
            assert!(
                message.contains("\"voice\""),
                "must name the mode in wire spelling, got: {message}"
            );
            assert!(
                message.contains("text_in_voice_out"),
                "must suggest the text-input modes as a remedy, got: {message}"
            );
            saw_error = true;
            break;
        }
    }
    assert!(saw_error, "expected a SessionCreateFailed error");

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    assert_eq!(
        state
            .active_sessions
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

/// Verify that the runner emits `TurnMetrics` with `ttft_ms` and `ttfb_ms`
/// populated after a full STT -> LLM -> TTS round-trip.  This exercises
/// the `timeout(metrics_rx.recv())` fix -- before the fix, `try_recv`
/// raced with the spawned `process_utterance` task and metrics were
/// lost.
#[tokio::test]
async fn turn_metrics_include_ttft_and_ttfb() {
    let state = test_state();
    let (client, server) = channel_pair();

    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    handshake_with_rate(&client, Some(16_000)).await;

    // Send speech audio: 20 x 320 samples at 16 kHz (400 ms).
    // The VAD accumulates 320-sample frames (20 ms @ 16 kHz).
    // PassthroughVad marks all frames as speech; onset needs >= 3
    // consecutive speech frames, so we need at least 3 chunks.
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

    // Signal end-of-utterance so the VAD flushes and STT commits.
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

    // Drain messages until we see TurnMetrics (with a generous timeout
    // to allow the silence turn-detector to fire).
    let deadline = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    let mut found_turn_metrics = false;

    while start.elapsed() < deadline {
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(ServerMessage::Control(ServerControl::TurnMetrics { snapshot })))) => {
                // The key assertion: both timing fields must be present.
                assert!(snapshot.ttft_ms().is_some(), "ttft_ms should be reported");
                assert!(snapshot.ttfb_ms().is_some(), "ttfb_ms should be reported");
                found_turn_metrics = true;
                break;
            }
            Ok(Ok(Some(_))) => continue, // skip other messages
            Ok(Ok(None) | Err(_)) | Err(_) => break, // channel closed
                                          // timeout
        }
    }

    assert!(found_turn_metrics, "TurnMetrics message never arrived");

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

// ── Playback-event ordering / stale-id rejection ──────────────
//
// The transport runner used to apply `PlaybackComplete` and
// `PlaybackReport` unconditionally -- even when their `utterance_id`
// referred to a turn that had never started, had already been
// finalized, or did not match the currently-active utterance.  That
// allowed delayed/replayed client reports to corrupt the turn state
// machine (spurious StateChanged -> Listening) and emit misattributed
// BargeInResolved events for the wrong utterance.  The tests below
// pin the corrected contract: stale playback events must be
// ignored deterministically.

/// Regression test: the `session_id` reported to the orchestrator via
/// `session_event_tx` (used as the captures-dir name and the
/// orchestrator's session record key) MUST equal the `session_id` sent
/// to the client in `SessionReady` (which the captures sink also uses).
///
/// Previously `run_session` reported lifecycle events using the
/// pre-handshake transport-layer UUID from `session.metadata()`, while
/// the captures sink and the client both received the post-handshake
/// UUID minted by `SessionStore::create_session`.  The mismatch made
/// the admin BFF unable to locate any captures directory for sessions
/// listed by the orchestrator (empty Conversation Playback panel).
#[tokio::test]
async fn orch_session_id_matches_session_ready_id() {
    // Build a state with a session_event_tx so we can observe the id
    // the runner reports for session-started / session-ended events.
    let base = test_state();
    let (sess_tx, mut sess_rx) = mpsc::channel::<crate::api::types::SessionRecord>(64);
    let state = Arc::new(TransportState {
        config: Arc::clone(&base.config),
        stt: base.stt.clone(),
        tts: base.tts.clone(),
        processors: Arc::clone(&base.processors),
        audio_post_processors: Arc::clone(&base.audio_post_processors),
        annotators: Arc::clone(&base.annotators),
        active_sessions: Arc::new(AtomicUsize::new(0)),
        max_sessions: base.max_sessions,
        ready: AtomicBool::new(true),
        shutdown: tokio_util::sync::CancellationToken::new(),
        turn_complete_tx: None,
        session_event_tx: Some(sess_tx),
        session_store: Arc::clone(&base.session_store),
        pipeline_event_tx: base.pipeline_event_tx.clone(),
        param_registry: Arc::clone(&base.param_registry),
        component_registry: Arc::clone(&base.component_registry),
        client_tools: Arc::clone(&base.client_tools),
    });

    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    // Drive the handshake and capture the SessionReady session_id.
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig::default(),
        )))
        .await
        .unwrap();
    let ready_id = match client
        .recv_server()
        .await
        .unwrap()
        .expect("expected SessionReady")
    {
        ServerMessage::Control(ServerControl::SessionReady(info)) => info.session_id,
        other => panic!("expected SessionReady, got {other:?}"),
    };
    assert!(
        !ready_id.is_empty(),
        "SessionReady session_id must be non-empty"
    );

    // End the session.
    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await;

    // Drain all SessionRecord events the runner emitted.  Every one
    // of them must carry the SAME id as SessionReady.
    let mut seen = Vec::new();
    while let Ok(rec) = sess_rx.try_recv() {
        seen.push(rec);
    }
    assert!(
        !seen.is_empty(),
        "expected at least one SessionRecord (started/ended) to be reported \
         to the orchestrator, got none"
    );
    for rec in &seen {
        assert_eq!(
            rec.id, ready_id,
            "SessionRecord.id must equal SessionReady.session_id (the captures-dir \
             name).  Got rec.id={}, ready_id={}.  Regression: lifecycle events are \
             being reported with the pre-handshake transport id again, which would \
             break the admin Conversation Playback panel.",
            rec.id, ready_id
        );
    }
}

// ── Playback-finalization and early-barge regression tests ─────
//
// Playback finalization: the PlaybackReport handler used to call
// `mark_playback_finalized`, which made the immediately-following
// `PlaybackComplete` (sent back-to-back from `handleClearAudio` on
// the client) get rejected as `"already finalized"`.  The server
// then never transitioned `Draining -> Listening` and the session
// hung forever.
//
// Early barge-in: an "early" server-VAD barge-in (cancel fires while the
// turn state is still `Processing`, so no audio frame and no
// `ClearAudio` were ever sent to the client) used to call
// `transition_to_draining` without any way for the client to drive
// `Draining -> Listening`.  The fix short-circuits that transition
// server-side via `transition_early_barge_to_listening`.

/// A failed component warm-up should surface a non-fatal `Warning` error to
/// the client at connect time (right after `SessionReady`), instead of
/// silently waiting for the first turn to fail. Regression net for the
/// "TTS sidecar not running but no error message" diagnosability gap.
#[tokio::test]
async fn tts_warmup_failure_emits_warning_at_connect() {
    struct FailWarmTts;

    #[async_trait::async_trait]
    impl crate::tts::TtsProvider for FailWarmTts {
        async fn synthesize(
            &self,
            _text: &str,
        ) -> Result<crate::audio::AudioFrame, crate::tts::TtsError> {
            Ok(crate::audio::AudioFrame {
                samples: vec![0; 1600],
                sample_rate: 16000,
                channels: 1,
            })
        }
        fn sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "fail-tts"
        }
        async fn warm_up(&self) -> Result<(), crate::tts::TtsError> {
            Err(crate::tts::TtsError::NetworkError(
                "connection refused (test)".into(),
            ))
        }
    }

    let base = test_state();
    let state = Arc::new(TransportState {
        config: Arc::clone(&base.config),
        stt: base.stt.clone(),
        tts: Some(Arc::new(FailWarmTts)),
        processors: Arc::clone(&base.processors),
        audio_post_processors: Arc::clone(&base.audio_post_processors),
        annotators: Arc::clone(&base.annotators),
        active_sessions: Arc::new(AtomicUsize::new(0)),
        max_sessions: base.max_sessions,
        ready: AtomicBool::new(true),
        shutdown: tokio_util::sync::CancellationToken::new(),
        turn_complete_tx: None,
        session_event_tx: None,
        session_store: Arc::clone(&base.session_store),
        pipeline_event_tx: base.pipeline_event_tx.clone(),
        param_registry: Arc::clone(&base.param_registry),
        component_registry: Arc::clone(&base.component_registry),
        client_tools: Arc::clone(&base.client_tools),
    });

    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });

    handshake(&client).await;

    // Read frames until the warm-up warning arrives (warm-ups run in
    // background tasks, so a couple of unrelated frames may precede it).
    let mut found = false;
    for _ in 0..20 {
        let next =
            tokio::time::timeout(std::time::Duration::from_secs(5), client.recv_server()).await;
        let Ok(Ok(Some(msg))) = next else {
            break;
        };
        if let ServerMessage::Control(ServerControl::Error {
            code,
            severity,
            component,
            ..
        }) = msg
        {
            assert_eq!(code, ErrorCode::TtsWarmupFailed);
            assert_eq!(severity, crate::transport::ErrorSeverity::Warning);
            assert_eq!(component, Some(crate::transport::ErrorComponent::Tts));
            found = true;
            break;
        }
    }
    assert!(
        found,
        "expected a TtsWarmupFailed Warning frame shortly after connect"
    );

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

/// The transport `/health` endpoint should report per-component health
/// pulled from the registry, so a single `curl` against the same port the
/// client connects to reveals (e.g.) a TTS backend that's down.
#[tokio::test]
async fn health_endpoint_reports_component_health() {
    struct UnhealthyTts;
    impl crate::component::Component for UnhealthyTts {
        fn id(&self) -> &'static str {
            "tts:stub"
        }
        fn kind(&self) -> crate::component::ComponentKind {
            crate::component::ComponentKind::Tts
        }
        fn health(&self) -> crate::component::HealthStatus {
            crate::component::HealthStatus::unhealthy("warm-up failed: connection refused")
        }
        fn telemetry(&self) -> crate::component::ComponentTelemetry {
            crate::component::ComponentTelemetry::default()
        }
    }

    let state = test_state();
    state
        .component_registry
        .register(Arc::new(UnhealthyTts) as Arc<dyn crate::component::Component>);

    let app = crate::transport::serve::transport_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client = voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
        std::time::Duration::from_secs(5),
    ))
    .unwrap();
    let resp = client
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["aggregate_health"], "unhealthy");
    let comps = body["components"].as_array().expect("components array");
    let tts = comps
        .iter()
        .find(|c| c["id"] == "tts:stub")
        .expect("tts:stub component present in /health");
    assert_eq!(tts["state"], "unhealthy");
    assert_eq!(tts["kind"], "tts");
    // The open probe path reports states only; the free-text reason (which
    // carries provider error detail) stays on the authenticated debug API.
    assert!(
        tts.get("reason").is_none(),
        "open /health must not carry component reasons: {tts}"
    );

    server.abort();
}

/// The session records sent to the orchestrator carry the pipeline config
/// hash and the negotiated sample rates, and `metadata.json` records the
/// STT mode (from the provider's declared capabilities) and the voice.
#[tokio::test]
async fn session_records_and_metadata_carry_hash_rates_mode_and_voice() {
    let captures = std::env::temp_dir().join(format!(
        "vp_session_meta_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let (event_tx, mut event_rx) = mpsc::channel(8);
    let captures_dir = captures.clone();
    let state = test_state_customised(true, true, None, move |state| {
        let cfg = Arc::get_mut(&mut state.config).expect("config not yet shared");
        cfg.pipeline.audit.enabled = true;
        cfg.pipeline.audit.captures_dir = Some(captures_dir);
        state.session_event_tx = Some(event_tx);
    });
    let expected_hash = crate::config::config_hash(&state.config);

    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig {
                sample_rate: 16_000,
                output_sample_rate: Some(24_000),
                voice: Some("hr_male_test".into()),
                ..Default::default()
            },
        )))
        .await
        .unwrap();
    let session_id = match client.recv_server().await.unwrap().expect("message") {
        ServerMessage::Control(ServerControl::SessionReady(info)) => info.session_id,
        other => panic!("expected SessionReady, got {other:?}"),
    };

    let start = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .expect("session start record")
        .expect("channel open");
    assert_eq!(start.id, session_id);
    assert_eq!(
        start.pipeline_config_hash.as_deref(),
        Some(expected_hash.as_str())
    );
    assert_eq!(start.input_sample_rate, Some(16_000));
    assert_eq!(start.output_sample_rate, Some(24_000));

    // metadata.json is written right after the start record is sent.
    let meta_path = captures.join(&session_id).join("metadata.json");
    for _ in 0..200 {
        if meta_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).expect("metadata.json"))
            .expect("metadata.json parses");
    // The mock STT declares no capabilities, so it is a batch provider.
    assert_eq!(meta["stt_mode"], "batch");
    assert_eq!(meta["tts_voice"], "hr_male_test");
    assert_eq!(meta["pipeline_config_hash"], expected_hash.as_str());

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    let end = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
        .await
        .expect("session end record")
        .expect("channel open");
    assert!(end.ended_at.is_some());
    assert_eq!(
        end.pipeline_config_hash.as_deref(),
        Some(expected_hash.as_str())
    );
    let _ = std::fs::remove_dir_all(&captures);
}

/// The continuous recorder is wired into the real runner, not just unit
/// tested: a session that sends audio leaves one stereo file behind, with
/// the user's audio on channel 0.
///
/// Uses `run_session` rather than poking the handlers so the tee points
/// themselves are covered -- a recorder that is never passed into the
/// context would pass every unit test in `audit::session_audio` and record
/// nothing in production.
#[tokio::test]
async fn a_session_writes_continuous_stereo_audio_when_configured() {
    let captures = std::env::temp_dir().join(format!(
        "vp_session_audio_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let captures_dir = captures.clone();
    let state = test_state_customised(true, true, None, move |state| {
        let cfg = Arc::get_mut(&mut state.config).expect("config not yet shared");
        cfg.pipeline.audit.enabled = true;
        cfg.pipeline.audit.captures_dir = Some(captures_dir);
        cfg.pipeline.audit.record_session_audio = true;
    });

    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig {
                sample_rate: 16_000,
                ..Default::default()
            },
        )))
        .await
        .unwrap();
    let session_id = match client.recv_server().await.unwrap().expect("message") {
        ServerMessage::Control(ServerControl::SessionReady(info)) => info.session_id,
        other => panic!("expected SessionReady, got {other:?}"),
    };

    // Loud, unmistakable user audio: the assertion is that it reaches
    // channel 0, so it has to be distinguishable from silence.
    for _ in 0..5 {
        client
            .send_client(ClientMessage::Audio(crate::pipeline::AudioChunk {
                samples: vec![8000; 1600],
                sample_rate: 16_000,
                channels: 1,
                ..Default::default()
            }))
            .await
            .unwrap();
    }

    drop(client);
    // The recording is written when the session ends. On a loaded machine
    // (a model loading next to the test run) ending has taken over 5 s; a
    // session that never ends still fails, at the timeout.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), handle).await;

    let path = captures.join(&session_id).join("session_audio.wav");
    for _ in 0..1000 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(
        u16::from_le_bytes([bytes[22], bytes[23]]),
        2,
        "session audio must be stereo"
    );

    // Interleaved i16 after the 44-byte canonical header: even samples are
    // the user, odd the agent.
    let pcm: Vec<i16> = bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    let user_peak = pcm.iter().step_by(2).map(|s| s.abs()).max().unwrap_or(0);
    assert!(
        user_peak > 1000,
        "expected the user's audio on channel 0, peak was {user_peak}"
    );

    let _ = std::fs::remove_dir_all(&captures);
}

/// Auditing a session must not start recording it continuously. The
/// per-utterance artefacts are a different consent decision from a
/// wall-to-wall recording of everything the microphone heard.
#[tokio::test]
async fn auditing_a_session_does_not_by_itself_record_continuous_audio() {
    let captures = std::env::temp_dir().join(format!(
        "vp_session_audio_off_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let captures_dir = captures.clone();
    let state = test_state_customised(true, true, None, move |state| {
        let cfg = Arc::get_mut(&mut state.config).expect("config not yet shared");
        cfg.pipeline.audit.enabled = true;
        cfg.pipeline.audit.captures_dir = Some(captures_dir);
        // record_session_audio deliberately left at its default.
    });

    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig {
                sample_rate: 16_000,
                ..Default::default()
            },
        )))
        .await
        .unwrap();
    let session_id = match client.recv_server().await.unwrap().expect("message") {
        ServerMessage::Control(ServerControl::SessionReady(info)) => info.session_id,
        other => panic!("expected SessionReady, got {other:?}"),
    };
    client
        .send_client(ClientMessage::Audio(crate::pipeline::AudioChunk {
            samples: vec![8000; 1600],
            sample_rate: 16_000,
            channels: 1,
            ..Default::default()
        }))
        .await
        .unwrap();

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;

    let path = captures.join(&session_id).join("session_audio.wav");
    assert!(
        !path.exists(),
        "continuous recording must stay off until it is asked for"
    );

    let _ = std::fs::remove_dir_all(&captures);
}

/// A worker shutting down ends each open session with a `Closing` frame
/// that says why, and turns away new work at `/readyz`.
#[tokio::test]
async fn shutdown_ends_an_open_session_with_a_closing_frame() {
    let state = test_state();
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    state.shutdown.cancel();

    let reason = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match client.recv_server().await.unwrap() {
                Some(ServerMessage::Control(ServerControl::Closing { reason })) => break reason,
                Some(_) => continue,
                None => panic!("the session ended without a Closing frame"),
            }
        }
    })
    .await
    .expect("the session did not end on shutdown");
    assert_eq!(reason, "worker_shutdown");
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the session task ended")
        .unwrap();
}

/// A recogniser that never reads its audio.
struct DeafStt;

#[async_trait::async_trait]
impl crate::stt::SttProvider for DeafStt {
    async fn transcribe(
        &self,
        _audio: &crate::audio::AudioFrame,
    ) -> Result<crate::stt::TranscriptionResult, crate::stt::SttError> {
        Ok(crate::stt::TranscriptionResult::new("", true))
    }

    async fn transcribe_stream(
        &self,
        _req: crate::stt::RecognitionRequest,
        _audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        _tx: mpsc::Sender<crate::stt::TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), crate::stt::SttError> {
        cancel.cancelled().await;
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16_000
    }

    fn name(&self) -> &'static str {
        "deaf"
    }

    fn capabilities(&self) -> std::collections::HashSet<crate::stt::SttCapability> {
        std::collections::HashSet::from([crate::stt::SttCapability::Streaming])
    }
}

/// Microphone audio past what the speech path holds, behind a recogniser
/// that reads none of it, does not stop the session: a ping sent after it
/// is still answered.
#[tokio::test]
async fn a_recogniser_that_stops_reading_does_not_stall_the_session() {
    let state = test_state_customised(true, true, None, |s| {
        s.stt = Some(Arc::new(DeafStt));
    });
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    let flood_then_ping = async {
        for _ in 0..3_000 {
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
            .send_client(ClientMessage::Control(ClientControl::Ping {
                timestamp_ms: 42,
            }))
            .await
            .unwrap();
        loop {
            match client.recv_server().await.unwrap() {
                Some(ServerMessage::Control(ServerControl::Pong {
                    client_timestamp_ms: 42,
                    ..
                })) => break,
                Some(_) => continue,
                None => panic!("the session ended"),
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), flood_then_ping)
        .await
        .expect("the session stopped answering behind a recogniser that does not read");

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
}

/// The user speaks, the recogniser finds no words, and the client is told:
/// from its side speech ended and nothing followed, which after a barge-in
/// looks like an agent that was cut off and then refused to answer.
#[tokio::test]
async fn speech_that_makes_no_turn_is_announced_to_the_client() {
    /// Hears every utterance and finds no words in it.
    struct DeafStt;
    #[async_trait::async_trait]
    impl crate::stt::SttProvider for DeafStt {
        async fn transcribe(
            &self,
            _audio: &crate::audio::AudioFrame,
        ) -> Result<crate::stt::TranscriptionResult, crate::stt::SttError> {
            Ok(crate::stt::TranscriptionResult::new("", true))
        }
        async fn transcribe_stream(
            &self,
            _req: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<crate::stt::TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), crate::stt::SttError> {
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    let _ = tx
                        .send(crate::stt::TranscriptionResult::new("", true))
                        .await;
                }
            }
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "deaf-stt"
        }
        fn capabilities(&self) -> std::collections::HashSet<crate::stt::SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let state = test_state_customised(true, true, None, |state| {
        state.stt = Some(Arc::new(DeafStt));
    });
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake_with_rate(&client, Some(16_000)).await;

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

    let start = std::time::Instant::now();
    let mut announced = None;
    while start.elapsed() < std::time::Duration::from_secs(10) {
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(ServerMessage::Control(ServerControl::UtteranceDropped {
                utterance_id,
                reason,
                ..
            })))) => {
                announced = Some((utterance_id, reason));
                break;
            }
            Ok(Ok(Some(ServerMessage::Message(m)))) => {
                assert!(
                    m.kind != "text",
                    "no user bubble for speech that made no turn: {m:?}"
                );
            }
            Ok(Ok(Some(_))) => continue,
            _ => break,
        }
    }
    assert_eq!(
        announced,
        Some((1, "empty_transcript".to_string())),
        "the client is told the speech made no turn"
    );

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
}
