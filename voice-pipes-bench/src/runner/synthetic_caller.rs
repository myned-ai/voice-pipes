//! Synthetic-caller scenario runner: bridges a `SyntheticCaller` to a live
//! pipeline WebSocket and evaluates stop conditions per turn.

use super::*;

/// Drive a [`ScenarioInput::SyntheticCaller`] scenario.
///
/// Bridges a [`SyntheticCaller`](crate::synthetic_caller::SyntheticCaller)
/// to an open pipeline WebSocket: caller transcripts become pipeline
/// `text` turns, pipeline transcripts are forwarded back to the caller
/// (audio bridging is the per-provider responsibility -- for the
/// in-tree `MockCaller` only the transcript path is exercised).
///
/// Stop conditions are evaluated after every pipeline turn.
pub(super) async fn run_synthetic_caller_scenario(
    ws_url: &str,
    experiment_id: &str,
    scenario: &TestScenario,
    caller_cfg: &SyntheticCallerConfig,
    config: &RunnerConfig,
) -> ScenarioResult {
    use crate::synthetic_caller::{SyntheticCaller, mock::MockCaller};

    // Resolve the caller backend.
    let caller_box: Box<dyn SyntheticCaller> = match &caller_cfg.caller {
        CallerKind::Mock { script } => match MockCaller::from_file(script) {
            Ok(c) => Box::new(c),
            Err(e) => {
                return synthetic_caller_error(
                    scenario,
                    format!("failed to load mock caller script: {e}"),
                );
            }
        },
        #[cfg(feature = "synthetic-caller")]
        CallerKind::OpenaiRealtime { model, voice } => {
            use crate::synthetic_caller::openai_realtime::OpenAiRealtimeCallerBuilder;
            let builder = OpenAiRealtimeCallerBuilder::default()
                .model(model.clone())
                .voice(voice.clone());
            match builder.connect().await {
                Ok(c) => Box::new(c),
                Err(e) => {
                    return synthetic_caller_error(
                        scenario,
                        format!("OpenAI Realtime caller connect failed: {e}"),
                    );
                }
            }
        }
        #[cfg(not(feature = "synthetic-caller"))]
        CallerKind::OpenaiRealtime { .. } => {
            return synthetic_caller_error(
                scenario,
                "OpenaiRealtime caller requires the `synthetic-caller` cargo feature".to_string(),
            );
        }
        #[cfg(feature = "synthetic-caller")]
        CallerKind::GeminiLive { model } => {
            use crate::synthetic_caller::gemini_live::GeminiLiveCallerBuilder;
            let builder = GeminiLiveCallerBuilder::default().model(model.clone());
            match builder.connect().await {
                Ok(c) => Box::new(c),
                Err(e) => {
                    return synthetic_caller_error(
                        scenario,
                        format!("Gemini Live caller connect failed: {e}"),
                    );
                }
            }
        }
        #[cfg(not(feature = "synthetic-caller"))]
        CallerKind::GeminiLive { .. } => {
            return synthetic_caller_error(
                scenario,
                "GeminiLive caller requires the `synthetic-caller` cargo feature".to_string(),
            );
        }
    };

    // Resolve persona_template (if any) by concatenating the looked-up
    // template body with the scenario's free-form persona text. The
    // template comes first so persona overrides can sit at the bottom.
    let persona_resolved: String = match caller_cfg.persona_template.as_deref() {
        Some(name) => match crate::synthetic_caller::prompts::lookup(name) {
            Some(body) => {
                if caller_cfg.persona.trim().is_empty() {
                    body.to_string()
                } else {
                    format!("{body}\n\n{}", caller_cfg.persona)
                }
            }
            None => {
                return synthetic_caller_error(
                    scenario,
                    format!(
                        "unknown persona_template `{name}` (known: {})",
                        crate::synthetic_caller::prompts::TEMPLATE_NAMES.join(", ")
                    ),
                );
            }
        },
        None => caller_cfg.persona.clone(),
    };

    let outcome = run_synthetic_caller_session(
        ws_url,
        experiment_id,
        &scenario.id,
        caller_box,
        &persona_resolved,
        &caller_cfg.stop,
        caller_cfg.barge_in.as_ref(),
        caller_cfg.bridge,
        config,
    )
    .await;

    let mut session_ids = Vec::new();
    let mut last_error: Option<String> = None;
    let synth_result = match outcome {
        Ok(out) => {
            if let Some(sid) = out.session_id {
                session_ids.push(sid);
            }
            out.result
        }
        Err(e) => {
            last_error = Some(format!("synthetic_caller session failed: {e}"));
            SyntheticCallerResult::default()
        }
    };

    let mut expectation_failures: Vec<String> = Vec::new();
    if let Some(exp) = &scenario.expected {
        score_synthetic_caller(exp, &synth_result, &mut expectation_failures);
    }

    match caller_cfg.bridge {
        BridgeMode::Text => {
            let pipeline_turns: Vec<&SyntheticTurn> = synth_result
                .turns
                .iter()
                .filter(|t| t.speaker == "pipeline")
                .collect();
            if !pipeline_turns.is_empty() {
                let has_pipeline_text = pipeline_turns.iter().any(|t| {
                    t.transcript
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(|s| !s.is_empty())
                });
                if !has_pipeline_text {
                    expectation_failures.push(
                        "pipeline produced turn(s) but no text transcript in text bridge; inspect session captures / turn metrics"
                            .to_string(),
                    );
                }
            }
        }
        BridgeMode::Audio => {}
    }

    let status = if last_error.is_some() {
        ScenarioRunStatus::Error
    } else if expectation_failures.is_empty() {
        ScenarioRunStatus::Passed
    } else {
        ScenarioRunStatus::Failed
    };

    let metrics = aggregate_synthetic_metrics(&synth_result);

    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids,
        status,
        metrics,
        latency_samples: Vec::new(),
        scoring: Some(ScenarioScoring {
            wer: None,
            cer: None,
            quality_score: None,
            quality_reasoning: None,
            expectations_met: expectation_failures.is_empty(),
            expectation_failures,
        }),
        error_message: last_error,
        repetitions_failed: None,
        tts_entries: None,
        turns: None,
        barge_in: None,
        synthetic_caller: Some(synth_result),
        gym: None,
    }
}

