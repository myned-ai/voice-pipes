use super::*;
use std::pin::Pin;
use std::sync::Mutex;

use futures_util::Stream;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, Streaming};

fn test_config(endpoint: &str) -> GoogleSttConfig {
    GoogleSttConfig {
        project_id: Some("proj".into()),
        location: "us".into(),
        model: "chirp_3".into(),
        language_codes: vec!["hr-HR".into()],
        endpoint: endpoint.into(),
        sample_rate: 16_000,
        interim_results: true,
        enable_voice_activity_events: true,
        enable_automatic_punctuation: true,
        enable_word_time_offsets: true,
        enable_word_confidence: false,
        profanity_filter: false,
        endpointing_sensitivity: Some(EndpointingSensitivity::Short),
        speech_start_timeout: None,
        speech_end_timeout: Some(Duration::from_millis(700)),
        credentials_path: None,
        access_token: Some("test-token".into()),
        stream_rotate: Duration::from_mins(4),
        persistent_session: false,
        max_reconnect_attempts: 3,
        reconnect_delay: Duration::from_millis(50),
        reconnect_delay_max: Duration::from_millis(200),
    }
}

fn client_for(cfg: GoogleSttConfig) -> GoogleClient {
    let auth = Arc::new(AuthSource::new(
        cfg.access_token.clone(),
        None,
        cfg.project_id.clone(),
    ));
    GoogleClient::new(cfg, Arc::new(tokio::sync::OnceCell::new()), auth)
}

// ── Wire mapping ─────────────────────────────────────────

/// A result mapped by its top alternative, as the stream maps it.
fn transcribed(r: &proto::StreamingRecognitionResult, seg: &mut u32) -> TranscriptionResult {
    result_to_transcription(r, &r.alternatives[0], seg)
}

#[test]
fn streaming_config_mirrors_settings() {
    let client = client_for(test_config("http://127.0.0.1:1"));
    let sc = client.streaming_config();
    let rc = sc.config.unwrap();
    assert_eq!(rc.model, "chirp_3");
    assert_eq!(rc.language_codes, vec!["hr-HR".to_string()]);
    match rc.decoding_config.unwrap() {
        DecodingConfig::ExplicitDecodingConfig(d) => {
            assert_eq!(d.encoding, AudioEncoding::Linear16 as i32);
            assert_eq!(d.sample_rate_hertz, 16_000);
            assert_eq!(d.audio_channel_count, 1);
        }
        DecodingConfig::AutoDecodingConfig(_) => panic!("expected explicit decoding"),
    }
    let f = rc.features.unwrap();
    assert!(f.enable_automatic_punctuation);
    assert!(f.enable_word_time_offsets);
    assert_eq!(f.max_alternatives, 1);
    let sf = sc.streaming_features.unwrap();
    assert!(sf.enable_voice_activity_events);
    assert!(sf.interim_results);
    assert_eq!(sf.endpointing_sensitivity, ProtoSensitivity::Short as i32);
    let vat = sf.voice_activity_timeout.unwrap();
    assert!(vat.speech_start_timeout.is_none());
    assert_eq!(vat.speech_end_timeout.unwrap().nanos, 700_000_000);
}

fn proto_result(text: &str, is_final: bool) -> proto::StreamingRecognitionResult {
    proto::StreamingRecognitionResult {
        alternatives: vec![proto::SpeechRecognitionAlternative {
            transcript: text.into(),
            confidence: if is_final { 0.93 } else { 0.0 },
            words: vec![],
        }],
        is_final,
        stability: 0.0,
        result_end_offset: Some(prost_types::Duration {
            seconds: 1,
            nanos: 500_000_000,
        }),
        channel_tag: 0,
        language_code: "hr-HR".into(),
    }
}

