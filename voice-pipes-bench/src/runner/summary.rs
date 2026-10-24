//! Scenario scoring and variant-summary aggregation.

use super::*;

// ── Scoring ─────────────────────────────────────────────────

pub(super) fn score(expected: &ScenarioExpected, metrics: &ScenarioMetrics) -> ScenarioScoring {
    let mut failures = Vec::new();

    // ── transcript_contains ─────────────────────────────────
    if let Some(ref stt) = metrics.stt_text {
        let lower = stt.to_lowercase();
        for needle in &expected.transcript_contains {
            if !lower.contains(&needle.to_lowercase()) {
                failures.push(format!(
                    "transcript missing expected substring: \"{needle}\""
                ));
            }
        }
    } else if !expected.transcript_contains.is_empty() {
        failures.push("no STT transcript captured".to_string());
    }

    // ── max_latency_ms ──────────────────────────────────────
    if let (Some(max), Some(actual)) = (expected.max_latency_ms, metrics.e2e_latency_ms)
        && actual > max as f64
    {
        failures.push(format!("e2e latency {actual:.0}ms exceeds max {max}ms"));
    }

    // ── WER / CER (word & character error rate) ─────────────
    let (wer, cer) = match (&expected.golden_transcript, &metrics.stt_text) {
        (Some(golden), Some(hyp)) => (
            Some(crate::scoring::compute_wer(golden, hyp)),
            Some(crate::scoring::compute_cer(golden, hyp)),
        ),
        _ => (None, None),
    };

    ScenarioScoring {
        wer,
        cer,
        quality_score: None,
        quality_reasoning: None,
        expectations_met: failures.is_empty(),
        expectation_failures: failures,
    }
}

// ── Aggregation helpers ─────────────────────────────────────

pub(super) fn avg(vals: &[f64]) -> Option<f64> {
    if vals.is_empty() {
        None
    } else {
        Some(vals.iter().sum::<f64>() / vals.len() as f64)
    }
}

pub(super) fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    Some(sorted[idx.min(sorted.len() - 1)])
}

