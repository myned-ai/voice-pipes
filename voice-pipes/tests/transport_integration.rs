//! Transport integration tests — full pipeline round-trips via direct transport.
//!
//! These tests wire mock STT/TTS/text providers through the transport
//! `run_session` flow, verifying the complete path:
//!   client audio → STT → LLM → TTS → client audio

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::time::timeout;

use voice_pipes::audio::AudioFrame;
use voice_pipes::config::{AdaptiveQualityConfig, PipelineConfig, TurnDetectionConfig};
use voice_pipes::pipeline::AudioChunk;
use voice_pipes::stt::{
    AudioChunk as SttAudioChunk, RecognitionRequest, SttCapability, SttError, SttProvider,
    TranscriptionResult,
};
use voice_pipes::text::{TextProcessor, TextProcessorError};
use voice_pipes::transport::direct::channel_pair;
use voice_pipes::transport::serve::{TransportState, run_session};
use voice_pipes::transport::session_store::{SessionStore, SessionStoreConfig};
use voice_pipes::transport::{
    ClientControl, ClientMessage, PipelineState, ServerControl, ServerMessage, SessionConfig,
};
use voice_pipes::tts::{TtsError, TtsProvider};

// ── Timeout for all test awaits ─────────────────────────────

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

// ── Mock STT ────────────────────────────────────────────────

/// Mock STT that collects audio and emits a final transcription
/// once it sees `is_last = true`.
struct MockStt {
    /// Text to return for every utterance.
    response_text: &'static str,
}

#[async_trait]
impl SttProvider for MockStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Ok(TranscriptionResult::new(self.response_text, true).with_confidence(Some(0.95)))
    }

    async fn transcribe_stream(
        &self,
        _req: RecognitionRequest,
        mut audio_rx: mpsc::Receiver<SttAudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        // Process multiple utterances: emit a final result each time
        // we see is_last, then continue listening for more audio.
        let mut got_samples = false;
        while let Some(chunk) = audio_rx.recv().await {
            if !chunk.samples.is_empty() {
                got_samples = true;
            }
            if chunk.is_last && got_samples {
                let _ = tx
                    .send(
                        TranscriptionResult::new(self.response_text, true)
                            .with_confidence(Some(0.95))
                            .with_is_end_of_turn(Some(true)),
                    )
                    .await;
                got_samples = false;
            }
        }
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "mock-stt"
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        let mut s = HashSet::new();
        s.insert(SttCapability::Streaming);
        s.insert(SttCapability::EndOfTurn);
        s
    }
}

// ── Mock TTS ────────────────────────────────────────────────

/// Mock TTS that returns a fixed-length audio frame for any input.
struct MockTts;

#[async_trait]
impl TtsProvider for MockTts {
    async fn synthesize(&self, _text: &str) -> Result<AudioFrame, TtsError> {
        Ok(AudioFrame {
            samples: vec![500; 1600], // 100ms at 16kHz
            sample_rate: 16000,
            channels: 1,
        })
    }

    fn sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "mock-tts"
    }
}

/// TTS that blocks inside `synthesize` until the test releases it.
///
/// `MockTts` returns instantly, so a turn driven by it completes before a
/// client `Cancel` can be written to the wire -- the cancel then arrives with
/// no active turn, which is a different code path from barge-in. Holding the
/// turn open here makes "interrupt a turn that is still producing"
/// deterministic instead of a race the test usually loses.
struct GatedMockTts {
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl TtsProvider for GatedMockTts {
    async fn synthesize(&self, _text: &str) -> Result<AudioFrame, TtsError> {
        self.release.notified().await;
        Ok(AudioFrame {
            samples: vec![500; 1600], // 100ms at 16kHz
            sample_rate: 16000,
            channels: 1,
        })
    }

    fn sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "gated-mock-tts"
    }
}

// ── Echo text processor ─────────────────────────────────────

/// Text processor that passes input through unchanged.
struct EchoProcessor;

