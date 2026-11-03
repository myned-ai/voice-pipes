use super::*;
use std::sync::Arc;

fn test_config() -> AssemblyAiConfig {
    AssemblyAiConfig {
        api_key: "test-key-redacted".into(),
        endpoint: "wss://streaming.assemblyai.com/v3/ws".into(),
        speech_model: "u3-rt-pro".into(),
        sample_rate: 16_000,
        min_turn_silence: None,
        max_turn_silence: None,
        end_of_turn_confidence_threshold: None,
        format_turns: None,
        keyterms_prompt: None,
        prompt: None,
        language_detection: None,
        max_reconnect_attempts: 3,
        reconnect_delay: Duration::from_secs(1),
        reconnect_delay_max: Duration::from_secs(30),
        persistent_session: false,
    }
}

#[test]
fn build_url_basic() {
    let client = WsClient::new(test_config());
    let url = client.build_url();
    assert!(url.starts_with("wss://streaming.assemblyai.com/v3/ws?"));
    assert!(url.contains("speech_model=u3-rt-pro"));
    assert!(url.contains("sample_rate=16000"));
}

#[test]
fn build_url_all_params() {
    let mut cfg = test_config();
    cfg.min_turn_silence = Some(200);
    cfg.max_turn_silence = Some(3000);
    cfg.end_of_turn_confidence_threshold = Some(0.5);
    cfg.format_turns = Some(true);
    cfg.keyterms_prompt = Some(vec!["Hello".into(), "World".into()]);
    cfg.prompt = Some("transcribe medical".into());
    cfg.language_detection = Some(true);

    let client = WsClient::new(cfg);
    let url = client.build_url();

    assert!(url.contains("min_turn_silence=200"));
    assert!(url.contains("max_turn_silence=3000"));
    assert!(url.contains("end_of_turn_confidence_threshold=0.5"));
    assert!(url.contains("format_turns=true"));
    assert!(url.contains("keyterms_prompt="));
    assert!(url.contains("prompt=transcribe+medical"));
    assert!(url.contains("language_detection=true"));
}

#[test]
fn build_url_keyterms_encoding() {
    let mut cfg = test_config();
    cfg.keyterms_prompt = Some(vec!["Dr. Patel".into(), "metoprolol".into()]);

    let client = WsClient::new(cfg);
    let url = client.build_url();

    // The keyterms should be a JSON array, URL-encoded.
    assert!(url.contains("keyterms_prompt="));
    // Decode and verify it's valid JSON.
    let idx = url.find("keyterms_prompt=").unwrap() + "keyterms_prompt=".len();
    let rest = &url[idx..];
    let encoded = rest.split('&').next().unwrap();
    let decoded = url::form_urlencoded::parse(format!("k={encoded}").as_bytes())
        .next()
        .unwrap()
        .1
        .to_string();
    let terms: Vec<String> = serde_json::from_str(&decoded).unwrap();
    assert_eq!(terms, vec!["Dr. Patel", "metoprolol"]);
}

#[test]
fn turn_to_result_partial() {
    let turn = TurnMessage {
        turn_order: 0,
        end_of_turn: false,
        transcript: "Hi, my name".into(),
        end_of_turn_confidence: 0.001,
        words: vec![
            super::super::messages::TurnWord {
                start: 2160,
                end: 2560,
                text: "Hi,".into(),
                confidence: 0.87,
                word_is_final: true,
            },
            super::super::messages::TurnWord {
                start: 2560,
                end: 2560,
                text: "my".into(),
                confidence: 0.99,
                word_is_final: true,
            },
        ],
        utterance: String::new(),
        language_code: None,
    };

    let mut seg = 0u32;
    let result = turn_to_result(&turn, &mut seg);
    assert!(!result.is_final);
    assert_eq!(result.text, "Hi, my name");
    assert_eq!(result.is_end_of_turn, Some(false));
    assert_eq!(result.words.len(), 2);
    assert_eq!(seg, 0); // not incremented for partial
}

#[test]
fn turn_to_result_final() {
    let turn = TurnMessage {
        turn_order: 0,
        end_of_turn: true,
        transcript: "Hi, my name is Sonny.".into(),
        end_of_turn_confidence: 0.43,
        words: vec![super::super::messages::TurnWord {
            start: 2160,
            end: 2560,
            text: "Hi,".into(),
            confidence: 0.87,
            word_is_final: true,
        }],
        utterance: "Hi, my name is Sonny.".into(),
        language_code: Some("en".into()),
    };

    let mut seg = 0u32;
    let result = turn_to_result(&turn, &mut seg);
    assert!(result.is_final);
    assert_eq!(result.is_end_of_turn, Some(true));
    assert_eq!(result.language, Some("en".into()));
    assert_eq!(seg, 1); // incremented for final
}