/// Aggregate per-turn synthetic-caller data into a `ScenarioMetrics` so
/// the dashboard's metric columns (E2E / TTFT / TTFB / STT Text /
/// Response) are populated for synthetic-caller scenarios.
pub(super) fn aggregate_synthetic_metrics(result: &SyntheticCallerResult) -> ScenarioMetrics {
    let mut stt_parts: Vec<String> = Vec::new();
    let mut resp_parts: Vec<String> = Vec::new();
    let mut e2e: Vec<f64> = Vec::new();
    let mut ttft: Vec<f64> = Vec::new();
    let mut ttfb: Vec<f64> = Vec::new();

    for turn in &result.turns {
        let text = turn.transcript.as_deref().unwrap_or("").trim();
        match turn.speaker.as_str() {
            "caller" => {
                if !text.is_empty() {
                    stt_parts.push(text.to_string());
                }
            }
            "pipeline" => {
                if !text.is_empty() {
                    resp_parts.push(text.to_string());
                }
                if let Some(v) = turn.latency_ms {
                    e2e.push(v);
                }
                if let Some(v) = turn.ttft_ms {
                    ttft.push(v);
                }
                if let Some(v) = turn.ttfb_ms {
                    ttfb.push(v);
                }
            }
            _ => {}
        }
    }

    fn avg(xs: &[f64]) -> Option<f64> {
        if xs.is_empty() {
            None
        } else {
            Some(xs.iter().sum::<f64>() / xs.len() as f64)
        }
    }

    ScenarioMetrics {
        e2e_latency_ms: avg(&e2e),
        ttft_ms: avg(&ttft),
        ttfb_ms: avg(&ttfb),
        stt_text: if stt_parts.is_empty() {
            None
        } else {
            Some(stt_parts.join(" | "))
        },
        response_text: if resp_parts.is_empty() {
            None
        } else {
            Some(resp_parts.join(" | "))
        },
        ..ScenarioMetrics::default()
    }
}

pub(super) fn synthetic_caller_error(scenario: &TestScenario, reason: String) -> ScenarioResult {
    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids: Vec::new(),
        status: ScenarioRunStatus::Error,
        metrics: ScenarioMetrics::default(),
        latency_samples: Vec::new(),
        scoring: None,
        error_message: Some(reason),
        repetitions_failed: None,
        tts_entries: None,
        turns: None,
        barge_in: None,
        synthetic_caller: None,
        gym: None,
    }
}

pub(super) struct SyntheticCallerSessionOutcome {
    session_id: Option<String>,
    result: SyntheticCallerResult,
}

