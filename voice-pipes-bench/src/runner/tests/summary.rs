use super::*;

/// When `latency_samples` are provided, `compute_summary` should use
/// the individual per-repetition values for percentile computation
/// rather than the single averaged `ScenarioMetrics` value.
#[test]
fn summary_uses_per_rep_latencies_for_percentiles() {
    // One scenario that ran 5 repetitions with distinct latencies.
    let scenarios = vec![ScenarioResult {
        scenario_id: "s1".into(),
        scenario_name: "scenario-1".into(),
        session_ids: vec![],
        status: ScenarioRunStatus::Passed,
        metrics: ScenarioMetrics {
            e2e_latency_ms: Some(300.0), // average of 100..500
            ttft_ms: Some(50.0),
            ttfb_ms: Some(80.0),
            stt_text: None,
            response_text: None,
            audio_duration_s: None,
            stt_latency_ms: None,
            llm_generation_ms: None,
            llm_tokens: None,
            tts_ttfb_ms: None,
            ..ScenarioMetrics::default()
        },
        latency_samples: vec![100.0, 200.0, 300.0, 400.0, 500.0],
        scoring: None,
        error_message: None,
        repetitions_failed: None,
        tts_entries: None,
        turns: None,
        barge_in: None,
        synthetic_caller: None,
        gym: None,
    }];

    let summary = compute_summary(&scenarios);

    // P50 should be ~300 (the median of 100..500), not the single 300
    // that would come from the averaged metrics.  The key difference
    // is visible when multiple scenarios exist -- here we at least
    // verify the percentile function sees all 5 data points.
    assert_eq!(summary.p50_e2e_latency_ms, Some(300));
    // P90 should pick 500 (nearest rank of 5 values).
    assert_eq!(summary.p90_e2e_latency_ms, Some(500));
    // P99 should also pick 500.
    assert_eq!(summary.p99_e2e_latency_ms, Some(500));
}

/// Two scenarios with different repetition counts -- the summary percentiles
/// should be computed over ALL individual repetitions, not scenario averages.
#[test]
fn summary_merges_repetitions_across_scenarios() {
    let scenarios = vec![
        ScenarioResult {
            scenario_id: "s1".into(),
            scenario_name: "fast".into(),
            session_ids: vec![],
            status: ScenarioRunStatus::Passed,
            metrics: ScenarioMetrics {
                e2e_latency_ms: Some(100.0),
                ttft_ms: None,
                ttfb_ms: None,
                stt_text: None,
                response_text: None,
                audio_duration_s: None,
                stt_latency_ms: None,
                llm_generation_ms: None,
                llm_tokens: None,
                tts_ttfb_ms: None,
                ..ScenarioMetrics::default()
            },
            latency_samples: vec![90.0, 100.0, 110.0],
            scoring: None,
            error_message: None,
            repetitions_failed: None,
            tts_entries: None,
            turns: None,
            barge_in: None,
            synthetic_caller: None,
        gym: None,
        },
        ScenarioResult {
            scenario_id: "s2".into(),
            scenario_name: "slow".into(),
            session_ids: vec![],
            status: ScenarioRunStatus::Passed,
            metrics: ScenarioMetrics {
                e2e_latency_ms: Some(900.0),
                ttft_ms: None,
                ttfb_ms: None,
                stt_text: None,
                response_text: None,
                audio_duration_s: None,
                stt_latency_ms: None,
                llm_generation_ms: None,
                llm_tokens: None,
                tts_ttfb_ms: None,
                ..ScenarioMetrics::default()
            },
            latency_samples: vec![800.0, 900.0, 1000.0],
            scoring: None,
            error_message: None,
            repetitions_failed: None,
            tts_entries: None,
            turns: None,
            barge_in: None,
            synthetic_caller: None,
        gym: None,
        },
    ];

    let summary = compute_summary(&scenarios);

    // 6 samples sorted: [90, 100, 110, 800, 900, 1000]
    // P50: index round(0.5 * 5) = 3 -> 800
    assert_eq!(summary.p50_e2e_latency_ms, Some(800));
    // P90: index round(0.9 * 5) = 5 -> 1000 (clamped)
    assert_eq!(summary.p90_e2e_latency_ms, Some(1000));

    // Average over all 6 samples.
    let expected_avg = (90.0 + 100.0 + 110.0 + 800.0 + 900.0 + 1000.0) / 6.0;
    assert!((summary.avg_e2e_latency_ms.unwrap() - expected_avg).abs() < 0.01);
}

