//! Barge-in scenario runner: drives `ScenarioInput::BargeIn`, firing an
//! interrupt mid-utterance and scoring the recovery.

use super::*;

/// Drive a `ScenarioInput::BargeIn` scenario.
///
/// Supports all three `InterruptMode` variants:
/// - `ClientCancel` -- sends `{action:"cancel"}`.
/// - `InjectAudio` -- sends a second `inject_audio` while the assistant
///   is speaking; the pipeline's server-side VAD fires the barge-in.
/// - `InjectText` -- sends a `text` turn during playback; the pipeline
///   takes the early-barge path and pre-empts the in-flight utterance.
pub(super) async fn run_barge_in_scenario(
    ws_url: &str,
    experiment_id: &str,
    scenario: &TestScenario,
    initial: &ConversationTurn,
    interrupt: &InterruptSpec,
    follow_up: Option<&ConversationTurn>,
    config: &RunnerConfig,
) -> ScenarioResult {
    let session = run_barge_in_session(
        ws_url,
        experiment_id,
        &scenario.id,
        initial,
        interrupt,
        follow_up,
        config,
    )
    .await;

    let mut session_ids = Vec::new();
    let mut turn_results: Vec<TurnResult> = Vec::new();
    let mut barge_in_result = BargeInResult {
        interrupted: false,
        trigger: None,
        barge_in_rtt_ms: None,
        recovery_ms: None,
        follow_up_completed: false,
    };
    let mut last_error: Option<String> = None;
    let mut latency_samples: Vec<f64> = Vec::new();

    match session {
        Ok(s) => {
            if let Some(sid) = s.session_id {
                session_ids.push(sid);
            }
            barge_in_result = s.barge_in;
            if let Some(ms) = s.initial_turn.e2e_latency_ms {
                latency_samples.push(ms);
            }
            turn_results.push(s.initial_turn);
            if let Some(t) = s.follow_up_turn {
                if let Some(ms) = t.e2e_latency_ms {
                    latency_samples.push(ms);
                }
                turn_results.push(t);
            }
        }
        Err(e) => {
            last_error = Some(format!("barge_in session failed: {e}"));
        }
    }

    let mut expectation_failures: Vec<String> = Vec::new();
    if let Some(exp) = &scenario.expected {
        score_barge_in(
            exp,
            &barge_in_result,
            &turn_results,
            &mut expectation_failures,
        );
    }

    let status = if last_error.is_some() {
        ScenarioRunStatus::Error
    } else if expectation_failures.is_empty() {
        ScenarioRunStatus::Passed
    } else {
        ScenarioRunStatus::Failed
    };

    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids,
        status,
        metrics: ScenarioMetrics::default(),
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
        turns: if turn_results.is_empty() {
            None
        } else {
            Some(turn_results)
        },
        barge_in: Some(barge_in_result),
        synthetic_caller: None,
        gym: None,
    }
}

pub(super) struct BargeInSessionOutcome {
    session_id: Option<String>,
    initial_turn: TurnResult,
    barge_in: BargeInResult,
    follow_up_turn: Option<TurnResult>,
}

