//! Conversation-scenario runner: drives `ScenarioInput::Conversation` over a
//! single WebSocket session so server-side turn history is preserved.

use super::*;

/// Drive a `ScenarioInput::Conversation` against one worker. Opens a
/// single WebSocket session so server-side conversation history is
/// preserved across turns. Each turn is dispatched, then the runner
/// waits for that turn's `turn_metrics` + `state_changed=listening`
/// before advancing.
pub(super) async fn run_conversation_scenario(
    ws_url: &str,
    experiment_id: &str,
    scenario: &TestScenario,
    turns: &[ConversationTurn],
    config: &RunnerConfig,
) -> ScenarioResult {
    let mut session_ids: Vec<String> = Vec::new();
    let mut turn_results: Vec<TurnResult> = Vec::new();
    let mut latency_samples: Vec<f64> = Vec::new();
    let mut last_error: Option<String> = None;

    let outcome =
        run_conversation_session(ws_url, experiment_id, &scenario.id, turns, config).await;

    match outcome {
        Ok(session) => {
            if let Some(sid) = session.session_id.clone() {
                session_ids.push(sid);
            }
            for turn in &session.turns {
                if let Some(ms) = turn.e2e_latency_ms {
                    latency_samples.push(ms);
                }
            }
            turn_results = session.turns;
        }
        Err(e) => {
            last_error = Some(format!("conversation session failed: {e}"));
        }
    }

    // Apply per-turn assertions.
    for (turn, result) in turns.iter().zip(turn_results.iter_mut()) {
        if let Some(exp) = &turn.expected {
            apply_turn_assertions(turn, exp, result);
        }
    }

    // Apply scenario-level assertions and compute pass/fail.
    let mut expectation_failures: Vec<String> = Vec::new();
    if let Some(exp) = &scenario.expected {
        score_conversation(exp, &turn_results, &mut expectation_failures);
    }
    for r in &turn_results {
        for f in &r.expectation_failures {
            expectation_failures.push(format!("turn {}: {f}", r.turn_id));
        }
        if let Some(err) = &r.error {
            expectation_failures.push(format!("turn {}: {err}", r.turn_id));
        }
    }

    let status = if last_error.is_some() {
        ScenarioRunStatus::Error
    } else if expectation_failures.is_empty() {
        ScenarioRunStatus::Passed
    } else {
        ScenarioRunStatus::Failed
    };

    let avg_e2e = if latency_samples.is_empty() {
        None
    } else {
        Some(latency_samples.iter().sum::<f64>() / latency_samples.len() as f64)
    };

    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids,
        status,
        metrics: ScenarioMetrics {
            e2e_latency_ms: avg_e2e,
            ..Default::default()
        },
        latency_samples,
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
        turns: Some(turn_results),
        barge_in: None,
        synthetic_caller: None,
        gym: None,
    }
}

pub(super) struct ConversationSessionOutcome {
    session_id: Option<String>,
    turns: Vec<TurnResult>,
}

pub(super) async fn run_conversation_session(
    ws_url: &str,
    experiment_id: &str,
    scenario_id: &str,
    turns: &[ConversationTurn],
    config: &RunnerConfig,
) -> anyhow::Result<ConversationSessionOutcome> {
    let ws_stream = connect_worker(ws_url).await?;
    let (mut sink, mut stream) = ws_stream.split();

    let configure_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "configure",
            "audio_format": "pcm_i16_le",
            "sample_rate": 16000,
            "tags": ["benchmark"]
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

    let mut turn_results: Vec<TurnResult> = Vec::with_capacity(turns.len());

    for turn in turns {
        let result = run_one_turn(
            &mut sink,
            &mut stream,
            experiment_id,
            scenario_id,
            turn,
            config,
        )
        .await;
        let is_error = result.error.is_some();
        turn_results.push(result);
        if is_error {
            break;
        }
    }

    let _ = sink.send(Message::Close(None)).await;

    Ok(ConversationSessionOutcome {
        session_id,
        turns: turn_results,
    })
}