pub(super) async fn run_synthetic_caller_session(
    ws_url: &str,
    _experiment_id: &str,
    scenario_id: &str,
    mut caller: Box<dyn crate::synthetic_caller::SyntheticCaller>,
    persona: &str,
    stop: &SyntheticStopConditions,
    barge_in_policy: Option<&BargeInPolicy>,
    bridge: BridgeMode,
    config: &RunnerConfig,
) -> anyhow::Result<SyntheticCallerSessionOutcome> {
    use crate::synthetic_caller::CallerEvent;
    use voice_pipes::transport::wire::BinaryHeader;

    let session_start = Instant::now();
    let max_duration = Duration::from_secs(stop.max_duration_s);
    let caller_silence_cap = stop.max_caller_silence_s.map(Duration::from_secs);

    let ws_stream = connect_worker(ws_url).await?;
    let (mut sink, mut stream) = ws_stream.split();

    // Configure handshake.
    //
    // We pin `output_sample_rate: 16000` to match the synthetic caller's
    // expected input rate (e.g. OpenAI Realtime caller resamples 16k→24k
    // internally in `encode_pipeline_audio_for_realtime`). Without this,
    // the pipeline emits TTS at the provider's native rate (Piper=22050,
    // Kokoro=24000, etc.), which the caller misinterprets as 16k → wrong
    // pitch/duration → upstream VAD does not register the audio as
    // speech → no commit/response → caller falls silent after turn 1.
    let configure_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "configure",
            "audio_format": "pcm_i16_le",
            "sample_rate": 16000,
            "output_sample_rate": 16000,
            "tags": ["benchmark", "synthetic-caller"]
        }
    });
    sink.send(Message::Text(configure_msg.to_string().into()))
        .await?;

    let mut session_id: Option<String> = None;
    let ready = timeout(Duration::from_secs(10), async {
        while let Some(msg) = stream.next().await {
            if let Ok(Message::Text(text)) = msg
                && let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text)
                && env.msg_type == "control"
                && let Some(action) = env.payload.get("action").and_then(|v| v.as_str())
                && action == "session_ready"
            {
                session_id = env
                    .payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
                return Ok::<(), anyhow::Error>(());
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

    caller.configure(persona).await?;

    let mut result = SyntheticCallerResult::default();
    let mut total_turns: u32 = 0;
    let mut pipeline_turn_index: u32 = 0;
    let mut consecutive_barge_failures: u32 = 0;

    loop {
        // Stop conditions evaluated up-front.
        //
        // `max_turns` is interpreted as the cap on *completed pipeline
        // replies* (i.e. conversational exchanges where the caller
        // spoke and the pipeline answered). This matches the
        // common-sense reading of "turn" and what scenario authors
        // expect when they write `max_turns: 4`: four user-bot
        // exchanges, not 2 (which is what counting caller+pipeline
        // entries separately would produce).
        if pipeline_turn_index >= stop.max_turns {
            result.stop_reason = Some(SyntheticStopReason::MaxTurns);
            break;
        }
        if session_start.elapsed() >= max_duration {
            result.stop_reason = Some(SyntheticStopReason::MaxDuration);
            break;
        }

        // ── Pull next caller utterance ─────────────────────
        // In Text mode we only need TranscriptDelta; in Audio mode we
        // also accumulate AudioDelta and ship it as binary frames.
        let mut caller_text = String::new();
        let mut caller_audio: Vec<i16> = Vec::new();
        let caller_idle_at = Instant::now();
        loop {
            // Respect the silence cap while waiting for the caller.
            if let Some(cap) = caller_silence_cap
                && caller_idle_at.elapsed() >= cap
            {
                result.stop_reason = Some(SyntheticStopReason::CallerSilence);
                return Ok(finish_session(sink, session_id, result));
            }
            // Wrap next_event() in a timeout so a dangling caller
            // (e.g. WS closed without sending anything) doesn't pin
            // this task forever and starve the silence-cap check above.
            // We poll at min(remaining cap, 1s) so the cap stays
            // accurate even when no events arrive.
            let poll_budget = match caller_silence_cap {
                Some(cap) => cap
                    .saturating_sub(caller_idle_at.elapsed())
                    .min(Duration::from_secs(1)),
                None => Duration::from_secs(1),
            };
            // Timeout -> re-evaluate stop conditions.
            let Ok(evt) = tokio::time::timeout(poll_budget, caller.next_event()).await else {
                continue;
            };
            match evt {
                Some(CallerEvent::TranscriptDelta(t)) => caller_text.push_str(&t),
                Some(CallerEvent::AudioDelta(s)) => {
                    if bridge == BridgeMode::Audio {
                        caller_audio.extend_from_slice(&s);
                    }
                }
                Some(CallerEvent::ResponseDone) => {
                    break;
                }
                Some(CallerEvent::Error(_)) => {
                    result.stop_reason = Some(SyntheticStopReason::CallerError);
                    return Ok(finish_session(
                        sink,
                        session_id,
                        SyntheticCallerResult {
                            stop_reason: Some(SyntheticStopReason::CallerError),
                            ..result
                        },
                    ));
                }
                None => {
                    // Caller exhausted -- treat as max_turns reached.
                    result.stop_reason = Some(SyntheticStopReason::MaxTurns);
                    return Ok(finish_session(sink, session_id, result));
                }
            }
        }
        let nothing_to_send = match bridge {
            BridgeMode::Text => caller_text.trim().is_empty(),
            BridgeMode::Audio => caller_audio.is_empty(),
        };
        if nothing_to_send {
            // Skip empty utterances but still count toward stop budget.
            total_turns = total_turns.saturating_add(1);
            continue;
        }

        result.turns.push(SyntheticTurn {
            speaker: "caller".into(),
            turn_index: total_turns,
            transcript: if caller_text.is_empty() {
                None
            } else {
                Some(caller_text.clone())
            },
            latency_ms: None,
            ttft_ms: None,
            ttfb_ms: None,
            barge_in: false,
        });
        total_turns = total_turns.saturating_add(1);

        // ── Send to pipeline ──────────────────────────────
        let turn_started = Instant::now();
        let pipeline_outcome = match bridge {
            BridgeMode::Text => {
                let text_msg = serde_json::json!({
                    "type": "message",
                    "payload": {
                        "kind": "text",
                        "text": caller_text,
                        "role": "user",
                        "message_id": format!("bench-{scenario_id}-synth-{total_turns}")
                    }
                });
                sink.send(Message::Text(text_msg.to_string().into()))
                    .await?;
                collect_outcome(
                    &mut stream,
                    session_id.clone(),
                    config.scenario_timeout,
                    config.settle_timeout,
                    turn_started,
                    true,
                )
                .await
            }
            BridgeMode::Audio => {
                // Stream the caller audio as PCM binary frames. We
                // chunk at 20 ms (320 samples @ 16 kHz mono i16) and
                // raise FLAG_END_OF_UTTERANCE on the final frame so
                // the pipeline's VAD/turn-taker closes the segment.
                const SAMPLES_PER_FRAME: usize = 320;
                let total_frames = caller_audio.len().div_ceil(SAMPLES_PER_FRAME).max(1);
                for (idx, chunk) in caller_audio.chunks(SAMPLES_PER_FRAME).enumerate() {
                    let is_last = idx + 1 == total_frames;
                    let header = BinaryHeader::audio_pcm(idx as u32, is_last, false);
                    let mut payload = Vec::with_capacity(chunk.len() * 2);
                    for s in chunk {
                        payload.extend_from_slice(&s.to_le_bytes());
                    }
                    let frame = header.encode_with_payload(&payload);
                    sink.send(Message::Binary(frame.into())).await?;
                }
                collect_outcome_audio_bridge(
                    &mut stream,
                    caller.as_mut(),
                    session_id.clone(),
                    config.scenario_timeout,
                    config.settle_timeout,
                    turn_started,
                )
                .await
            }
        };
        let outcome = match pipeline_outcome {
            Ok(o) => o,
            Err(e) => {
                result.stop_reason = Some(SyntheticStopReason::PipelineError);
                result.turns.push(SyntheticTurn {
                    speaker: "pipeline".into(),
                    turn_index: total_turns,
                    transcript: Some(format!("<error: {e}>")),
                    latency_ms: None,
                    ttft_ms: None,
                    ttfb_ms: None,
                    barge_in: false,
                });
                return Ok(finish_session(sink, session_id, result));
            }
        };
        pipeline_turn_index = pipeline_turn_index.saturating_add(1);
        let pipeline_text = outcome.response_text.clone();
        let pipeline_latency = outcome.e2e_latency_ms;

        // In Text mode, forward the pipeline transcript back so live
        // S2S backends can keep the conversation going. In Audio mode
        // the pipeline TTS audio has already been forwarded inside
        // `collect_outcome_audio_bridge`, so we skip the text echo to
        // avoid double-prompting the caller -- the caller's upstream
        // VAD (e.g. OpenAI Realtime server_vad) will auto-commit and
        // auto-generate a response when it detects end-of-speech in
        // the pushed audio.
        if bridge == BridgeMode::Text
            && let Some(ref t) = pipeline_text
        {
            let _ = caller.push_text(t).await;
        }

        result.turns.push(SyntheticTurn {
            speaker: "pipeline".into(),
            turn_index: total_turns,
            transcript: pipeline_text,
            latency_ms: pipeline_latency,
            ttft_ms: outcome.ttft_ms,
            ttfb_ms: outcome.ttfb_ms,
            barge_in: false,
        });
        total_turns = total_turns.saturating_add(1);

        // ── Optional forced barge-in policy ───────────────
        if let Some(policy) = barge_in_policy
            && policy.every_n_turns > 0
            && pipeline_turn_index.is_multiple_of(policy.every_n_turns)
        {
            result.barge_in_attempts = result.barge_in_attempts.saturating_add(1);
            let cancel_msg = serde_json::json!({
                "type": "control",
                "payload": { "action": "cancel" }
            });
            let cancel_send = sink
                .send(Message::Text(cancel_msg.to_string().into()))
                .await;
            let cancelled_caller = caller.cancel().await;
            let succeeded = cancel_send.is_ok() && cancelled_caller.is_ok();
            if succeeded {
                result.barge_in_successes = result.barge_in_successes.saturating_add(1);
                consecutive_barge_failures = 0;
                if let Some(last) = result.turns.last_mut() {
                    last.barge_in = true;
                }
            } else {
                consecutive_barge_failures = consecutive_barge_failures.saturating_add(1);
                if policy.max_consecutive_failures > 0
                    && consecutive_barge_failures >= policy.max_consecutive_failures
                {
                    result.stop_reason = Some(SyntheticStopReason::BargeInFailureCap);
                    return Ok(finish_session(sink, session_id, result));
                }
            }
        }
    }

    result.total_duration_ms = Some(session_start.elapsed().as_millis() as u64);
    Ok(finish_session(sink, session_id, result))
}

pub(super) fn finish_session(
    sink: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    session_id: Option<String>,
    result: SyntheticCallerResult,
) -> SyntheticCallerSessionOutcome {
    // Drop the sink without an explicit Close frame; the runtime will
    // tear the WS down when the stream is dropped on the caller's side.
    drop(sink);
    SyntheticCallerSessionOutcome { session_id, result }
}

/// Apply scenario-level synthetic-caller assertions.
pub(super) fn score_synthetic_caller(
    expected: &ScenarioExpected,
    result: &SyntheticCallerResult,
    failures: &mut Vec<String>,
) {
    let pipeline_turns: Vec<&SyntheticTurn> = result
        .turns
        .iter()
        .filter(|t| t.speaker == "pipeline")
        .collect();

    if let Some(min) = expected.min_completed_turns {
        let got = pipeline_turns.len() as u32;
        if got < min {
            failures.push(format!("min_completed_turns: got {got}, want >= {min}"));
        }
    }
    for needle in &expected.pipeline_must_say {
        let needle_lc = needle.to_lowercase();
        let hit = pipeline_turns.iter().any(|t| {
            t.transcript
                .as_deref()
                .is_some_and(|s| s.to_lowercase().contains(&needle_lc))
        });
        if !hit {
            failures.push(format!(
                "pipeline_must_say: no pipeline turn contained {needle:?}"
            ));
        }
    }
    if let Some(cap) = expected.max_pipeline_turn_latency_ms {
        for t in &pipeline_turns {
            if let Some(ms) = t.latency_ms
                && ms > cap as f64
            {
                failures.push(format!(
                    "pipeline turn {}: latency {ms:.0}ms exceeds cap {cap}ms",
                    t.turn_index
                ));
            }
        }
    }
    if let Some(cap) = expected.max_barge_in_failure_rate
        && result.barge_in_attempts > 0
    {
        let failed = result
            .barge_in_attempts
            .saturating_sub(result.barge_in_successes) as f64;
        let rate = failed / result.barge_in_attempts as f64;
        if rate > cap {
            failures.push(format!(
                "max_barge_in_failure_rate: got {rate:.2}, cap {cap:.2}"
            ));
        }
    }
}

#[cfg(test)]
#[path = "tests/synthetic_caller.rs"]
mod tests;