pub(super) async fn run_barge_in_session(
    ws_url: &str,
    experiment_id: &str,
    scenario_id: &str,
    initial: &ConversationTurn,
    interrupt: &InterruptSpec,
    follow_up: Option<&ConversationTurn>,
    config: &RunnerConfig,
) -> anyhow::Result<BargeInSessionOutcome> {
    let ws_stream = connect_worker(ws_url).await?;
    let (mut sink, mut stream) = ws_stream.split();

    // Configure handshake.
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

    // Send the initial turn.
    let initial_wall = Instant::now();
    send_turn_input(&mut sink, experiment_id, scenario_id, initial, config).await?;

    // Watch for the trigger event, then fire the interrupt.
    let trigger_outcome = wait_for_trigger_and_interrupt(
        &mut sink,
        &mut stream,
        interrupt,
        scenario_id,
        experiment_id,
        config,
    )
    .await?;

    // Voice barge-in (audio interrupt, no scripted follow-up): the
    // interrupting utterance IS the next user turn, and the provider's
    // answer arrives interleaved with the interrupted turn's flush and any
    // stray residue of the cancelled response. A "stop at first Listening"
    // observer would either swallow the answer or capture residue, so this
    // mode uses a unified collector that watches interrupt evidence AND
    // accumulates all agent output until content quiescence; the
    // `pipeline_must_say` assertion then judges the whole capture.
    let (post, voice_reply_turn) = match trigger_outcome.pending_uplink {
        Some(frames) => {
            // Re-anchor at actual speech start, then run the paced uplink
            // and the collector concurrently — interrupt evidence (and on
            // voice mode the provider's answer) arrives mid-stream.
            let sent_at = Instant::now();
            if follow_up.is_none() {
                let (uplink, v) = tokio::join!(
                    uplink_frames_paced(&mut sink, frames),
                    collect_voice_barge_in(&mut stream, sent_at, config)
                );
                if let Err(e) = uplink {
                    warn!(error = %e, "barge_in: interrupt audio uplink failed");
                }
                let turn = TurnResult {
                    turn_id: "interrupt-reply".to_string(),
                    utterance_id: None,
                    stt_text: v.stt_text.clone(),
                    response_text: v.response_text.clone(),
                    e2e_latency_ms: v.reply_e2e_ms,
                    completed: v.reply_completed,
                    agent_turns_seen: 0,
                    expectation_failures: Vec::new(),
                    error: v.provider_error.clone(),
                };
                (v.post, Some(turn))
            } else {
                let (uplink, post) = tokio::join!(
                    uplink_frames_paced(&mut sink, frames),
                    collect_post_interrupt(&mut stream, sent_at, config)
                );
                if let Err(e) = uplink {
                    warn!(error = %e, "barge_in: interrupt audio uplink failed");
                }
                (post, None)
            }
        }
        None => (
            collect_post_interrupt(&mut stream, trigger_outcome.interrupt_sent_at, config).await,
            None,
        ),
    };

    let initial_turn = TurnResult {
        turn_id: initial.id.clone(),
        utterance_id: post.utterance_id,
        stt_text: None,
        response_text: None,
        e2e_latency_ms: Some(initial_wall.elapsed().as_secs_f64() * 1000.0),
        completed: false,
        agent_turns_seen: 0,
        expectation_failures: Vec::new(),
        error: None,
    };

    let barge_in = BargeInResult {
        interrupted: post.interrupted,
        trigger: post
            .interrupted
            .then(|| trigger_for_mode(&interrupt.mode).to_string()),
        barge_in_rtt_ms: post.barge_in_rtt_ms,
        recovery_ms: post.recovery_ms,
        follow_up_completed: false,
    };

    // Optional follow-up turn.
    let mut follow_up_turn = None;
    let mut barge_in = barge_in;
    if let Some(turn) = follow_up {
        let result = run_one_turn(
            &mut sink,
            &mut stream,
            experiment_id,
            scenario_id,
            turn,
            config,
        )
        .await;
        barge_in.follow_up_completed = result.completed;
        follow_up_turn = Some(result);
    } else if let Some(result) = voice_reply_turn {
        barge_in.follow_up_completed = result.completed;
        follow_up_turn = Some(result);
    }

    let _ = sink.send(Message::Close(None)).await;

    Ok(BargeInSessionOutcome {
        session_id,
        initial_turn,
        barge_in,
        follow_up_turn,
    })
}

/// Helper: send a `ConversationTurn` input over an open sink.
pub(super) async fn send_turn_input<Si>(
    sink: &mut Si,
    experiment_id: &str,
    scenario_id: &str,
    turn: &ConversationTurn,
    config: &RunnerConfig,
) -> anyhow::Result<()>
where
    Si: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let msg = match &turn.input {
        TurnInput::Text { text } => serde_json::json!({
            "type": "message",
            "payload": {
                "kind": "text",
                "text": text,
                "role": "user",
                "message_id": format!("bench-{scenario_id}-{}", turn.id),
            }
        }),
        TurnInput::Audio { path } => serde_json::json!({
            "type": "control",
            "payload": {
                "action": "inject_audio",
                "source": path.display().to_string(),
                "scenario_id": scenario_id,
                "experiment_id": experiment_id,
                "speed": config.inject_speed,
            }
        }),
    };
    sink.send(Message::Text(msg.to_string().into()))
        .await
        .map_err(|e| anyhow::anyhow!("ws send: {e}"))
}

