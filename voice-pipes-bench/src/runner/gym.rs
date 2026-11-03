//! Gym task scenario runner: drives a `ScenarioInput::GymTask` over a
//! WebSocket session, injecting client tools, handling in-flight tool calls,
//! tracking metrics, and computing composite RL rewards.

use super::*;
use voice_pipes_gym::{
    AcousticAdversary, BankingGoal, BankingTaskEnv, ClientToolCall, ClinicBookingTaskEnv,
    ClinicGoal, GymRewardBreakdown, OrderGoal, OrderSupportTaskEnv, TaskEnvironment,
    TaskEvaluation,
};

/// Drive a `ScenarioInput::GymTask` against one worker.
pub(super) async fn run_gym_scenario(
    ws_url: &str,
    experiment_id: &str,
    scenario: &TestScenario,
    task: &GymTaskConfig,
    config: &RunnerConfig,
) -> ScenarioResult {
    let mut tool_calls_executed = 0;

    let mut env: Box<dyn TaskEnvironment> = match task.domain.as_str() {
        "banking" => Box::new(BankingTaskEnv::new_default()),
        "order_support" | "orders" => Box::new(OrderSupportTaskEnv::new_default()),
        "clinic_booking" | "clinic" => Box::new(ClinicBookingTaskEnv::new_default()),
        unknown => {
            return gym_error_result(
                scenario,
                format!("unknown task domain `{unknown}` in gym configuration"),
            );
        }
    };

    let session_res = run_gym_session(
        ws_url,
        experiment_id,
        &scenario.id,
        task,
        &mut *env,
        config,
        &mut tool_calls_executed,
    )
    .await;

    let (session_id, turns, last_error) = match session_res {
        Ok(outcome) => (outcome.session_id, outcome.turns, None),
        Err(e) => (None, Vec::new(), Some(format!("gym session failed: {e}"))),
    };

    let task_eval = evaluate_task_goal(task, &*env);
    build_gym_scenario_result(
        scenario,
        task,
        task_eval,
        session_id,
        turns,
        last_error,
        tool_calls_executed,
    )
}

fn evaluate_task_goal(task: &GymTaskConfig, env: &dyn TaskEnvironment) -> TaskEvaluation {
    match task.domain.as_str() {
        "banking" => match serde_json::from_value::<BankingGoal>(task.goal.clone()) {
            Ok(goal) => {
                if let Some(banking_env) = env.as_any().downcast_ref::<BankingTaskEnv>() {
                    banking_env.verify_goal(&goal)
                } else {
                    TaskEvaluation::failure("Environment downcast to BankingTaskEnv failed")
                }
            }
            Err(e) => TaskEvaluation::failure(format!("Invalid BankingGoal definition: {e}")),
        },
        "order_support" | "orders" => {
            match serde_json::from_value::<OrderGoal>(task.goal.clone()) {
                Ok(goal) => {
                    if let Some(order_env) = env.as_any().downcast_ref::<OrderSupportTaskEnv>() {
                        order_env.verify_goal(&goal)
                    } else {
                        TaskEvaluation::failure(
                            "Environment downcast to OrderSupportTaskEnv failed",
                        )
                    }
                }
                Err(e) => TaskEvaluation::failure(format!("Invalid OrderGoal definition: {e}")),
            }
        }
        "clinic_booking" | "clinic" => {
            match serde_json::from_value::<ClinicGoal>(task.goal.clone()) {
                Ok(goal) => {
                    if let Some(clinic_env) = env.as_any().downcast_ref::<ClinicBookingTaskEnv>() {
                        clinic_env.verify_goal(&goal)
                    } else {
                        TaskEvaluation::failure(
                            "Environment downcast to ClinicBookingTaskEnv failed",
                        )
                    }
                }
                Err(e) => TaskEvaluation::failure(format!("Invalid ClinicGoal definition: {e}")),
            }
        }
        _ => TaskEvaluation::failure("Unsupported domain verification"),
    }
}

