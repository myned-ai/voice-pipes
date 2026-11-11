use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use voice_pipes_sdk::TranscriptionResult;
use voice_pipes_sdk::turn::{
    TurnConfig, TurnDecision, TurnDetector, TurnDetectorFactory, TurnSignal,
};

use crate::config::VAP_KEYS;
use crate::detector::{
    FUTURE_BIN_START, FloorModel, Prediction, Rings, VapTurnDetector, window_from_input,
};
use crate::factory::VapTurnFactory;

// ── Ring alignment ──────────────────────────────────────────
//
// Tested without a model: the alignment rules are the part most likely to be
// wrong, and requiring an exported checkpoint on disk would stop anyone
// checking them.

#[test]
fn the_user_channel_drives_the_clock() {
    let mut r = Rings::new(8);
    r.push_user(&[1.0, 1.0, 1.0]);
    assert_eq!(r.user.len(), 3);
    assert_eq!(r.agent.len(), 3, "the agent channel follows with silence");
    assert!(r.agent.iter().all(|&s| s == 0.0));
}

#[test]
fn agent_audio_lands_on_the_tail_without_extending_the_timeline() {
    let mut r = Rings::new(8);
    r.push_user(&[1.0; 4]);
    r.push_agent(&[9.0, 9.0]);
    assert_eq!(r.user.len(), 4);
    assert_eq!(
        r.agent.len(),
        4,
        "the agent must not run ahead of the clock"
    );
    assert_eq!(
        r.agent.iter().copied().collect::<Vec<_>>(),
        vec![0.0, 0.0, 9.0, 9.0]
    );
}

#[test]
fn agent_audio_before_any_user_audio_is_dropped() {
    // There is no timeline to place it on yet, and inventing one would put
    // the two channels permanently out of step.
    let mut r = Rings::new(8);
    r.push_agent(&[9.0, 9.0]);
    assert!(r.agent.is_empty());
    assert!(r.user.is_empty());
}

#[test]
fn agent_audio_longer_than_the_timeline_keeps_its_most_recent_samples() {
    let mut r = Rings::new(8);
    r.push_user(&[1.0, 1.0]);
    r.push_agent(&[7.0, 8.0, 9.0]);
    assert_eq!(r.agent.iter().copied().collect::<Vec<_>>(), vec![8.0, 9.0]);
}

#[test]
fn both_channels_evict_together_so_they_stay_aligned() {
    let mut r = Rings::new(4);
    r.push_user(&[1.0, 2.0, 3.0, 4.0]);
    r.push_agent(&[5.0, 5.0, 5.0, 5.0]);
    r.push_user(&[9.0, 9.0]);
    assert_eq!(r.user.len(), 4);
    assert_eq!(r.agent.len(), 4);
    assert_eq!(
        r.user.iter().copied().collect::<Vec<_>>(),
        vec![3.0, 4.0, 9.0, 9.0]
    );
    // The two oldest agent samples were evicted with their user samples, and
    // the two new positions are silence until agent audio arrives.
    assert_eq!(
        r.agent.iter().copied().collect::<Vec<_>>(),
        vec![5.0, 5.0, 0.0, 0.0]
    );
}

#[test]
fn a_partly_filled_window_is_padded_at_the_front() {
    // Front-padding keeps the newest audio at the end of the window, where
    // the model's last frame reads from.
    let mut r = Rings::new(4);
    r.push_user(&[1.0, 2.0]);
    r.push_agent(&[3.0, 3.0]);
    let w = r.stereo_window();
    assert_eq!(w.len(), 8, "channel-major [user.., agent..]");
    assert_eq!(&w[0..4], &[0.0, 0.0, 1.0, 2.0]);
    assert_eq!(&w[4..8], &[0.0, 0.0, 3.0, 3.0]);
}