pub(super) struct TriggerOutcome {
    interrupt_sent_at: Instant,
    /// Pre-encoded mic frames for a voice (`InjectAudio`) interrupt. The
    /// session loop uplinks them paced, concurrently with post-interrupt
    /// collection; `interrupt_sent_at` is re-anchored when the uplink
    /// starts.
    pending_uplink: Option<Vec<Vec<u8>>>,
}

/// Dispatch the configured interrupt: send control/text interrupts
/// immediately; prepare (but do not send) voice-interrupt mic frames.
async fn dispatch_interrupt<Si>(
    sink: &mut Si,
    interrupt: &InterruptSpec,
    scenario_id: &str,
    experiment_id: &str,
    inject_speed: f32,
) -> anyhow::Result<TriggerOutcome>
where
    Si: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    if let InterruptMode::InjectAudio { path } = &interrupt.mode {
        return Ok(TriggerOutcome {
            interrupt_sent_at: Instant::now(),
            pending_uplink: Some(load_interrupt_frames(path).await?),
        });
    }
    let sent_at = fire_interrupt(
        sink,
        &interrupt.mode,
        scenario_id,
        experiment_id,
        inject_speed,
    )
    .await?;
    Ok(TriggerOutcome {
        interrupt_sent_at: sent_at,
        pending_uplink: None,
    })
}

/// Read messages from `stream` until the configured trigger fires, then
/// dispatch the interrupt action and return the wall-clock instant the
/// interrupt was sent.
pub(super) async fn wait_for_trigger_and_interrupt<Si, St>(
    sink: &mut Si,
    stream: &mut St,
    interrupt: &InterruptSpec,
    scenario_id: &str,
    experiment_id: &str,
    config: &RunnerConfig,
) -> anyhow::Result<TriggerOutcome>
where
    Si: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    St: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = Instant::now() + config.scenario_timeout;
    let mut first_audio_at: Option<Instant> = None;

    loop {
        // Decide our wait window. If we already saw FirstAudio and need
        // a PlaybackOffset, that delay is the sole gate.
        if let (InterruptFireOn::PlaybackOffset { offset_ms }, Some(first)) =
            (interrupt.fire_on, first_audio_at)
        {
            let target = first + Duration::from_millis(offset_ms);
            let now = Instant::now();
            if now >= target {
                return dispatch_interrupt(
                    sink,
                    interrupt,
                    scenario_id,
                    experiment_id,
                    config.inject_speed,
                )
                .await;
            }
            // Sleep until target (but respect deadline).
            let wait = (target - now).min(deadline.saturating_duration_since(now));
            tokio::time::sleep(wait).await;
            // Loop continues; we'll re-check and fire on next iteration.
            continue;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("barge_in: timeout waiting for trigger event");
        }

        match timeout(remaining, stream.next()).await {
            Ok(Some(Ok(Message::Binary(_)))) => {
                // First binary frame == first audio chunk on the wire.
                if first_audio_at.is_none() {
                    first_audio_at = Some(Instant::now());
                    match interrupt.fire_on {
                        InterruptFireOn::FirstAudio | InterruptFireOn::TtsStarted => {
                            return dispatch_interrupt(
                                sink,
                                interrupt,
                                scenario_id,
                                experiment_id,
                                config.inject_speed,
                            )
                            .await;
                        }
                        InterruptFireOn::PlaybackOffset { .. } => {
                            // Loop back; the offset branch above handles
                            // the wait + send.
                        }
                    }
                }
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text) {
                    // Detect server errors early so we don't spin until
                    // timeout.
                    if env.msg_type == "control" {
                        let action = env
                            .payload
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if action == "error" {
                            let m = env
                                .payload
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            anyhow::bail!("server error before trigger: {m}");
                        }
                        if action == "closing" {
                            anyhow::bail!("ws closing before trigger");
                        }
                    }
                }
            }
            Ok(Some(Ok(Message::Close(_))) | None) => {
                anyhow::bail!("ws closed before trigger");
            }
            Ok(Some(Ok(_))) => {} // ping/pong
            Ok(Some(Err(e))) => anyhow::bail!("ws read: {e}"),
            Err(_) => anyhow::bail!("barge_in: timeout waiting for trigger event"),
        }
    }
}

