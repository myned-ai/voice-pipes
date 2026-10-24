/// A sink that accepts the first message and then never
/// becomes ready again — the shape of a peer that stopped reading once
/// the kernel send buffer filled.
struct StallAfterFirst {
    accepted: usize,
}

impl futures_util::Sink<axum::extract::ws::Message> for StallAfterFirst {
    type Error = std::convert::Infallible;
    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        if self.accepted == 0 {
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Pending
        }
    }
    fn start_send(
        mut self: std::pin::Pin<&mut Self>,
        _item: axum::extract::ws::Message,
    ) -> Result<(), Self::Error> {
        self.accepted += 1;
        Ok(())
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// When a write does not complete within `write_timeout` the
/// pump must (a) stop, so `ws_tx.send` fails and the session pump
/// unblocks, and (b) tell the runner via `Close` — instead of sitting
/// on the stalled write for the lifetime of the TCP connection.
#[tokio::test(start_paused = true)]
async fn write_pump_times_out_on_a_peer_that_stops_reading() {
    use super::{DecodedClientMessage, write_pump};
    use axum::extract::ws::Message;

    let (ws_tx, ws_rx) = tokio::sync::mpsc::channel::<Message>(8);
    let (decoded_tx, mut decoded_rx) = tokio::sync::mpsc::channel(8);
    let pump = tokio::spawn(write_pump(
        StallAfterFirst { accepted: 0 },
        ws_rx,
        std::time::Duration::from_secs(5),
        decoded_tx,
    ));

    ws_tx.send(Message::Text("first".into())).await.unwrap(); // accepted
    ws_tx.send(Message::Text("stalls".into())).await.unwrap(); // never ready
    // Let the pump pick both up so its timeout starts now, not after
    // the first `advance` below.
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    // Well under the timeout: still stalled, nothing reported.
    tokio::time::advance(std::time::Duration::from_secs(4)).await;
    assert!(decoded_rx.try_recv().is_err());
    assert!(!pump.is_finished());

    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    let reported = tokio::time::timeout(std::time::Duration::from_secs(1), decoded_rx.recv())
        .await
        .expect("Close reported to the runner");
    assert!(matches!(reported, Some(DecodedClientMessage::Close)));
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), pump)
        .await
        .expect("pump task finished");
    assert!(
        ws_tx.send(Message::Text("after".into())).await.is_err(),
        "later sends must fail so the session pump unblocks"
    );
}

/// The failure policy is a pure function so the loop above can
/// stay untested at the socket level.
#[test]
fn decode_failure_policy_backs_off_logging_and_closes_after_a_run() {
    use super::{MAX_CONSECUTIVE_DECODE_FAILURES, decode_failure_action};
    let logged: Vec<u32> = (1..=40).filter(|c| decode_failure_action(*c).log).collect();
    assert_eq!(logged, vec![1, 2, 4, 8, 16, 32]);
    assert!(!decode_failure_action(MAX_CONSECUTIVE_DECODE_FAILURES - 1).close);
    assert!(decode_failure_action(MAX_CONSECUTIVE_DECODE_FAILURES).close);
    assert!(decode_failure_action(MAX_CONSECUTIVE_DECODE_FAILURES + 7).close);
}

use super::*;
use crate::transport::ClientControl;
use crate::transport::wire::{BinaryHeader, FLAG_END_OF_UTTERANCE, MSG_TYPE_AUDIO_PCM_I16};

#[test]
fn decode_binary_audio_roundtrip() {
    let samples: Vec<i16> = vec![100, -200, 300];
    let pcm = crate::transport::wire::encode_pcm_i16(&samples);
    let header = BinaryHeader {
        msg_type: MSG_TYPE_AUDIO_PCM_I16,
        flags: 0,
        seq_num: 42,
    };
    let frame = header.encode_with_payload(&pcm);

    let msg = decode_client_message(Message::Binary(frame.into()))
        .unwrap()
        .unwrap();
    match msg {
        ClientMessage::Audio(chunk) => {
            assert_eq!(chunk.samples, vec![100, -200, 300]);
            assert!(!chunk.is_end_of_utterance);
        }
        other => panic!("expected Audio, got {other:?}"),
    }
}

#[test]
fn decode_binary_end_of_utterance() {
    let samples: Vec<i16> = vec![1];
    let pcm = crate::transport::wire::encode_pcm_i16(&samples);
    let header = BinaryHeader {
        msg_type: MSG_TYPE_AUDIO_PCM_I16,
        flags: FLAG_END_OF_UTTERANCE,
        seq_num: 0,
    };
    let frame = header.encode_with_payload(&pcm);

    let msg = decode_client_message(Message::Binary(frame.into()))
        .unwrap()
        .unwrap();
    match msg {
        ClientMessage::Audio(chunk) => assert!(chunk.is_end_of_utterance),
        other => panic!("expected Audio, got {other:?}"),
    }
}

