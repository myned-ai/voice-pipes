//! End-to-end round-trip tests for `RealtimeClient<MockRealtimeProtocol>`.
//!
//! These exercise the abstraction layer the realtime runner depends on
//! (`connect`, `apply_session_config`, `send_audio`, `send_text`,
//! `cancel`, `send_function_output`, `next_event`) against a scripted
//! WS server. The transport-level runner is covered separately by the
//! manual smoke tests in `voice-pipes-admin`; this file pins the
//! realtime-stack contract.

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::timeout;

use crate::realtime::{
    Modalities, RealtimeClient, RealtimeEvent, SessionConfig as RealtimeSessionConfig,
    TurnDetection,
};

use super::mock::{MockRealtimeProtocol, MockServer, encode_pcm16};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn base_session_cfg() -> RealtimeSessionConfig {
    RealtimeSessionConfig::new("be brief")
        .with_voice(Some("mock-voice".into()))
        .with_modalities(Modalities::AudioAndText)
        .with_turn_detection(TurnDetection::default())
}

/// Happy path: scripted server sends `ResponseStarted` → 3×
/// `ResponseAudioDelta` → `ResponseTextDelta` → `ResponseCompleted`.
/// The client should observe every event in order and the recorded
/// outbound envelopes should contain the `session_update` we applied.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_round_trip() {
    let script = vec![
        serde_json::json!({ "type": "response_started", "response_id": "r1" }),
        serde_json::json!({
            "type": "response_audio_delta",
            "samples": vec![10_i16, 20, 30, 40],
            "sample_rate": 16_000,
        }),
        serde_json::json!({
            "type": "response_audio_delta",
            "samples": vec![50_i16, 60, 70, 80],
            "sample_rate": 16_000,
        }),
        serde_json::json!({ "type": "response_text_delta", "text": "hi" }),
        serde_json::json!({ "type": "response_completed", "response_id": "r1" }),
    ];
    let server = MockServer::start(script).await;
    let proto = MockRealtimeProtocol::new(server.url());

    let mut client = timeout(TEST_TIMEOUT, RealtimeClient::connect(proto, 16_000))
        .await
        .expect("connect timed out")
        .expect("connect failed");
    client
        .apply_session_config(base_session_cfg())
        .await
        .expect("apply_session_config");

    let mut got = Vec::new();
    for _ in 0..5 {
        let ev = timeout(TEST_TIMEOUT, client.next_event())
            .await
            .expect("event timeout")
            .expect("event stream closed early");
        got.push(ev);
    }

    assert!(
        matches!(&got[0], RealtimeEvent::ResponseStarted { response_id }
            if response_id.as_deref() == Some("r1"))
    );
    let audio_count = got
        .iter()
        .filter(|e| matches!(e, RealtimeEvent::ResponseAudioDelta { .. }))
        .count();
    assert_eq!(audio_count, 2, "expected 2 audio deltas, got {got:?}");
    assert!(matches!(&got[3], RealtimeEvent::ResponseTextDelta { text } if text == "hi"));
    assert!(matches!(&got[4], RealtimeEvent::ResponseCompleted { .. }));

    // The session_update envelope from `apply_session_config` should
    // appear in the recorded outbound stream with our instructions.
    // The mock server only enters its read loop after pushing every
    // scripted frame, so poll briefly for the envelope to land.
    let mut session_update = None;
    for _ in 0..40 {
        let recorded = server.recorded();
        if let Some(env) = recorded
            .iter()
            .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("session_update"))
            .cloned()
        {
            session_update = Some(env);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let session_update =
        session_update.expect("session_update envelope was not recorded by mock server");
    assert_eq!(
        session_update.get("instructions").and_then(|x| x.as_str()),
        Some("be brief")
    );

    let _ = client.close().await;
}

/// One frame that is not JSON is reported and skipped; the frames after
/// it still arrive, so a provider glitch does not end the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_json_text_frame_is_skipped_not_fatal() {
    let script = vec![
        "this is not json".to_string(),
        serde_json::json!({ "type": "response_started", "response_id": "r1" }).to_string(),
    ];
    let server = MockServer::start_raw(script, HashMap::new()).await;
    let proto = MockRealtimeProtocol::new(server.url());
    let mut client = timeout(TEST_TIMEOUT, RealtimeClient::connect(proto, 16_000))
        .await
        .expect("connect timed out")
        .expect("connect failed");

    let first = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("event timeout")
        .expect("stream closed");
    assert!(
        matches!(&first, RealtimeEvent::Error { code: Some(c), recoverable: true, .. } if c == "bad_frame"),
        "{first:?}"
    );
    let second = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("event timeout")
        .expect("stream closed after the bad frame");
    assert!(
        matches!(second, RealtimeEvent::ResponseStarted { .. }),
        "{second:?}"
    );
    let _ = client.close().await;
}