/// Send the interrupt over the wire. Returns the instant it was sent
/// (for `InjectAudio`: when the first mic frame went out, since that is
/// when the "user" audibly starts speaking).
pub(super) async fn fire_interrupt<Si>(
    sink: &mut Si,
    mode: &InterruptMode,
    scenario_id: &str,
    _experiment_id: &str,
    _inject_speed: f32,
) -> anyhow::Result<Instant>
where
    Si: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let msg = match mode {
        InterruptMode::ClientCancel => serde_json::json!({
            "type": "control",
            "payload": { "action": "cancel" }
        }),
        InterruptMode::InjectAudio { .. } => {
            // Voice interrupts are not fired here: the caller uplinks the
            // prepared mic frames (see `load_interrupt_frames`) concurrently
            // with post-interrupt collection, because the paced stream takes
            // seconds and the interrupt evidence arrives mid-stream.
            anyhow::bail!("InjectAudio interrupts are dispatched by the session loop");
        }
        InterruptMode::InjectText { text } => serde_json::json!({
            "type": "message",
            "payload": {
                "kind": "text",
                "text": text,
                "role": "user",
                "message_id": format!("bench-{scenario_id}-bargein"),
            }
        }),
    };
    sink.send(Message::Text(msg.to_string().into()))
        .await
        .map_err(|e| anyhow::anyhow!("ws send interrupt: {e}"))?;
    Ok(Instant::now())
}

/// Load a 16 kHz mono WAV and pre-encode it as client mic frames: 20 ms
/// chunks, ~700 ms of appended silence so server VADs see the utterance
/// end, and the end-of-utterance flag on the final frame.
async fn load_interrupt_frames(path: &std::path::Path) -> anyhow::Result<Vec<Vec<u8>>> {
    use voice_pipes::transport::wire::BinaryHeader;

    // Containment (same untrusted-path concern as variant configs): the path comes
    // from the untrusted suite definition; require it inside the CWD.
    let base = std::env::current_dir()?.canonicalize()?;
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| anyhow::anyhow!("interrupt audio {}: {e}", path.display()))?;
    if !canonical.starts_with(&base) {
        anyhow::bail!(
            "interrupt audio path {} escapes the working directory",
            path.display()
        );
    }

    let frame = voice_pipes::audio::wav_reader::read_wav(&canonical)?;
    if frame.channels != 1 || frame.sample_rate != 16_000 {
        anyhow::bail!(
            "interrupt audio must be 16 kHz mono (got {} Hz, {} ch) -- the \
             benchmark session negotiates a 16 kHz mic uplink",
            frame.sample_rate,
            frame.channels
        );
    }

    const SAMPLES_PER_FRAME: usize = 320; // 20 ms @ 16 kHz
    // A real mic keeps streaming silence after the words end, and some
    // provider endpointers (ElevenLabs Agents) only finalise the user turn
    // while the audio timeline keeps advancing — a stream that stops dead
    // shortly after speech leaves the utterance uncommitted and the agent
    // never answers. Providers that endpoint quickly just ignore the tail.
    const TRAILING_SILENCE_SAMPLES: usize = 64_000; // 4 s

    let mut samples = frame.samples;
    samples.extend(std::iter::repeat_n(0i16, TRAILING_SILENCE_SAMPLES));

    let total_frames = samples.len().div_ceil(SAMPLES_PER_FRAME).max(1);
    let mut frames = Vec::with_capacity(total_frames);
    for (idx, chunk) in samples.chunks(SAMPLES_PER_FRAME).enumerate() {
        let is_last = idx + 1 == total_frames;
        let header = BinaryHeader::audio_pcm(idx as u32, is_last, false);
        let mut payload = Vec::with_capacity(chunk.len() * 2);
        for s in chunk {
            payload.extend_from_slice(&s.to_le_bytes());
        }
        frames.push(header.encode_with_payload(&payload));
    }
    Ok(frames)
}