// Partials and the final of the *same* turn must carry the same
// segment_index; the next turn advances it.
#[test]
fn partial_and_final_of_same_turn_share_segment_index() {
    fn make_turn(end_of_turn: bool, transcript: &str) -> TurnMessage {
        TurnMessage {
            turn_order: 0,
            end_of_turn,
            transcript: transcript.into(),
            end_of_turn_confidence: 0.9,
            words: vec![],
            utterance: transcript.into(),
            language_code: Some("en".into()),
        }
    }

    let mut seg = 0u32;
    let partial = turn_to_result(&make_turn(false, "hi"), &mut seg);
    assert_eq!(partial.segment_index, 0);
    assert!(!partial.is_final);

    let final_res = turn_to_result(&make_turn(true, "hi there"), &mut seg);
    assert_eq!(
        final_res.segment_index, 0,
        "the final must share the partial's segment index"
    );
    assert!(final_res.is_final);

    // A partial of the next turn advances to 1.
    let next = turn_to_result(&make_turn(false, "bye"), &mut seg);
    assert_eq!(next.segment_index, 1);
}

#[test]
fn turn_to_result_word_timestamps() {
    let turn = TurnMessage {
        turn_order: 0,
        end_of_turn: true,
        transcript: "Hello".into(),
        end_of_turn_confidence: 0.9,
        words: vec![super::super::messages::TurnWord {
            start: 1000,
            end: 1500,
            text: "Hello".into(),
            confidence: 0.95,
            word_is_final: true,
        }],
        utterance: String::new(),
        language_code: None,
    };

    let mut seg = 0u32;
    let result = turn_to_result(&turn, &mut seg);
    assert_eq!(result.words.len(), 1);
    assert_eq!(result.words[0].start, Duration::from_secs(1));
    assert_eq!(result.words[0].end, Duration::from_millis(1500));
    assert_eq!(result.words[0].confidence, Some(0.95));
}

#[test]
fn turn_to_result_confidence_avg() {
    let turn = TurnMessage {
        turn_order: 0,
        end_of_turn: true,
        transcript: "A B".into(),
        end_of_turn_confidence: 0.5,
        words: vec![
            super::super::messages::TurnWord {
                start: 0,
                end: 100,
                text: "A".into(),
                confidence: 0.8,
                word_is_final: true,
            },
            super::super::messages::TurnWord {
                start: 100,
                end: 200,
                text: "B".into(),
                confidence: 0.6,
                word_is_final: true,
            },
        ],
        utterance: String::new(),
        language_code: None,
    };

    let mut seg = 0u32;
    let result = turn_to_result(&turn, &mut seg);
    let expected = f32::midpoint(0.8, 0.6);
    assert!((result.confidence.unwrap() - expected).abs() < f32::EPSILON);
}

#[test]
fn backoff_delay_exponential() {
    let client = WsClient::new(test_config());
    assert_eq!(client.backoff_delay(1), Duration::from_secs(1));
    assert_eq!(client.backoff_delay(2), Duration::from_secs(2));
    assert_eq!(client.backoff_delay(3), Duration::from_secs(4));
}

#[test]
fn backoff_delay_capped() {
    let mut cfg = test_config();
    cfg.reconnect_delay_max = Duration::from_secs(5);
    let client = WsClient::new(cfg);

    // Attempt 4 would be 8000ms, but capped at 5000ms.
    assert_eq!(client.backoff_delay(4), Duration::from_secs(5));
}

// ── Error classification (incident 2026-07-14) ──────────

#[test]
fn classify_concurrency_error_as_transient() {
    let err = classify_server_error(
        Some(1008),
        "Unauthorized Connection: Too many concurrent sessions",
    );
    assert!(matches!(err, SttError::RateLimited { .. }), "{err}");
    assert!(err.is_transient());
}

#[test]
fn classify_close_policy_concurrency_is_transient() {
    use voice_pipes_net::tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    let err = classify_close(
        Some(CloseCode::Policy),
        "Unauthorized Connection: Too many concurrent sessions",
    );
    assert!(err.is_transient(), "{err}");
}

#[test]
fn classify_bad_api_key_as_permanent() {
    let err = classify_server_error(Some(1008), "Invalid API key");
    assert!(matches!(err, SttError::AuthError(_)), "{err}");
    assert!(!err.is_transient());

    let err = classify_server_error(None, "Unauthorized");
    assert!(matches!(err, SttError::AuthError(_)), "{err}");
}

