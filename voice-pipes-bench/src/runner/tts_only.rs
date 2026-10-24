//! TTS-only corpus runner: drives `ScenarioInput::TtsOnlyCorpus` over one
//! shared session so cold/warm start tracking stays meaningful.

use super::*;

/// Drive a [`ScenarioInput::TtsOnlyCorpus`] against a worker.
///
/// Opens a single WebSocket session, then sends one
/// `tts_only_synthesize` per corpus entry and awaits the matching
/// `tts_only_result` / `tts_only_error`.  Sharing the WS across all
/// entries keeps cold-start tracking meaningful -- the first entry hits
/// the cold path, subsequent entries are warm.
pub(super) async fn run_tts_only_scenario(
    ws_url: &str,
    variant: &Variant,
    scenario: &TestScenario,
    entries: &[TtsCorpusEntry],
    config: &RunnerConfig,
) -> ScenarioResult {
    let mut outcomes = match drive_tts_only_session(ws_url, entries, config).await {
        Ok(v) => v,
        Err(e) => {
            warn!(
                scenario = %scenario.name,
                variant = %variant.name,
                error = %e,
                "tts_only session setup failed"
            );
            return ScenarioResult {
                scenario_id: scenario.id.clone(),
                scenario_name: scenario.name.clone(),
                session_ids: Vec::new(),
                status: ScenarioRunStatus::Error,
                metrics: ScenarioMetrics::default(),
                latency_samples: Vec::new(),
                scoring: None,
                error_message: Some(format!("tts_only session failed: {e:#}")),
                repetitions_failed: Some(entries.len() as u32),
                tts_entries: None,
                turns: None,
                barge_in: None,
                synthetic_caller: None,
                gym: None,
            };
        }
    };

    if let Some(scorer) = config.tts_scorer.as_ref() {
        apply_tts_scoring(scorer.as_ref(), entries, &mut outcomes).await;
    }

    aggregate_tts_only_outcomes(scenario, entries, &outcomes)
}

/// Run the configured scorer against every successful outcome (those
/// with a persisted `audio_ref`). Scoring failures are swallowed and
/// surfaced as `score = None` for that entry -- they do not fail the
/// benchmark since the scorer is auxiliary.
pub(super) async fn apply_tts_scoring(
    scorer: &dyn TtsScorer,
    entries: &[TtsCorpusEntry],
    outcomes: &mut [TtsOnlyEntryOutcome],
) {
    for (entry, outcome) in entries.iter().zip(outcomes.iter_mut()) {
        let Some(audio_ref) = outcome.audio_ref.as_ref() else {
            continue;
        };
        let path = std::path::PathBuf::from(&audio_ref.path);
        match scorer.score(&entry.text, &path).await {
            Ok(score) => outcome.score = Some(score),
            Err(e) => warn!(
                entry_id = %entry.id,
                error = %e,
                "tts scoring failed"
            ),
        }
    }
}