/// Uplink pre-encoded mic frames paced at realtime (20 ms per frame). A
/// barge-in is a human talking over the assistant — provider turn-taking
/// stacks, notably `ElevenLabs` Agents, assume a live mic cadence and can
/// drop a burst-uploaded utterance. Run this concurrently with the
/// post-interrupt collector: interrupt evidence arrives mid-stream.
async fn uplink_frames_paced<Si>(sink: &mut Si, frames: Vec<Vec<u8>>) -> anyhow::Result<()>
where
    Si: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let mut pacer = tokio::time::interval(Duration::from_millis(20));
    pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    for frame in frames {
        pacer.tick().await;
        sink.send(Message::Binary(frame.into()))
            .await
            .map_err(|e| anyhow::anyhow!("ws send interrupt audio: {e}"))?;
    }
    Ok(())
}

/// Map an interrupt mode to its expected `interrupted.trigger` value.
pub(super) fn trigger_for_mode(mode: &InterruptMode) -> &'static str {
    match mode {
        InterruptMode::ClientCancel => "client_cancel",
        InterruptMode::InjectAudio { .. } => "server_vad",
        InterruptMode::InjectText { .. } => "early_barge",
    }
}

#[derive(Default)]
pub(super) struct PostInterruptOutcome {
    interrupted: bool,
    barge_in_rtt_ms: Option<u64>,
    recovery_ms: Option<u64>,
    utterance_id: Option<u32>,
}

/// Outcome of the unified voice-barge-in collector: interrupt evidence
/// plus everything the agent said after the interrupt.
#[derive(Default)]
pub(super) struct VoiceBargeOutcome {
    pub(super) post: PostInterruptOutcome,
    /// Agent text captured after the interrupt (residue + answer).
    pub(super) response_text: Option<String>,
    /// Provider transcription of the interrupting utterance, when echoed.
    pub(super) stt_text: Option<String>,
    /// Interrupt-audio start -> last completed reply turn.
    pub(super) reply_e2e_ms: Option<f64>,
    /// A non-interrupted `turn_metrics` arrived after the interrupt.
    pub(super) reply_completed: bool,
    /// The provider connection dropped mid-scenario (`clear_audio` with
    /// reason `provider_closed`) — infrastructure failure, not a barge-in.
    pub(super) provider_error: Option<String>,
}

