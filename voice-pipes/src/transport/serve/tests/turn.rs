use super::*;

/// `emit_record` must never block the runner. When the bounded
/// telemetry channel is full, the record is dropped (not queued, not
/// awaited), so a stalled orchestrator forwarder can't grow memory or stall
/// turn completion.
#[tokio::test]
async fn emit_record_drops_on_full_without_blocking() {
    let (tx, mut rx) = mpsc::channel::<u32>(1);

    emit_record(&tx, 1, "test"); // fills the single slot
    emit_record(&tx, 2, "test"); // channel full -> dropped, must not block

    assert_eq!(rx.recv().await, Some(1));
    assert!(
        rx.try_recv().is_err(),
        "the over-capacity record must have been dropped"
    );
}

/// A closed channel (consumer gone) is silently ignored, not an error.
#[tokio::test]
async fn emit_record_ignores_closed_channel() {
    let (tx, rx) = mpsc::channel::<u32>(4);
    drop(rx);
    emit_record(&tx, 1, "test"); // must not panic
}

fn sample_health() -> crate::api::types::PlaybackHealth {
    crate::api::types::PlaybackHealth {
        underrun_count: 3,
        total_silence_ms: 900,
        audio_ms: 5000,
        heard_ms: 5000,
        wall_ms: 5900,
        gaps: vec![crate::transport::PlaybackGap {
            at_audio_ms: 480,
            silence_ms: 300,
        }],
        ..crate::api::types::PlaybackHealth::default()
    }
}

/// The cascade turn record drains
/// `tracker.playback_health` into `SessionDetail.playback_health`,
/// and `start_turn` clears any stale value.
#[test]
fn cascade_turn_record_carries_playback_health() {
    let mut tracker = TurnTracker::new();
    tracker.playback_health = Some(sample_health());
    let snapshot = TurnMetricsSnapshot::default();
    let record = build_turn_record(
        "s1",
        &snapshot,
        &mut tracker,
        "stt",
        "tts",
        16000,
        Some(16000),
        &None,
        vec![],
    );
    let detail = record.detail.expect("detail");
    assert_eq!(detail.playback_health, Some(sample_health()));
    assert!(
        tracker.playback_health.is_none(),
        "take() must drain the tracker"
    );

    tracker.playback_health = Some(sample_health());
    tracker.start_turn(2, CancellationToken::new());
    assert!(
        tracker.playback_health.is_none(),
        "start_turn must clear stale health"
    );
}

/// The realtime turn record carries the
/// health passed by `persist_turn` (from the shared pending slot).
#[cfg(feature = "realtime")]
#[test]
fn realtime_turn_record_carries_playback_health() {
    let snapshot = TurnMetricsSnapshot::default();
    let record = build_realtime_turn_record(
        "s1",
        &snapshot,
        "gemini-live",
        16000,
        16000,
        &None,
        vec![],
        None,
        Some(sample_health()),
    );
    assert_eq!(
        record.detail.expect("detail").playback_health,
        Some(sample_health())
    );
    let record_none = build_realtime_turn_record(
        "s1",
        &snapshot,
        "gemini-live",
        16000,
        16000,
        &None,
        vec![],
        None,
        None,
    );
    assert_eq!(record_none.detail.expect("detail").playback_health, None);
}

#[tokio::test]
async fn emit_turn_metrics_preserves_tts_ttfb_and_updates_commit_to_first_audio() {
    use crate::pipeline::TurnComponentMetrics;
    use crate::pipeline::extras::{LlmExtras, TtsExtras};
    let (out_tx, mut out_rx) = mpsc::channel(8);
    let (pe_tx, _pe_rx) = tokio::sync::broadcast::channel(8);

    let pipe_snap = TurnMetricsSnapshot {
        utterance_id: 1,
        total_ms: 900,
        interrupted: false,
        components: vec![
            TurnComponentMetrics {
                component_id: "llm:test".into(),
                kind: "llm".into(),
                started_at_ms: 0,
                finished_at_ms: Some(250),
                latency_ms: Some(250),
                provider: Some("test-llm".into()),
                cold_start: false,
                error: None,
                extras: serde_json::to_value(LlmExtras {
                    ttft_ms: Some(245),
                    llm_wait_ms: Some(250.0),
                    ..Default::default()
                })
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
            },
            TurnComponentMetrics {
                component_id: "tts:test".into(),
                kind: "tts".into(),
                started_at_ms: 250,
                finished_at_ms: Some(600),
                latency_ms: Some(350),
                provider: Some("test-tts".into()),
                cold_start: false,
                error: None,
                extras: serde_json::to_value(TtsExtras {
                    ttfb_ms: Some(180),
                    tts_wait_ms: Some(182.4),
                    output_wait_ms: Some(5.0),
                    commit_to_first_audio_ms: Some(437.4),
                    synthesis_ms: Some(350),
                    audio_duration_ms: Some(2000),
                    ..Default::default()
                })
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
            },
        ],
        ..TurnMetricsSnapshot::default()
    };

    let mut tracker = TurnTracker::new();
    tracker.start_turn(1, CancellationToken::new());
    tracker.ttfb_recorded = Some(std::time::Duration::from_millis(450));

    let snap = emit_turn_metrics(
        &mut tracker,
        false,
        &pipe_snap,
        &None,
        &out_tx,
        &pe_tx,
        "stt",
        "tts",
    )
    .await;

    let tts = snap.component_by_kind("tts").expect("tts component");
    assert_eq!(
        tts.extras
            .get("ttfb_ms")
            .and_then(serde_json::Value::as_u64),
        Some(180),
        "P2.2: runner must preserve TTS's own ttfb_ms instead of overwriting with commit-to-first-audio"
    );
    assert!(
        tts.extras.get("tts_internal_ttfb_ms").is_none(),
        "P2.2: tts_internal_ttfb_ms workaround key must not be inserted"
    );
    assert_eq!(
        tts.extras
            .get("commit_to_first_audio_ms")
            .and_then(serde_json::Value::as_f64),
        Some(450.0)
    );
    let out_wait = tts
        .extras
        .get("output_wait_ms")
        .and_then(serde_json::Value::as_f64)
        .expect("output_wait_ms");
    assert!(
        (out_wait - 17.6).abs() < 0.01,
        "expected 17.6, got {out_wait}"
    );

    let msg = out_rx.recv().await.expect("TurnMetrics message");
    match msg {
        ServerMessage::Control(ServerControl::TurnMetrics { snapshot }) => {
            assert_eq!(snapshot.ttfb_ms(), Some(180));
        }
        other => panic!("expected TurnMetrics, got {other:?}"),
    }
}