/// A connection reset mid-call surfaces as `Closed` (the reconnect
/// path), not as a fatal error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transport_reset_surfaces_as_closed() {
    let server = MockServer::start_resetting().await;
    let proto = MockRealtimeProtocol::new(server.url());
    let mut client = timeout(TEST_TIMEOUT, RealtimeClient::connect(proto, 16_000))
        .await
        .expect("connect timed out")
        .expect("connect failed");
    let ev = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("event timeout")
        .expect("stream closed");
    assert!(
        matches!(ev, RealtimeEvent::Closed { code: None, .. }),
        "{ev:?}"
    );
}

/// A provider that holds the socket open but answers nothing is
/// reported closed once the liveness window passes, so a half-open
/// connection cannot leave a session mute until the client hangs up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_provider_is_reported_closed_after_the_liveness_window() {
    let server = MockServer::start_silent().await;
    let proto = MockRealtimeProtocol::new(server.url());
    let limits = crate::realtime::client::ClientLimits {
        ping_interval: Duration::from_millis(50),
        liveness_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let mut client = timeout(
        TEST_TIMEOUT,
        RealtimeClient::connect_with(proto, 16_000, limits),
    )
    .await
    .expect("connect timed out")
    .expect("connect failed");
    let ev = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("liveness must fire")
        .expect("stream closed");
    assert!(
        matches!(&ev, RealtimeEvent::Closed { code: None, reason } if reason.contains("no frame")),
        "{ev:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// A protocol that learns its input rate after connecting (an `ElevenLabs`
/// agent configured for 24 kHz says so in its first frame) has the
/// microphone converted to the new rate from then on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_uplink_follows_an_input_rate_learnt_after_connecting() {
    use std::sync::atomic::Ordering;
    use voice_pipes_sdk::audio::resample::StreamingResampler;

    let mut reactions = HashMap::new();
    reactions.insert(
        "cancel".to_string(),
        serde_json::json!({ "type": "response_cancelled", "response_id": "r1" }),
    );
    let server = MockServer::start_with(Vec::new(), reactions).await;
    let proto = MockRealtimeProtocol::new(server.url());
    let input_rate = proto.input_rate();
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    // One 20 ms frame at 16 kHz before the vendor names its rate, one after.
    let frame: Vec<i16> = (0..320).map(|i| ((i * 97) % 2000) as i16 - 1000).collect();
    client.send_audio(&frame, 16_000).await.expect("send_audio");
    input_rate.store(24_000, Ordering::Relaxed);
    client.send_audio(&frame, 16_000).await.expect("send_audio");
    // The cancel's reply proves the server has read both appends.
    client.cancel().await.expect("cancel");
    timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("cancel reply timeout")
        .expect("event stream closed");

    let sent: Vec<usize> = server
        .recorded()
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("audio_append"))
        .map(|v| {
            let b64 = v.get("audio").and_then(|a| a.as_str()).expect("audio");
            voice_pipes_sdk::s2s::codec::decode_pcm16_b64(b64, 24_000)
                .expect("decode")
                .len()
        })
        .collect();
    let at_24k = StreamingResampler::new(16_000, 24_000)
        .expect("resampler")
        .process_chunk(&frame)
        .len();
    assert_eq!(sent, [320, at_24k]);
    let _ = client.close().await;
}

/// Barge-in shape: client sends audio → cancel; the server replies to
/// the cancel envelope with a scripted `response_cancelled`. Asserts
/// the recorded audio envelope carries the encoded PCM and that the
/// `cancel` envelope round-trips to the client as `ResponseCancelled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_round_trip() {
    let mut reactions = HashMap::new();
    reactions.insert(
        "cancel".to_string(),
        serde_json::json!({ "type": "response_cancelled", "response_id": "r1" }),
    );
    let server = MockServer::start_with(Vec::new(), reactions).await;
    let proto = MockRealtimeProtocol::new(server.url());

    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    client
        .apply_session_config(base_session_cfg())
        .await
        .expect("apply_session_config");

    // >= 20 ms at 16 kHz (320 samples) so the uplink coalescer flushes
    // the frame on the same call instead of buffering it.
    let samples: Vec<i16> = [100, 200, 300, 400].repeat(100);
    client
        .send_audio(&samples, 16_000)
        .await
        .expect("send_audio");
    client.cancel().await.expect("cancel");

    let ev = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("cancel reply timeout")
        .expect("event stream closed");
    assert!(matches!(
        ev,
        RealtimeEvent::ResponseCancelled { response_id } if response_id.as_deref() == Some("r1")
    ));

    let recorded = server.recorded();
    let audio_env = recorded
        .iter()
        .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("audio_append"))
        .expect("audio_append envelope not recorded");
    let b64 = audio_env
        .get("audio")
        .and_then(|x| x.as_str())
        .expect("audio_append.audio missing");
    assert_eq!(b64, &encode_pcm16(&samples));

    assert!(
        recorded
            .iter()
            .any(|v| v.get("type").and_then(|t| t.as_str()) == Some("cancel")),
        "cancel envelope was not recorded: {recorded:?}"
    );

    let _ = client.close().await;
}