/// Watch the stream after a voice (mic-audio) interrupt: record interrupt
/// evidence (`clear_audio` / interrupted `turn_metrics` -> RTT, first
/// `listening` -> recovery) and keep accumulating agent output until no
/// *content* has arrived for `settle_timeout` (pings/pongs do not extend
/// the window) or `scenario_timeout` elapses. Unlike
/// [`collect_post_interrupt`], this does NOT stop at the flush `listening`
/// -- the provider's answer to the interrupting utterance arrives after
/// it, interleaved with any residue of the cancelled response.
pub(super) async fn collect_voice_barge_in<St>(
    stream: &mut St,
    interrupt_sent_at: Instant,
    config: &RunnerConfig,
) -> VoiceBargeOutcome
where
    St: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut out = VoiceBargeOutcome::default();
    let mut interrupted_at: Option<Instant> = None;
    let mut response_parts: Vec<String> = Vec::new();
    let deadline = Instant::now() + config.scenario_timeout;
    let mut last_content = Instant::now();

    loop {
        let now = Instant::now();
        let quiet_deadline = last_content + config.settle_timeout;
        let read_deadline = quiet_deadline.min(deadline);
        if now >= read_deadline {
            break;
        }

        match timeout(read_deadline - now, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                last_content = Instant::now();
                let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text) else {
                    continue;
                };
                match env.msg_type.as_str() {
                    "message" | "text" => {
                        // Only capture agent speech from after the interrupt;
                        // pre-interrupt captions belong to the story turn.
                        if !out.post.interrupted {
                            continue;
                        }
                        let role = env
                            .payload
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let is_stt = env
                            .payload
                            .get("message_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|id| {
                                id.starts_with("stt-")
                                    || id.starts_with("utterance-")
                                    || id.starts_with("bench-")
                            });
                        if let Some(t) = env.payload.get("text").and_then(|v| v.as_str()) {
                            if role == "user" || is_stt {
                                // The pipeline's echo of the transcribed
                                // interrupt utterance.
                                out.stt_text = Some(t.to_string());
                            } else {
                                response_parts.push(t.to_string());
                            }
                        }
                    }
                    "audio_meta" => {
                        if !out.post.interrupted {
                            continue;
                        }
                        if let Some(full) = env.payload.get("full_text").and_then(|v| v.as_str()) {
                            response_parts.push(full.to_string());
                        }
                    }
                    "control" => {
                        let action = env
                            .payload
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        match action {
                            "clear_audio" | "text_truncated" => {
                                // A `provider_closed` clear is the provider
                                // connection dying (e.g. an ElevenLabs 1002
                                // quota close), not the VAD interrupting —
                                // counting it as barge-in evidence produced
                                // convincing-but-fake RTT numbers.
                                let reason = env
                                    .payload
                                    .get("reason")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if reason == "provider_closed" {
                                    if out.provider_error.is_none() {
                                        out.provider_error = Some(
                                            "provider connection closed mid-scenario".to_string(),
                                        );
                                    }
                                } else if !out.post.interrupted {
                                    out.post.interrupted = true;
                                    let now = Instant::now();
                                    interrupted_at = Some(now);
                                    out.post.barge_in_rtt_ms =
                                        Some(now.duration_since(interrupt_sent_at).as_millis()
                                            as u64);
                                    out.post.utterance_id = env
                                        .payload
                                        .get("utterance_id")
                                        .and_then(serde_json::Value::as_u64)
                                        .map(|v| v as u32);
                                }
                            }
                            "turn_metrics" => {
                                let interrupted_flag = env
                                    .payload
                                    .get("interrupted")
                                    .and_then(serde_json::Value::as_bool)
                                    .or_else(|| {
                                        env.payload
                                            .get("snapshot")
                                            .and_then(|s| s.get("interrupted"))
                                            .and_then(serde_json::Value::as_bool)
                                    })
                                    .unwrap_or(false);
                                if interrupted_flag
                                    && !out.post.interrupted
                                    && out.provider_error.is_none()
                                {
                                    out.post.interrupted = true;
                                    let now = Instant::now();
                                    interrupted_at = Some(now);
                                    out.post.barge_in_rtt_ms =
                                        Some(now.duration_since(interrupt_sent_at).as_millis()
                                            as u64);
                                } else if !interrupted_flag && out.post.interrupted {
                                    // A completed reply turn after the
                                    // interrupt (answer, or a residue turn --
                                    // `pipeline_must_say` disambiguates).
                                    out.reply_completed = true;
                                    out.reply_e2e_ms = Some(
                                        Instant::now()
                                            .duration_since(interrupt_sent_at)
                                            .as_secs_f64()
                                            * 1000.0,
                                    );
                                }
                            }
                            "state_changed" => {
                                let state = env
                                    .payload
                                    .get("state")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if state == "listening"
                                    && out.post.recovery_ms.is_none()
                                    && let Some(t0) = interrupted_at
                                {
                                    out.post.recovery_ms =
                                        Some(Instant::now().duration_since(t0).as_millis() as u64);
                                }
                            }
                            "closing" => break,
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Ok(Some(Ok(Message::Binary(_)))) => {
                // Agent audio (answer or residue) -- content, extends the
                // quiescence window.
                last_content = Instant::now();
            }
            // Err: quiet for settle_timeout (or scenario deadline).
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => break,
            Ok(Some(Ok(_))) => {} // ping/pong: NOT content, window unchanged
        }
    }

    if !response_parts.is_empty() {
        out.response_text = Some(response_parts.join(""));
    }
    out
}