#[test]
fn interim_and_final_of_same_turn_share_segment_index() {
    let mut seg = 0u32;
    let partial = transcribed(&proto_result(" Državno", false), &mut seg);
    assert!(!partial.is_final);
    assert_eq!(partial.text, "Državno");
    assert_eq!(partial.segment_index, 0);
    assert_eq!(partial.is_end_of_turn, Some(false));
    assert!(partial.confidence.is_none(), "interims carry no confidence");

    let fin = transcribed(&proto_result("Državno izborno.", true), &mut seg);
    assert!(fin.is_final);
    assert_eq!(fin.segment_index, 0, "final shares the partial's index");
    assert_eq!(fin.is_end_of_turn, Some(true));
    assert_eq!(fin.confidence, Some(0.93));
    assert_eq!(fin.language.as_deref(), Some("hr-HR"));
    assert_eq!(fin.end_time, Some(Duration::from_millis(1500)));
    assert_eq!(seg, 1);

    let next = transcribed(&proto_result("Dalje", false), &mut seg);
    assert_eq!(next.segment_index, 1);
}

#[test]
fn word_timestamps_map_to_word_info() {
    let mut r = proto_result("hello world", true);
    r.alternatives[0].words = vec![
        proto::WordInfo {
            start_offset: Some(prost_types::Duration {
                seconds: 0,
                nanos: 100_000_000,
            }),
            end_offset: Some(prost_types::Duration {
                seconds: 0,
                nanos: 400_000_000,
            }),
            word: "hello".into(),
            confidence: 0.9,
            speaker_label: String::new(),
        },
        proto::WordInfo {
            start_offset: Some(prost_types::Duration {
                seconds: 0,
                nanos: 500_000_000,
            }),
            end_offset: Some(prost_types::Duration {
                seconds: 1,
                nanos: 0,
            }),
            word: "world".into(),
            confidence: 0.0,
            speaker_label: String::new(),
        },
    ];
    let mut seg = 0;
    let tr = transcribed(&r, &mut seg);
    assert_eq!(tr.words.len(), 2);
    assert_eq!(tr.words[0].start, Duration::from_millis(100));
    assert_eq!(tr.words[0].end, Duration::from_millis(400));
    assert_eq!(tr.words[0].confidence, Some(0.9));
    assert!(tr.words[1].confidence.is_none());
    assert_eq!(tr.start_time, Some(Duration::from_millis(100)));
}

#[test]
fn activity_events_map_to_speech_started_and_end_of_turn() {
    let mut seg = 0;
    let mut active = false;

    let begin = proto::StreamingRecognizeResponse {
        results: vec![],
        speech_event_type: SpeechEventType::SpeechActivityBegin as i32,
        speech_event_offset: Some(prost_types::Duration {
            seconds: 2,
            nanos: 0,
        }),
    };
    let out = response_to_results(&begin, &mut seg, &mut active);
    assert_eq!(out.len(), 1);
    assert!(out[0].speech_started);
    assert!(!out[0].is_final);
    assert_eq!(out[0].start_time, Some(Duration::from_secs(2)));
    assert!(active);

    let end = proto::StreamingRecognizeResponse {
        results: vec![],
        speech_event_type: SpeechEventType::SpeechActivityEnd as i32,
        speech_event_offset: None,
    };
    let out = response_to_results(&end, &mut seg, &mut active);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].is_end_of_turn, Some(true));
    assert!(out[0].text.is_empty());
    assert!(!active);

    // v1 single-utterance event is a no-op; empty interims are dropped.
    let noop = proto::StreamingRecognizeResponse {
        results: vec![proto_result("", false)],
        speech_event_type: SpeechEventType::EndOfSingleUtterance as i32,
        speech_event_offset: None,
    };
    assert!(response_to_results(&noop, &mut seg, &mut active).is_empty());
    assert_eq!(seg, 0);
}

#[test]
fn final_result_clears_speech_active() {
    let mut seg = 0;
    let mut active = true;
    let resp = proto::StreamingRecognizeResponse {
        results: vec![proto_result("done", true)],
        speech_event_type: 0,
        speech_event_offset: None,
    };
    let out = response_to_results(&resp, &mut seg, &mut active);
    assert_eq!(out.len(), 1);
    assert!(!active);
    assert_eq!(seg, 1);
}