#[async_trait]
impl TextProcessor for EchoProcessor {
    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        Ok(input.to_string())
    }

    fn name(&self) -> &'static str {
        "echo"
    }
}

// ── Helpers ─────────────────────────────────────────────────

fn test_config() -> PipelineConfig {
    PipelineConfig {
        pipeline: voice_pipes::config::PipelineSettings::default(),
        speculative: voice_pipes::config::SpeculativeConfig::default(),
        stt: None,
        tts: None,
        output: None,
        vad: None,
        grading: None,
        turn_detection: Some(TurnDetectionConfig {
            mode: "silence".into(),
            silence_timeout_ms: 100, // fast timeout for tests
            options: std::collections::HashMap::default(),
        }),
        barge_in: None,
        adaptive_quality: AdaptiveQualityConfig::default(),
        transport: None,
        text_processor: vec![],
        audio_post_processor: vec![],
        audio_filter: vec![],
        realtime_provider: None,
        auth: None,
        agent: None,
        client_tool: vec![],
        client_tools_path: None,
    }
}

fn test_state(stt: impl SttProvider + 'static) -> Arc<TransportState> {
    test_state_with_tts(stt, MockTts)
}

fn test_state_with_tts(
    stt: impl SttProvider + 'static,
    tts: impl TtsProvider + 'static,
) -> Arc<TransportState> {
    Arc::new(TransportState {
        config: Arc::new(test_config()),
        stt: Some(Arc::new(stt)),
        tts: Some(Arc::new(tts)),
        processors: Arc::new(vec![Box::new(EchoProcessor)]),
        audio_post_processors: Arc::new(vec![]),
        annotators: Arc::new(vec![]),
        active_sessions: Arc::new(AtomicUsize::new(0)),
        max_sessions: 16,
        ready: AtomicBool::new(true),
        shutdown: tokio_util::sync::CancellationToken::new(),
        turn_complete_tx: None,
        session_event_tx: None,
        session_store: Arc::new(SessionStore::new(SessionStoreConfig::default())),
        pipeline_event_tx: tokio::sync::broadcast::channel(16).0,
        param_registry: std::sync::Arc::new(voice_pipes::api::param_store::ParamRegistry::new()),
        component_registry: std::sync::Arc::new(voice_pipes::component::ComponentRegistry::new()),
        client_tools: std::sync::Arc::new(vec![]),
    })
}

/// Consume messages until we find a specific variant, returning it.
/// Other messages are collected in the returned Vec.
async fn recv_until<F>(
    client: &voice_pipes::transport::direct::DirectTransport,
    mut pred: F,
) -> (ServerMessage, Vec<ServerMessage>)
where
    F: FnMut(&ServerMessage) -> bool,
{
    let mut others = Vec::new();
    loop {
        let msg = timeout(TEST_TIMEOUT, client.recv_server())
            .await
            .expect("timed out waiting for message")
            .expect("transport error")
            .expect("transport closed unexpectedly");
        if pred(&msg) {
            return (msg, others);
        }
        others.push(msg);
    }
}

/// Send an audio chunk to the server via the client transport.
async fn send_audio(
    client: &voice_pipes::transport::direct::DirectTransport,
    samples: Vec<i16>,
    is_end: bool,
) {
    client
        .send_client(ClientMessage::Audio(AudioChunk {
            samples,
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: is_end,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        }))
        .await
        .expect("failed to send audio");
}

/// Pick the server-allocated `utterance_id` for the active turn from
/// messages already drained by [`recv_until`]. `TurnMetrics` is the
/// authoritative source — its uid is always the real per-turn id even
/// when that id happens to be `0` (the first STT-driven utterance of a
/// session). Audio frames are a fallback because their `utterance_id`
/// field defaults to `0` for non-captioned chunks and is only
/// guaranteed to be set on captioned chunks. Returns `None` only when
/// no `TurnMetrics` was seen and `captured_audio_uid` plus all observed
/// audio uids were `0` (i.e. nothing identifying ever arrived).
fn pick_server_utterance_id(captured_audio_uid: u32, drained: &[ServerMessage]) -> Option<u32> {
    if let Some(uid) = drained.iter().find_map(|m| match m {
        ServerMessage::Control(ServerControl::TurnMetrics { snapshot }) => {
            Some(snapshot.utterance_id)
        }
        _ => None,
    }) {
        return Some(uid);
    }

    std::iter::once(captured_audio_uid)
        .chain(drained.iter().filter_map(|m| match m {
            ServerMessage::Audio(chunk) => Some(chunk.utterance_id),
            _ => None,
        }))
        .max()
}