/// When `latency_samples` are empty (legacy data), `compute_summary`
/// falls back to the averaged `ScenarioMetrics` values.
#[test]
fn summary_falls_back_to_scenario_avg_when_no_samples() {
    let scenarios = vec![
        ScenarioResult {
            scenario_id: "s1".into(),
            scenario_name: "a".into(),
            session_ids: vec![],
            status: ScenarioRunStatus::Passed,
            metrics: ScenarioMetrics {
                e2e_latency_ms: Some(200.0),
                ttft_ms: None,
                ttfb_ms: None,
                stt_text: None,
                response_text: None,
                audio_duration_s: None,
                stt_latency_ms: None,
                llm_generation_ms: None,
                llm_tokens: None,
                tts_ttfb_ms: None,
                ..ScenarioMetrics::default()
            },
            latency_samples: vec![],
            scoring: None,
            error_message: None,
            repetitions_failed: None,
            tts_entries: None,
            turns: None,
            barge_in: None,
            synthetic_caller: None,
        gym: None,
        },
        ScenarioResult {
            scenario_id: "s2".into(),
            scenario_name: "b".into(),
            session_ids: vec![],
            status: ScenarioRunStatus::Passed,
            metrics: ScenarioMetrics {
                e2e_latency_ms: Some(400.0),
                ttft_ms: None,
                ttfb_ms: None,
                stt_text: None,
                response_text: None,
                audio_duration_s: None,
                stt_latency_ms: None,
                llm_generation_ms: None,
                llm_tokens: None,
                tts_ttfb_ms: None,
                ..ScenarioMetrics::default()
            },
            latency_samples: vec![],
            scoring: None,
            error_message: None,
            repetitions_failed: None,
            tts_entries: None,
            turns: None,
            barge_in: None,
            synthetic_caller: None,
        gym: None,
        },
    ];

    let summary = compute_summary(&scenarios);
    // With 2 values [200, 400]: P50 idx = round(0.5 * 1) = 1 -> 400
    assert_eq!(summary.p50_e2e_latency_ms, Some(400));
    assert_eq!(summary.p90_e2e_latency_ms, Some(400));
}

#[test]
fn summary_aggregates_conversation_and_barge_in() {
    fn turn(id: &str, ms: f64, completed: bool) -> TurnResult {
        TurnResult {
            agent_turns_seen: 0,
            turn_id: id.into(),
            utterance_id: None,
            stt_text: None,
            response_text: None,
            e2e_latency_ms: Some(ms),
            completed,
            expectation_failures: Vec::new(),
            error: None,
        }
    }
    let scenarios = vec![
        ScenarioResult {
            scenario_id: "conv".into(),
            scenario_name: "conv".into(),
            session_ids: vec![],
            status: ScenarioRunStatus::Passed,
            metrics: ScenarioMetrics::default(),
            latency_samples: vec![],
            scoring: None,
            error_message: None,
            repetitions_failed: None,
            tts_entries: None,
            turns: Some(vec![turn("t1", 100.0, true), turn("t2", 200.0, true)]),
            barge_in: None,
            synthetic_caller: None,
        gym: None,
        },
        ScenarioResult {
            scenario_id: "barge1".into(),
            scenario_name: "barge1".into(),
            session_ids: vec![],
            status: ScenarioRunStatus::Passed,
            metrics: ScenarioMetrics::default(),
            latency_samples: vec![],
            scoring: None,
            error_message: None,
            repetitions_failed: None,
            tts_entries: None,
            turns: None,
            barge_in: Some(BargeInResult {
                interrupted: true,
                trigger: Some("client_cancel".into()),
                barge_in_rtt_ms: Some(120),
                recovery_ms: Some(300),
                follow_up_completed: true,
            }),
            synthetic_caller: None,
        gym: None,
        },
        ScenarioResult {
            scenario_id: "barge2".into(),
            scenario_name: "barge2".into(),
            session_ids: vec![],
            status: ScenarioRunStatus::Failed,
            metrics: ScenarioMetrics::default(),
            latency_samples: vec![],
            scoring: None,
            error_message: None,
            repetitions_failed: None,
            tts_entries: None,
            turns: None,
            barge_in: Some(BargeInResult {
                interrupted: false,
                trigger: None,
                barge_in_rtt_ms: None,
                recovery_ms: None,
                follow_up_completed: false,
            }),
            synthetic_caller: None,
        gym: None,
        },
    ];

    let summary = compute_summary(&scenarios);
    assert_eq!(summary.conversation_scenarios, Some(1));
    assert_eq!(summary.avg_turns_completed, Some(2.0));
    assert_eq!(summary.avg_turn_latency_ms, Some(150.0));
    assert_eq!(summary.barge_in_scenarios, Some(2));
    assert_eq!(summary.barge_in_success_rate, Some(0.5));
    assert_eq!(summary.p50_barge_in_rtt_ms, Some(120));
    assert_eq!(summary.p50_recovery_ms, Some(300));
}

#[test]
fn score_populates_wer_and_cer_from_golden_transcript() {
    let expected = ScenarioExpected {
        golden_transcript: Some("hello world".into()),
        ..ScenarioExpected::default()
    };
    let metrics = ScenarioMetrics {
        stt_text: Some("hello word".into()),
        ..ScenarioMetrics::default()
    };
    let scored = score(&expected, &metrics);
    assert!(scored.expectations_met);
    assert_eq!(scored.wer, Some(0.5));
    assert!(scored.cer.is_some_and(|c| c > 0.0 && c < 0.2));
}

