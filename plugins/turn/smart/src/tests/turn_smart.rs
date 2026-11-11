//! Tests for the Smart Turn detector's pre-model gating.
//!
//! The model itself needs a real ONNX export, so these cover the cheap checks
//! that decide whether an inference happens at all — the part that starved
//! the blocking pool when it was missing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{
    CompletionModel, MIN_SILENCE_FOR_INFERENCE, SmartTurnDetector, SmartTurnFactory,
    claim_inference,
};

fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

const LONG_PAUSE: Duration = Duration::from_millis(400);

#[test]
fn a_real_pause_with_speech_runs_the_model() {
    let in_flight = flag();
    let claim = claim_inference(
        "hello there",
        LONG_PAUSE,
        MIN_SILENCE_FOR_INFERENCE,
        &in_flight,
    );
    assert!(claim.is_some());
    assert!(in_flight.load(Ordering::Acquire), "slot is claimed");
}

#[test]
fn an_empty_transcript_never_reaches_the_model() {
    let in_flight = flag();
    assert!(claim_inference("   ", LONG_PAUSE, MIN_SILENCE_FOR_INFERENCE, &in_flight).is_none());
    assert!(!in_flight.load(Ordering::Acquire), "slot stays free");
}

/// The regression: `evaluate` runs on every interim result, and an interim
/// result arrives after a few tens of ms of silence. Running the model on
/// those saturated the blocking pool and hung the session in "speaking".
#[test]
fn a_pause_too_short_to_end_a_turn_never_reaches_the_model() {
    let in_flight = flag();
    for pause_ms in [0, 40, 100, 249] {
        let claim = claim_inference(
            "still talking",
            Duration::from_millis(pause_ms),
            MIN_SILENCE_FOR_INFERENCE,
            &in_flight,
        );
        assert!(claim.is_none(), "{pause_ms} ms must not run the model");
    }
    assert!(!in_flight.load(Ordering::Acquire), "slot stays free");
}

#[test]
fn the_threshold_itself_runs_the_model() {
    let in_flight = flag();
    assert!(
        claim_inference(
            "done now",
            MIN_SILENCE_FOR_INFERENCE,
            MIN_SILENCE_FOR_INFERENCE,
            &in_flight
        )
        .is_some()
    );
}

/// The second half of the fix: overlapping evaluations must not queue a
/// second inference behind the first.
#[test]
fn only_one_inference_is_claimed_at_a_time() {
    let in_flight = flag();

    let first = claim_inference("hello", LONG_PAUSE, MIN_SILENCE_FOR_INFERENCE, &in_flight)
        .expect("first call claims the slot");
    assert!(
        claim_inference(
            "hello there",
            LONG_PAUSE,
            MIN_SILENCE_FOR_INFERENCE,
            &in_flight
        )
        .is_none(),
        "a second concurrent evaluation must skip, not queue"
    );

    drop(first);
    assert!(
        !in_flight.load(Ordering::Acquire),
        "the guard freed the slot"
    );
    assert!(
        claim_inference(
            "hello there",
            LONG_PAUSE,
            MIN_SILENCE_FOR_INFERENCE,
            &in_flight
        )
        .is_some(),
        "the next evaluation runs once the slot is free"
    );
}

/// A panic inside the blocking closure must not wedge the detector into
/// permanently skipping every future evaluation.
#[test]
fn a_panic_while_holding_the_slot_frees_it() {
    let in_flight = flag();

    let claimed = claim_inference("hello", LONG_PAUSE, MIN_SILENCE_FOR_INFERENCE, &in_flight)
        .expect("slot claimed");
    let panicked = std::panic::catch_unwind(move || {
        let _guard = claimed;
        panic!("inference blew up");
    });
    assert!(panicked.is_err());

    assert!(
        !in_flight.load(Ordering::Acquire),
        "slot freed during unwind"
    );
    assert!(
        claim_inference("hello", LONG_PAUSE, MIN_SILENCE_FOR_INFERENCE, &in_flight).is_some(),
        "the detector still works after a panicking inference"
    );
}

// ── Settings ─────────────────────────────────────────────────

fn settings(pairs: &[(&str, serde_json::Value)]) -> voice_pipes_sdk::turn::TurnConfig {
    let mut cfg = voice_pipes_sdk::turn::TurnConfig::new("smart", 1000);
    for (k, v) in pairs {
        cfg.options.insert((*k).to_string(), v.clone());
    }
    cfg
}

/// A missing model, a value of the wrong type, one out of range and a key
/// Smart Turn does not read are each reported against their own key.
#[test]
fn each_bad_setting_is_reported_against_its_key() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let errors = SmartTurnFactory.validate(&settings(&[
        ("smart_threshold", serde_json::json!("high")),
        ("smart_window_secs", serde_json::json!(60)),
        ("smart_min_audio_ms", serde_json::json!(-5)),
        ("vap_threshold", serde_json::json!(0.5)),
    ]));
    let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
    for key in [
        "turn_detection.smart_model_path",
        "turn_detection.smart_threshold",
        "turn_detection.smart_window_secs",
        "turn_detection.smart_min_audio_ms",
        "turn_detection.vap_threshold",
    ] {
        assert!(fields.contains(&key), "{key} not in {fields:?}");
    }
}