pub(super) async fn run_one_turn<Si, St>(
    sink: &mut Si,
    stream: &mut St,
    experiment_id: &str,
    scenario_id: &str,
    turn: &ConversationTurn,
    config: &RunnerConfig,
) -> TurnResult
where
    Si: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    St: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let wall_start = Instant::now();
    let is_text_only = matches!(turn.input, TurnInput::Text { .. });

    let send_res: anyhow::Result<()> = match &turn.input {
        TurnInput::Text { text } => {
            // Canonical client->server text input:
            // `{"type":"message", "payload": PipelineMessage{kind="text", ...}}`.
            // The deprecated `{"type":"text", ...}` form is rejected by the
            // current ws codec with `unknown message type: text`.
            let msg = serde_json::json!({
                "type": "message",
                "payload": {
                    "kind": "text",
                    "text": text,
                    "role": "user",
                    "message_id": format!("bench-{scenario_id}-{}", turn.id),
                }
            });
            sink.send(Message::Text(msg.to_string().into()))
                .await
                .map_err(|e| anyhow::anyhow!("ws send: {e}"))
        }
        TurnInput::Audio { path } => {
            let msg = serde_json::json!({
                "type": "control",
                "payload": {
                    "action": "inject_audio",
                    "source": path.display().to_string(),
                    "scenario_id": scenario_id,
                    "experiment_id": experiment_id,
                    "speed": config.inject_speed,
                }
            });
            sink.send(Message::Text(msg.to_string().into()))
                .await
                .map_err(|e| anyhow::anyhow!("ws send: {e}"))
        }
    };
    if let Err(e) = send_res {
        return TurnResult {
            turn_id: turn.id.clone(),
            utterance_id: None,
            stt_text: None,
            response_text: None,
            e2e_latency_ms: None,
            completed: false,
            agent_turns_seen: 0,
            expectation_failures: Vec::new(),
            error: Some(format!("send failed: {e}")),
        };
    }

    match collect_outcome(
        stream,
        None,
        config.scenario_timeout,
        config.settle_timeout,
        wall_start,
        is_text_only,
    )
    .await
    {
        Ok(outcome) => {
            let mut agent_turns_seen = outcome.agent_snapshots.len() as u32;
            let mut response_text = outcome.response_text;
            let mut expectation_failures = Vec::new();

            // Agent follow-ups (a filler's late result, proactive
            // speech): the scripted-turn collector deliberately does
            // not complete on them; drain them here when expected.
            if turn.expect_followups > agent_turns_seen {
                let window = Duration::from_millis(turn.followup_timeout_ms.unwrap_or(15_000));
                let followups = collect_agent_followups(
                    stream,
                    turn.expect_followups - agent_turns_seen,
                    window,
                )
                .await;
                agent_turns_seen += followups.seen;
                if let Some(text) = followups.text {
                    // Append so transcript_contains can assert against
                    // filler + follow-up combined.
                    response_text = Some(match response_text {
                        Some(mut existing) => {
                            existing.push(' ');
                            existing.push_str(&text);
                            existing
                        }
                        None => text,
                    });
                }
            }
            if agent_turns_seen < turn.expect_followups {
                expectation_failures.push(format!(
                    "expected {} agent follow-up turn(s), saw {agent_turns_seen}",
                    turn.expect_followups
                ));
            }

            TurnResult {
                turn_id: turn.id.clone(),
                utterance_id: None,
                stt_text: outcome.stt_text,
                response_text,
                e2e_latency_ms: outcome.e2e_latency_ms,
                // A turn is "completed" only if the server actually reported
                // end-of-turn metrics — not because e2e_latency_ms fell back to
                // wall-clock (which is always Some, making a silent-but-alive
                // pipeline look completed).
                completed: outcome.turn_metrics_seen,
                agent_turns_seen,
                expectation_failures,
                error: None,
            }
        }
        Err(e) => TurnResult {
            turn_id: turn.id.clone(),
            utterance_id: None,
            stt_text: None,
            response_text: None,
            e2e_latency_ms: None,
            completed: false,
            agent_turns_seen: 0,
            expectation_failures: Vec::new(),
            error: Some(format!("collect failed: {e}")),
        },
    }
}

pub(super) fn apply_turn_assertions(
    turn: &ConversationTurn,
    expected: &TurnExpected,
    result: &mut TurnResult,
) {
    let _ = turn;
    for needle in &expected.transcript_contains {
        let haystack = result
            .stt_text
            .as_deref()
            .or(result.response_text.as_deref())
            .unwrap_or("");
        if !haystack.to_lowercase().contains(&needle.to_lowercase()) {
            result
                .expectation_failures
                .push(format!("transcript missing substring {needle:?}"));
        }
    }
    if let (Some(golden), Some(actual)) = (&expected.golden_transcript, &result.stt_text)
        && !actual.eq_ignore_ascii_case(golden.trim())
    {
        result.expectation_failures.push(format!(
            "stt_text != golden_transcript (got {actual:?}, want {golden:?})"
        ));
    }
    if let (Some(cap), Some(actual)) = (expected.max_latency_ms, result.e2e_latency_ms)
        && actual > cap as f64
    {
        result
            .expectation_failures
            .push(format!("e2e_latency_ms {actual:.0} exceeds cap {cap}"));
    }
}

pub(super) fn score_conversation(
    expected: &ScenarioExpected,
    turns: &[TurnResult],
    failures: &mut Vec<String>,
) {
    if let Some(want) = expected.turns_completed {
        let got = turns.iter().filter(|t| t.completed).count() as u32;
        if got < want {
            failures.push(format!("turns_completed: got {got}, want >= {want}"));
        }
    }
    if let Some(cap) = expected.max_latency_ms_per_turn {
        for t in turns {
            if let Some(ms) = t.e2e_latency_ms
                && ms > cap as f64
            {
                failures.push(format!(
                    "turn {}: e2e_latency_ms {ms:.0} exceeds per-turn cap {cap}",
                    t.turn_id
                ));
            }
        }
    }
    if let Some(pct) = expected.max_latency_drift_pct {
        let first = turns.iter().find_map(|t| t.e2e_latency_ms);
        if let Some(base) = first {
            let allowed = base * (1.0 + pct / 100.0);
            for t in turns.iter().skip(1) {
                if let Some(ms) = t.e2e_latency_ms
                    && ms > allowed
                {
                    failures.push(format!(
                            "turn {}: latency drift {ms:.0}ms > {allowed:.0}ms (base {base:.0}ms + {pct}%)",
                            t.turn_id
                        ));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/conversation.rs"]
mod tests;