#[test]
fn classify_unknown_error_as_transient_api_error() {
    let err = classify_server_error(Some(3005), "some other failure");
    assert!(
        matches!(err, SttError::ApiError { status: 3005, .. }),
        "{err}"
    );
    assert!(err.is_transient());
}

// ── Session lifecycle (mock server) ──────────────────────

/// Start a mock `AssemblyAI` server; returns its address and a receiver of
/// all text frames it saw. `script` controls post-Begin behavior.
async fn mock_server(
    script: MockScript,
) -> (std::net::SocketAddr, mpsc::UnboundedReceiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (seen_tx, seen_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = voice_pipes_net::tokio_tungstenite::accept_async(stream).await else {
            return;
        };

        match script {
            MockScript::ErrorBeforeBegin => {
                let _ = ws.send(Message::Text(
                    r#"{"type":"Error","error_code":1008,"error":"Unauthorized Connection: Too many concurrent sessions"}"#.into(),
                )).await;
                // Hold the socket briefly so the client reads the frame.
                let _ = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
                return;
            }
            MockScript::SilentAfterTerminate | MockScript::RespondTermination => {}
            MockScript::GarbageThenTurn => {
                let _ = ws
                    .send(Message::Text(
                        r#"{"type":"Begin","id":"mock-session","expires_at":9999999999}"#.into(),
                    ))
                    .await;
                for garbage in MALFORMED_FRAMES {
                    let _ = ws.send(Message::Text((*garbage).into())).await;
                }
                let _ = ws
                    .send(Message::Binary(vec![0xff, 0x00, 0x13].into()))
                    .await;
                let _ = ws.send(Message::Text(FINAL_TURN.into())).await;
            }
        }

        if !matches!(script, MockScript::GarbageThenTurn) {
            let _ = ws
                .send(Message::Text(
                    r#"{"type":"Begin","id":"mock-session","expires_at":9999999999}"#.into(),
                ))
                .await;
        }

        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(text) = msg {
                let is_terminate = text.contains("Terminate");
                let _ = seen_tx.send(text.to_string());
                if is_terminate {
                    match script {
                        MockScript::RespondTermination | MockScript::GarbageThenTurn => {
                            let _ = ws.send(Message::Text(
                                r#"{"type":"Termination","audio_duration_seconds":1.0,"session_duration_seconds":1.0}"#.into(),
                            )).await;
                        }
                        // Never answer: the client's termination
                        // deadline must fire (L1 regression).
                        MockScript::SilentAfterTerminate => {}
                        MockScript::ErrorBeforeBegin => unreachable!(),
                    }
                }
            }
        }
    });
    (addr, seen_rx)
}

#[derive(Clone, Copy)]
enum MockScript {
    ErrorBeforeBegin,
    SilentAfterTerminate,
    RespondTermination,
    /// After `Begin`, malformed frames, then one valid final turn.
    GarbageThenTurn,
}

/// Frames a broken or hostile server might send: not JSON, JSON of the
/// wrong shape, an unknown type, a known type missing its fields, a wrong
/// field type, and an empty frame.
const MALFORMED_FRAMES: &[&str] = &[
    "not json at all",
    "[1, 2, 3]",
    r#"{"type":"FutureMessage","payload":{}}"#,
    r#"{"type":"Turn"}"#,
    r#"{"type":"Turn","turn_order":"zero","end_of_turn":"maybe"}"#,
    r#"{"no_type":true}"#,
    "",
];

const FINAL_TURN: &str = r#"{"type":"Turn","turn_order":0,"turn_is_formatted":true,"end_of_turn":true,"transcript":"Still here.","end_of_turn_confidence":0.9,"words":[{"start":0,"end":400,"text":"Still","confidence":0.9,"word_is_final":true},{"start":400,"end":800,"text":"here.","confidence":0.9,"word_is_final":true}],"utterance":"Still here."}"#;