/// With the model present and every setting in range, nothing is reported.
#[test]
fn good_settings_pass() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let model = std::env::temp_dir().join("vp-smart-turn-settings-test.onnx");
    std::fs::write(&model, b"not really a model").unwrap();
    let errors = SmartTurnFactory.validate(&settings(&[
        (
            "smart_model_path",
            serde_json::json!(model.to_string_lossy()),
        ),
        ("smart_threshold", serde_json::json!(0.6)),
        ("smart_window_secs", serde_json::json!(8)),
        ("smart_min_audio_ms", serde_json::json!(500)),
    ]));
    assert!(errors.is_empty(), "{errors:?}");
}

/// The wizard entry writes the settings this factory reads, with numbers as
/// numbers, and the model to fetch where its path points.
#[test]
fn the_wizard_entry_writes_settings_the_factory_reads() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let choices = crate::wizard_choices();
    assert_eq!(choices.len(), 1);
    let rendered = choices[0].render_defaults("mode", "smart");
    assert_eq!(
        rendered.model_path.as_deref(),
        Some("models/smart-turn-v3.2-cpu.onnx")
    );
    let table: toml::Table = rendered.toml.parse().expect("valid TOML");
    let mut cfg = voice_pipes_sdk::turn::TurnConfig::new("smart", 2000);
    for (k, v) in table {
        if k == "mode" || k == "silence_timeout_ms" {
            continue;
        }
        cfg.options
            .insert(k, serde_json::to_value(v).expect("a JSON value"));
    }
    // Only the model file is missing here, which the wizard downloads.
    let errors = SmartTurnFactory.validate(&cfg);
    let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
    assert_eq!(fields, ["turn_detection.smart_model_path"], "{errors:?}");
}

/// The TOML the wizard writes for this entry with every prompt accepted:
/// what the engine's built-in entry wrote before the move, less the section
/// header and the trailing blank line the stage adds.
#[test]
fn the_wizard_entry_writes_what_the_built_in_one_did() {
    let choice = crate::wizard_choices()
        .into_iter()
        .next()
        .expect("offered in the wizard");
    assert_eq!(
        choice.render_defaults("mode", "smart").toml,
        "mode = \"smart\"\nsilence_timeout_ms = 2000\n\
         smart_model_path = \"models/smart-turn-v3.2-cpu.onnx\"\nsmart_threshold = 0.5\n\
         smart_window_secs = 8\nsmart_min_audio_ms = 500\n"
    );
}

/// A fraction where a whole number is wanted says so, rather than
/// reporting a range the value is inside; the audio minimum has an upper
/// bound, the one the schema shows.
#[test]
fn a_wrong_type_and_a_wrong_value_get_their_own_messages() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let message = |key: &str, value: serde_json::Value| {
        SmartTurnFactory
            .validate(&settings(&[(key, value)]))
            .into_iter()
            .find(|e| e.field == format!("turn_detection.{key}"))
            .map(|e| e.message)
    };
    assert_eq!(
        message("smart_window_secs", serde_json::json!(8.5)).as_deref(),
        Some("must be a whole number from 1 to 8, got 8.5")
    );
    assert_eq!(
        message("smart_min_audio_ms", serde_json::json!(60_000)).as_deref(),
        Some("must be from 0 to 10000, got 60000")
    );
    assert_eq!(
        message("smart_min_audio_ms", serde_json::json!(10_000)),
        None
    );
}

/// The settings are read once, when the detector is built, so the console
/// shows them as changed by a restart, with the ranges parsing enforces.
#[test]
fn the_schema_offers_no_live_edits_and_matches_the_parser() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let schema = SmartTurnFactory.param_schema();
    assert!(schema.iter().all(|d| !d.mutable), "{schema:?}");
    let max = |key: &str| {
        schema
            .iter()
            .find(|d| d.key == key)
            .and_then(|d| d.constraints.as_ref())
            .and_then(|c| c.max)
    };
    assert_eq!(max("smart_window_secs"), Some(8.0));
    assert_eq!(max("smart_min_audio_ms"), Some(10_000.0));
    assert_eq!(max("smart_pool_size"), Some(128.0));
    assert_eq!(max("smart_threads"), Some(256.0));
}

#[test]
fn pool_size_and_threads_out_of_range_are_reported() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let errors = SmartTurnFactory.validate(&settings(&[
        ("smart_pool_size", serde_json::json!(0)),
        ("smart_threads", serde_json::json!(300)),
    ]));
    assert!(
        errors
            .iter()
            .any(|e| e.field == "turn_detection.smart_pool_size"),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.field == "turn_detection.smart_threads"),
        "{errors:?}"
    );
}