#[test]
fn audio_is_split_under_the_message_cap() {
    let samples = vec![1i16; 40_000]; // 80 KB
    let pieces = split_audio(&samples);
    assert!(pieces.len() >= 5);
    assert!(pieces.iter().all(|p| p.len() <= MAX_AUDIO_MESSAGE_BYTES));
    let total: usize = pieces.iter().map(Vec::len).sum();
    assert_eq!(total, 80_000);
    // 100 ms at 16 kHz is far below the cap.
    let frame = split_audio(&vec![0i16; 1600]);
    assert_eq!(frame.len(), 1);
    assert_eq!(frame[0].len(), 3200);
}

/// A backlog the RPC is not draining loses its oldest audio, never the
/// configuration message that must go first.
#[test]
fn backlog_sheds_oldest_audio_but_keeps_the_config() {
    let mut outbound = std::collections::VecDeque::new();
    outbound.push_back(proto::StreamingRecognizeRequest {
        recognizer: "r".into(),
        streaming_request: Some(StreamingRequest::StreamingConfig(
            proto::StreamingRecognitionConfig::default(),
        )),
    });
    for i in 0..5u8 {
        outbound.push_back(proto::StreamingRecognizeRequest {
            recognizer: String::new(),
            streaming_request: Some(StreamingRequest::Audio(vec![i])),
        });
    }
    assert_eq!(shed_backlog(&mut outbound, 3), 3);
    assert_eq!(outbound.len(), 3);
    assert!(matches!(
        outbound[0].streaming_request,
        Some(StreamingRequest::StreamingConfig(_))
    ));
    assert!(
        matches!(&outbound[1].streaming_request, Some(StreamingRequest::Audio(v)) if v == &[3])
    );
    assert!(
        matches!(&outbound[2].streaming_request, Some(StreamingRequest::Audio(v)) if v == &[4])
    );
    // Nothing to shed once only the config remains.
    assert_eq!(shed_backlog(&mut outbound, 0), 2);
    assert_eq!(outbound.len(), 1);
}

#[test]
fn status_classification() {
    assert!(matches!(
        classify_status(&Status::unauthenticated("bad")),
        SttError::AuthError(_)
    ));
    assert!(!classify_status(&Status::unauthenticated("bad")).is_transient());
    assert!(matches!(
        classify_status(&Status::resource_exhausted("quota")),
        SttError::RateLimited { .. }
    ));
    assert!(classify_status(&Status::unavailable("down")).is_transient());
    assert!(classify_status(&Status::out_of_range("305s")).is_transient());
    assert!(matches!(
        classify_status(&Status::invalid_argument("model")),
        SttError::ConfigError(_)
    ));
    assert!(!classify_status(&Status::invalid_argument("model")).is_transient());
}

#[test]
fn backoff_is_exponential_and_capped() {
    let client = client_for(test_config("http://127.0.0.1:1"));
    assert_eq!(client.backoff_delay(1), Duration::from_millis(50));
    assert_eq!(client.backoff_delay(2), Duration::from_millis(100));
    assert_eq!(client.backoff_delay(3), Duration::from_millis(200));
    assert_eq!(client.backoff_delay(6), Duration::from_millis(200));
}

// ── Lifecycle against an in-process mock server ──────────