/// Send Configure + wait for `SessionReady`. Avoids the 5-second
/// handshake timeout when no Configure is sent.
async fn handshake(client: &voice_pipes::transport::direct::DirectTransport) {
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            SessionConfig {
                sample_rate: 16000,
                ..Default::default()
            },
        )))
        .await
        .expect("failed to send Configure");
    let msg = timeout(TEST_TIMEOUT, client.recv_server())
        .await
        .expect("handshake timed out")
        .expect("transport error")
        .expect("transport closed");
    assert!(
        matches!(msg, ServerMessage::Control(ServerControl::SessionReady(_))),
        "expected SessionReady, got {msg:?}"
    );
}

// ── Tests ───────────────────────────────────────────────────

/// Full voice round-trip: send audio → receive STT → LLM → TTS audio back.
#[tokio::test]
async fn full_voice_round_trip() {
    let state = test_state(MockStt {
        response_text: "Hello world from mock STT.",
    });
    let (client, server) = channel_pair();

    let handle = tokio::spawn(run_session(Box::new(server), state));

    // 1. Handshake (Configure → SessionReady).
    handshake(&client).await;

    // 2. Send audio with speech, then end-of-utterance.
    send_audio(&client, vec![1000; 1600], false).await; // 100ms of "speech"
    send_audio(&client, vec![], true).await; // end-of-utterance

    // 3. Expect Processing state.
    let (processing_msg, _) = recv_until(&client, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Processing
            })
        )
    })
    .await;
    assert!(matches!(
        processing_msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Processing
        })
    ));

    // 4. Expect Speaking state.
    let (speaking_msg, _) = recv_until(&client, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Speaking
            })
        )
    })
    .await;
    assert!(matches!(
        speaking_msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Speaking
        })
    ));

    // 5. Expect at least one audio message.
    let (audio_msg, _) = recv_until(&client, |m| matches!(m, ServerMessage::Audio(_))).await;
    let captured_audio_uid = match audio_msg {
        ServerMessage::Audio(chunk) => {
            assert!(!chunk.samples.is_empty(), "expected non-empty audio");
            chunk.utterance_id
        }
        _ => panic!("expected Audio message"),
    };

    // 6. Expect return to Draining (after end-of-utterance audio).
    let (draining_msg, draining_others) = recv_until(&client, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Draining
            })
        )
    })
    .await;
    // Capture the server-allocated utterance_id so the PlaybackComplete
    // below addresses the active turn. TurnMetrics carries the
    // authoritative server uid; fall back to non-zero audio uids if
    // TurnMetrics has not landed yet.
    let server_utterance_id = pick_server_utterance_id(captured_audio_uid, &draining_others)
        .expect("server should have emitted a uid via TurnMetrics or audio frames");
    assert!(matches!(
        draining_msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Draining
        })
    ));

    // 7. Unblock Draining.
    client
        .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
            utterance_id: server_utterance_id,
        }))
        .await
        .unwrap();

    let (listening_msg, _) = recv_until(&client, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Listening
            })
        )
    })
    .await;
    assert!(matches!(
        listening_msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Listening
        })
    ));

    // Cleanup.
    drop(client);
    let _ = timeout(TEST_TIMEOUT, handle).await;
}