/// After firing the interrupt, watch the stream for evidence the
/// pipeline interrupted (`clear_audio` / `text_truncated` / interrupted
/// `turn_metrics`) and returned to Listening.
pub(super) async fn collect_post_interrupt<St>(
    stream: &mut St,
    interrupt_sent_at: Instant,
    config: &RunnerConfig,
) -> PostInterruptOutcome
where
    St: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut out = PostInterruptOutcome::default();
    let mut interrupted_at: Option<Instant> = None;
    let deadline = Instant::now() + config.settle_timeout;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match timeout(remaining, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text) else {
                    continue;
                };
                if env.msg_type != "control" {
                    continue;
                }
                let action = env
                    .payload
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match action {
                    "clear_audio" | "text_truncated" => {
                        if !out.interrupted {
                            out.interrupted = true;
                            let now = Instant::now();
                            interrupted_at = Some(now);
                            out.barge_in_rtt_ms =
                                Some(now.duration_since(interrupt_sent_at).as_millis() as u64);
                            out.utterance_id = env
                                .payload
                                .get("utterance_id")
                                .and_then(serde_json::Value::as_u64)
                                .map(|v| v as u32);
                        }
                    }
                    "turn_metrics" => {
                        let interrupted_flag = env
                            .payload
                            .get("interrupted")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        if interrupted_flag && !out.interrupted {
                            out.interrupted = true;
                            let now = Instant::now();
                            interrupted_at = Some(now);
                            out.barge_in_rtt_ms =
                                Some(now.duration_since(interrupt_sent_at).as_millis() as u64);
                            out.utterance_id = env
                                .payload
                                .get("utterance_id")
                                .and_then(serde_json::Value::as_u64)
                                .map(|v| v as u32);
                        }
                    }
                    "state_changed" => {
                        let state = env
                            .payload
                            .get("state")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if state == "listening" {
                            if let Some(t0) = interrupted_at {
                                out.recovery_ms =
                                    Some(Instant::now().duration_since(t0).as_millis() as u64);
                            }
                            return out;
                        }
                    }
                    "closing" => return out,
                    _ => {}
                }
            }
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => return out,
            // Stale audio after cancel, or ping/pong -- ignore.
            Ok(Some(Ok(_))) => {}
        }
    }
    out
}

/// Apply scenario-level barge-in assertions.
pub(super) fn score_barge_in(
    expected: &ScenarioExpected,
    barge: &BargeInResult,
    turns: &[TurnResult],
    failures: &mut Vec<String>,
) {
    if expected.must_interrupt && !barge.interrupted {
        failures.push("must_interrupt: no interruption observed".into());
    }
    if let Some(want) = expected.interruption_trigger {
        let observed = barge.trigger.as_deref().unwrap_or("");
        let matches = match want {
            ExpectedTrigger::Any => barge.interrupted,
            ExpectedTrigger::ClientCancel => observed == "client_cancel",
            ExpectedTrigger::ServerVad => observed == "server_vad",
            ExpectedTrigger::EarlyBarge => observed == "early_barge",
        };
        if !matches {
            failures.push(format!(
                "interruption_trigger: want {want:?}, got {observed:?}"
            ));
        }
    }
    if let (Some(cap), Some(actual)) = (expected.max_barge_in_rtt_ms, barge.barge_in_rtt_ms)
        && actual > cap
    {
        failures.push(format!("max_barge_in_rtt_ms: got {actual}ms, cap {cap}ms"));
    }
    if let (Some(cap), Some(actual)) = (expected.max_recovery_ms, barge.recovery_ms)
        && actual > cap
    {
        failures.push(format!("max_recovery_ms: got {actual}ms, cap {cap}ms"));
    }
    if expected.follow_up_must_complete && !barge.follow_up_completed {
        failures.push("follow_up_must_complete: follow-up turn did not complete".into());
    }
    // Same semantics as the synthetic-caller check: each substring must
    // appear in at least one agent transcript (case-insensitive). This is
    // what distinguishes a real post-interrupt answer from stray residue of
    // the cancelled response.
    for want in &expected.pipeline_must_say {
        let said = turns.iter().any(|t| {
            t.response_text
                .as_deref()
                .is_some_and(|r| r.to_lowercase().contains(&want.to_lowercase()))
        });
        if !said {
            failures.push(format!(
                "pipeline_must_say: {want:?} not found in any agent transcript"
            ));
        }
    }
    // Surface per-turn errors from the follow-up at scenario level too.
    for t in turns {
        if let Some(err) = &t.error {
            failures.push(format!("turn {}: {err}", t.turn_id));
        }
    }
}

#[cfg(test)]
#[path = "tests/barge_in.rs"]
mod tests;