#[derive(Debug, Default, Clone)]
struct CallRecord {
    authorization: Option<String>,
    user_project: Option<String>,
    recognizer: String,
    config_first: bool,
    audio_bytes: usize,
    audio_messages: usize,
    max_message_bytes: usize,
    half_closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MockScript {
    /// Echo: BEGIN + interim on first audio, final on half-close.
    Normal,
    /// Reject the first call with `Unavailable`, then behave normally.
    UnavailableOnce,
    /// Reject every call with `Unauthenticated`.
    Unauthenticated,
    /// Never answer the half-close (drain must time out).
    SilentAfterHalfClose,
    /// Like `Normal`, but the final after a half-close takes a while.
    SlowFinal,
    /// Accept the call and then neither read the request stream nor send
    /// response headers: the client's audio backs up on the connection.
    WithholdHeaders,
    /// Accept every call, then fail it on the first audio message with a
    /// transient status.
    FailOnFirstAudio,
}

#[derive(Debug)]
struct MockSpeech {
    script: MockScript,
    calls: Arc<Mutex<Vec<CallRecord>>>,
    attempts: Arc<Mutex<u32>>,
}

type RespStream =
    Pin<Box<dyn Stream<Item = Result<proto::StreamingRecognizeResponse, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl proto::speech_server::Speech for MockSpeech {
    type StreamingRecognizeStream = RespStream;

    async fn streaming_recognize(
        &self,
        request: Request<Streaming<proto::StreamingRecognizeRequest>>,
    ) -> Result<Response<Self::StreamingRecognizeStream>, Status> {
        let attempt = {
            let mut a = self.attempts.lock().unwrap();
            *a += 1;
            *a
        };
        match self.script {
            MockScript::Unauthenticated => {
                return Err(Status::unauthenticated("Request had invalid credentials"));
            }
            MockScript::UnavailableOnce if attempt == 1 => {
                return Err(Status::unavailable("try again"));
            }
            MockScript::WithholdHeaders => {
                // Hold the request open without touching it; the call
                // only ends when the client goes away.
                let _request = request;
                std::future::pending::<()>().await;
                unreachable!();
            }
            _ => {}
        }
        let mut rec = CallRecord {
            authorization: request
                .metadata()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(String::from),
            user_project: request
                .metadata()
                .get("x-goog-user-project")
                .and_then(|v| v.to_str().ok())
                .map(String::from),
            ..Default::default()
        };
        let mut inbound = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<proto::StreamingRecognizeResponse, Status>>(16);
        let calls = Arc::clone(&self.calls);
        let script = self.script;
        tokio::spawn(async move {
            let mut first = true;
            let mut half_closed = false;
            loop {
                match inbound.message().await {
                    Ok(Some(req)) => match req.streaming_request {
                        Some(StreamingRequest::StreamingConfig(_)) => {
                            rec.config_first = first;
                            rec.recognizer = req.recognizer;
                        }
                        Some(StreamingRequest::Audio(bytes)) => {
                            if script == MockScript::FailOnFirstAudio {
                                let _ = tx.send(Err(Status::internal("stream rejected"))).await;
                                break;
                            }
                            if rec.audio_messages == 0 {
                                let _ = tx
                                    .send(Ok(proto::StreamingRecognizeResponse {
                                        results: vec![],
                                        speech_event_type: SpeechEventType::SpeechActivityBegin
                                            as i32,
                                        speech_event_offset: None,
                                    }))
                                    .await;
                                let _ = tx
                                    .send(Ok(proto::StreamingRecognizeResponse {
                                        results: vec![proto_result("hel", false)],
                                        speech_event_type: 0,
                                        speech_event_offset: None,
                                    }))
                                    .await;
                            }
                            rec.audio_messages += 1;
                            rec.audio_bytes += bytes.len();
                            rec.max_message_bytes = rec.max_message_bytes.max(bytes.len());
                            // Google's VAD closes the activity window
                            // on its own; model it after a few frames
                            // so rotation sees a quiet moment.
                            if rec.audio_messages == 3 {
                                let _ = tx
                                    .send(Ok(proto::StreamingRecognizeResponse {
                                        results: vec![],
                                        speech_event_type: SpeechEventType::SpeechActivityEnd
                                            as i32,
                                        speech_event_offset: None,
                                    }))
                                    .await;
                            }
                        }
                        None => {}
                    },
                    Ok(None) => {
                        half_closed = true;
                        break;
                    }
                    Err(_) => break,
                }
                first = false;
            }
            rec.half_closed = half_closed;
            if half_closed && script == MockScript::SlowFinal {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            if half_closed && script != MockScript::SilentAfterHalfClose {
                let _ = tx
                    .send(Ok(proto::StreamingRecognizeResponse {
                        results: vec![proto_result("hello", true)],
                        speech_event_type: SpeechEventType::SpeechActivityEnd as i32,
                        speech_event_offset: None,
                    }))
                    .await;
            }
            calls.lock().unwrap().push(rec);
            if script == MockScript::SilentAfterHalfClose {
                // Hold the response stream open well past the drain
                // deadline.
                tokio::time::sleep(DRAIN_TIMEOUT * 3).await;
            }
            drop(tx);
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

async fn mock_server(script: MockScript) -> (String, Arc<Mutex<Vec<CallRecord>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let svc = proto::speech_server::SpeechServer::new(MockSpeech {
        script,
        calls: Arc::clone(&calls),
        attempts: Arc::new(Mutex::new(0)),
    });
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await;
    });
    (format!("http://{addr}"), calls)
}

fn chunk(samples: usize, is_last: bool) -> AudioChunk {
    AudioChunk {
        samples: vec![7i16; samples],
        is_last,
    }
}

/// Collect everything the client emits until it finishes.
async fn drain(mut rx: mpsc::Receiver<TranscriptionResult>) -> Vec<TranscriptionResult> {
    let mut out = Vec::new();
    while let Some(r) = rx.recv().await {
        out.push(r);
    }
    out
}

#[tokio::test]
async fn per_utterance_mode_opens_one_stream_per_utterance_and_loses_no_audio() {
    let (endpoint, calls) = mock_server(MockScript::Normal).await;
    let core = Arc::new(voice_pipes_sdk::component::ComponentCore::new(
        "stt:google-test",
        voice_pipes_sdk::component::ComponentKind::Stt,
    ));
    let client = client_for(test_config(&endpoint)).with_core(Arc::clone(&core));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, result_rx) = mpsc::channel::<TranscriptionResult>(64);
    let results = tokio::spawn(drain(result_rx));

    let run = tokio::spawn(async move {
        client
            .run(
                audio_rx,
                result_tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
    });

    // Utterance 1: 3 × 100 ms + an end-of-utterance flush chunk.
    for _ in 0..3 {
        audio_tx.send(chunk(1600, false)).await.unwrap();
    }
    audio_tx.send(chunk(8000, true)).await.unwrap();
    // Utterance 2 starts immediately — while stream 1 is draining.
    audio_tx.send(chunk(1600, false)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    audio_tx.send(chunk(1600, true)).await.unwrap();
    drop(audio_tx);

    let res = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("session must end when audio closes")
        .unwrap();
    assert!(res.is_ok(), "{res:?}");

    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "one stream per utterance: {calls:?}");
    for c in &calls {
        assert_eq!(c.authorization.as_deref(), Some("Bearer test-token"));
        assert_eq!(c.user_project.as_deref(), Some("proj"));
        assert_eq!(c.recognizer, "projects/proj/locations/us/recognizers/_");
        assert!(c.config_first, "config must be the first message");
        assert!(c.max_message_bytes <= MAX_AUDIO_MESSAGE_BYTES);
    }
    let total_bytes: usize = calls.iter().map(|c| c.audio_bytes).sum();
    assert_eq!(total_bytes, (3 * 1600 + 8000 + 1600 + 1600) * 2);
    assert_eq!(
        calls[1].audio_bytes,
        (1600 + 1600) * 2,
        "utterance 2 intact"
    );

    let results = results.await.unwrap();
    let finals: Vec<_> = results.iter().filter(|r| r.is_final).collect();
    assert_eq!(finals.len(), 2);
    assert_eq!(finals[0].segment_index, 0);
    assert_eq!(finals[1].segment_index, 1);
    assert!(results.iter().any(|r| r.speech_started));

    // Health: one latency sample per final, measured from the end of
    // speech — not the session length (which tripped auto-degrade at
    // "p99 41 s" on a real session).
    let tel = core.telemetry();
    assert_eq!(tel.processed_total, 2);
    assert!(tel.latency_ms.p99 < 2_000, "{:?}", tel.latency_ms);
}

#[tokio::test]
async fn cancel_ends_the_session_promptly() {
    let (endpoint, _calls) = mock_server(MockScript::Normal).await;
    let client = client_for(test_config(&endpoint));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        audio_tx.send(chunk(1600, false)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel2.cancel();
        // Keep the sender alive so only cancellation can end the run.
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(audio_tx);
    });
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("cancel must complete promptly");
    assert!(res.is_ok(), "{res:?}");
}

#[tokio::test]
async fn unauthenticated_is_permanent_and_not_retried() {
    let (endpoint, _calls) = mock_server(MockScript::Unauthenticated).await;
    let client = client_for(test_config(&endpoint));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(64);
    audio_tx.send(chunk(1600, false)).await.unwrap();
    let started = Instant::now();
    let err = client
        .run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SttError::AuthError(_)), "{err}");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "must not sit in reconnect backoff"
    );
}

