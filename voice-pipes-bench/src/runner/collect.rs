//! Per-session outcome collection: drains a WebSocket session into a
//! [`RunOutcome`], including the synthetic-caller audio-bridge variant.

use super::*;

/// How long [`collect_outcome`] lingers for the trailing `turn_metrics`
/// envelope after the turn already completed via a state/audio signal.
const TURN_METRICS_GRACE: Duration = Duration::from_millis(500);

// ── Messages collected from one WebSocket session ───────────

pub(super) struct RunOutcome {
    pub(super) session_id: Option<String>,
    pub(super) stt_text: Option<String>,
    pub(super) response_text: Option<String>,
    pub(super) e2e_latency_ms: Option<f64>,
    pub(super) ttft_ms: Option<f64>,
    pub(super) ttfb_ms: Option<f64>,
    pub(super) audio_duration_s: Option<f64>,
    pub(super) stt_latency_ms: Option<f64>,
    pub(super) llm_generation_ms: Option<f64>,
    pub(super) llm_tokens: Option<u32>,
    pub(super) tts_ttfb_ms: Option<f64>,
    pub(super) tts_synthesis_ms: Option<f64>,
    pub(super) tts_audio_duration_s: Option<f64>,
    pub(super) tts_sample_rate: Option<u32>,
    pub(super) tts_chars: Option<u32>,
    pub(super) tts_rtf: Option<f64>,
    pub(super) tts_provider: Option<String>,
    pub(super) tts_cold_start: bool,
    pub(super) tts_error: Option<String>,
    /// Whether the server actually reported end-of-turn `TurnMetrics`.
    /// Distinct from `e2e_latency_ms`, which falls back to wall-clock and is
    /// therefore always `Some` — a silent-but-alive pipeline would otherwise
    /// look "completed".
    pub(super) turn_metrics_seen: bool,
    /// Agent-initiated turns (`initiator: agent` — idle prompts, filler
    /// follow-ups, proactive speech) observed while waiting for the
    /// scripted turn. They deliberately do NOT complete the scripted
    /// turn (that would revive the phantom-turn class); the
    /// conversation runner counts them against `expect_followups`.
    pub(super) agent_snapshots: Vec<voice_pipes::pipeline::TurnMetricsSnapshot>,
}

// ── Lightweight JSON envelope (matches wire.rs JsonEnvelope) ─

#[derive(Debug, Deserialize)]
pub(super) struct JsonEnvelope {
    #[serde(rename = "type")]
    pub(super) msg_type: String,
    #[serde(default)]
    pub(super) payload: serde_json::Value,
}

// ── Message collection loop ─────────────────────────────────