pub(super) fn compute_summary(scenarios: &[ScenarioResult]) -> VariantSummary {
    let run = scenarios.len() as u32;
    let passed = scenarios
        .iter()
        .filter(|s| s.status == ScenarioRunStatus::Passed)
        .count() as u32;
    let failed = run - passed;

    // Use per-repetition samples when available; fall back to scenario averages.
    let latencies: Vec<f64> = {
        let samples: Vec<f64> = scenarios
            .iter()
            .flat_map(|s| s.latency_samples.iter().copied())
            .collect();
        if samples.is_empty() {
            scenarios
                .iter()
                .filter_map(|s| s.metrics.e2e_latency_ms)
                .collect()
        } else {
            samples
        }
    };
    let ttfts: Vec<f64> = scenarios.iter().filter_map(|s| s.metrics.ttft_ms).collect();
    let ttfbs: Vec<f64> = scenarios.iter().filter_map(|s| s.metrics.ttfb_ms).collect();

    let mut lat_sorted: Vec<u64> = latencies.iter().map(|v| *v as u64).collect();
    lat_sorted.sort_unstable();

    let wers: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.scoring.as_ref().and_then(|sc| sc.wer))
        .collect();
    let quality_scores: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.scoring.as_ref().and_then(|sc| sc.quality_score))
        .collect();

    // Per-component metrics.
    let stt_lats: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.metrics.stt_latency_ms)
        .collect();
    let llm_gens: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.metrics.llm_generation_ms)
        .collect();
    let tts_ttfbs: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.metrics.tts_ttfb_ms)
        .collect();
    let tts_synths: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.metrics.tts_synthesis_ms)
        .collect();
    let tts_audio_durs: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.metrics.tts_audio_duration_s)
        .collect();
    let tts_rtfs: Vec<f64> = scenarios.iter().filter_map(|s| s.metrics.tts_rtf).collect();
    let tts_cps: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| s.metrics.tts_chars_per_second)
        .collect();

    // TTS TTFB percentiles (separate sorted view).
    let mut tts_ttfb_sorted: Vec<u64> = tts_ttfbs.iter().map(|v| *v as u64).collect();
    tts_ttfb_sorted.sort_unstable();

    // TTS failure rate: scenarios that recorded a tts_error / total scenarios.
    let tts_fail_count = scenarios
        .iter()
        .filter(|s| s.metrics.tts_error.is_some())
        .count();
    let tts_failure_rate = if scenarios.is_empty() {
        None
    } else {
        Some(tts_fail_count as f64 / scenarios.len() as f64)
    };

    // Cold vs warm split is per-rep granularity, but we only have scenario-
    // level aggregates here.  Approximate: scenarios with `tts_cold_start =
    // true` had at least one cold rep contribute to their `tts_synthesis_ms`,
    // so use those as cold means and the rest as warm means.
    let cold_scenarios: Vec<f64> = scenarios
        .iter()
        .filter(|s| s.metrics.tts_cold_start)
        .filter_map(|s| s.metrics.tts_synthesis_ms)
        .collect();
    let warm_scenarios: Vec<f64> = scenarios
        .iter()
        .filter(|s| !s.metrics.tts_cold_start)
        .filter_map(|s| s.metrics.tts_synthesis_ms)
        .collect();

    // tokens_per_second: for each scenario with both llm_tokens and llm_generation_ms,
    // compute tok/s and average across scenarios.
    let tps_values: Vec<f64> = scenarios
        .iter()
        .filter_map(|s| {
            let tokens = s.metrics.llm_tokens? as f64;
            let gen_ms = s.metrics.llm_generation_ms?;
            (gen_ms > 0.0).then(|| tokens / (gen_ms / 1000.0))
        })
        .collect();

    // ── Multi-turn / barge-in aggregates ──
    let conv_scenarios: Vec<&ScenarioResult> =
        scenarios.iter().filter(|s| s.turns.is_some()).collect();
    let conversation_scenarios = if conv_scenarios.is_empty() {
        None
    } else {
        Some(conv_scenarios.len() as u32)
    };
    let avg_turns_completed = if conv_scenarios.is_empty() {
        None
    } else {
        let counts: Vec<f64> = conv_scenarios
            .iter()
            .map(|s| {
                s.turns
                    .as_ref()
                    .map_or(0.0, |ts| ts.iter().filter(|t| t.completed).count() as f64)
            })
            .collect();
        avg(&counts)
    };
    let turn_latencies: Vec<f64> = conv_scenarios
        .iter()
        .flat_map(|s| s.turns.as_deref().unwrap_or(&[]))
        .filter_map(|t| t.e2e_latency_ms)
        .collect();
    let avg_turn_latency_ms = avg(&turn_latencies);

    let barge_results: Vec<&BargeInResult> = scenarios
        .iter()
        .filter_map(|s| s.barge_in.as_ref())
        .collect();
    let barge_in_scenarios = if barge_results.is_empty() {
        None
    } else {
        Some(barge_results.len() as u32)
    };
    let barge_in_success_rate = if barge_results.is_empty() {
        None
    } else {
        let ok = barge_results.iter().filter(|b| b.interrupted).count();
        Some(ok as f64 / barge_results.len() as f64)
    };
    let mut barge_rtts: Vec<u64> = barge_results
        .iter()
        .filter_map(|b| b.barge_in_rtt_ms)
        .collect();
    barge_rtts.sort_unstable();
    let mut barge_recovs: Vec<u64> = barge_results.iter().filter_map(|b| b.recovery_ms).collect();
    barge_recovs.sort_unstable();

    VariantSummary {
        scenarios_run: run,
        scenarios_passed: passed,
        scenarios_failed: failed,
        avg_e2e_latency_ms: avg(&latencies),
        p50_e2e_latency_ms: percentile(&lat_sorted, 50.0),
        p90_e2e_latency_ms: percentile(&lat_sorted, 90.0),
        p99_e2e_latency_ms: percentile(&lat_sorted, 99.0),
        avg_ttft_ms: avg(&ttfts),
        avg_ttfb_ms: avg(&ttfbs),
        avg_wer: avg(&wers),
        avg_quality_score: avg(&quality_scores),
        total_cost_estimate: None,
        avg_stt_latency_ms: avg(&stt_lats),
        avg_llm_generation_ms: avg(&llm_gens),
        avg_tts_ttfb_ms: avg(&tts_ttfbs),
        p50_tts_ttfb_ms: percentile(&tts_ttfb_sorted, 50.0),
        p90_tts_ttfb_ms: percentile(&tts_ttfb_sorted, 90.0),
        p99_tts_ttfb_ms: percentile(&tts_ttfb_sorted, 99.0),
        avg_tts_synthesis_ms: avg(&tts_synths),
        avg_tts_audio_duration_s: avg(&tts_audio_durs),
        avg_tts_rtf: avg(&tts_rtfs),
        avg_tts_chars_per_second: avg(&tts_cps),
        tts_failure_rate,
        cold_tts_synthesis_ms: avg(&cold_scenarios),
        warm_tts_synthesis_ms: avg(&warm_scenarios),
        avg_tokens_per_second: avg(&tps_values),
        conversation_scenarios,
        avg_turns_completed,
        avg_turn_latency_ms,
        barge_in_scenarios,
        barge_in_success_rate,
        p50_barge_in_rtt_ms: percentile(&barge_rtts, 50.0),
        p95_barge_in_rtt_ms: percentile(&barge_rtts, 95.0),
        p50_recovery_ms: percentile(&barge_recovs, 50.0),
        p95_recovery_ms: percentile(&barge_recovs, 95.0),
    }
}

/// Produce an empty variant result when no worker is available.
pub(super) fn empty_variant_result(
    variant: &Variant,
    suite: &TestSuite,
    reason: &str,
) -> VariantResults {
    let scenarios = suite
        .scenarios
        .iter()
        .map(|s| ScenarioResult {
            scenario_id: s.id.clone(),
            scenario_name: s.name.clone(),
            session_ids: Vec::new(),
            status: ScenarioRunStatus::Error,
            metrics: ScenarioMetrics::default(),
            latency_samples: Vec::new(),
            scoring: None,
            error_message: Some(reason.to_string()),
            repetitions_failed: None,
            tts_entries: None,
            turns: None,
            barge_in: None,
            synthetic_caller: None,
            gym: None,
        })
        .collect::<Vec<_>>();

    let summary = compute_summary(&scenarios);
    VariantResults {
        variant_id: variant.id.clone(),
        variant_name: variant.name.clone(),
        scenarios,
        summary,
        config_summary: None,
        error_reason: Some(reason.to_string()),
    }
}

#[cfg(test)]
#[path = "tests/summary.rs"]
mod tests;