/// Barge-in: cancel mid-processing should return to Listening
/// and discard any pending TTS output.
#[tokio::test]
async fn barge_in_cancels_processing() {
    // The TTS blocks until `release` fires, so the turn is still producing
    // when the cancel lands -- the situation barge-in is actually about.
    let release = Arc::new(tokio::sync::Notify::new());
    let state = test_state_with_tts(
        MockStt {
            response_text: "This is a long response that should get cancelled.",
        },
        GatedMockTts {
            release: Arc::clone(&release),
        },
    );
    let (client, server) = channel_pair();

    let handle = tokio::spawn(run_session(Box::new(server), state));

    handshake(&client).await;

    // Send audio to trigger processing.
    send_audio(&client, vec![1000; 1600], false).await;
    send_audio(&client, vec![], true).await;

    // Wait for Processing state.
    let _ = recv_until(&client, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Processing
            })
        )
    })
    .await;

    // Send Cancel (barge-in) while synthesis is still blocked.
    client
        .send_client(ClientMessage::Control(ClientControl::Cancel))
        .await
        .unwrap();

    // Should get ClearAudio before returning to Listening.
    let (clear_msg, _) = recv_until(&client, |m| {
        matches!(m, ServerMessage::Control(ServerControl::ClearAudio { .. }))
    })
    .await;
    match clear_msg {
        ServerMessage::Control(ServerControl::ClearAudio { reason, .. }) => {
            assert_eq!(
                reason, "client_cancel",
                "ClearAudio reason should be client_cancel"
            );
        }
        _ => panic!("expected ClearAudio"),
    }

    // Should get StateChanged → Draining after ClearAudio. Capture any
    // server-allocated utterance_id seen along the way so PlaybackComplete
    // below can target the active turn.
    let (msg, draining_others) = recv_until(&client, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Draining
            })
        )
    })
    .await;
    assert!(matches!(
        msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Draining
        })
    ));

    // The turn's metrics must say it was interrupted. Without this the test
    // still passes when the turn merely ran to completion on its own.
    let interrupted = draining_others.iter().any(|m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::TurnMetrics { snapshot })
                if snapshot.interrupted
        )
    });
    assert!(
        interrupted,
        "the cancelled turn must report interrupted metrics: {draining_others:?}"
    );

    let server_utterance_id = pick_server_utterance_id(0, &draining_others).unwrap_or(1);

    client
        .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
            utterance_id: server_utterance_id,
        }))
        .await
        .unwrap();

    let (listening_msg, _) = recv_until(&client, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Listening
            })
        )
    })
    .await;
    assert!(matches!(
        listening_msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Listening
        })
    ));

    // Let the (already cancelled) synthesis task fall through on teardown.
    release.notify_waiters();
    drop(client);
    let _ = timeout(TEST_TIMEOUT, handle).await;
}

/// Multiple concurrent sessions: each session gets its own pipeline
/// and processes independently.
#[tokio::test]
async fn concurrent_sessions() {
    let state = test_state(MockStt {
        response_text: "concurrent test response.",
    });

    let (client_a, server_a) = channel_pair();
    let (client_b, server_b) = channel_pair();

    let state_a = Arc::clone(&state);
    let state_b = Arc::clone(&state);

    let handle_a = tokio::spawn(run_session(Box::new(server_a), state_a));
    let handle_b = tokio::spawn(run_session(Box::new(server_b), state_b));

    // Both sessions handshake.
    handshake(&client_a).await;
    handshake(&client_b).await;

    assert_eq!(state.active_sessions.load(Ordering::Relaxed), 2);

    // Send audio on session A only.
    send_audio(&client_a, vec![1000; 1600], false).await;
    send_audio(&client_a, vec![], true).await;

    // Session A should transition to Processing.
    let (proc_msg, _) = recv_until(&client_a, |m| {
        matches!(
            m,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Processing
            })
        )
    })
    .await;
    assert!(matches!(
        proc_msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Processing
        })
    ));

    // Session B should still be alive (ping/pong).
    client_b
        .send_client(ClientMessage::Control(ClientControl::Ping {
            timestamp_ms: 99,
        }))
        .await
        .unwrap();
    let pong = timeout(TEST_TIMEOUT, client_b.recv_server())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            pong,
            ServerMessage::Control(ServerControl::Pong {
                client_timestamp_ms: 99,
                ..
            })
        ),
        "expected Pong on session B, got {pong:?}"
    );

    // Close both.
    drop(client_a);
    drop(client_b);
    let _ = timeout(TEST_TIMEOUT, handle_a).await;
    let _ = timeout(TEST_TIMEOUT, handle_b).await;

    assert_eq!(state.active_sessions.load(Ordering::Relaxed), 0);
}