/// Load a real export through the factory and ask it about one second of
/// audio after a pause: the session loads, inference runs, and the answer
/// is a probability. Skipped unless `VP_SMART_TURN_MODEL` points at a file,
/// since the model is downloaded separately.
#[tokio::test]
#[ignore = "needs the downloaded model: set VP_SMART_TURN_MODEL"]
async fn a_real_export_loads_and_answers_with_a_probability() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let Ok(model) = std::env::var("VP_SMART_TURN_MODEL") else {
        panic!("set VP_SMART_TURN_MODEL to run against a real export")
    };
    let detector = SmartTurnFactory
        .build(
            &settings(&[("smart_model_path", serde_json::json!(model))]),
            &voice_pipes_sdk::BuildContext::new(),
        )
        .expect("the export loads");
    // A 220 Hz tone stands in for a second of voiced audio.
    let tone: Vec<i16> = (0..16_000)
        .map(|i| {
            let t = f64::from(i) / 16_000.0;
            #[expect(clippy::cast_possible_truncation, reason = "bounded by the amplitude")]
            let s = ((t * 220.0 * std::f64::consts::TAU).sin() * 8_000.0) as i16;
            s
        })
        .collect();
    detector.feed_audio(&tone, 16_000);
    let result = voice_pipes_sdk::TranscriptionResult::new("hello there", true);
    let signal = detector.evaluate(&result, Duration::from_millis(600)).await;
    let p = signal
        .confidence
        .expect("the model ran and reported a probability");
    assert!((0.0..=1.0).contains(&p), "{p}");
}

// ── The detector over a stub model ───────────────────────────

/// A model that takes `delay` and answers `probability`, counting calls
/// (a call is counted when it starts).
struct StubModel {
    delay: Duration,
    probability: f32,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl CompletionModel for StubModel {
    fn probability(&mut self, _features: Vec<f32>) -> anyhow::Result<f32> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        Ok(self.probability)
    }
}

fn stub_detector(
    delay_ms: u64,
    probability: f32,
) -> (SmartTurnDetector, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let model = StubModel {
        delay: Duration::from_millis(delay_ms),
        probability,
        calls: Arc::clone(&calls),
    };
    (
        SmartTurnDetector::with_model(Box::new(model), 0.5, 8, 500, 10_000),
        calls,
    )
}

fn result(text: &str) -> voice_pipes_sdk::TranscriptionResult {
    voice_pipes_sdk::TranscriptionResult::new(text, false)
}

/// The regression: inference held the lock `feed_audio` takes on the STT
/// task, so every audio chunk waited out a model run. Audio now goes to
/// the ring while the model runs.
#[tokio::test]
async fn audio_is_taken_while_the_model_runs() {
    use voice_pipes_sdk::turn::TurnDetector;
    let (detector, calls) = stub_detector(1_000, 0.9);
    let detector = Arc::new(detector);
    detector.feed_audio(&vec![1000; 16_000], 16_000);
    let running = {
        let d = Arc::clone(&detector);
        tokio::spawn(async move { d.evaluate(&result("hello"), LONG_PAUSE).await })
    };
    // Feed only once the model is running.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while calls.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the model started");
    let t0 = std::time::Instant::now();
    detector.feed_audio(&[1000; 1_600], 16_000);
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "feed_audio waited {:?} for the model",
        t0.elapsed()
    );
    assert!(running.await.unwrap().is_complete());
}

/// With less audio than the model needs there is no probability: the
/// signal carries none, rather than a confident "not finished" of 0.
#[tokio::test]
async fn too_little_audio_reports_no_probability() {
    use voice_pipes_sdk::turn::TurnDetector;
    let (detector, calls) = stub_detector(0, 0.9);
    detector.feed_audio(&[1000; 1_600], 16_000); // 100 ms, under the 500 ms minimum
    let signal = detector.evaluate(&result("hello"), LONG_PAUSE).await;
    assert!(!signal.is_complete());
    assert_eq!(signal.confidence, None);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the model was not asked");
}

/// The window holds one turn: when the turn ends, the next starts empty
/// instead of being judged on the last one's tail.
#[test]
fn a_turn_ending_clears_the_window() {
    use voice_pipes_sdk::turn::TurnDetector;
    let (detector, _calls) = stub_detector(0, 0.9);
    detector.feed_audio(&vec![1000; 16_000], 16_000);
    assert_eq!(detector.buffered_samples(), 16_000);
    detector.turn_ended();
    assert_eq!(detector.buffered_samples(), 0);
}

/// The model reads the last 8 s, so a longer window is refused rather than
/// held for nothing.
#[test]
fn a_window_longer_than_the_model_reads_is_refused() {
    use voice_pipes_sdk::turn::TurnDetectorFactory;
    let errors =
        SmartTurnFactory.validate(&settings(&[("smart_window_secs", serde_json::json!(9))]));
    assert!(
        errors
            .iter()
            .any(|e| e.field == "turn_detection.smart_window_secs"),
        "{errors:?}"
    );
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