#[test]
fn after_turn_ended_agent_audio_advances_the_clock_with_user_silence() {
    // When upstream VAD (silero/energy) or echo-gating is active, no user
    // mic frames reach the turn detector while the user is silent and the
    // agent is speaking. Once turn_ended() marks the end of the user's turn,
    // agent TTS chunks must advance the stereo timeline (with 0.0 on the user
    // channel) rather than repeatedly overwriting the last user frame.
    let mut r = Rings::new(6);
    r.push_user(&[1.0, 2.0]);
    r.end_user_turn();
    r.push_agent(&[7.0, 8.0]);
    r.push_agent(&[9.0]);
    assert_eq!(
        r.user.iter().copied().collect::<Vec<_>>(),
        vec![1.0, 2.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(
        r.agent.iter().copied().collect::<Vec<_>>(),
        vec![0.0, 0.0, 7.0, 8.0, 9.0]
    );

    // Once user speech resumes (barge-in / next turn), push_user reclaims the
    // clock and subsequent overlapping push_agent overwrites the tail again.
    r.push_user(&[3.0, 4.0]);
    r.push_agent(&[6.0]);
    assert_eq!(
        r.user.iter().copied().collect::<Vec<_>>(),
        vec![2.0, 0.0, 0.0, 0.0, 3.0, 4.0]
    );
    assert_eq!(
        r.agent.iter().copied().collect::<Vec<_>>(),
        vec![0.0, 7.0, 8.0, 9.0, 0.0, 6.0]
    );
}

// ── Input-shape introspection ───────────────────────────────

#[test]
fn a_fixed_stereo_shape_yields_the_window() {
    assert_eq!(window_from_input(&[1, 2, 32_000]), Some(32_000));
}

#[test]
fn a_dynamic_or_wrong_shape_is_rejected() {
    // -1 is how a dynamic axis is reported, and the window has to be
    // concrete: a dynamic export does not survive a different input length.
    assert_eq!(window_from_input(&[1, 2, -1]), None);
    assert_eq!(window_from_input(&[1, 1, 32_000]), None, "mono");
    assert_eq!(window_from_input(&[2, 32_000]), None, "wrong rank");
    assert_eq!(window_from_input(&[]), None, "no shape");
}

// ── Probabilities to signal ─────────────────────────────────

fn predict(p_now: f32, p_future: f32) -> TurnSignal {
    Prediction { p_now, p_future }.to_signal(0.5)
}

#[test]
fn a_user_holding_the_floor_is_incomplete() {
    let s = predict(0.9, 0.9);
    assert!(!s.is_complete());
    // p_now is P(the *user* holds the floor), so confidence is its complement.
    assert!((s.confidence.expect("confidence") - 0.1).abs() < 1e-6);
    assert_eq!(s.expected_end_in, None, "no end in sight");
}

#[test]
fn a_user_who_has_yielded_completes_now() {
    let s = predict(0.2, 0.2);
    assert_eq!(s.decision, TurnDecision::LikelyComplete);
    assert!((s.confidence.expect("confidence") - 0.8).abs() < 1e-6);
    assert_eq!(s.expected_end_in, Some(Duration::ZERO));
}

#[test]
fn holding_now_but_yielding_soon_reports_lead_time() {
    // This is the whole point of the detector: the turn has not ended, and
    // the pipeline is told how long it has before it does.
    let s = predict(0.8, 0.3);
    assert_eq!(s.decision, TurnDecision::Incomplete);
    assert_eq!(s.expected_end_in, Some(FUTURE_BIN_START));
    assert!(s.confidence.expect("confidence") < 0.5);
}

#[test]
fn the_reported_lead_time_is_the_bins_near_edge() {
    // The model says "inside the future bin", not a point in time, so the
    // signal must not imply precision the prediction does not have.
    let s = predict(0.8, 0.0);
    assert_eq!(s.expected_end_in, Some(FUTURE_BIN_START));
    let s2 = predict(0.8, 0.49);
    assert_eq!(
        s2.expected_end_in,
        Some(FUTURE_BIN_START),
        "a weaker future prediction still reports the same bin edge"
    );
}

// ── Factory validation ──────────────────────────────────────

fn cfg() -> TurnConfig {
    TurnConfig::new("vap", 1000)
}

#[test]
fn a_missing_model_path_is_a_config_error() {
    let errors = VapTurnFactory.validate(&cfg());
    assert!(
        errors.iter().any(|e| e.field.contains("vap_model_path")),
        "expected a vap_model_path error, got {errors:?}"
    );
}

#[test]
fn a_model_path_that_does_not_exist_is_a_config_error() {
    let mut c = cfg();
    c.options.insert(
        "vap_model_path".into(),
        serde_json::json!("definitely/not/here.onnx"),
    );
    let errors = VapTurnFactory.validate(&c);
    assert!(errors.iter().any(|e| e.field.contains("vap_model_path")));
}

#[test]
fn a_threshold_outside_zero_to_one_is_a_config_error() {
    let mut c = cfg();
    c.options.insert(
        "vap_model_path".into(),
        serde_json::json!("definitely/not/here.onnx"),
    );
    c.options
        .insert("vap_threshold".into(), serde_json::json!(1.5));
    let errors = VapTurnFactory.validate(&c);
    assert!(errors.iter().any(|e| e.field.contains("vap_threshold")));
}

#[test]
fn the_factory_registers_under_its_mode_name() {
    assert_eq!(VapTurnFactory.name(), "vap");
}

/// A run interval past the bound, or not a whole number, is refused with
/// its own message.
#[test]
fn the_run_interval_is_bounded_and_whole() {
    let message = |value: serde_json::Value| {
        let mut c = cfg();
        c.options.insert("vap_min_interval_ms".into(), value);
        VapTurnFactory
            .validate(&c)
            .into_iter()
            .find(|e| e.field == "turn_detection.vap_min_interval_ms")
            .map(|e| e.message)
    };
    assert_eq!(message(serde_json::json!(200)), None);
    assert_eq!(
        message(serde_json::json!(60_000)).as_deref(),
        Some("must be from 0 to 10000, got 60000")
    );
    assert_eq!(
        message(serde_json::json!(0.5)).as_deref(),
        Some("must be a whole number from 0 to 10000, got 0.5")
    );
}

/// Every setting the detector reads is described for the console, and
/// none as live-editable: each is read once, when the detector is built.
#[test]
fn the_schema_describes_every_setting_as_restart_only() {
    let schema = VapTurnFactory.param_schema();
    let keys: Vec<&str> = schema.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(keys, VAP_KEYS);
    assert!(schema.iter().all(|d| !d.mutable), "{schema:?}");
}

#[test]
fn pool_size_and_threads_out_of_range_are_reported() {
    let mut c = cfg();
    c.options
        .insert("vap_pool_size".into(), serde_json::json!(0));
    c.options
        .insert("vap_threads".into(), serde_json::json!(300));
    let errors = VapTurnFactory.validate(&c);
    assert!(
        errors
            .iter()
            .any(|e| e.field == "turn_detection.vap_pool_size"),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.field == "turn_detection.vap_threads"),
        "{errors:?}"
    );
}

// ── Against a real exported model ───────────────────────────

/// Load and run an actual export, when one is available.
///
/// Skipped unless `VP_VAP_MODEL` points at a file, because the model is
/// ~23 MB and is produced by a separate Python tool rather than the build.
/// Everything above tests the logic around the model; this is the only
/// thing that can catch the export and the detector disagreeing about
/// shapes, output order, or what the numbers mean.
#[tokio::test]
#[ignore = "needs an exported model: set VP_VAP_MODEL"]
async fn a_real_export_loads_and_predicts() {
    let Ok(path) = std::env::var("VP_VAP_MODEL") else {
        panic!("set VP_VAP_MODEL to an exported vap .onnx to run this")
    };

    let detector = VapTurnDetector::new(std::path::Path::new(&path), 0.5, 0, 10_000)
        .expect("model loads and the startup probe passes");
    assert!(
        detector.window_samples.is_multiple_of(16_000),
        "window should be a whole number of seconds at 16 kHz, got {} samples",
        detector.window_samples
    );

    let result = TranscriptionResult::new("hello", false).with_speech_started(true);

    // Fill the window with speech-shaped audio on the user's channel.
    let n = detector.window_samples;
    let speech: Vec<i16> = (0..n)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            let env = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 4.0 * t).sin();
            ((t * 220.0 * 2.0 * std::f32::consts::PI).sin() * env * 8000.0) as i16
        })
        .collect();
    detector.feed_audio(&speech, 16_000);

    let signal = detector
        .evaluate(&result, std::time::Duration::from_millis(50))
        .await;

    // The assertion is that the pipeline gets a usable prediction, not that
    // the model reaches a particular verdict on synthetic audio.
    let confidence = signal
        .confidence
        .expect("a real prediction must carry confidence");
    assert!(
        (0.0..=1.0).contains(&confidence),
        "confidence out of range: {confidence}"
    );
    assert_ne!(
        signal.decision,
        TurnDecision::ForcedComplete,
        "50 ms of silence must not trip the 10 s fallback"
    );
}