/// Session handles graceful disconnect — active count returns to 0.
#[tokio::test]
async fn graceful_disconnect() {
    let state = test_state(MockStt {
        response_text: "ignored.",
    });
    let (client, server) = channel_pair();

    let handle = tokio::spawn(run_session(Box::new(server), Arc::clone(&state)));

    handshake(&client).await;

    assert_eq!(state.active_sessions.load(Ordering::Relaxed), 1);

    // Drop client = transport closes.
    drop(client);
    let _ = timeout(TEST_TIMEOUT, handle).await;

    assert_eq!(state.active_sessions.load(Ordering::Relaxed), 0);
}

/// Ping/pong works during an active session.
#[tokio::test]
async fn ping_pong_during_session() {
    let state = test_state(MockStt {
        response_text: "test.",
    });
    let (client, server) = channel_pair();

    let handle = tokio::spawn(run_session(Box::new(server), state));

    handshake(&client).await;

    // Send audio to make the pipeline busy.
    send_audio(&client, vec![1000; 1600], false).await;

    // Send ping while audio is flowing.
    client
        .send_client(ClientMessage::Control(ClientControl::Ping {
            timestamp_ms: 123,
        }))
        .await
        .unwrap();

    // Should get pong back even with pipeline active.
    let (pong, _) = recv_until(&client, |m| {
        matches!(m, ServerMessage::Control(ServerControl::Pong { .. }))
    })
    .await;
    match pong {
        ServerMessage::Control(ServerControl::Pong {
            client_timestamp_ms,
            ..
        }) => assert_eq!(client_timestamp_ms, 123),
        _ => panic!("expected Pong"),
    }

    drop(client);
    let _ = timeout(TEST_TIMEOUT, handle).await;
}

/// Back-to-back utterances: send two utterances sequentially and
/// verify the pipeline cycles through Processing → Speaking → Listening
/// for each.
#[tokio::test]
async fn back_to_back_utterances() {
    let state = test_state(MockStt {
        response_text: "Response from STT mock.",
    });
    let (client, server) = channel_pair();

    let handle = tokio::spawn(run_session(Box::new(server), state));

    handshake(&client).await;

    for i in 0..2 {
        // Send audio utterance.
        send_audio(&client, vec![1000; 1600], false).await;
        send_audio(&client, vec![], true).await;

        // Expect full cycle: Processing → Speaking → Draining.
        let (_, _) = recv_until(&client, |m| {
            matches!(
                m,
                ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Processing
                })
            )
        })
        .await;

        let (_, _) = recv_until(&client, |m| {
            matches!(
                m,
                ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Speaking
                })
            )
        })
        .await;

        let (_, draining_others) = recv_until(&client, |m| {
            matches!(
                m,
                ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Draining
                })
            )
        })
        .await;
        // Capture the server-allocated utterance_id from this turn so
        // PlaybackComplete addresses the currently-active utterance.
        let server_utterance_id = pick_server_utterance_id(0, &draining_others)
            .expect("server should have emitted an audio/metrics frame with a uid");

        client
            .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
                utterance_id: server_utterance_id,
            }))
            .await
            .unwrap();

        let (_, _) = recv_until(&client, |m| {
            matches!(
                m,
                ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Listening
                })
            )
        })
        .await;

        eprintln!("utterance {i} completed full cycle");
    }

    drop(client);
    let _ = timeout(TEST_TIMEOUT, handle).await;
}
