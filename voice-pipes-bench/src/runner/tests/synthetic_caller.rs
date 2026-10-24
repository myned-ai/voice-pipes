use super::*;

fn make_synth_result(
    pipeline_turns: &[(&str, Option<f64>)],
    attempts: u32,
    successes: u32,
) -> SyntheticCallerResult {
    let mut turns = Vec::new();
    for (i, (text, latency)) in pipeline_turns.iter().enumerate() {
        turns.push(SyntheticTurn {
            speaker: "pipeline".into(),
            turn_index: i as u32,
            transcript: Some((*text).to_string()),
            latency_ms: *latency,
            ttft_ms: None,
            ttfb_ms: None,
            barge_in: false,
        });
    }
    SyntheticCallerResult {
        turns,
        stop_reason: Some(SyntheticStopReason::MaxTurns),
        total_duration_ms: Some(1000),
        barge_in_attempts: attempts,
        barge_in_successes: successes,
    }
}

#[test]
fn score_synthetic_caller_flags_min_turns() {
    let exp = ScenarioExpected {
        min_completed_turns: Some(3),
        ..Default::default()
    };
    let res = make_synth_result(&[("hello", Some(100.0))], 0, 0);
    let mut failures = Vec::new();
    score_synthetic_caller(&exp, &res, &mut failures);
    assert!(
        failures.iter().any(|f| f.contains("min_completed_turns")),
        "expected min_completed_turns failure, got {failures:?}"
    );
}

#[test]
fn score_synthetic_caller_flags_must_say_miss() {
    let exp = ScenarioExpected {
        pipeline_must_say: vec!["banana".into()],
        ..Default::default()
    };
    let res = make_synth_result(&[("apple sauce", None), ("orange juice", None)], 0, 0);
    let mut failures = Vec::new();
    score_synthetic_caller(&exp, &res, &mut failures);
    assert!(
        failures.iter().any(|f| f.contains("pipeline_must_say")),
        "expected pipeline_must_say failure, got {failures:?}"
    );
}

#[test]
fn score_synthetic_caller_must_say_is_case_insensitive() {
    let exp = ScenarioExpected {
        pipeline_must_say: vec!["BaNaNa".into()],
        ..Default::default()
    };
    let res = make_synth_result(&[("I love bananas!", None)], 0, 0);
    let mut failures = Vec::new();
    score_synthetic_caller(&exp, &res, &mut failures);
    assert!(failures.is_empty(), "got failures: {failures:?}");
}

#[test]
fn score_synthetic_caller_flags_latency_cap() {
    let exp = ScenarioExpected {
        max_pipeline_turn_latency_ms: Some(500),
        ..Default::default()
    };
    let res = make_synth_result(&[("ok", Some(800.0))], 0, 0);
    let mut failures = Vec::new();
    score_synthetic_caller(&exp, &res, &mut failures);
    assert!(
        failures.iter().any(|f| f.contains("latency")),
        "expected latency failure, got {failures:?}"
    );
}

#[test]
fn score_synthetic_caller_flags_barge_in_failure_rate() {
    let exp = ScenarioExpected {
        max_barge_in_failure_rate: Some(0.25),
        ..Default::default()
    };
    // 4 attempts, 1 success -> 75% failure rate.
    let res = make_synth_result(&[("ok", None)], 4, 1);
    let mut failures = Vec::new();
    score_synthetic_caller(&exp, &res, &mut failures);
    assert!(
        failures
            .iter()
            .any(|f| f.contains("max_barge_in_failure_rate")),
        "expected barge_in failure rate failure, got {failures:?}"
    );
}

#[test]
fn score_synthetic_caller_passes_when_within_budget() {
    let exp = ScenarioExpected {
        min_completed_turns: Some(1),
        pipeline_must_say: vec!["hello".into()],
        max_pipeline_turn_latency_ms: Some(1000),
        max_barge_in_failure_rate: Some(0.5),
        ..Default::default()
    };
    let res = make_synth_result(&[("hello world", Some(500.0))], 2, 2);
    let mut failures = Vec::new();
    score_synthetic_caller(&exp, &res, &mut failures);
    assert!(failures.is_empty(), "got failures: {failures:?}");
}