#[tokio::test]
async fn unavailable_reconnects_with_backoff_and_resends_pending_audio() {
    let (endpoint, calls) = mock_server(MockScript::UnavailableOnce).await;
    let client = client_for(test_config(&endpoint));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, result_rx) = mpsc::channel::<TranscriptionResult>(64);
    let results = tokio::spawn(drain(result_rx));
    audio_tx.send(chunk(1600, false)).await.unwrap();
    audio_tx.send(chunk(1600, true)).await.unwrap();
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
    .expect("must finish");
    assert!(res.is_ok(), "{res:?}");
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "the failed attempt records nothing");
    // Audio queued before the failed open is carried to the retry.
    assert_eq!(calls[0].audio_bytes, (1600 + 1600) * 2);
    assert!(results.await.unwrap().iter().any(|r| r.is_final));
}

#[tokio::test]
async fn drain_times_out_when_server_never_finishes() {
    let (endpoint, calls) = mock_server(MockScript::SilentAfterHalfClose).await;
    let client = client_for(test_config(&endpoint));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(64);
    audio_tx.send(chunk(1600, true)).await.unwrap();
    drop(audio_tx);
    let res = tokio::time::timeout(
        DRAIN_TIMEOUT + Duration::from_secs(3),
        client.run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("drain deadline must fire");
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(calls.lock().unwrap().len(), 1);
}

/// Streaming contract: one `run` serves two utterances (one upstream
/// stream each in per-utterance mode), finalised under segment 0 and 1.
#[tokio::test]
async fn two_utterances_on_one_stream_get_consecutive_segments() {
    let (endpoint, calls) = mock_server(MockScript::Normal).await;
    let client = Arc::new(client_for(test_config(&endpoint)));
    let run = {
        let client = Arc::clone(&client);
        move |audio_rx, result_tx, cancel| async move { client.run(audio_rx, result_tx, cancel).await }
    };
    voice_pipes_sdk::testing::two_utterances_on_one_stream(run, || {
        vec![vec![7i16; 1600], vec![7i16; 1600]]
    })
    .await;
    assert_eq!(calls.lock().unwrap().len(), 2);
}

/// An utterance that ends while the previous stream is still draining is
/// carried whole into the next stream, which must half-close for it
/// right away instead of waiting for audio that will never come.
#[tokio::test]
async fn utterance_completed_during_drain_gets_its_own_half_close() {
    let (endpoint, calls) = mock_server(MockScript::SlowFinal).await;
    let client = client_for(test_config(&endpoint));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, mut result_rx) = mpsc::channel::<TranscriptionResult>(64);
    let run = tokio::spawn(async move {
        client
            .run(
                audio_rx,
                result_tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
    });

    // Utterance 1 ends; stream 1 drains for ~400 ms.
    audio_tx.send(chunk(1600, false)).await.unwrap();
    audio_tx.send(chunk(1600, true)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Utterance 2 starts and ends entirely inside that drain.
    audio_tx.send(chunk(1600, false)).await.unwrap();
    audio_tx.send(chunk(1600, true)).await.unwrap();

    // Both finals must arrive while the audio channel is still open.
    let mut finals = 0;
    let deadline = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline);
    while finals < 2 {
        tokio::select! {
            r = result_rx.recv() => match r {
                Some(r) if r.is_final => finals += 1,
                Some(_) => {}
                None => break,
            },
            () = &mut deadline => break,
        }
    }
    assert_eq!(
        finals, 2,
        "utterance 2 must be finalised without more audio"
    );
    drop(audio_tx);
    let res = tokio::time::timeout(Duration::from_secs(3), run)
        .await
        .expect("session must end when audio closes")
        .unwrap();
    assert!(res.is_ok(), "{res:?}");

    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|c| c.half_closed), "{calls:?}");
    assert_eq!(calls[1].audio_bytes, 2 * 1600 * 2);
}