/// Synthetic-caller audio-bridge variant of [`collect_outcome`].
///
/// Reads pipeline output until `state_changed=listening` or a
/// `turn_metrics` event arrives. Pipeline binary audio frames are
/// decoded with [`BinaryHeader`] and forwarded to the caller's mic via
/// [`SyntheticCaller::push_audio`], so live S2S backends keep
/// conversational context end-to-end.
///
/// Only `response_text` and `e2e_latency_ms` are populated; the
/// synthetic-caller session loop only consumes those two fields.
pub(super) async fn collect_outcome_audio_bridge<S>(
    stream: &mut S,
    caller: &mut dyn crate::synthetic_caller::SyntheticCaller,
    session_id: Option<String>,
    scenario_timeout: Duration,
    settle_timeout: Duration,
    wall_start: Instant,
) -> anyhow::Result<RunOutcome>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    use voice_pipes::transport::wire::BinaryHeader;

    let mut response_parts: Vec<String> = Vec::new();
    let mut turn_metrics: Option<voice_pipes::pipeline::TurnMetricsSnapshot> = None;
    // A pipeline turn is only fully demarcated when we have seen BOTH
    // the `turn_metrics` control envelope (LLM/TTS book-keeping flushed)
    // AND a terminal audio signal -- either the binary `is_end_of_response`
    // flag on the last TTS frame or a `state_changed: listening`
    // control envelope. Any single one of these can fire *before* the
    // TTS audio has finished streaming on the wire: returning early
    // leaves the tail frames to be picked up by the next call to this
    // function, which makes the runner double-count one real pipeline
    // turn as two phantom turns (with `latency_ms: 0` on the second
    // because no second `turn_metrics` ever arrives). Worse, the next
    // caller utterance gets pushed into the pipeline while it is still
    // draining audio, which the pipeline's VAD interprets as a
    // self-barge interruption rather than a fresh turn.
    let mut turn_metrics_seen = false;
    let mut audio_terminal_seen = false;
    let mut agent_snapshots: Vec<voice_pipes::pipeline::TurnMetricsSnapshot> = Vec::new();

    let deadline = Instant::now() + scenario_timeout;
    let mut last_activity = Instant::now();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            warn!("audio-bridge scenario timed out");
            break;
        }
        // Use settle timeout once we've seen any activity so we don't
        // wait forever on a stalled connection.
        let read_timeout = remaining.min(settle_timeout);

        match timeout(read_timeout, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                last_activity = Instant::now();
                let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text) else {
                    continue;
                };
                match env.msg_type.as_str() {
                    "message" | "text" => {
                        // Pipeline LLM response text (skip STT echoes).
                        // Canonical envelope is `"message"` (PipelineMessage with
                        // `kind:"text"`); `"text"` is the deprecated legacy form.
                        let role = env
                            .payload
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if role == "user" {
                            continue;
                        }
                        let is_stt = env
                            .payload
                            .get("message_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|id| {
                                id.starts_with("stt-")
                                    || id.starts_with("utterance-")
                                    || id.starts_with("bench-")
                            });
                        if !is_stt && let Some(t) = env.payload.get("text").and_then(|v| v.as_str())
                        {
                            response_parts.push(t.to_string());
                        }
                    }
                    "audio_meta" => {
                        // In audio-bridge mode the pipeline LLM response text
                        // arrives as audio_meta envelopes alongside TTS audio
                        // frames: per-fragment `text` (caption) and final
                        // `full_text` on the EOU sentinel. Prefer full_text
                        // when present (authoritative) to avoid duplicating
                        // the per-caption text.
                        if let Some(full) = env.payload.get("full_text").and_then(|v| v.as_str()) {
                            response_parts.clear();
                            response_parts.push(full.to_string());
                        } else if let Some(t) = env.payload.get("text").and_then(|v| v.as_str()) {
                            response_parts.push(t.to_string());
                        }
                    }
                    "control" => {
                        if let Some(action) = env.payload.get("action").and_then(|v| v.as_str()) {
                            match action {
                                "turn_metrics" => {
                                    let snap = env.payload.get("snapshot").and_then(|v| {
                                        serde_json::from_value::<
                                            voice_pipes::pipeline::TurnMetricsSnapshot,
                                        >(v.clone())
                                        .ok()
                                    });
                                    match snap {
                                        Some(snap)
                                            if snap.initiator
                                                == voice_pipes::pipeline::TurnInitiator::Agent =>
                                        {
                                            // Agent-initiated turn: record it, but it
                                            // must not complete the caller's turn.
                                            agent_snapshots.push(snap);
                                        }
                                        Some(snap) => {
                                            turn_metrics = Some(snap);
                                            turn_metrics_seen = true;
                                        }
                                        None => turn_metrics_seen = true,
                                    }
                                }
                                "state_changed" => {
                                    let state = env
                                        .payload
                                        .get("state")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if state == "listening" {
                                        audio_terminal_seen = true;
                                    }
                                }
                                "error" => {
                                    let msg = env
                                        .payload
                                        .get("message")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown error");
                                    anyhow::bail!("server error: {msg}");
                                }
                                "closing" => break,
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                last_activity = Instant::now();
                match BinaryHeader::decode(&bytes) {
                    Ok((header, payload)) => {
                        if header.msg_type == voice_pipes::transport::wire::MSG_TYPE_AUDIO_PCM_I16 {
                            let samples: Vec<i16> = payload
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|c| i16::from_le_bytes(*c))
                                .collect();
                            if !samples.is_empty()
                                && let Err(e) = caller.push_audio(&samples).await
                            {
                                warn!(error = %e, "caller push_audio failed");
                            }
                            if header.is_end_of_response() {
                                audio_terminal_seen = true;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "bad pipeline binary header");
                    }
                }
            }
            Ok(Some(Ok(Message::Close(_))) | None) => break,
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => {
                warn!(error = %e, "WS read error in audio bridge");
                break;
            }
            Err(_) => {
                // Settle timeout -- bail out if the pipeline has gone
                // quiet for longer than the cap.
                if last_activity.elapsed() >= settle_timeout {
                    break;
                }
            }
        }

        if turn_metrics_seen && audio_terminal_seen {
            break;
        }
    }

    // ── Trailing-silence flush ──────────────────────────────
    // OpenAI Realtime (and Gemini Live) use server-side VAD that only
    // fires `speech_stopped` → `input_audio_buffer.committed` →
    // `response.create` after observing a trailing-silence window
    // (~500 ms default). The pipeline TTS stream stops abruptly at
    // end-of-turn, so without an explicit silence flush the realtime
    // model sees an open-ended "still speaking" buffer, never commits,
    // and the next caller turn never fires → the runner trips the
    // `caller_silence` cap with `pipeline_turns == 1`.
    //
    // We push 800 ms of zeroed PCM @ 16 kHz mono (12 800 samples) which
    // exceeds the realtime server-VAD default window with margin. For
    // text/mock callers `push_audio` is a no-op, so this is benign.
    {
        const FLUSH_SAMPLES: usize = 16_000 * 800 / 1000; // 12 800
        let silence = vec![0i16; FLUSH_SAMPLES];
        if let Err(e) = caller.push_audio(&silence).await {
            warn!(error = %e, "trailing-silence flush failed");
        }
    }

    let wall_ms = wall_start.elapsed().as_secs_f64() * 1000.0;
    let e2e_ms = turn_metrics
        .as_ref()
        .map(|tm| tm.total_ms as f64)
        .or(Some(wall_ms));

    Ok(RunOutcome {
        session_id,
        stt_text: None,
        response_text: if response_parts.is_empty() {
            None
        } else {
            Some(response_parts.join(""))
        },
        e2e_latency_ms: e2e_ms,
        ttft_ms: turn_metrics
            .as_ref()
            .and_then(|tm| tm.ttft_ms().map(|v| v as f64)),
        ttfb_ms: turn_metrics
            .as_ref()
            .and_then(|tm| tm.ttfb_ms().map(|v| v as f64)),
        audio_duration_s: None,
        stt_latency_ms: None,
        llm_generation_ms: None,
        llm_tokens: None,
        tts_ttfb_ms: None,
        tts_synthesis_ms: None,
        tts_audio_duration_s: None,
        tts_sample_rate: None,
        tts_chars: None,
        tts_rtf: None,
        tts_provider: None,
        tts_cold_start: false,
        tts_error: None,
        // This path completes a turn only when the server actually reported
        // end-of-turn metrics.
        turn_metrics_seen,
        agent_snapshots,
    })
}