/// Open one WS, replay all corpus entries, return per-entry outcomes.
pub(super) async fn drive_tts_only_session(
    ws_url: &str,
    entries: &[TtsCorpusEntry],
    config: &RunnerConfig,
) -> anyhow::Result<Vec<TtsOnlyEntryOutcome>> {
    let ws_stream = connect_worker(ws_url).await?;

    let (mut sink, mut stream) = ws_stream.split();

    // Configure (minimal handshake).
    let configure_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "configure",
            "audio_format": "pcm_i16_le",
            "sample_rate": 16000,
            "tags": ["benchmark", "tts_only"]
        }
    });
    sink.send(Message::Text(configure_msg.to_string().into()))
        .await?;

    // Wait for SessionReady.
    let mut session_id: Option<String> = None;
    let ready = timeout(Duration::from_secs(10), async {
        while let Some(msg) = stream.next().await {
            if let Ok(Message::Text(text)) = msg
                && let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text)
                && env.msg_type == "control"
                && env.payload.get("action").and_then(|v| v.as_str()) == Some("session_ready")
            {
                session_id = env
                    .payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                return Ok(());
            }
        }
        Err(anyhow::anyhow!("WS closed before SessionReady"))
    })
    .await;
    match ready {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => anyhow::bail!("SessionReady timeout"),
    }

    let mut outcomes = Vec::with_capacity(entries.len());
    for entry in entries {
        let req = serde_json::json!({
            "type": "control",
            "payload": {
                "action": "tts_only_synthesize",
                "id": entry.id,
                "text": entry.text,
                "language": entry.language,
            }
        });
        sink.send(Message::Text(req.to_string().into())).await?;
        let outcome = collect_tts_only_outcome(
            &mut stream,
            &entry.id,
            session_id.clone(),
            config.scenario_timeout,
        )
        .await?;
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

/// Per-entry result from a `tts_only_synthesize` round-trip.
pub(super) struct TtsOnlyEntryOutcome {
    session_id: Option<String>,
    metrics: Option<voice_pipes::transport::TtsOnlyMetrics>,
    /// On-disk WAV pointer used by the WER round-trip scorer to read the
    /// produced audio without re-streaming bytes over the wire.
    audio_ref: Option<voice_pipes::transport::AudioRef>,
    error: Option<String>,
    /// Populated by [`apply_tts_scoring`] when a scorer is configured.
    score: Option<TtsScore>,
}

/// Wait for the `tts_only_result` (or `tts_only_error`) matching `expected_id`.
pub(super) async fn collect_tts_only_outcome<S>(
    stream: &mut S,
    expected_id: &str,
    session_id: Option<String>,
    scenario_timeout: Duration,
) -> anyhow::Result<TtsOnlyEntryOutcome>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = Instant::now() + scenario_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("tts_only_synthesize timeout for id {expected_id}");
        }
        let msg = match timeout(remaining, stream.next()).await {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => return Err(e.into()),
            Ok(None) => anyhow::bail!("WS closed before tts_only_result for id {expected_id}"),
            Err(_) => anyhow::bail!("tts_only_synthesize timeout for id {expected_id}"),
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => {
                anyhow::bail!("WS closed before tts_only_result for id {expected_id}");
            }
            _ => continue,
        };
        let env: JsonEnvelope = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if env.msg_type != "control" {
            continue;
        }
        let action = env
            .payload
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let payload_id = env.payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "tts_only_result" if payload_id == expected_id => {
                let metrics: voice_pipes::transport::TtsOnlyMetrics = serde_json::from_value(
                    env.payload
                        .get("metrics")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                )?;
                let audio_ref: voice_pipes::transport::AudioRef = serde_json::from_value(
                    env.payload
                        .get("audio_ref")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                )?;
                return Ok(TtsOnlyEntryOutcome {
                    session_id,
                    metrics: Some(metrics),
                    audio_ref: Some(audio_ref),
                    error: None,
                    score: None,
                });
            }
            "tts_only_error" if payload_id == expected_id => {
                let error = env
                    .payload
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                return Ok(TtsOnlyEntryOutcome {
                    session_id,
                    metrics: None,
                    audio_ref: None,
                    error: Some(error),
                    score: None,
                });
            }
            _ => {}
        }
    }
}

/// Audio came back, speech did not. Workers that predate the
/// `peak_amplitude` / `speech_ratio` fields report neither, and are never
/// counted as silent.
fn is_silent(m: &voice_pipes::transport::TtsOnlyMetrics) -> bool {
    match (m.speech_ratio, m.peak_amplitude) {
        (Some(ratio), _) if f64::from(ratio) < TTS_SILENT_SPEECH_RATIO => true,
        (_, Some(peak)) if peak < TTS_SILENT_PEAK => true,
        _ => false,
    }
}