/// Google accepting the call but neither reading nor answering must not
/// wedge the session: audio backs up locally, and cancellation still ends
/// the run at once.
#[tokio::test]
async fn cancel_is_honoured_while_the_server_withholds_headers() {
    let (endpoint, _calls) = mock_server(MockScript::WithholdHeaders).await;
    let client = client_for(test_config(&endpoint));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        // ~35 s of audio: far beyond what the connection can buffer for
        // a peer that never reads.
        for _ in 0..350 {
            if audio_tx.send(chunk(1600, false)).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel2.cancel();
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(audio_tx);
    });
    let res = tokio::time::timeout(
        Duration::from_secs(3),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("cancel must complete while the server is unresponsive");
    assert!(res.is_ok(), "{res:?}");
}

/// A pipeline that holds its result receiver but stops reading leaves the
/// client waiting to hand on a transcript; cancellation must still end the
/// run at once.
#[tokio::test]
async fn cancel_is_honoured_while_results_back_up() {
    let (endpoint, _calls) = mock_server(MockScript::Normal).await;
    let client = client_for(test_config(&endpoint));
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    // Room for the speech-start event only; the interim behind it waits.
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        for _ in 0..3 {
            if audio_tx.send(chunk(1600, false)).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel2.cancel();
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(audio_tx);
    });
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("cancel must complete while a result waits");
    assert!(res.is_ok(), "{res:?}");
}