/// Malformed frames from the server are skipped, never fatal and never a
/// panic: the valid turn that follows them still arrives, and the session
/// ends cleanly.
#[tokio::test]
async fn malformed_frames_are_skipped_and_the_session_continues() {
    let (addr, _seen) = mock_server(MockScript::GarbageThenTurn).await;
    let client = client_for(addr, false);

    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, mut result_rx) = mpsc::channel::<TranscriptionResult>(8);
    let run = tokio::spawn(async move {
        client
            .run(
                audio_rx,
                result_tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
    });

    let result = tokio::time::timeout(Duration::from_secs(5), result_rx.recv())
        .await
        .expect("the valid turn arrives")
        .expect("channel open");
    assert!(result.is_final);
    assert_eq!(result.text, "Still here.");

    audio_tx
        .send(AudioChunk {
            samples: vec![0i16; 160],
            is_last: true,
        })
        .await
        .unwrap();
    // The utterance's end closes its vendor session; the stream itself
    // ends when its audio does.
    drop(audio_tx);
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("the session ends")
        .unwrap();
    assert!(outcome.is_ok(), "{outcome:?}");
}

fn client_for(addr: std::net::SocketAddr, persistent: bool) -> WsClient {
    let mut cfg = test_config();
    // A path is required: an empty path yields a malformed handshake
    // request line (`GET ?query`).
    cfg.endpoint = format!("ws://{addr}/v3/ws");
    cfg.persistent_session = persistent;
    cfg.max_reconnect_attempts = 0;
    WsClient::new(cfg)
}