pub(super) async fn collect_outcome<S>(
    stream: &mut S,
    session_id: Option<String>,
    scenario_timeout: Duration,
    settle_timeout: Duration,
    wall_start: Instant,
    is_text_only: bool,
) -> anyhow::Result<RunOutcome>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    use voice_pipes::transport::wire::BinaryHeader;

    let mut stt_parts: Vec<String> = Vec::new();
    let mut response_parts: Vec<String> = Vec::new();
    let mut turn_metrics: Option<voice_pipes::pipeline::TurnMetricsSnapshot> = None;
    let mut agent_snapshots: Vec<voice_pipes::pipeline::TurnMetricsSnapshot> = Vec::new();
    let mut injection_done = is_text_only;
    let mut audio_duration_s: Option<f64> = None;
    let mut pipeline_done = false;
    // Text-only turns start with `injection_done = true`, so a stray
    // `state_changed: listening` would end the turn immediately. Realtime
    // (S2S) sessions emit exactly that when the provider session finishes
    // opening -- often *after* the client already submitted its text -- which
    // made the collector declare turn 1 done at provider-connect time and
    // fire turn 2 into the provider's still-active response
    // (`conversation_already_has_active_response`). Only honour `listening`
    // as end-of-turn once the pipeline has been seen `processing` this turn.
    let mut processing_seen = !is_text_only;
    // The `turn_metrics` envelope trails the terminal state/audio signal
    // (the runner emits TurnCompleted -> listening -> TurnMetricsReady).
    // Returning the instant the state signal lands loses the snapshot --
    // the turn reports `completed: false`, server-side latency is dropped,
    // and the late envelope bleeds into the NEXT turn's collect. When the
    // turn completes via a state/audio signal alone, linger briefly for
    // the trailing snapshot instead of returning immediately.
    let mut metrics_grace: Option<Instant> = None;

    let deadline = Instant::now() + scenario_timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            warn!("scenario timed out");
            break;
        }

        // After injection is done, switch to a shorter settle timeout
        // so we don't wait forever if the pipeline produces no more output.
        let read_timeout = if let Some(g) = metrics_grace {
            g.saturating_duration_since(Instant::now())
                .min(settle_timeout)
        } else if injection_done {
            remaining.min(settle_timeout)
        } else {
            remaining
        };

        match timeout(read_timeout, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text) {
                    match env.msg_type.as_str() {
                        "message" | "text" => {
                            // Canonical envelope is `"message"` (PipelineMessage
                            // with `kind:"text"`); `"text"` is the deprecated
                            // legacy form.
                            if let Some(t) = env.payload.get("text").and_then(|v| v.as_str()) {
                                // Skip the pipeline's broadcast echo of the
                                // user's own injected text. Without this guard
                                // the text-bridge runner records its own input
                                // as the "pipeline response", so the scenario
                                // bundle shows `caller == pipeline` transcripts.
                                let role = env
                                    .payload
                                    .get("role")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if role == "user" {
                                    continue;
                                }
                                // Heuristic: messages with "message_id" starting with
                                // "stt-", "utterance-", or "bench-" are user/STT
                                // input, not LLM output.
                                let is_stt = env
                                    .payload
                                    .get("message_id")
                                    .and_then(|v| v.as_str())
                                    .is_some_and(|id| {
                                        id.starts_with("stt-")
                                            || id.starts_with("utterance-")
                                            || id.starts_with("bench-")
                                    });
                                if is_stt {
                                    stt_parts.push(t.to_string());
                                } else {
                                    response_parts.push(t.to_string());
                                }
                            }
                        }
                        "audio_meta" => {
                            // When TTS is enabled the LLM response text only
                            // surfaces as `audio_meta` captions alongside the
                            // binary TTS frames -- a plain `message` envelope
                            // for the assistant turn may never be emitted.
                            // Mirror `collect_outcome_audio_bridge`: prefer
                            // `full_text` (authoritative final caption) when
                            // present, otherwise concatenate per-fragment
                            // `text` captions.
                            if let Some(full) =
                                env.payload.get("full_text").and_then(|v| v.as_str())
                            {
                                response_parts.clear();
                                response_parts.push(full.to_string());
                            } else if let Some(t) = env.payload.get("text").and_then(|v| v.as_str())
                            {
                                response_parts.push(t.to_string());
                            }
                        }
                        "data" => {
                            let kind = env
                                .payload
                                .get("kind")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if kind == "injection_complete" {
                                injection_done = true;
                                if let Some(inner) = env.payload.get("payload") {
                                    audio_duration_s = inner
                                        .get("audio_duration_s")
                                        .and_then(serde_json::Value::as_f64);
                                }
                                debug!("injection_complete received");
                            }
                        }
                        "control" => {
                            if let Some(action) = env.payload.get("action").and_then(|v| v.as_str())
                            {
                                match action {
                                    "turn_metrics" => {
                                        // Wire shape: { action: "turn_metrics",
                                        // snapshot: TurnMetricsSnapshot }
                                        let snap = env.payload.get("snapshot").and_then(|v| {
                                            serde_json::from_value::<
                                                voice_pipes::pipeline::TurnMetricsSnapshot,
                                            >(v.clone())
                                            .ok()
                                        });
                                        match snap {
                                            Some(snap)
                                                if snap.initiator
                                                    == voice_pipes::pipeline::TurnInitiator::Agent =>
                                            {
                                                // Agent-initiated turn: record it, but
                                                // it must not complete the scripted turn.
                                                agent_snapshots.push(snap);
                                            }
                                            Some(snap) => {
                                                turn_metrics = Some(snap);
                                                // TurnMetrics arrives after the full
                                                // pipeline pass -> pipeline done.
                                                pipeline_done = true;
                                            }
                                            None => pipeline_done = true,
                                        }
                                    }
                                    "state_changed" => {
                                        let state = env
                                            .payload
                                            .get("state")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        if state == "processing" {
                                            processing_seen = true;
                                        }
                                        if state == "listening" && injection_done && processing_seen
                                        {
                                            // Pipeline returned to Listening after
                                            // injection -- all processing complete.
                                            pipeline_done = true;
                                        }
                                    }
                                    "error" => {
                                        let msg = env
                                            .payload
                                            .get("message")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown error");
                                        anyhow::bail!("server error: {msg}");
                                    }
                                    "closing" => {
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                // Some pipelines (notably text-bridge with binary TTS)
                // may not emit a final text/control marker for the turn.
                // Honor end-of-response on binary frames so we don't sit
                // until settle_timeout and report inflated wall latency.
                if let Ok((header, _payload)) = BinaryHeader::decode(&bytes)
                    && header.msg_type == voice_pipes::transport::wire::MSG_TYPE_AUDIO_PCM_I16
                    && header.is_end_of_response()
                {
                    pipeline_done = true;
                }
            }
            Ok(Some(Ok(Message::Close(_))) | None) => break,
            Ok(Some(Ok(_))) => {} // Ping/Pong
            Ok(Some(Err(e))) => {
                warn!(error = %e, "WS read error");
                break;
            }
            Err(_) => {
                // Timeout -- if injection + pipeline are done, this is expected.
                if injection_done {
                    break;
                }
                anyhow::bail!("scenario timed out waiting for injection_complete");
            }
        }

        if pipeline_done && injection_done {
            if turn_metrics.is_some() {
                break;
            }
            match metrics_grace {
                None => metrics_grace = Some(Instant::now() + TURN_METRICS_GRACE),
                Some(g) if Instant::now() >= g => break,
                Some(_) => {}
            }
        }
    }

    let wall_ms = wall_start.elapsed().as_secs_f64() * 1000.0;

    // Prefer server-reported TurnMetrics for latency; fall back to wall-clock.
    let e2e_ms = turn_metrics
        .as_ref()
        .map(|tm| tm.total_ms as f64)
        .or(Some(wall_ms));
    let ttft_ms = turn_metrics
        .as_ref()
        .and_then(|tm| tm.ttft_ms().map(|v| v as f64));
    let ttfb_ms = turn_metrics
        .as_ref()
        .and_then(|tm| tm.ttfb_ms().map(|v| v as f64));
    let stt_latency_ms = turn_metrics
        .as_ref()
        .and_then(|tm| tm.stt_latency_ms().map(|v| v as f64));
    let llm_generation_ms = turn_metrics
        .as_ref()
        .and_then(|tm| tm.llm_generation_ms().map(|v| v as f64));
    let llm_tokens_estimate = turn_metrics
        .as_ref()
        .and_then(|tm| tm.component_by_kind("llm"))
        .and_then(|c| c.extras.get("token_estimate"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as u32);
    let llm_tokens_real = turn_metrics
        .as_ref()
        .and_then(|tm| tm.component_by_kind("llm"))
        .and_then(|c| c.extras.get("token_count"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as u32);
    // Prefer the provider-reported real count; fall back to the
    // whitespace-split estimate so summary numerics stay populated for
    // backends that do not return `usage`.
    let llm_tokens = llm_tokens_real.or(llm_tokens_estimate);
    // Realtime S2S turns emit one `realtime` component instead of the
    // stt/llm/tts slices; its extras carry the same `ttfb_ms` (plus
    // audio_output_ms), so fall back to it for the audio-side numbers.
    let tts_comp = turn_metrics.as_ref().and_then(|tm| {
        tm.component_by_kind("tts")
            .or_else(|| tm.component_by_kind("realtime"))
    });
    let tts_ttfb_ms = tts_comp
        .and_then(|c| c.extras.get("ttfb_ms"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as f64);
    let tts_synthesis_ms = tts_comp
        .and_then(|c| c.extras.get("synthesis_ms"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as f64);
    let tts_audio_duration_s = tts_comp
        .and_then(|c| c.extras.get("audio_duration_ms"))
        // Realtime component's name for the same measurement.
        .or_else(|| tts_comp.and_then(|c| c.extras.get("audio_output_ms")))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as f64 / 1000.0);
    let tts_sample_rate = tts_comp
        .and_then(|c| c.extras.get("sample_rate"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as u32);
    let tts_chars = tts_comp
        .and_then(|c| c.extras.get("chars"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as u32);
    let tts_rtf = tts_comp
        .and_then(|c| c.extras.get("rtf"))
        .and_then(serde_json::Value::as_f64);
    let tts_provider = tts_comp.and_then(|c| c.provider.clone());
    let tts_cold_start = tts_comp.is_some_and(|c| c.cold_start);
    let tts_error = tts_comp.and_then(|c| c.error.clone());

    // The server snapshot's `llm_response_text` is the authoritative turn
    // text. The wire delivers the same words on two channels — per-chunk
    // `audio_meta` captions AND assistant text deltas — and providers that
    // emit their full turn text as one delta (ElevenLabs `agent_response`)
    // make the concatenation self-interleave ("The capital of FranceThe
    // capital of France is Paris.is Paris."). Fall back to the concatenation
    // only when the snapshot carries no text.
    let response_text = turn_metrics
        .as_ref()
        .and_then(|tm| tm.llm_response_text.clone())
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            if response_parts.is_empty() {
                None
            } else {
                Some(response_parts.join(""))
            }
        });

    Ok(RunOutcome {
        session_id,
        stt_text: if stt_parts.is_empty() {
            None
        } else {
            Some(stt_parts.join(" "))
        },
        response_text,
        e2e_latency_ms: e2e_ms,
        ttft_ms,
        ttfb_ms,
        audio_duration_s,
        stt_latency_ms,
        llm_generation_ms,
        llm_tokens,
        tts_ttfb_ms,
        tts_synthesis_ms,
        tts_audio_duration_s,
        tts_sample_rate,
        tts_chars,
        tts_rtf,
        tts_provider,
        tts_cold_start,
        tts_error,
        // Real end-of-turn signal, not the wall-clock-backed e2e_ms.
        turn_metrics_seen: turn_metrics.is_some(),
        agent_snapshots,
    })
}

// ── Agent follow-up collection ──────────────────────────────

/// What a follow-up wait observed.
pub(super) struct FollowupOutcome {
    /// Agent-initiated turns whose `turn_metrics` completed in the window.
    pub(super) seen: u32,
    /// Assistant text streamed during the window (the spoken follow-ups).
    pub(super) text: Option<String>,
}

/// Wait for `count` agent-initiated turns (idle prompts, filler
/// follow-ups, proactive speech) after a scripted turn completed. Used
/// by `ConversationTurn::expect_followups`: the scripted-turn collector
/// deliberately ignores agent turns, and this drains them explicitly.
pub(super) async fn collect_agent_followups<S>(
    stream: &mut S,
    count: u32,
    window: Duration,
) -> FollowupOutcome
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut seen = 0u32;
    let mut parts: Vec<String> = Vec::new();
    let deadline = Instant::now() + window;

    while seen < count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text) else {
                    continue;
                };
                match env.msg_type.as_str() {
                    "message" | "text" => {
                        let role = env
                            .payload
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if role == "user" {
                            continue;
                        }
                        if let Some(t) = env.payload.get("text").and_then(|v| v.as_str()) {
                            parts.push(t.to_string());
                        }
                    }
                    "control" => {
                        let action = env
                            .payload
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if action == "turn_metrics" {
                            let initiator = env
                                .payload
                                .get("snapshot")
                                .and_then(|s| s.get("initiator"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("user");
                            if initiator == "agent" {
                                seen += 1;
                            } else {
                                warn!(
                                    "unexpected user-initiated turn_metrics during \
                                     follow-up window -- counting it anyway to avoid a stall"
                                );
                                seen += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => break,
            Ok(Some(Ok(_))) => {}
        }
    }

    FollowupOutcome {
        seen,
        text: if parts.is_empty() {
            None
        } else {
            Some(parts.join(""))
        },
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/collect.rs"]
mod tests;