/// A stream Google accepts and then fails at once does not restore the
/// reconnect budget: the run gives up instead of looping.
#[tokio::test]
async fn streams_failed_right_after_open_exhaust_the_budget() {
    let (endpoint, _calls) = mock_server(MockScript::FailOnFirstAudio).await;
    let mut cfg = test_config(&endpoint);
    cfg.max_reconnect_attempts = 2;
    let client = client_for(cfg);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(64);
    tokio::spawn(async move {
        loop {
            if audio_tx.send(chunk(1600, false)).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        client.run(
            audio_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("must give up")
    .unwrap_err();
    assert!(
        matches!(err, SttError::ReconnectExhausted { attempts: 3, .. }),
        "{err}"
    );
    assert!(!err.is_transient());
}

/// A token provider that never answers.
#[derive(Debug)]
struct StallingProvider;

#[async_trait::async_trait]
impl gcp_auth::TokenProvider for StallingProvider {
    async fn token(&self, _scopes: &[&str]) -> Result<Arc<gcp_auth::Token>, gcp_auth::Error> {
        std::future::pending().await
    }

    async fn project_id(&self) -> Result<Arc<str>, gcp_auth::Error> {
        std::future::pending().await
    }
}

/// A credential fetch that hangs (a metadata server that does not answer)
/// must not hold the worker: cancellation still ends the run.
#[tokio::test]
async fn stalled_credential_fetch_is_abandoned_on_cancel() {
    let cfg = test_config("http://127.0.0.1:1");
    let auth = Arc::new(AuthSource::with_provider(Arc::new(StallingProvider)));
    let client = GoogleClient::new(cfg, Arc::new(tokio::sync::OnceCell::new()), auth);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (result_tx, _result_rx) = mpsc::channel::<TranscriptionResult>(64);
    audio_tx.send(chunk(1600, false)).await.unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel2.cancel();
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(audio_tx);
    });
    let res = tokio::time::timeout(
        Duration::from_secs(2),
        client.run(audio_rx, result_tx, cancel),
    )
    .await
    .expect("cancel must interrupt the credential fetch");
    assert!(res.is_ok(), "{res:?}");
}

#[tokio::test]
async fn persistent_stream_rotates_at_the_boundary_without_losing_audio() {
    let (endpoint, calls) = mock_server(MockScript::Normal).await;
    let mut cfg = test_config(&endpoint);
    cfg.persistent_session = true;
    cfg.stream_rotate = Duration::from_millis(400);
    let client = client_for(cfg);
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(64);
    let (result_tx, result_rx) = mpsc::channel::<TranscriptionResult>(256);
    let results = tokio::spawn(drain(result_rx));

    let run = tokio::spawn(async move {
        client
            .run(
                audio_rx,
                result_tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
    });
    // ~1.2 s of continuous 100 ms frames: crosses the 400 ms rotation
    // mark at least twice. Mark one utterance boundary in the middle;
    // in persistent mode that must NOT close the stream by itself.
    let frames = 12;
    for i in 0..frames {
        audio_tx.send(chunk(1600, i == 5)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(audio_tx);

    let res = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("session must end when audio closes")
        .unwrap();
    assert!(res.is_ok(), "{res:?}");

    let calls = calls.lock().unwrap().clone();
    assert!(calls.len() >= 2, "expected rotation: {calls:?}");
    let total_bytes: usize = calls.iter().map(|c| c.audio_bytes).sum();
    assert_eq!(
        total_bytes,
        frames * 1600 * 2,
        "no audio lost across rotation"
    );
    let results = results.await.unwrap();
    // Every stream half-close yields a final from the mock.
    assert_eq!(results.iter().filter(|r| r.is_final).count(), calls.len());
    // The mid-run `is_last` did not close the stream: the first stream
    // outlived the rotation mark, not the utterance boundary.
    assert!(calls[0].audio_messages >= 4, "{calls:?}");
}

// ── Malformed responses ─────────────────────────────────────

/// A response Google should never send (results without alternatives, an
/// empty transcript, negative or absurd offsets, an unknown event type) maps
/// to what it can without a panic: nothing, or a result with no timing.
#[test]
fn malformed_responses_never_panic() {
    let (mut seg, mut active) = (0u32, false);
    let mut empty = proto_result("", true);
    empty.alternatives.clear();
    let mut negative = proto_result("hello", true);
    negative.result_end_offset = Some(prost_types::Duration {
        seconds: -5,
        nanos: -1,
    });
    negative.alternatives[0].words = vec![proto::WordInfo {
        start_offset: Some(prost_types::Duration {
            seconds: i64::MIN,
            nanos: 0,
        }),
        end_offset: None,
        word: "hello".into(),
        confidence: f32::NAN,
        speaker_label: String::new(),
    }];
    let resp = proto::StreamingRecognizeResponse {
        results: vec![empty, negative],
        speech_event_type: 9_999,
        speech_event_offset: None,
    };
    let out = response_to_results(&resp, &mut seg, &mut active);
    assert_eq!(out.len(), 1, "the result without alternatives is skipped");
    assert_eq!(out[0].text, "hello");
    assert_eq!(
        out[0].words[0].start,
        Duration::ZERO,
        "an unrepresentable offset is dropped"
    );
}