#[tokio::test]
async fn error_before_begin_classified_as_rate_limited() {
    let (addr, _seen) = mock_server(MockScript::ErrorBeforeBegin).await;
    let client = client_for(addr, false);

    let (_audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    let err = client
        .run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
    // RateLimited is transient, so run() consumes its (zero) reconnect
    // budget and reports the reconnect failure -- the point is that it
    // must NOT surface as a permanent AuthError, and the transient
    // classification must be visible in the wrapped message.
    assert!(
        !matches!(err, SttError::AuthError(_)),
        "concurrency limit must not classify as auth error: {err}"
    );
    assert!(
        err.to_string().contains("rate limited"),
        "expected the RateLimited classification to surface: {err}"
    );
}

/// L1 regression: after `is_last` sends Terminate, a server that never
/// answers must not hold the session (and the pipeline) open forever.
#[tokio::test]
async fn terminate_without_response_times_out() {
    let (addr, mut seen) = mock_server(MockScript::SilentAfterTerminate).await;
    let client = client_for(addr, false);

    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    audio_tx
        .send(AudioChunk {
            samples: vec![0i16; 1600],
            is_last: true,
        })
        .await
        .unwrap();
    // The utterance's end closes its vendor session; the stream itself
    // ends when its audio does.
    drop(audio_tx);

    let res = tokio::time::timeout(
        TERMINATION_TIMEOUT + Duration::from_secs(3),
        client.run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("session must close within the termination deadline");
    assert!(res.is_ok(), "{res:?}");
    // The server must have seen our Terminate.
    let saw_terminate =
        std::iter::from_fn(|| seen.try_recv().ok()).any(|t| t.contains("Terminate"));
    assert!(saw_terminate);
}

/// L2 regression: cancellation must send Terminate (not a bare Close)
/// and complete promptly when the server responds.
#[tokio::test]
async fn cancel_sends_terminate_and_completes() {
    let (addr, mut seen) = mock_server(MockScript::RespondTermination).await;
    let client = client_for(addr, false);

    let (_audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel2.cancel();
    });

    let res = tokio::time::timeout(
        Duration::from_secs(3),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("cancel must complete promptly");
    assert!(res.is_ok(), "{res:?}");
    let saw_terminate =
        std::iter::from_fn(|| seen.try_recv().ok()).any(|t| t.contains("Terminate"));
    assert!(saw_terminate, "cancel path must send Terminate");
}

/// Persistent mode sends `ForceEndpoint` on end-of-utterance and only
/// terminates when the audio channel closes.
#[tokio::test]
async fn persistent_session_force_endpoint_then_terminate_on_close() {
    let (addr, mut seen) = mock_server(MockScript::RespondTermination).await;
    let client = client_for(addr, true);

    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    audio_tx
        .send(AudioChunk {
            samples: vec![0i16; 1600],
            is_last: true,
        })
        .await
        .unwrap();

    let run = tokio::spawn(async move {
        client
            .run(
                audio_rx,
                result_tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
    });

    // Give the client time to deliver ForceEndpoint, then end the session.
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(audio_tx);

    let res = tokio::time::timeout(Duration::from_secs(3), run)
        .await
        .expect("session must close after channel close")
        .unwrap();
    assert!(res.is_ok(), "{res:?}");

    let frames: Vec<String> = std::iter::from_fn(|| seen.try_recv().ok()).collect();
    let fe = frames.iter().position(|t| t.contains("ForceEndpoint"));
    let term = frames.iter().position(|t| t.contains("Terminate"));
    assert!(fe.is_some(), "expected ForceEndpoint frame, got {frames:?}");
    assert!(term.is_some(), "expected Terminate frame, got {frames:?}");
    assert!(
        fe < term,
        "ForceEndpoint must precede Terminate: {frames:?}"
    );
}

// ── Reconnect budget and connect bounds (mock server) ────

/// A mock that accepts any number of connections; `script` decides what
/// each session does after `Begin`. Returns the address and a counter of
/// accepted connections.
async fn multi_session_mock(
    script: MultiScript,
) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicU32>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            tokio::spawn(async move {
                let Ok(mut ws) = voice_pipes_net::tokio_tungstenite::accept_async(stream).await
                else {
                    return;
                };
                let _ = ws
                    .send(Message::Text(
                        r#"{"type":"Begin","id":"mock-session","expires_at":9999999999}"#.into(),
                    ))
                    .await;
                // Wait for the first audio frame.
                let Some(Ok(_)) = ws.next().await else {
                    return;
                };
                match script {
                    MultiScript::RejectAfterBegin => {
                        let _ = ws
                            .send(Message::Text(
                                r#"{"type":"Error","error_code":3005,"error":"stream rejected"}"#
                                    .into(),
                            ))
                            .await;
                        let _ = ws.close(None).await;
                    }
                    MultiScript::TranscriptThenDrop { healthy_from } => {
                        let _ = ws
                            .send(Message::Text(
                                r#"{"type":"Turn","turn_order":0,"turn_is_formatted":true,"end_of_turn":true,"transcript":"hi","end_of_turn_confidence":0.9,"words":[],"utterance":"hi"}"#.into(),
                            ))
                            .await;
                        if n < healthy_from {
                            // Drop the socket without a Close frame.
                            drop(ws);
                            return;
                        }
                        while let Some(Ok(msg)) = ws.next().await {
                            if let Message::Text(text) = msg
                                && text.contains("Terminate")
                            {
                                let _ = ws.send(Message::Text(
                                    r#"{"type":"Termination","audio_duration_seconds":1.0,"session_duration_seconds":1.0}"#.into(),
                                )).await;
                            }
                        }
                    }
                    MultiScript::FinalOnTerminate => {
                        while let Some(Ok(msg)) = ws.next().await {
                            if let Message::Text(text) = msg
                                && text.contains("Terminate")
                            {
                                let _ = ws.send(Message::Text(
                                    r#"{"type":"Turn","turn_order":0,"turn_is_formatted":true,"end_of_turn":true,"transcript":"hi","end_of_turn_confidence":0.9,"words":[],"utterance":"hi"}"#.into(),
                                )).await;
                                let _ = ws.send(Message::Text(
                                    r#"{"type":"Termination","audio_duration_seconds":1.0,"session_duration_seconds":1.0}"#.into(),
                                )).await;
                                return;
                            }
                        }
                    }
                    MultiScript::FinalPerEndpoint => {
                        // Persistent-session shape: every ForceEndpoint
                        // yields a final turn; Terminate is answered.
                        let mut turn_order = 0;
                        loop {
                            let Some(Ok(msg)) = ws.next().await else {
                                return;
                            };
                            let Message::Text(text) = msg else {
                                continue;
                            };
                            if text.contains("ForceEndpoint") {
                                let frame = format!(
                                    r#"{{"type":"Turn","turn_order":{turn_order},"turn_is_formatted":true,"end_of_turn":true,"transcript":"hi","end_of_turn_confidence":0.9,"words":[],"utterance":"hi"}}"#
                                );
                                turn_order += 1;
                                let _ = ws.send(Message::Text(frame.into())).await;
                            } else if text.contains("Terminate") {
                                let _ = ws.send(Message::Text(
                                    r#"{"type":"Termination","audio_duration_seconds":1.0,"session_duration_seconds":1.0}"#.into(),
                                )).await;
                                return;
                            }
                        }
                    }
                    MultiScript::ResetAfterTerminate => {
                        while let Some(Ok(msg)) = ws.next().await {
                            if let Message::Text(text) = msg
                                && text.contains("Terminate")
                            {
                                // Vanish instead of answering: the client
                                // must treat this as the session having
                                // ended.
                                drop(ws);
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    (addr, accepted)
}

#[derive(Clone, Copy)]
enum MultiScript {
    /// `Begin`, then a transient server error on the first audio frame.
    RejectAfterBegin,
    /// `Begin`, a transcript, then the socket vanishes; from connection
    /// number `healthy_from` on, the session behaves normally.
    TranscriptThenDrop { healthy_from: u32 },
    /// Answer `Terminate` by resetting the connection.
    ResetAfterTerminate,
    /// A final turn per `ForceEndpoint`; `Terminate` answered normally.
    FinalPerEndpoint,
    /// The service's answer to `Terminate`: the final turn, then
    /// `Termination` (default mode, one session per utterance).
    FinalOnTerminate,
}

/// Streaming contract: one persistent session serves two utterances, each
/// finalised under its own segment index.
#[tokio::test]
async fn persistent_session_serves_two_utterances_on_one_stream() {
    let (addr, accepted) = multi_session_mock(MultiScript::FinalPerEndpoint).await;
    let client = Arc::new(client_for(addr, true));
    let run = {
        let client = Arc::clone(&client);
        move |audio_rx, result_tx, cancel| async move { client.run(audio_rx, result_tx, cancel).await }
    };
    voice_pipes_sdk::testing::two_utterances_on_one_stream(run, || {
        vec![vec![0i16; 1600], vec![0i16; 1600]]
    })
    .await;
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
}

fn reconnecting_client(addr: std::net::SocketAddr, max_attempts: u32) -> WsClient {
    let mut cfg = test_config();
    cfg.endpoint = format!("ws://{addr}/v3/ws");
    cfg.max_reconnect_attempts = max_attempts;
    cfg.reconnect_delay = Duration::from_millis(5);
    cfg.reconnect_delay_max = Duration::from_millis(10);
    WsClient::new(cfg)
}

/// Feed 100 ms frames every 20 ms until the returned sender is dropped.
fn feed_audio(audio_tx: mpsc::Sender<AudioChunk>) {
    tokio::spawn(async move {
        loop {
            if audio_tx
                .send(AudioChunk {
                    samples: vec![0i16; 1600],
                    is_last: false,
                })
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}

/// A server that accepts every connection and rejects the stream right
/// after must not be retried forever: the accepted `Begin` does not
/// restore the attempt counter, so the run ends once the budget is spent.
#[tokio::test]
async fn rejection_after_begin_exhausts_the_reconnect_budget() {
    let (addr, accepted) = multi_session_mock(MultiScript::RejectAfterBegin).await;
    let client = reconnecting_client(addr, 2);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    feed_audio(audio_tx);

    let err = tokio::time::timeout(
        Duration::from_secs(5),
        client.run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("must give up instead of looping")
    .unwrap_err();
    assert!(
        matches!(err, SttError::ReconnectExhausted { attempts: 3, .. }),
        "{err}"
    );
    assert!(!err.is_transient());
    assert!(err.to_string().contains("stream rejected"), "{err}");
    assert_eq!(
        accepted.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "initial attempt plus two reconnects"
    );
}

/// A session that delivered a transcript proved the connection works, so
/// its failure restores the counter and reconnects keep coming even with
/// a budget of one.
#[tokio::test]
async fn sessions_that_delivered_transcripts_keep_reconnecting() {
    let (addr, accepted) =
        multi_session_mock(MultiScript::TranscriptThenDrop { healthy_from: 4 }).await;
    let client = reconnecting_client(addr, 1);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, result_rx) = mpsc::channel::<TranscriptionResult>(64);
    feed_audio(audio_tx);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            if counter.load(std::sync::atomic::Ordering::SeqCst) >= 4 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                cancel2.cancel();
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let results = tokio::spawn(async move {
        let mut rx = result_rx;
        let mut n = 0;
        while rx.recv().await.is_some() {
            n += 1;
        }
        n
    });

    let res = tokio::time::timeout(
        Duration::from_secs(5),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("must end on cancel");
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert!(results.await.unwrap() >= 4, "each session emitted a final");
}

/// The delay after many failures stays a real, capped delay rather than
/// wrapping to zero.
#[test]
fn backoff_after_many_attempts_stays_capped() {
    let mut cfg = test_config();
    cfg.reconnect_delay = Duration::from_millis(100);
    cfg.reconnect_delay_max = Duration::from_secs(30);
    let client = WsClient::new(cfg);
    assert_eq!(client.backoff_delay(33), Duration::from_secs(30));
    assert_eq!(client.backoff_delay(64), Duration::from_secs(30));
}

/// A connect that never completes must give up on cancel instead of
/// holding the worker until the OS gives up. A listener that never accepts
/// stands in for a black-holed endpoint.
#[tokio::test]
async fn black_holed_connect_is_abandoned_on_cancel() {
    // Bind but never accept: the TCP handshake completes in the kernel,
    // the WebSocket upgrade never gets an answer.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = reconnecting_client(addr, 0);
    let (_audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel2.cancel();
    });
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("cancel must interrupt the connect");
    assert!(res.is_ok(), "{res:?}");
    drop(listener);
}

/// A connection reset while waiting for `Termination` ends the session:
/// the server was told to stop, reconnecting would open a session nobody
/// asked for.
#[tokio::test]
async fn reset_after_terminate_completes_without_reconnecting() {
    let (addr, accepted) = multi_session_mock(MultiScript::ResetAfterTerminate).await;
    let client = reconnecting_client(addr, 3);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    audio_tx
        .send(AudioChunk {
            samples: vec![0i16; 1600],
            is_last: true,
        })
        .await
        .unwrap();
    // The utterance's end closes its vendor session; the stream itself
    // ends when its audio does.
    drop(audio_tx);
    let res = tokio::time::timeout(
        Duration::from_secs(3),
        client.run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("session must end once the socket is gone");
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn avg_confidence_empty() {
    assert!(avg_confidence(&[]).is_none());
}

#[test]
fn avg_confidence_skips_non_final() {
    let words = vec![
        super::super::messages::TurnWord {
            start: 0,
            end: 100,
            text: "A".into(),
            confidence: 0.9,
            word_is_final: true,
        },
        super::super::messages::TurnWord {
            start: 100,
            end: 200,
            text: "B".into(),
            confidence: 0.1,
            word_is_final: false, // not final -- excluded
        },
    ];
    let avg = avg_confidence(&words).unwrap();
    assert!((avg - 0.9).abs() < f32::EPSILON);
}

/// The regression: in its default mode the client ended the whole stream
/// with the first utterance, so the pipeline restarted it for every turn
/// (with the backoff meant for failures) and the segment index began again
/// at zero. The stream now serves every utterance, one vendor session
/// each, with the index running on.
#[tokio::test]
async fn default_mode_serves_utterances_on_one_stream_one_session_each() {
    let (addr, accepted) = multi_session_mock(MultiScript::FinalOnTerminate).await;
    let client = Arc::new(client_for(addr, false));
    let run = {
        let client = Arc::clone(&client);
        move |audio_rx, result_tx, cancel| async move { client.run(audio_rx, result_tx, cancel).await }
    };
    voice_pipes_sdk::testing::two_utterances_on_one_stream(run, || {
        vec![vec![0i16; 1600], vec![0i16; 1600]]
    })
    .await;
    assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Two utterances queued back to back each get their own final in the
/// default mode too.
#[tokio::test]
async fn default_mode_keeps_back_to_back_utterances_apart() {
    let (addr, _accepted) = multi_session_mock(MultiScript::FinalOnTerminate).await;
    let client = Arc::new(client_for(addr, false));
    let run = {
        let client = Arc::clone(&client);
        move |audio_rx, result_tx, cancel| async move { client.run(audio_rx, result_tx, cancel).await }
    };
    voice_pipes_sdk::testing::back_to_back_utterances_stay_apart(
        run,
        || vec![vec![0i16; 1600], vec![0i16; 1600]],
        Duration::from_secs(5),
    )
    .await;
}

// ── Cancellation while a send waits (mock server) ────────

/// A mock that sends `Begin` and then `turns` final turns. With `reads`
/// it answers `Terminate`; without, it never reads the socket again, so
/// the client's sends back up once the connection's buffers are full.
async fn held_mock(turns: usize, reads: bool) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = voice_pipes_net::tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let _ = ws
            .send(Message::Text(
                r#"{"type":"Begin","id":"mock-session","expires_at":9999999999}"#.into(),
            ))
            .await;
        for _ in 0..turns {
            let _ = ws.send(Message::Text(FINAL_TURN.into())).await;
        }
        if !reads {
            // Holds `ws` open, unread, until the test's runtime ends.
            std::future::pending::<()>().await;
        }
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(text) = msg
                && text.contains("Terminate")
            {
                let _ = ws.send(Message::Text(
                    r#"{"type":"Termination","audio_duration_seconds":1.0,"session_duration_seconds":1.0}"#.into(),
                )).await;
            }
        }
    });
    addr
}

/// A mock that reports the byte length of every binary frame it receives
/// and answers `Terminate`.
async fn frame_recording_mock() -> (std::net::SocketAddr, mpsc::UnboundedReceiver<usize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (lens_tx, lens_rx) = mpsc::unbounded_channel::<usize>();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = voice_pipes_net::tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let _ = ws
            .send(Message::Text(
                r#"{"type":"Begin","id":"mock-session","expires_at":9999999999}"#.into(),
            ))
            .await;
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                Message::Binary(bytes) => {
                    let _ = lens_tx.send(bytes.len());
                }
                Message::Text(text) if text.contains("Terminate") => {
                    let _ = ws.send(Message::Text(
                        r#"{"type":"Termination","audio_duration_seconds":2.0,"session_duration_seconds":2.0}"#.into(),
                    )).await;
                }
                _ => {}
            }
        }
    });
    (addr, lens_rx)
}

/// `AssemblyAI` refuses an audio message longer than one second. A chunk
/// the pipeline hands over whole, however long, goes out as frames within
/// that cap, with nothing lost.
#[tokio::test]
async fn a_long_chunk_is_split_into_frames_within_the_cap() {
    let (addr, mut lens) = frame_recording_mock().await;
    let client = client_for(addr, false);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    // Two seconds at 16 kHz, plus a sliver so the pieces are not exact.
    let samples = 2 * 16_000 + 160;
    audio_tx
        .send(AudioChunk {
            samples: vec![3i16; samples],
            is_last: true,
        })
        .await
        .unwrap();
    drop(audio_tx);
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        client.run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("the session ends");
    assert!(res.is_ok(), "{res:?}");

    let frames: Vec<usize> = std::iter::from_fn(|| lens.try_recv().ok()).collect();
    assert!(frames.len() >= 2, "{frames:?}");
    // One second of 16-bit mono at 16 kHz.
    let cap_bytes = 16_000 * 2;
    let floor_bytes = 800 * 2;
    for &len in &frames {
        assert!(
            len <= cap_bytes,
            "frame of {len} bytes over the cap: {frames:?}"
        );
        assert!(
            len >= floor_bytes,
            "frame of {len} bytes under 50 ms: {frames:?}"
        );
    }
    assert_eq!(frames.iter().sum::<usize>(), samples * 2);
}

#[test]
fn split_frames_is_even_and_never_empty() {
    let lens = |n: usize| -> Vec<usize> {
        split_frames(&vec![0i16; n], 16_000)
            .map(<[i16]>::len)
            .collect()
    };
    assert!(lens(0).is_empty());
    assert_eq!(lens(1600), vec![1600]);
    assert_eq!(lens(16_000), vec![16_000]);
    assert_eq!(lens(16_001), vec![8001, 8000]);
    assert_eq!(lens(48_000), vec![16_000; 3]);
}

/// A vendor that stops reading leaves the client's audio send waiting on a
/// full socket. Cancellation must still end the run, within the bound the
/// client gives a session to terminate.
#[tokio::test]
async fn cancel_is_honoured_while_the_vendor_stops_reading() {
    let addr = held_mock(0, false).await;
    let client = client_for(addr, false);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(2);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(8);
    let cancel = tokio_util::sync::CancellationToken::new();
    let run = tokio::spawn({
        let cancel = cancel.clone();
        async move { client.run(audio_rx, result_tx, cancel).await }
    });

    // Feed one-second chunks until the client stops taking them: it is
    // then waiting on a send the vendor will never read.
    let mut wedged = false;
    for _ in 0..4096 {
        let fed = tokio::time::timeout(
            Duration::from_millis(500),
            audio_tx.send(AudioChunk {
                samples: vec![1i16; 16_000],
                is_last: false,
            }),
        )
        .await;
        if fed.is_err() {
            wedged = true;
            break;
        }
    }
    assert!(wedged, "the client never backed up on the unread socket");

    let cancelled_at = std::time::Instant::now();
    cancel.cancel();
    let res = tokio::time::timeout(TERMINATION_TIMEOUT + Duration::from_secs(1), run)
        .await
        .expect("cancel must end the run while a send waits")
        .unwrap();
    assert!(res.is_ok(), "{res:?}");
    assert!(
        cancelled_at.elapsed() <= TERMINATION_TIMEOUT + Duration::from_millis(500),
        "took {:?}",
        cancelled_at.elapsed()
    );
    drop(audio_tx);
}

/// A pipeline that holds its result receiver but stops reading leaves the
/// client waiting to hand on a transcript. Cancellation must still end the
/// run promptly.
#[tokio::test]
async fn cancel_is_honoured_while_results_back_up() {
    let addr = held_mock(8, true).await;
    let client = client_for(addr, false);
    let (_audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel2.cancel();
    });
    let res = tokio::time::timeout(
        Duration::from_secs(3),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("cancel must end the run while a result waits");
    assert!(res.is_ok(), "{res:?}");
}