/// Reduce per-entry TTS-only outcomes into a [`ScenarioResult`].
pub(super) fn aggregate_tts_only_outcomes(
    scenario: &TestScenario,
    entries: &[TtsCorpusEntry],
    outcomes: &[TtsOnlyEntryOutcome],
) -> ScenarioResult {
    let mut tts_ttfbs = Vec::new();
    let mut tts_synth_all = Vec::new();
    let mut tts_synth_cold = Vec::new();
    let mut tts_synth_warm = Vec::new();
    let mut tts_audio_durs = Vec::new();
    let mut tts_rtfs = Vec::new();
    let mut tts_chars_total = Vec::new();
    let mut tts_chars_per_sec = Vec::new();
    let mut tts_sample_rate_last = None;
    let mut tts_provider_last: Option<String> = None;
    let mut session_ids = Vec::new();
    let mut failures: u32 = 0;
    let mut silent: u32 = 0;
    let mut last_error: Option<String> = None;

    for outcome in outcomes {
        if let Some(ref sid) = outcome.session_id
            && !session_ids.contains(sid)
        {
            session_ids.push(sid.clone());
        }
        match (&outcome.metrics, &outcome.error) {
            (Some(m), _) => {
                let t = m;
                if is_silent(t) {
                    silent += 1;
                }
                if let Some(v) = t.ttfb_ms {
                    tts_ttfbs.push(v as f64);
                }
                if let Some(v) = t.synthesis_ms {
                    tts_synth_all.push(v as f64);
                    if t.cold_start {
                        tts_synth_cold.push(v as f64);
                    } else {
                        tts_synth_warm.push(v as f64);
                    }
                }
                let secs = t.audio_duration_ms.map_or(0.0, |v| v as f64 / 1000.0);
                if secs > 0.0 {
                    tts_audio_durs.push(secs);
                }
                if let Some(rtf) = t.rtf {
                    tts_rtfs.push(f64::from(rtf));
                }
                if let Some(c) = t.chars {
                    tts_chars_total.push(c);
                    if secs > 0.0 {
                        tts_chars_per_sec.push(f64::from(c) / secs);
                    }
                }
                if let Some(sr) = t.sample_rate {
                    tts_sample_rate_last = Some(sr);
                }
                if let Some(ref p) = t.provider {
                    tts_provider_last = Some(p.clone());
                }
            }
            (None, Some(e)) => {
                failures += 1;
                last_error = Some(e.clone());
            }
            (None, None) => {
                failures += 1;
                last_error = Some("missing metrics and error".to_string());
            }
        }
    }

    let avg_chars = if tts_chars_total.is_empty() {
        None
    } else {
        let sum: u64 = tts_chars_total.iter().map(|c| u64::from(*c)).sum();
        Some((sum / tts_chars_total.len() as u64) as u32)
    };

    let metrics = ScenarioMetrics {
        e2e_latency_ms: avg(&tts_synth_all),
        ttft_ms: None,
        ttfb_ms: avg(&tts_ttfbs),
        stt_text: None,
        response_text: None,
        audio_duration_s: avg(&tts_audio_durs),
        stt_latency_ms: None,
        llm_generation_ms: None,
        llm_tokens: None,
        tts_ttfb_ms: avg(&tts_ttfbs),
        tts_synthesis_ms: avg(&tts_synth_all),
        tts_audio_duration_s: avg(&tts_audio_durs),
        tts_sample_rate: tts_sample_rate_last,
        tts_chars: avg_chars,
        tts_rtf: avg(&tts_rtfs),
        tts_chars_per_second: avg(&tts_chars_per_sec),
        tts_provider: tts_provider_last,
        tts_cold_start: !tts_synth_cold.is_empty(),
        tts_error: (failures > 0).then(|| {
            format!(
                "{failures}/{} entries failed: {}",
                entries.len(),
                last_error.as_deref().unwrap_or("unknown"),
            )
        }),
        tts_silent_entries: Some(silent),
    };

    let total = entries.len() as u32;
    // Silence is a failed entry: the provider returned audio, but not
    // speech. It is scored separately from transport/provider errors so
    // the two failure classes stay distinguishable in the results.
    let status = if failures == total && total > 0 {
        ScenarioRunStatus::Error
    } else if failures > 0 || silent > 0 {
        ScenarioRunStatus::Failed
    } else {
        ScenarioRunStatus::Passed
    };
    let error_message = match (failures > 0, silent > 0) {
        (true, true) => Some(format!(
            "{failures}/{total} entries failed ({}); {silent}/{total} silent (no speech energy)",
            last_error.as_deref().unwrap_or("unknown")
        )),
        (true, false) => last_error.map(|e| format!("{failures}/{total} entries failed: {e}")),
        (false, true) => Some(format!(
            "{silent}/{total} entries silent: the provider returned audio with no speech energy"
        )),
        (false, false) => None,
    };

    // Build per-entry results so the UI can render the table + play
    // each clip individually.
    let entry_results: Vec<TtsEntryResult> = entries
        .iter()
        .zip(outcomes.iter())
        .map(|(entry, outcome)| {
            let metrics = outcome.metrics.as_ref();
            let audio_path = outcome.audio_ref.as_ref().map(|a| a.path.clone());
            let audio_available = audio_path
                .as_deref()
                .is_some_and(|p| std::path::Path::new(p).is_file());
            TtsEntryResult {
                entry_id: entry.id.clone(),
                text: entry.text.clone(),
                bucket: entry.bucket,
                class: entry.class,
                ttfb_ms: metrics.and_then(|m| m.ttfb_ms),
                synthesis_ms: metrics.and_then(|m| m.synthesis_ms),
                audio_duration_ms: metrics.and_then(|m| m.audio_duration_ms),
                sample_rate: metrics.and_then(|m| m.sample_rate),
                chars: metrics.and_then(|m| m.chars),
                rtf: metrics.and_then(|m| m.rtf.map(f64::from)),
                cold_start: metrics.is_some_and(|m| m.cold_start),
                provider: metrics.and_then(|m| m.provider.clone()),
                audio_path,
                audio_available,
                error: outcome.error.clone(),
                peak_amplitude: metrics.and_then(|m| m.peak_amplitude),
                speech_ratio: metrics.and_then(|m| m.speech_ratio.map(f64::from)),
                silent: metrics.is_some_and(is_silent),
                transcript: outcome.score.as_ref().map(|s| s.transcript.clone()),
                wer: outcome.score.as_ref().map(|s| s.wer),
                cer: outcome.score.as_ref().map(|s| s.cer),
            }
        })
        .collect();

    // Fold WER/CER from the round-trip scorer (when configured) into
    // the single ScenarioScoring slot. We average across all scored
    // entries; `expectations_met` is true iff at least one entry was
    // scored and no scoring slot is missing for a successful entry.
    let wers: Vec<f64> = outcomes
        .iter()
        .filter_map(|o| o.score.as_ref().map(|s| s.wer))
        .collect();
    let cers: Vec<f64> = outcomes
        .iter()
        .filter_map(|o| o.score.as_ref().map(|s| s.cer))
        .collect();
    let scoring = if wers.is_empty() {
        None
    } else {
        let avg_wer = avg(&wers).unwrap_or(0.0);
        Some(ScenarioScoring {
            wer: Some(avg_wer),
            cer: avg(&cers),
            quality_score: Some((1.0 - avg_wer).clamp(0.0, 1.0)),
            quality_reasoning: None,
            expectations_met: failures == 0,
            expectation_failures: Vec::new(),
        })
    };

    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids,
        status,
        metrics,
        latency_samples: tts_ttfbs.clone(),
        scoring,
        error_message,
        repetitions_failed: (failures > 0).then_some(failures),
        tts_entries: Some(entry_results),
        turns: None,
        barge_in: None,
        synthetic_caller: None,
        gym: None,
    }
}

#[cfg(test)]
#[path = "tests/tts_only.rs"]
mod tests;