/// A provider that builds a tool call directly, skipping the SDK helper's
/// size check, still cannot hand the engine oversized arguments: the
/// client turns the call into a recoverable error, and the next event
/// arrives as usual.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_tool_call_is_refused_by_the_client() {
    let big = format!(
        "{{\"text\":\"{}\"}}",
        "x".repeat(crate::realtime::event::MAX_TOOL_ARGUMENT_BYTES)
    );
    let script = vec![
        serde_json::json!({
            "type": "function_call",
            "call_id": "call-big",
            "name": "echo",
            "arguments": big,
        }),
        serde_json::json!({ "type": "response_completed", "response_id": "r1" }),
    ];
    let server = MockServer::start(script).await;
    let mut client = RealtimeClient::connect(MockRealtimeProtocol::new(server.url()), 16_000)
        .await
        .expect("connect");

    let ev = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("event timeout")
        .expect("stream closed");
    match ev {
        RealtimeEvent::Error {
            code, recoverable, ..
        } => {
            assert_eq!(code.as_deref(), Some("tool_arguments_too_large"));
            assert!(recoverable, "one bad call does not end the session");
        }
        other => panic!("expected the call to be refused, got {other:?}"),
    }
    let next = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("event timeout")
        .expect("stream closed");
    assert!(
        matches!(next, RealtimeEvent::ResponseCompleted { .. }),
        "{next:?}"
    );
    let _ = client.close().await;
}

/// Tool-dispatch shape: server scripts a `function_call`. The test
/// sends `send_function_output` back and asserts the resulting
/// envelope carries the correct `call_id` + `result`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn function_call_output_round_trip() {
    let script = vec![serde_json::json!({
        "type": "function_call",
        "call_id": "call-42",
        "name": "echo",
        "arguments": "{\"text\":\"ping\"}",
    })];
    let server = MockServer::start(script).await;
    let proto = MockRealtimeProtocol::new(server.url());

    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    client
        .apply_session_config(base_session_cfg())
        .await
        .expect("apply_session_config");

    let ev = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("event timeout")
        .expect("stream closed");
    let (call_id, name) = match ev {
        RealtimeEvent::FunctionCall {
            call_id,
            name,
            arguments,
        } => {
            assert_eq!(arguments, "{\"text\":\"ping\"}");
            (call_id, name)
        }
        other => panic!("expected FunctionCall, got {other:?}"),
    };
    assert_eq!(call_id, "call-42");
    assert_eq!(name, "echo");

    client
        .send_function_output(&call_id, "ping", "{\"reply\":\"pong\"}", false)
        .await
        .expect("send_function_output");

    // Give the server a moment to record the envelope.
    for _ in 0..20 {
        let recorded = server.recorded();
        if recorded
            .iter()
            .any(|v| v.get("type").and_then(|t| t.as_str()) == Some("function_call_output"))
        {
            let env = recorded
                .iter()
                .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("function_call_output"))
                .unwrap()
                .clone();
            assert_eq!(env.get("call_id").and_then(|x| x.as_str()), Some("call-42"));
            assert_eq!(
                env.get("result").and_then(|x| x.as_str()),
                Some("{\"reply\":\"pong\"}")
            );
            let _ = client.close().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "function_call_output envelope never recorded: {:?}",
        server.recorded()
    );
}

/// `next_event` must surface a fatal `RealtimeEvent::Error` scripted by
/// the server so the runner's M3 reconnect path can decide whether to
/// retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fatal_error_round_trip() {
    let script = vec![serde_json::json!({
        "type": "error",
        "code": "auth_failed",
        "message": "bad token",
        "recoverable": false,
    })];
    let server = MockServer::start(script).await;
    let proto = MockRealtimeProtocol::new(server.url());

    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let ev = timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("error event timeout")
        .expect("stream closed before error");
    assert!(matches!(
        ev,
        RealtimeEvent::Error { code, recoverable, .. }
            if code.as_deref() == Some("auth_failed") && !recoverable
    ));

    let _ = client.close().await;
}