#[test]
fn decode_text_message() {
    let envelope = serde_json::json!({
        "type": "message",
        "message_id": "msg-1",
        "timestamp_ms": 12345,
        "payload": {
            "kind": "text",
            "text": "hello world",
            "message_id": "msg-1",
            "role": "user"
        }
    });
    let text = serde_json::to_string(&envelope).unwrap();
    let msg = decode_client_message(Message::Text(text.into()))
        .unwrap()
        .unwrap();
    match msg {
        ClientMessage::Message(m) => {
            assert_eq!(m.kind, "text");
            assert_eq!(m.text.as_deref(), Some("hello world"));
            assert_eq!(m.message_id, "msg-1");
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

#[test]
fn decode_control_cancel() {
    let envelope = serde_json::json!({
        "type": "control",
        "message_id": "ctrl-1",
        "timestamp_ms": 0,
        "payload": {
            "action": "cancel"
        }
    });
    let text = serde_json::to_string(&envelope).unwrap();
    let msg = decode_client_message(Message::Text(text.into()))
        .unwrap()
        .unwrap();
    match msg {
        ClientMessage::Control(ClientControl::Cancel) => {}
        other => panic!("expected Cancel, got {other:?}"),
    }
}

#[test]
fn decode_unknown_binary_type() {
    let header = BinaryHeader {
        msg_type: 0xFF,
        flags: 0,
        seq_num: 0,
    };
    let frame = header.encode_with_payload(&[]);
    let err = decode_client_message(Message::Binary(frame.into())).unwrap_err();
    assert!(err.to_string().contains("unknown binary message type"));
}

// ── heartbeat / RTT ──────────────────────────────

/// RTT reflects the simulated delay between ping send and pong receipt.
#[test]
fn rtt_from_pong_reflects_delay() {
    let sent_at_ms = 1_000u64;
    let payload = sent_at_ms.to_le_bytes();
    let rtt = rtt_from_pong(1_042, &payload).unwrap();
    assert_eq!(rtt, Duration::from_millis(42));

    // Zero-delay pong.
    assert_eq!(
        rtt_from_pong(1_000, &payload).unwrap(),
        Duration::from_millis(0)
    );
}

/// Unsolicited/foreign pongs (wrong payload size or a timestamp from
/// the future) must not produce an RTT sample.
#[test]
fn rtt_from_pong_rejects_foreign_payloads() {
    assert!(rtt_from_pong(1_000, b"").is_none());
    assert!(rtt_from_pong(1_000, b"abc").is_none());
    assert!(rtt_from_pong(1_000, &2_000u64.to_le_bytes()).is_none());
}

/// A connection that stops answering pings is declared lost — and the
/// pump told to close it — after exactly `max_missed_pongs` silent
/// intervals; a pong in between resets the count.
#[test]
fn heartbeat_closes_after_max_missed_pongs() {
    let health = ConnectionHealthMonitor::new(crate::transport::health::HealthConfig {
        max_missed_pongs: 2,
        ..Default::default()
    });
    let awaiting = AtomicBool::new(false);

    // Tick 1: nothing outstanding — just ping.
    assert_eq!(
        heartbeat_tick(&health, &awaiting),
        HeartbeatAction::SendPing
    );
    awaiting.store(true, Ordering::Release);

    // Tick 2: ping unanswered (1 miss) — still pinging.
    assert_eq!(
        heartbeat_tick(&health, &awaiting),
        HeartbeatAction::SendPing
    );

    // A pong arrives: read pump clears the flag and resets the count.
    awaiting.store(false, Ordering::Release);
    health.record_pong(Duration::from_millis(30));
    assert_eq!(
        heartbeat_tick(&health, &awaiting),
        HeartbeatAction::SendPing
    );

    // Two consecutive silent intervals → Lost → close.
    awaiting.store(true, Ordering::Release);
    assert_eq!(
        heartbeat_tick(&health, &awaiting),
        HeartbeatAction::SendPing
    );
    assert_eq!(
        heartbeat_tick(&health, &awaiting),
        HeartbeatAction::CloseLost
    );
}

/// A pong whose payload does not echo our timestamp still proves
/// liveness. The read pump clears `awaiting_pong` and calls
/// `record_liveness()` for any pong, so an actively-ponging peer behind a
/// payload-rewriting middlebox is never declared Lost — mirroring the read
/// pump's Pong arm.
#[test]
fn foreign_payload_pong_keeps_connection_alive() {
    let health = ConnectionHealthMonitor::new(crate::transport::health::HealthConfig {
        max_missed_pongs: 2,
        ..Default::default()
    });
    let awaiting = AtomicBool::new(false);

    // Drive the connection to the brink twice, each time "rescued" by a
    // foreign-payload pong. Without the liveness reset this would go Lost.
    for _ in 0..5 {
        // Ping sent, unanswered for one interval (1 miss).
        awaiting.store(true, Ordering::Release);
        assert_eq!(
            heartbeat_tick(&health, &awaiting),
            HeartbeatAction::SendPing
        );

        // A pong with a non-echoing payload arrives: the read pump's logic
        // for any pong is `store(false)` + `record_liveness()`.
        assert!(rtt_from_pong(1_000, b"garbage").is_none());
        awaiting.store(false, Ordering::Release);
        health.record_liveness();

        assert_ne!(health.quality(), ConnectionQuality::Lost);
    }
    // Never produced an RTT sample from the foreign pongs.
    assert!(health.rtt_ms().is_none());
}

#[test]
fn decode_unknown_json_type() {
    let envelope = serde_json::json!({
        "type": "foobar",
        "message_id": "x",
        "timestamp_ms": 0,
        "payload": {}
    });
    let text = serde_json::to_string(&envelope).unwrap();
    let err = decode_client_message(Message::Text(text.into())).unwrap_err();
    assert!(err.to_string().contains("unknown message type"));
}

// ── inbound audio rate-limit decisions ───────────

/// A chunk as the codec produces it: `sample_rate` stamped with the
/// 48 kHz default regardless of the session's negotiated rate.
fn audio_chunk(samples: usize, eou: bool) -> crate::pipeline::AudioChunk {
    crate::pipeline::AudioChunk {
        samples: vec![0i16; samples],
        sample_rate: 48_000,
        channels: 1,
        is_end_of_utterance: eou,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    }
}

/// A rejected chunk carrying the end-of-utterance flag must be
/// forwarded (samples stripped) rather than dropped — losing the
/// commit signal truncated the transcript AND hung the turn.
#[test]
fn rate_limited_eou_chunk_is_forwarded_not_dropped() {
    let mut limiter = None;
    // Drain the 10 s burst at the negotiated 16 kHz in one admit…
    assert_eq!(
        rate_limit_audio(&mut limiter, 16_000, &audio_chunk(160_000, false)),
        RateDecision::Admit
    );
    // …then over-budget chunks: plain audio drops, EOU is preserved.
    assert_eq!(
        rate_limit_audio(&mut limiter, 16_000, &audio_chunk(1_600, false)),
        RateDecision::Drop
    );
    assert_eq!(
        rate_limit_audio(&mut limiter, 16_000, &audio_chunk(1_600, true)),
        RateDecision::ForwardEouOnly
    );
}

/// The limiter must be sized from the session's negotiated input rate,
/// not the codec's 48 kHz stamp (which gave 16 kHz sessions a 3× too
/// loose budget), and must rebuild if the effective rate changes.
#[test]
fn limiter_sized_from_negotiated_rate_not_codec_default() {
    let mut limiter = None;
    let _ = rate_limit_audio(&mut limiter, 16_000, &audio_chunk(1_600, false));
    assert_eq!(limiter.as_ref().unwrap().sample_rate(), 16_000);

    let _ = rate_limit_audio(&mut limiter, 24_000, &audio_chunk(1_600, false));
    assert_eq!(limiter.as_ref().unwrap().sample_rate(), 24_000);

    // Before the handshake publishes a rate (0), fall back to the
    // chunk's own stamp.
    let mut fresh = None;
    let _ = rate_limit_audio(&mut fresh, 0, &audio_chunk(1_600, false));
    assert_eq!(fresh.as_ref().unwrap().sample_rate(), 48_000);
}

/// A bare end-of-utterance marker (no samples) never costs tokens and
/// is never dropped, even on an empty bucket.
#[test]
fn empty_eou_marker_is_always_admitted() {
    let mut limiter = None;
    assert_eq!(
        rate_limit_audio(&mut limiter, 16_000, &audio_chunk(160_000, false)),
        RateDecision::Admit
    );
    assert_eq!(
        rate_limit_audio(&mut limiter, 16_000, &audio_chunk(0, true)),
        RateDecision::Admit
    );
}