fn build_gym_scenario_result(
    scenario: &TestScenario,
    task: &GymTaskConfig,
    task_eval: TaskEvaluation,
    session_id: Option<String>,
    turns: Vec<TurnResult>,
    last_error: Option<String>,
    tool_calls_executed: u32,
) -> ScenarioResult {
    let mut latency_samples = Vec::new();
    for turn in &turns {
        if let Some(ms) = turn.e2e_latency_ms {
            latency_samples.push(ms);
        }
    }

    let avg_e2e = if latency_samples.is_empty() {
        None
    } else {
        Some(latency_samples.iter().sum::<f64>() / latency_samples.len() as f64)
    };

    let reward = GymRewardBreakdown::compute(
        &task.reward_weights,
        &task_eval,
        if task_eval.success { 1.0 } else { 0.0 },
        0.2,
        avg_e2e,
        None,
    );

    let status = if last_error.is_some() {
        ScenarioRunStatus::Error
    } else if task_eval.success {
        ScenarioRunStatus::Passed
    } else {
        ScenarioRunStatus::Failed
    };

    let gym_result = GymResult {
        domain: task.domain.clone(),
        evaluation: task_eval.clone(),
        reward,
        tool_calls_executed,
    };

    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids: session_id.into_iter().collect(),
        status,
        metrics: ScenarioMetrics {
            e2e_latency_ms: avg_e2e,
            ..Default::default()
        },
        latency_samples,
        scoring: Some(ScenarioScoring {
            wer: None,
            cer: None,
            quality_score: Some(task_eval.score as f64),
            quality_reasoning: Some(task_eval.message.clone()),
            expectations_met: task_eval.success,
            expectation_failures: if task_eval.success {
                Vec::new()
            } else {
                vec![task_eval.message]
            },
        }),
        error_message: last_error,
        repetitions_failed: None,
        tts_entries: None,
        turns: Some(turns),
        barge_in: None,
        synthetic_caller: None,
        gym: Some(gym_result),
    }
}

fn gym_error_result(scenario: &TestScenario, error: String) -> ScenarioResult {
    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids: Vec::new(),
        status: ScenarioRunStatus::Error,
        metrics: ScenarioMetrics::default(),
        latency_samples: Vec::new(),
        scoring: None,
        error_message: Some(error),
        repetitions_failed: None,
        tts_entries: None,
        turns: None,
        barge_in: None,
        synthetic_caller: None,
        gym: None,
    }
}

struct GymSessionOutcome {
    session_id: Option<String>,
    turns: Vec<TurnResult>,
}

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Message,
>;
type WsStream = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn run_gym_session(
    ws_url: &str,
    _experiment_id: &str,
    _scenario_id: &str,
    task: &GymTaskConfig,
    env: &mut dyn TaskEnvironment,
    _config: &RunnerConfig,
    tool_calls_executed: &mut u32,
) -> anyhow::Result<GymSessionOutcome> {
    let (mut sink, mut stream, session_id) = connect_and_configure_session(ws_url, env).await?;

    let adversary = AcousticAdversary::new(task.adversary.clone());
    let mut prompts = Vec::new();
    prompts.push(adversary.perturb_prompt(&task.initial_prompt, 1));
    for (idx, follow_up) in task.follow_up_prompts.iter().enumerate() {
        prompts.push(adversary.perturb_prompt(follow_up, (idx + 2) as u64));
    }

    let mut turn_results: Vec<TurnResult> = Vec::with_capacity(prompts.len());
    for (turn_idx, prompt) in prompts.iter().enumerate() {
        let turn = drive_single_gym_turn(
            &mut sink,
            &mut stream,
            env,
            prompt,
            turn_idx,
            tool_calls_executed,
        )
        .await?;
        turn_results.push(turn);
    }

    Ok(GymSessionOutcome {
        session_id,
        turns: turn_results,
    })
}