// ── The detector over a stub model ───────────────────────────

/// A model that takes `delay` and says the user holds the floor, counting
/// calls (a call is counted when it starts).
struct StubModel {
    delay: Duration,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl FloorModel for StubModel {
    fn predict(&mut self, _stereo: &[f32]) -> anyhow::Result<Prediction> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        Ok(Prediction {
            p_now: 0.9,
            p_future: 0.9,
        })
    }
}

fn stub_detector(
    delay_ms: u64,
    interval_ms: u64,
) -> (Arc<VapTurnDetector>, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let model = StubModel {
        delay: Duration::from_millis(delay_ms),
        calls: Arc::clone(&calls),
    };
    (
        Arc::new(VapTurnDetector::with_model(
            Box::new(model),
            1_600,
            0.5,
            interval_ms,
            60_000,
        )),
        calls,
    )
}

async fn wait_for_calls(calls: &std::sync::atomic::AtomicUsize, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while calls.load(Ordering::SeqCst) < n && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), n);
}

/// The regression: inference held the lock the audio feeds take, so the
/// STT task and the playback path waited out every model run.
#[tokio::test]
async fn audio_is_taken_while_the_model_runs() {
    let (detector, calls) = stub_detector(1_000, 0);
    detector.feed_audio(&[1000; 1_600], 16_000);
    let running = {
        let d = Arc::clone(&detector);
        let r = TranscriptionResult::new("hi", false);
        tokio::spawn(async move { d.evaluate(&r, Duration::from_millis(50)).await })
    };
    wait_for_calls(&calls, 1).await;
    let t0 = Instant::now();
    detector.feed_audio(&[1000; 160], 16_000);
    detector.feed_agent_audio(&[1000; 160], 16_000);
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "the feeds waited {:?} for the model",
        t0.elapsed()
    );
    running.await.unwrap();
}

/// The regression: the run interval was stamped before the in-flight
/// check, so an evaluation skipped because one was running counted as a
/// run and pushed the next one back a whole interval.
#[tokio::test]
async fn a_skipped_evaluation_does_not_delay_the_next_run() {
    let (detector, calls) = stub_detector(300, 250);
    let r = TranscriptionResult::new("hi", false);
    let first = {
        let d = Arc::clone(&detector);
        let r = r.clone();
        tokio::spawn(async move { d.evaluate(&r, Duration::from_millis(50)).await })
    };
    wait_for_calls(&calls, 1).await;
    tokio::time::sleep(Duration::from_millis(260)).await;
    // In flight: skipped.
    detector.evaluate(&r, Duration::from_millis(50)).await;
    first.await.unwrap();
    // The interval since the one real run has passed: this one runs.
    detector.evaluate(&r, Duration::from_millis(50)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn metric_conformance() {
    let res = voice_pipes_sdk::testing::assert_vad_metrics_conformance(
        std::time::Duration::from_millis(500),
        std::time::Duration::from_millis(1000),
        std::time::Duration::from_millis(150),
    );
    assert!(res.is_ok(), "{res:?}");
}