async fn connect_and_configure_session(
    ws_url: &str,
    env: &dyn TaskEnvironment,
) -> anyhow::Result<(WsSink, WsStream, Option<String>)> {
    let ws_stream = connect_worker(ws_url).await?;
    let (mut sink, mut stream) = ws_stream.split();

    let client_tools_json: Vec<serde_json::Value> = env
        .tools()
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters
            })
        })
        .collect();

    let configure_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "configure",
            "audio_format": "pcm_i16_le",
            "sample_rate": 16000,
            "tags": ["benchmark", "gym"],
            "options": {
                "client_tools": client_tools_json
            }
        }
    });

    sink.send(Message::Text(configure_msg.to_string().into()))
        .await?;

    let mut session_id: Option<String> = None;
    let ready = timeout(Duration::from_secs(10), async {
        while let Some(msg) = stream.next().await {
            if let Ok(Message::Text(text)) = msg
                && let Ok(env_envelope) = serde_json::from_str::<JsonEnvelope>(&text)
                && env_envelope.msg_type == "control"
                && let Some(action) = env_envelope.payload.get("action").and_then(|v| v.as_str())
                && action == "session_ready"
            {
                session_id = env_envelope
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
        Ok(Ok(())) => Ok((sink, stream, session_id)),
        Ok(Err(e)) => Err(e),
        Err(_) => anyhow::bail!("SessionReady timeout"),
    }
}

async fn drive_single_gym_turn(
    sink: &mut WsSink,
    stream: &mut WsStream,
    env: &mut dyn TaskEnvironment,
    prompt: &str,
    turn_idx: usize,
    tool_calls_executed: &mut u32,
) -> anyhow::Result<TurnResult> {
    let turn_id = format!("turn-{}", turn_idx + 1);
    let turn_start = Instant::now();

    let prompt_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "text",
            "text": prompt
        }
    });

    sink.send(Message::Text(prompt_msg.to_string().into()))
        .await?;

    let mut turn_completed = false;
    let mut turn_latency = None;

    let turn_future = async {
        while let Some(msg) = stream.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(_) => continue,
                Err(e) => return Err(anyhow::anyhow!("WS error during turn: {e}")),
            };

            let Ok(envelope) = serde_json::from_str::<JsonEnvelope>(&text) else {
                continue;
            };

            if envelope.msg_type == "message"
                && let Some(kind) = envelope.payload.get("kind").and_then(|v| v.as_str())
                && kind == "trigger_action"
            {
                handle_trigger_action(&envelope.payload, env, sink, tool_calls_executed).await?;
            }

            if envelope.msg_type == "control"
                && let Some(action) = envelope.payload.get("action").and_then(|v| v.as_str())
                && (action == "turn_completed" || action == "state_changed")
            {
                turn_latency = Some(turn_start.elapsed().as_secs_f64() * 1000.0);
                turn_completed = true;
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    let _ = timeout(Duration::from_secs(30), turn_future).await;

    Ok(TurnResult {
        turn_id,
        utterance_id: None,
        stt_text: None,
        response_text: None,
        e2e_latency_ms: turn_latency,
        completed: turn_completed,
        agent_turns_seen: 0,
        expectation_failures: Vec::new(),
        error: if turn_completed {
            None
        } else {
            Some("Turn timed out or closed prematurely".to_string())
        },
    })
}

async fn handle_trigger_action(
    payload: &serde_json::Value,
    env: &mut dyn TaskEnvironment,
    sink: &mut WsSink,
    tool_calls_executed: &mut u32,
) -> anyhow::Result<()> {
    let call_id = payload
        .get("call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let action_name = payload
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parameters = payload
        .get("parameters")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let call = ClientToolCall::new(&call_id, &action_name, parameters);
    let result_val = match env.execute(&call) {
        Ok(val) => val,
        Err(err) => serde_json::json!({ "error": err }),
    };

    *tool_calls_executed = tool_calls_executed.saturating_add(1);

    let ack_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "client_tool_result",
            "call_id": call_id,
            "result": result_val
        }
    });

    sink.send(Message::Text(ack_msg.to_string().into())).await?;
    Ok(())
}
