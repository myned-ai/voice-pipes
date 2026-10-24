//! Realtime S2S session runner, split across submodules by concern.
//!
//! When a pipeline config contains a `[realtime_provider]` block, the
//! transport dispatcher routes the session here instead of the classic
//! STT -> LLM -> TTS runner in [`super::runner`]. The realtime backend
//! (`OpenAI` Realtime, Gemini Live, …) owns the full speech-to-speech
//! round-trip; this runner is essentially a bidirectional bridge
//! between the transport session and the [`RealtimeClient`].

pub mod context;
pub mod endpoint_observer;
pub mod turn;

#[cfg(test)]
#[path = "tests/suite/mod.rs"]
mod tests;

use tracing::{debug, info, warn};

use crate::realtime::{
    Modalities, RealtimeClient, RealtimeEvent, RealtimeProtocol,
    SessionConfig as RealtimeSessionConfig, ToolRegistry, TurnDetection, build_tool_registry,
};
use crate::transport::session::TransportSession;
use crate::transport::{
    ClientControl, ClientMessage, ErrorCode, PipelineState, ServerControl, ServerMessage,
};

use super::setup;
use super::state::TransportState;

use context::RealtimeSessionContext;
use turn::{PendingInterruption, PlaybackTail, RealtimeTurn};

/// Run a single client session backed by a Realtime S2S provider.
///
/// Returns `(handshake_session_id, result)` mirroring
/// [`super::runner::run_session`]'s inner contract so the outer
/// `run_session` accounting (`active_sessions` counter, lifecycle event,
/// final log line) works uniformly across classic and realtime modes.
#[expect(
    clippy::too_many_lines,
    clippy::similar_names,
    reason = "single select-loop owning the whole session lifecycle; the opt_f32/opt_u32/opt_str helper trio is intentionally parallel"
)]
pub async fn run_realtime_session_inner(
    session: Box<dyn TransportSession + Send>,
    state: &TransportState,
    proto: Box<dyn RealtimeProtocol>,
) -> (Option<String>, Result<Vec<String>, anyhow::Error>) {
    let Some(setup::BootstrappedSession {
        session_id,
        negotiated,
        client_config,
        streaming_audit,
        in_rx,
        out_tx,
        suppress_audio,
        pump_cancel,
        pump_handle,
        observations,
        conditions,
    }) = setup::bootstrap_session(session, state).await
    else {
        return (None, Ok(Vec::new()));
    };
    // The pump polls the raw flag; the runner drives it through the gate so
    // suppression is always tied to a `ClearAudio` the client will answer.
    let suppress_audio = turn::SuppressGate::new(suppress_audio);
    // Keep the inbound channel drained (audio dropped, pings
    // answered, the rest held for replay) while the provider and agent
    // connects run below. The classic runner has done this around its
    // component setup since the "worker not listening" incident; the
    // realtime path had no drain, so a client that started streaming on
    // `session_ready` could stall the WS pump and trip the heartbeat
    // before the loop ever started.
    let session_recorder = setup::setup_session_recorder(state, &session_id);
    let drain = setup::SetupDrain::spawn(
        in_rx,
        out_tx.clone(),
        session_id.clone(),
        session_recorder
            .clone()
            .map(|r| (r, negotiated.input_sample_rate)),
    );
    let client_sample_rate = negotiated.input_sample_rate;
    // Tell the client when the tier moves. Ends with the session.
    crate::transport::adaptive::spawn_quality_reporter(conditions.clone(), out_tx.clone());

    let output_sample_rate = negotiated.output_sample_rate;

    // ── Open the realtime client ────────────────────────────
    // The dispatcher in `run_session` only calls us when
    // `realtime_provider` is Some; the else branch is unreachable
    // unless config is mutated mid-flight (which the runtime does not
    // support today).
    let Some(rt_cfg_ref) = state.config.realtime_provider.as_ref() else {
        let err = anyhow::anyhow!("realtime runner reached without [realtime_provider] config");
        drop(out_tx);
        pump_cancel.cancel();
        return (Some(session_id), Err(err));
    };
    let mut rt_cfg = rt_cfg_ref.clone();

    // Apply updates from the dynamic param store so that mid-flight
    // configuration edits apply to the next session.
    let store_arc = state.param_registry.store("realtime");
    let store_ref = store_arc.as_ref().and_then(|s| {
        s.as_any()
            .downcast_ref::<crate::api::param_store::AtomicParamStore>()
    });
    if let Some(store) = store_ref {
        if let Some(m) = store.string("model") {
            rt_cfg.model = Some((*m).clone());
        }
        if let Some(v) = store.string("voice") {
            rt_cfg.voice = Some((*v).clone());
        }
        if let Some(vad) = store.string("vad") {
            rt_cfg.vad = Some((*vad).clone());
        }
        if let Some(i) = store.string("instructions") {
            rt_cfg.instructions = Some((*i).clone());
        }
    }

    let provider_label = rt_cfg.provider.clone();
    let provider_model = rt_cfg.model.clone();
    info!(
        session_id = %session_id,
        provider = %provider_label,
        model = ?rt_cfg.model,
        voice = ?rt_cfg.voice,
        client_sample_rate,
        output_sample_rate,
        "opening realtime S2S session"
    );

    let emit_rate = rt_cfg
        .output_emit_rate()
        .unwrap_or(crate::config::DEFAULT_OUTPUT_EMIT_RATE);
    let mut client = match RealtimeClient::connect(proto, emit_rate).await {
        Ok(c) => c,
        Err(e) => {
            warn!(
                session_id = %session_id,
                provider = %provider_label,
                error = %e,
                "failed to open realtime provider session"
            );
            let _ = out_tx
                .send(ServerMessage::Control(ServerControl::Error {
                    code: ErrorCode::RealtimeConnectFailed,
                    message: format!("realtime provider connect failed: {e}"),
                    severity: crate::transport::ErrorSeverity::Error,
                    turn_id: None,
                    component: Some(crate::transport::ErrorComponent::Transport),
                }))
                .await;
            drop(out_tx);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), pump_handle).await;
            pump_cancel.cancel();
            return (Some(session_id), Err(e));
        }
    };

    // Map [realtime_provider] → neutral RealtimeSessionConfig.
    //
    // Precedence: per-session overrides from the client's `Configure`
    // payload (`SessionConfig.voice`, `SessionConfig.options.*`) win
    // over the static `[realtime_provider]` worker.toml defaults.
    // This lets the Playground UI tweak voice / instructions / VAD
    // knobs without restarting the worker.
    //
    // Recognised `options` keys (client + worker.toml share the same
    // namespace):
    //   instructions         : string (system prompt)
    //   vad_threshold        : f32   (default 0.7 — bumped from
    //                                  OpenAI's 0.5 because speakers
    //                                  + default 0.5 routinely false-
    //                                  fires on the agent's own TTS)
    //   vad_silence_ms       : u32   (default 500 ms)
    //   vad_prefix_padding_ms: u32   (default 500 ms — gives AEC
    //                                  more head-room before commit)
    let empty_opts: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    // Only allowlisted client options reach the provider path.
    // Everything below (`opt_*`, the provider_options merge, client tools)
    // reads the filtered map; refused keys are logged once here and were
    // already echoed to the client in `session_ready`.
    let filtered_opts = super::client_overrides::filter(
        client_config.as_ref().map_or(&empty_opts, |c| &c.options),
        &rt_cfg.client_overrides,
    );
    if !filtered_opts.ignored.is_empty() {
        warn!(
            session_id = %session_id,
            ignored = ?filtered_opts.ignored,
            "client options refused: not in [realtime_provider] client_overrides"
        );
    }
    let client_opts = &filtered_opts.allowed;
    let worker_opts = &rt_cfg.options;
    let opt_f32 = |k: &str| {
        client_opts
            .get(k)
            .or_else(|| worker_opts.get(k))
            .and_then(serde_json::Value::as_f64)
            .map(|v| v as f32)
    };
    let opt_u32 = |k: &str| {
        client_opts
            .get(k)
            .or_else(|| worker_opts.get(k))
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as u32)
    };
    let opt_str = |k: &str| {
        client_opts
            .get(k)
            .or_else(|| worker_opts.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let turn_detection = match rt_cfg.vad.as_deref() {
        Some("none" | "off") => TurnDetection::None,
        Some("semantic_vad") => TurnDetection::SemanticVad {
            eagerness: opt_str("vad_eagerness"),
        },
        // Default ("server_vad", missing, anything else) keeps provider VAD.
        _ => TurnDetection::ServerVad {
            silence_ms: Some(opt_u32("vad_silence_ms").unwrap_or(500)),
            threshold: Some(opt_f32("vad_threshold").unwrap_or(0.7)),
            prefix_padding_ms: Some(opt_u32("vad_prefix_padding_ms").unwrap_or(500)),
        },
    };
    // Tool dispatch + agent channel. The agent channel is the session's out-of-turn entry
    // point: spawned tool dispatches report completion on it, and the
    // `AgentSessionHandle` given to the tool-registry factory lets the
    // agentic system push late tool results, out-of-turn speech, and
    // rich client messages. Registries come from
    // `register_tool_registry_factory` (empty default).
    let (agent_tx, mut agent_rx) = tokio::sync::mpsc::channel::<crate::agent::AgentEvent>(32);
    let agent_handle =
        crate::agent::AgentSessionHandle::new(agent_tx.clone()).with_session_id(&session_id);
    // Tool registry precedence: a configured `[agent] endpoint` mounts
    // the remote agent as the tool backend; otherwise the
    // compiled-in factory registry (empty default).
    let agent_endpoint = state.config.agent.as_ref().and_then(|a| a.endpoint.clone());
    let tool_registry: ToolRegistry = match agent_endpoint {
        #[cfg(feature = "text-grpc")]
        Some(endpoint) => match crate::agent::grpc_link::GrpcAgentLink::connect(&endpoint).await {
            Ok(link) => {
                let backend = crate::agent::tool_backend::AgentToolBackend::start(
                    std::sync::Arc::new(link),
                    agent_handle.clone(),
                );
                match backend.registry().await {
                    Ok(registry) => registry,
                    Err(e) => {
                        warn!(
                            session_id = %session_id,
                            error = %e,
                            "agent ListTools failed; continuing without agent tools"
                        );
                        build_tool_registry(agent_handle)
                    }
                }
            }
            Err(e) => {
                warn!(
                    session_id = %session_id,
                    endpoint = %endpoint,
                    error = %e,
                    "agent connect failed; continuing without agent tools"
                );
                build_tool_registry(agent_handle)
            }
        },
        #[cfg(not(feature = "text-grpc"))]
        Some(endpoint) => {
            warn!(
                session_id = %session_id,
                endpoint = %endpoint,
                "[agent] endpoint configured but the text-grpc feature is disabled"
            );
            build_tool_registry(agent_handle)
        }
        None => build_tool_registry(agent_handle),
    };
    // Client-side tools: the per-session
    // allowlist merges the worker's static declarations (inline +
    // catalog files, resolved at boot) with tools injected via
    // `Configure.options.client_tools` (later wins on collision).
    // Dispatch is a runner branch, never a `ToolHandler` — the pipeline
    // only routes these to the client. A runtime-injected tool may not
    // take a name the server registry answers to: the client is told
    // and the tool is dropped.
    let session_client_tools = {
        let server_names: std::collections::HashSet<String> = tool_registry
            .tool_specs()
            .into_iter()
            .map(|t| t.name)
            .collect();
        let (runtime_tools, refused) = crate::agent::client_tools::reject_reserved_names(
            crate::agent::client_tools::parse_options_tools(client_opts),
            |name| server_names.contains(name),
        );
        if !refused.is_empty() {
            warn!(
                session_id = %session_id,
                refused = ?refused,
                "client tools refused: names belong to server-side tools"
            );
            let _ = out_tx
                .send(ServerMessage::Control(ServerControl::Error {
                    code: ErrorCode::ClientToolRefused,
                    message: format!(
                        "client tools refused (names belong to server-side tools): {}",
                        refused.join(", ")
                    ),
                    severity: crate::transport::ErrorSeverity::Warning,
                    turn_id: None,
                    component: None,
                }))
                .await;
        }
        crate::agent::client_tools::ClientToolIndex::merge([
            state.client_tools.as_slice(),
            runtime_tools.as_slice(),
        ])
    };
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    // Phantom-transcript window: set on FunctionCall, cleared on audible
    // speech; zero-audio transcripts inside it never reach the client.
    let mut phantom_transcript_guard = false;
    // Dedupe memory for late provider re-delivery of the final user
    // transcription (see RealtimeSessionContext docs).
    let mut last_flushed_user_transcript: Option<String> = None;
    // Per-session client overrides take precedence over worker.toml
    // defaults for voice / system-prompt. An empty client string is
    // treated as "not set" so users can clear an explicit override by
    // blanking the Playground field.
    let voice_override = client_config
        .as_ref()
        .and_then(|c| c.voice.as_ref())
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .or_else(|| rt_cfg.voice.clone());
    // A client addition (only present when `instructions` is in
    // `client_overrides`) is appended after the operator's prompt, never
    // substituted for it.
    let instructions_override = super::client_overrides::merge_instructions(
        rt_cfg.instructions.as_deref(),
        client_opts.get("instructions").and_then(|v| v.as_str()),
    );
    // Optional knowledge base file — appended to instructions at
    // session-start. Read on every connect (cheap; files are small)
    // so operators can edit the KB without restarting the worker.
    // Failures are logged once and treated as "no KB" — we never
    // want a missing/unreadable file to break a live session.
    let kb_text =
        rt_cfg
            .knowledge_base_path
            .as_ref()
            .and_then(|p| match std::fs::read_to_string(p) {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!(
                        session_id = %session_id,
                        path = %p.display(),
                        error = %e,
                        "failed to read realtime knowledge_base_path; continuing without it"
                    );
                    None
                }
            });
    let mut merged_instructions = match (instructions_override.is_empty(), kb_text) {
        (true, Some(kb)) => kb,
        (false, Some(kb)) => format!("{instructions_override}\n\n{kb}"),
        (_, None) => instructions_override,
    };
    // Belt-and-suspenders for the silent-by-default client-tool policy:
    // the per-call ack already instructs the model not to narrate,
    // but a standing system-prompt convention survives providers that
    // gloss over function outputs.
    if !session_client_tools.is_empty() {
        if !merged_instructions.is_empty() {
            merged_instructions.push_str("\n\n");
        }
        merged_instructions.push_str(
            "Some tools are executed silently on the user's device (client-side tools). \
             When you call one, do not narrate, announce, or confirm the action — no \
             \"okay\", \"done\", \"let me check\", or similar. After calling a tool, say \
             nothing and produce no audio until the user speaks again or you are \
             explicitly instructed to respond. Function results and acknowledgements \
             are background data, never something to react to out loud.",
        );
    }
    // Merge provider_options: client overrides win, worker.toml
    // defaults fill the rest. Providers read whichever keys they
    // care about and ignore the rest.
    let mut provider_options = worker_opts.clone();
    for (k, v) in client_opts {
        provider_options.insert(k.clone(), v.clone());
    }
    // Advertised tools = server (agent) tools + client tools. The runner
    // checks the client allowlist first at dispatch time, so a shadowed
    // server spec is dropped here to keep the provider's declared list
    // collision-free.
    let mut tools = tool_registry.tool_specs();
    if !session_client_tools.is_empty() {
        tools.retain(|t| {
            let shadowed = session_client_tools.lookup(&t.name).is_some();
            if shadowed {
                warn!(
                    session_id = %session_id,
                    tool = %t.name,
                    "server tool shadowed by a client tool of the same name"
                );
            }
            !shadowed
        });
        tools.extend(session_client_tools.tool_specs());
    }
    // An agent answering a person: the defaults wait for the user to speak
    // and transcribe what they say.
    let rt_session_cfg = RealtimeSessionConfig::new(merged_instructions)
        .with_voice(voice_override)
        // OpenAI Realtime GA `output_modalities` rejects the combined
        // `["audio", "text"]` value (`invalid_value` on session.update);
        // it only accepts `["audio"]` or `["text"]`. We want spoken
        // replies plus the transcript-via-audio path, so request audio
        // only — providers still surface the agent transcript via
        // `response.audio_transcript.delta` events independently of the
        // modality vector. Gemini Live's `responseModalities` accepts
        // either value too, so `Audio` is the portable default.
        .with_modalities(Modalities::Audio)
        .with_turn_detection(turn_detection)
        .with_tools(tools)
        .with_provider_options(provider_options);
    if let Err(e) = client.apply_session_config(rt_session_cfg.clone()).await {
        warn!(
            session_id = %session_id,
            provider = %provider_label,
            error = %e,
            "failed to apply realtime session config"
        );
        let _ = out_tx
            .send(ServerMessage::Control(ServerControl::Error {
                code: ErrorCode::RealtimeConfigureFailed,
                message: format!("realtime session_update failed: {e}"),
                severity: crate::transport::ErrorSeverity::Error,
                turn_id: None,
                component: Some(crate::transport::ErrorComponent::Transport),
            }))
            .await;
        drop(out_tx);
        let _ = client.close().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), pump_handle).await;
        pump_cancel.cancel();
        return (Some(session_id), Err(e));
    }

    // Mirror the classic runner: announce `Listening` once the provider
    // session is wired up so the client transitions out of its initial
    // "connecting" state. Without this, the admin-UI playground keeps
    // its playback `AudioContext` un-started (it only calls
    // `playback.start()` on `state -> speaking`) and every enqueued
    // chunk is silently dropped because `ctx` is null.
    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Listening,
        }))
        .await;

    // ── Main bridge loop ────────────────────────────────────
    let mut audio_chunks_uplinked: u64 = 0;
    let mut audio_chunks_downlinked: u64 = 0;
    let mut session_audio_emitted_ms: u64 = 0;
    let mut loop_err: Option<anyhow::Error> = None;

    // Per-turn metric accumulator. A "turn" in realtime terms is the
    // span from a provider `ResponseStarted` to `ResponseCompleted`
    // (or `ResponseCancelled`). We index turns by a monotonically-
    // incrementing `utterance_id` so wire-level surfaces (audit
    // captures, `TurnMetricsReady`, `PipelineMessage.message_id`) line
    // up with the dashboard.
    // This session's own event channel, recorded into its capture. Not the
    // worker-wide one: see `session_events` for what sharing it cost.
    let pe_tx = super::session_events::session_event_channel(
        streaming_audit.as_ref(),
        &state.pipeline_event_tx,
    );

    // The base is advertised to clients in `session_ready`; keep them one value.
    let mut next_utterance_id: u32 = crate::transport::FIRST_UTTERANCE_ID;
    let mut current_turn: Option<RealtimeTurn> = None;
    // Accumulated input-audio duration in milliseconds, summed across
    // every uplinked chunk and snapped onto the current turn at
    // `ResponseCompleted` time. Reset per turn.
    let mut input_audio_ms_acc: u64 = 0;
    // Most-recently-flushed interrupted turn waiting for a client
    // `PlaybackReport` so we can (a) tell the client the authoritative
    // truncated assistant text via `TextTruncated` and (b) tell the
    // provider to prune its conversation history to what was actually
    // heard (`conversation.item.truncate`). Replaced on every new
    // interruption; cleared once consumed.
    let mut pending_interruption: Option<PendingInterruption> = None;

    // ── Post-completion playback tail ───────────────────────
    // The just-completed turn the client may still be playing (S2S audio
    // arrives much faster than realtime). Keeps the audible tail
    // interruptible: a `speech_started` here becomes a barge-in instead
    // of a no-op. Armed on `ResponseCompleted`, cleared on the client's
    // `PlaybackComplete` or wall-clock expiry. See `turn::PlaybackTail`.
    let mut playback_tail: Option<PlaybackTail> = None;
    // Client playback health for the most recent turn.
    // Shared between the control-frame handler and the persist_turn
    // closure: the quality frame usually arrives after the server-side turn
    // completed, so `persist_turn` takes whatever is present at build time
    // and the capture artifact (written on arrival) is the always-correct
    // consumer.
    let pending_playback_health: std::sync::Arc<
        parking_lot::Mutex<Option<crate::api::types::PlaybackHealth>>,
    > = std::sync::Arc::new(parking_lot::Mutex::new(None));

    // ── Input-audio capture buffer ──────────────────────────
    // Realtime providers do not surface the user's microphone audio
    // back to us, so we have to keep our own copy if we want the
    // admin-UI Audio Capture widget to play "INPUT (MIC)". We append
    // every uplinked sample here and flush as a single
    // `AuditEvent::MicRaw` (keyed by the turn's `utterance_id`) at
    // every turn-finish site. The buffer is intentionally session-
    // scoped: user audio frequently arrives before `ResponseStarted`,
    // so we cannot attribute it to a turn until the turn actually
    // exists. Sample rate is the last seen client rate (chunks are
    // already stamped with `client_sample_rate` upstream).
    let mut pending_input_audio: Vec<i16> = Vec::new();
    let mut pending_input_rate: u32 = client_sample_rate;

    // ── Pending user transcript ────────────────────────────
    // The realtime provider returns the user's transcribed words on
    // a different event than its audio output, so the
    // `TurnMetricsSnapshot` we build at turn-finish time does not
    // carry it. Buffer the most recent final transcript here and
    // drain it into the persisted `TurnRecord.detail.stt.text` so
    // the admin Sessions detail view shows the user's words in the
    // turn row (mirroring how classic STT pipelines populate the
    // same field).
    let mut pending_user_transcript: Option<String> = None;
    let mut pending_user_transcript_flushed = false;
    let mut ignoring_stray_deltas = false;
    // TTFB anchor: the instant the user finished speaking (provider
    // `InputSpeechStopped`, the last `InputTranscript` before a response,
    // or a text submission) captured before the assistant turn exists.
    // Drained into the turn's `ttfb_anchor` at creation so realtime TTFB
    // is measured from end-of-user-speech rather than first-audio-byte,
    // and its wall clock is stamped into the turn snapshot as
    // `user_speech_end_epoch_ms` for the admin perceived-E2E metric.
    let mut request_anchor: Option<turn::RequestAnchor> = None;
    // vp's own end-of-speech observer on the uplink and its latest stamp.
    // Measurement only.
    let mut endpoint_observer = endpoint_observer::EndpointObserver::from_config(
        state
            .config
            .realtime_provider
            .as_ref()
            .and_then(|c| c.endpoint_observer.as_ref()),
    );
    let mut vp_speech_end: Option<turn::SpeechEndStamp> = None;

    // ── Agent-event delivery state ─────────────────────────
    // `responses` mirrors the provider response lifecycle
    // (ResponseStarted → Completed/Cancelled, matched by response id where
    // the provider has one) so agent output never races an in-flight
    // response (providers reject concurrent responses). `kick_pending`
    // defers a post-tool-output `response.create`; `pending_agent` queues
    // `WhenIdle` events. All three are select-loop locals: single-writer
    // by construction.
    let mut responses = turn::ResponseTracker::default();
    let mut withdrawn_calls = turn::WithdrawnCalls::default();
    // Whether this session can resume on a new connection, and whether
    // the provider has said the current one is closing.
    let mut resumption = turn::Resumption::default();
    let mut going_away = false;
    let mut kick_pending = false;
    let mut pending_agent: std::collections::VecDeque<crate::agent::AgentEvent> =
        std::collections::VecDeque::new();

    // ── Provider-close storm tracking ──────────────────────
    // Closes spaced closer than this window count as one escalating
    // streak (see the `RealtimeEvent::Closed` arm); reconnects stop at
    // `MAX_RAPID_PROVIDER_CLOSES`. Sized comfortably above the 1 s
    // reconnect back-off + connect time so a dying-on-arrival
    // connection always lands inside the window.
    const RAPID_CLOSE_WINDOW: std::time::Duration = std::time::Duration::from_secs(15);
    let mut rapid_closes: u32 = 0;
    let mut total_provider_closes: u32 = 0;
    let mut last_provider_close: Option<tokio::time::Instant> = None;

    // Captured once so we can persist a `TurnRecord` for every
    // completed (or interrupted) realtime turn without re-reading
    // `state.config` / `client_config` from inside the hot loop. This
    // mirrors what the classic runner does via `build_turn_record`
    // and is what makes realtime sessions show up with non-zero
    // turn counts (and per-turn detail rows) in the admin Sessions
    // view.
    let pipeline_name_for_turns = state.config.pipeline.name.clone();
    let session_tags_for_turns: Vec<String> = client_config
        .as_ref()
        .map(|c| c.tags.clone())
        .unwrap_or_default();
    let turn_record_tx = state.turn_complete_tx.clone();
    let session_id_for_turns = session_id.clone();
    let provider_label_for_turns = provider_label.clone();
    let playback_health_for_turns = std::sync::Arc::clone(&pending_playback_health);
    let persist_turn = |snapshot: crate::pipeline::metrics::TurnMetricsSnapshot,
                        user_text: Option<String>| {
        let Some(tx) = turn_record_tx.as_ref() else {
            return;
        };
        let record = super::turn::build_realtime_turn_record(
            &session_id_for_turns,
            &snapshot,
            &provider_label_for_turns,
            client_sample_rate,
            output_sample_rate,
            &pipeline_name_for_turns,
            session_tags_for_turns.clone(),
            user_text,
            playback_health_for_turns.lock().take(),
        );
        super::turn::emit_record(tx, record, "turn");
    };

    // Post-audio chain, shared with the cascade runner (same instances run
    // for every connection). Threaded into the context so provider audio and
    // the EOU sentinel ride the same `audio_meta.extras` path as cascade.
    let session_post_processors = std::sync::Arc::clone(&state.audio_post_processors);
    // Off-path annotators: one hub task each; dropped with the session.
    let annotators = crate::pipeline::AnnotatorHubs::spawn(
        &state.annotators,
        &session_id,
        out_tx.clone(),
        output_sample_rate,
    );

    macro_rules! make_ctx {
        () => {
            RealtimeSessionContext {
                session_id: &session_id,
                provider_label: &provider_label,
                provider_model: &provider_model,
                client: &mut client,
                out_tx: &out_tx,
                post_processors: session_post_processors.as_slice(),
                annotators: &annotators,
                session_recorder: session_recorder.as_ref(),
                observations: observations.clone(),
                suppress_audio: &suppress_audio,
                streaming_audit: &streaming_audit,
                pe_tx: &pe_tx,
                client_sample_rate,
                output_sample_rate,
                audio_chunks_uplinked: &mut audio_chunks_uplinked,
                audio_chunks_downlinked: &mut audio_chunks_downlinked,
                next_utterance_id: &mut next_utterance_id,
                current_turn: &mut current_turn,
                input_audio_ms_acc: &mut input_audio_ms_acc,
                pending_interruption: &mut pending_interruption,
                playback_tail: &mut playback_tail,
                pending_input_audio: &mut pending_input_audio,
                pending_input_rate: &mut pending_input_rate,
                pending_user_transcript: &mut pending_user_transcript,
                pending_user_transcript_flushed: &mut pending_user_transcript_flushed,
                ignoring_stray_deltas: &mut ignoring_stray_deltas,
                session_audio_emitted_ms: &mut session_audio_emitted_ms,
                request_anchor: &mut request_anchor,
                endpoint_observer: &mut endpoint_observer,
                vp_speech_end: &mut vp_speech_end,
                persist_turn: &persist_turn,
                pending_playback_health: &pending_playback_health,
                client_tools: &session_client_tools,
                client_calls: &mut client_calls,
                phantom_transcript_guard: &mut phantom_transcript_guard,
                last_flushed_user_transcript: &mut last_flushed_user_transcript,
            }
        };
    }

    // Inferred-completion idle window (ElevenLabs — no end-of-response
    // wire event; see RealtimeProtocol::inferred_completion_idle_ms).
    // Re-armed on every audio/text/function-call event of a response;
    // when it fires, the response is treated as completed so the client
    // leaves "speaking", the turn persists, and queued agent events
    // drain — instead of all of that waiting for the *next* turn.
    // Setup complete: reclaim the inbound receiver (held frames first).
    let mut in_rx = match drain.finish().await {
        Ok(rx) => rx,
        Err(join_err) => {
            warn!(
                session_id = %session_id,
                error = %join_err,
                "setup drain task panicked; closing session"
            );
            let _ = client.close().await;
            drop(out_tx);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), pump_handle).await;
            pump_cancel.cancel();
            return (
                Some(session_id),
                Err(anyhow::anyhow!("setup drain task panicked: {join_err}")),
            );
        }
    };

    let inferred_idle: Option<std::time::Duration> = client
        .inferred_completion_idle_ms()
        .map(std::time::Duration::from_millis);
    let mut response_idle_deadline: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            biased;

            // ── Worker shutting down ────────────────────────
            () = state.shutdown.cancelled() => {
                info!(session_id = %session_id, "worker shutting down; ending the session");
                break;
            }

            // ── Inbound from transport ──────────────────────
            msg = in_rx.recv() => {
                match msg {
                    Some(ClientMessage::Audio(chunk)) => {
                        let mut ctx = make_ctx!();
                        if let Err(e) = ctx.handle_client_audio(chunk).await {
                            warn!(
                                session_id = %session_id,
                                provider = %provider_label,
                                error = %e,
                                "realtime send_audio failed"
                            );
                            loop_err = Some(e);
                            break;
                        }
                    }
                    Some(ClientMessage::Control(ClientControl::Cancel)) => {
                        let mut ctx = make_ctx!();
                        if let Err(e) = ctx.handle_client_cancel().await {
                            warn!(
                                session_id = %session_id,
                                error = %e,
                                "realtime cancel failed"
                            );
                        }
                    }
                    Some(ClientMessage::Control(ClientControl::Ping { timestamp_ms })) => {
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_client_ping(timestamp_ms).await;
                    }
                    Some(ClientMessage::Control(ClientControl::PlaybackReport {
                        utterance_id,
                        last_played_offset_ms,
                        last_played_text,
                    })) => {
                        let mut ctx = make_ctx!();
                        // Local persistence/events are completed before the
                        // provider call inside the handler; a
                        // remaining error means the provider's history kept
                        // the unheard tail — loud, not silent.
                        if let Err(e) = ctx.handle_client_playback_report(utterance_id, last_played_offset_ms, last_played_text).await {
                            warn!(
                                utterance_id,
                                error = %e,
                                "playback report: provider history truncation failed \
                                 (turn persisted locally; provider may keep unheard text)"
                            );
                        }
                    }
                    Some(ClientMessage::Control(ClientControl::PlaybackComplete { utterance_id })) => {
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_client_playback_complete(utterance_id);
                    }
                    Some(ClientMessage::Control(ClientControl::PlaybackQuality {
                        utterance_id, underrun_count, total_silence_ms, audio_ms, heard_ms, wall_ms, gaps,
                        arrival_wall_ms, interrupted, annotation_late_count, annotation_late_ms_max,
                    })) => {
                        let ctx = make_ctx!();
                        ctx.handle_client_playback_quality(utterance_id, crate::api::types::PlaybackHealth {
                            underrun_count, total_silence_ms, audio_ms, heard_ms, wall_ms, gaps,
                            arrival_wall_ms, interrupted, annotation_late_count, annotation_late_ms_max,
                        });
                    }
                    Some(ClientMessage::Control(ClientControl::TimingReport {
                        utterance_id, event, client_timestamp_ms, ..
                    })) => {
                        let mut ctx = make_ctx!();
                        ctx.handle_client_timing_report(utterance_id, event, client_timestamp_ms);
                    }
                    Some(ClientMessage::Control(ClientControl::ClientToolResult {
                        call_id, phase, result, speak,
                    })) => {
                        let mut ctx = make_ctx!();
                        // A resolved Completed/Failed becomes a Generate
                        // event: Silent = context-only injection, WhenIdle
                        // (opt-in speak) = generated confirmation that
                        // queues behind any active response.
                        if let Some(ev) = ctx.resolve_client_tool_result(
                            &call_id, phase, result.as_ref(), speak,
                        ) {
                            let _ = ctx.handle_agent_event(
                                ev,
                                &mut responses,
                                &mut kick_pending,
                                &mut pending_agent,
                            ).await;
                        }
                    }
                    Some(ClientMessage::Control(ClientControl::InjectAudio { .. })) => {
                        warn!(
                            session_id = %session_id,
                            "InjectAudio is not supported in realtime sessions yet"
                        );
                    }
                    Some(ClientMessage::Control(ClientControl::TtsOnlySynthesize { id, .. })) => {
                        let _ = out_tx
                            .send(ServerMessage::Control(ServerControl::TtsOnlyError {
                                id,
                                error: "TtsOnlySynthesize is not supported on realtime providers".to_string(),
                            }))
                            .await;
                    }
                    Some(ClientMessage::Control(ClientControl::NetReport {
                        playback_buffer_ms,
                        underruns,
                        audio_received_ms,
                        wall_ms,
                        uplink_backlog_bytes,
                    })) => {
                        // S3: the in-flight view. `PlaybackQuality` says the
                        // same thing more accurately but only once the turn is
                        // over, which is too late to act on during a long one.
                        let arrival_rtf = (audio_received_ms > 0 && wall_ms > 0)
                            .then(|| wall_ms as f32 / audio_received_ms as f32);
                        observations.observe(crate::transport::adaptive::Observation {
                            arrival_rtf,
                            underruns: Some(underruns),
                            uplink_backlog_bytes: Some(uplink_backlog_bytes),
                            ..Default::default()
                        });
                        let _ = playback_buffer_ms; // reported for diagnostics
                    }
                    Some(ClientMessage::Control(
                        ClientControl::Configure(_) | ClientControl::Resume { .. },
                    )) => {
                        warn!(
                            session_id = %session_id,
                            "duplicate Configure/Resume on an established realtime session -- ignoring"
                        );
                    }
                    Some(ClientMessage::Message(pm)) => {
                        let mut ctx = make_ctx!();
                        if let Err(e) = ctx.handle_client_message(pm).await {
                            warn!(
                                session_id = %session_id,
                                provider = %provider_label,
                                error = %e,
                                "realtime send_text failed"
                            );
                        }
                    }
                    None => {
                        debug!(session_id = %session_id, "transport closed");
                        break;
                    }
                }
            }

            // ── Agent events (spawned tool dispatches + agentic
            // system via AgentSessionHandle) ─────────────────
            agent_ev = agent_rx.recv() => {
                // `agent_tx` is held by this function, so recv() only
                // returns None at shutdown-teardown; treat it as such.
                let Some(agent_ev) = agent_ev else {
                    debug!(session_id = %session_id, "agent channel closed");
                    break;
                };
                if let crate::agent::AgentEvent::ToolDispatchDone { call_id, name, .. } = &agent_ev
                    && withdrawn_calls.take(call_id)
                {
                    debug!(
                        session_id = %session_id,
                        call_id = %call_id,
                        tool = %name,
                        "tool finished after the provider withdrew its call; result not sent"
                    );
                    continue;
                }
                let mut ctx = make_ctx!();
                let _ = ctx.handle_agent_event(
                    agent_ev,
                    &mut responses,
                    &mut kick_pending,
                    &mut pending_agent,
                ).await;
            }

            // ── Inbound from realtime provider ──────────────
            ev = client.next_event() => {
                let Some(ev) = ev else {
                    debug!(session_id = %session_id, "realtime event stream ended");
                    break;
                };
                match ev {
                    RealtimeEvent::ResponseAudioDelta { samples, sample_rate, alignment } => {
                        if let Some(d) = inferred_idle {
                            response_idle_deadline = Some(tokio::time::Instant::now() + d);
                        }
                        let mut ctx = make_ctx!();
                        let alignment = alignment
                            .map(|words| words.into_iter().map(Into::into).collect());
                        let _ = ctx.handle_provider_audio_delta(samples, sample_rate, alignment).await;
                    }
                    RealtimeEvent::ResponseTextDelta { text } => {
                        if let Some(d) = inferred_idle {
                            response_idle_deadline = Some(tokio::time::Instant::now() + d);
                        }
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_provider_text_delta(text).await;
                    }
                    RealtimeEvent::InputTranscript { text, is_final } => {
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_provider_input_transcript(text, is_final).await;
                    }
                    RealtimeEvent::ResponseCompleted { response_id } => {
                        response_idle_deadline = None;
                        let verdict = responses.on_terminal(response_id.as_deref());
                        if verdict == turn::TerminalVerdict::Stale {
                            debug!(
                                session_id = %session_id,
                                response_id = ?response_id,
                                "ignoring completion of a response that is no longer tracked"
                            );
                        } else {
                            {
                                let mut ctx = make_ctx!();
                                let _ = ctx.handle_provider_response_completed().await;
                            }
                            // Only the active response finishing frees the
                            // provider for queued agent work: the deferred
                            // post-tool-output kick first, then WhenIdle
                            // events until one of them starts a response.
                            if verdict == turn::TerminalVerdict::Current {
                                if kick_pending {
                                    kick_pending = false;
                                    let mut ctx = make_ctx!();
                                    ctx.request_deferred_kick(&mut responses).await;
                                }
                                while !responses.is_active() {
                                    let Some(ev) = pending_agent.pop_front() else { break };
                                    let mut ctx = make_ctx!();
                                    let _ = ctx.handle_agent_event(
                                        ev,
                                        &mut responses,
                                        &mut kick_pending,
                                        &mut pending_agent,
                                    ).await;
                                }
                            }
                        }
                    }
                    RealtimeEvent::ResponseCancelled { response_id } => {
                        response_idle_deadline = None;
                        let verdict = responses.on_terminal(response_id.as_deref());
                        if verdict == turn::TerminalVerdict::Stale {
                            debug!(
                                session_id = %session_id,
                                response_id = ?response_id,
                                "ignoring cancellation of a response that is no longer tracked"
                            );
                        } else {
                            {
                                let mut ctx = make_ctx!();
                                let _ = ctx.handle_provider_response_cancelled().await;
                            }
                            if verdict == turn::TerminalVerdict::Current {
                                if kick_pending {
                                    kick_pending = false;
                                    let mut ctx = make_ctx!();
                                    ctx.request_deferred_kick(&mut responses).await;
                                }
                                while !responses.is_active() {
                                    let Some(ev) = pending_agent.pop_front() else { break };
                                    let mut ctx = make_ctx!();
                                    let _ = ctx.handle_agent_event(
                                        ev,
                                        &mut responses,
                                        &mut kick_pending,
                                        &mut pending_agent,
                                    ).await;
                                }
                            }
                        }
                    }
                    RealtimeEvent::ResponseStarted { response_id } => {
                        responses.on_started(response_id);
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_provider_response_started().await;
                    }
                    RealtimeEvent::InputSpeechStarted => {
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_provider_input_speech_started().await;
                    }
                    RealtimeEvent::ResponseOutputItemAdded { item_id } => {
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_provider_item_added(item_id);
                    }
                    RealtimeEvent::InputSpeechStopped => {
                        let mut ctx = make_ctx!();
                        ctx.note_user_speech_end();
                    }
                    RealtimeEvent::SessionUpdated => resumption.confirmed(),
                    RealtimeEvent::InputAudioCommitted => {}
                    RealtimeEvent::ResponseUsage {
                        response_id,
                        total_tokens,
                        input_tokens,
                        output_tokens,
                        input_text_tokens,
                        input_audio_tokens,
                        input_cached_tokens,
                        output_text_tokens,
                        output_audio_tokens,
                    } => {
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_provider_usage(
                            response_id,
                            total_tokens,
                            input_tokens,
                            output_tokens,
                            input_text_tokens,
                            input_audio_tokens,
                            input_cached_tokens,
                            output_text_tokens,
                            output_audio_tokens,
                        );
                    }
                    RealtimeEvent::RateLimits { items } => {
                        let mut ctx = make_ctx!();
                        let _ = ctx.handle_provider_rate_limits(&items);
                    }
                    RealtimeEvent::FunctionCall { call_id, name, arguments } => {
                        // A tool call means the response is still alive
                        // (the model may speak again after the result) —
                        // give it a fresh idle window.
                        if let Some(d) = inferred_idle
                            && response_idle_deadline.is_some()
                        {
                            response_idle_deadline = Some(tokio::time::Instant::now() + d);
                        }
                        let mut ctx = make_ctx!();
                        ctx.handle_provider_function_call(call_id, name, arguments, &tool_registry, &agent_tx).await;
                    }
                    RealtimeEvent::FunctionCallsCancelled { call_ids } => {
                        debug!(session_id = %session_id, call_ids = ?call_ids, "provider withdrew tool calls");
                        withdrawn_calls.withdraw(call_ids);
                    }
                    RealtimeEvent::SessionResumable { token } => resumption.issued(token),
                    RealtimeEvent::GoingAway { time_left } => {
                        info!(
                            session_id = %session_id,
                            time_left = ?time_left,
                            resumable = resumption.can_resume(),
                            "provider is closing the connection"
                        );
                        going_away = resumption.can_resume();
                    }
                    RealtimeEvent::Error { code, message, recoverable } => {
                        let mut ctx = make_ctx!();
                        match ctx.handle_provider_error(code, message, recoverable).await {
                            Ok(true) => {}
                            _ => {
                                break;
                            }
                        }
                    }
                    RealtimeEvent::Closed { code, reason } => {
                        response_idle_deadline = None;
                        // Rapid-close counter: a close arriving shortly after
                        // the previous one means the last reconnect died
                        // immediately — a permanent condition the classifier
                        // may not recognise by text. Reset when connections
                        // survive past the window.
                        let now = tokio::time::Instant::now();
                        rapid_closes = match last_provider_close {
                            Some(prev) if now.duration_since(prev) < RAPID_CLOSE_WINDOW => {
                                rapid_closes + 1
                            }
                            _ => 1,
                        };
                        last_provider_close = Some(now);
                        total_provider_closes += 1;
                        let mut ctx = make_ctx!();
                        match ctx
                            .handle_provider_closed(
                                code,
                                reason,
                                rapid_closes,
                                total_provider_closes,
                                &rt_session_cfg,
                                state,
                                &mut resumption,
                                &mut in_rx,
                            )
                            .await
                        {
                            Ok(true) => {
                                going_away = false;
                                // A reconnect is a fresh provider
                                // session. Anything we believed about the old
                                // one is stale: a response that was active when
                                // it died will never complete, so agent events
                                // queued behind it would wait forever and the
                                // next real response could be dropped as a
                                // "stray delta".
                                responses.reset();
                                withdrawn_calls.reset();
                                kick_pending = false;
                                *ctx.ignoring_stray_deltas = false;
                                while !responses.is_active() {
                                    let Some(ev) = pending_agent.pop_front() else { break };
                                    let _ = ctx
                                        .handle_agent_event(
                                            ev,
                                            &mut responses,
                                            &mut kick_pending,
                                            &mut pending_agent,
                                        )
                                        .await;
                                }
                            }
                            Ok(false) => {
                                break;
                            }
                            Err(e) => {
                                loop_err = Some(e);
                                break;
                            }
                        }
                    }
                    // An event newer than this build: the provider speaks a
                    // vocabulary the runner has no handling for yet.
                    other => {
                        debug!(session_id = %session_id, event = ?other, "unhandled realtime event");
                    }
                }
            }

            // ── Inferred response completion (idle) ─────────
            // Only armed for providers without an end-of-response wire
            // event (ElevenLabs). Same handling as a provider-sent
            // ResponseCompleted.
            () = async { tokio::time::sleep_until(response_idle_deadline.unwrap_or_else(tokio::time::Instant::now)).await },
                if response_idle_deadline.is_some() =>
            {
                response_idle_deadline = None;
                debug!(
                    session_id = %session_id,
                    provider = %provider_label,
                    "response idle window elapsed; treating response as completed"
                );
                {
                    let mut ctx = make_ctx!();
                    let _ = ctx.handle_provider_response_completed().await;
                }
                responses.finish_inferred();
                if kick_pending {
                    kick_pending = false;
                    let mut ctx = make_ctx!();
                    ctx.request_deferred_kick(&mut responses).await;
                }
                while !responses.is_active() {
                    let Some(ev) = pending_agent.pop_front() else { break };
                    let mut ctx = make_ctx!();
                    let _ = ctx.handle_agent_event(
                        ev,
                        &mut responses,
                        &mut kick_pending,
                        &mut pending_agent,
                    ).await;
                }
            }
        }

        // The provider is closing this connection: once nothing is under
        // way (no response, no turn, nothing queued for the model), move
        // the session to a new one, resumed, so the close cuts nothing.
        if going_away
            && !responses.is_active()
            && !kick_pending
            && pending_agent.is_empty()
            && current_turn.is_none()
            && resumption.can_resume()
        {
            going_away = false;
            let mut ctx = make_ctx!();
            if ctx
                .move_to_new_connection(&rt_session_cfg, state, &mut resumption)
                .await
            {
                responses.reset();
                *ctx.ignoring_stray_deltas = false;
            }
        }
    }

    info!(
        session_id = %session_id,
        provider = %provider_label,
        audio_chunks_uplinked,
        audio_chunks_downlinked,
        "realtime session ending"
    );

    // ── Shutdown ────────────────────────────────────────────
    // Persist a turn still in flight so it (and short single-exchange
    // sessions) reaches the session store even when the provider never
    // delivered a completion boundary before teardown.
    make_ctx!().flush_open_turn(true);
    let _ = client.close().await;
    // Say goodbye like the classic runner does, then give the
    // pump a bounded window to write it (it exits by itself after
    // `Closing`) before cancelling. Cancelling first raced the flush and a
    // server-initiated ending usually reached the client as a bare close.
    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::Closing {
            reason: crate::transport::serve::runner::closing_reason(state).to_string(),
        }))
        .await;
    drop(out_tx);
    let mut pump_handle = pump_handle;
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut pump_handle)
        .await
        .is_err()
    {
        pump_cancel.cancel();
        pump_handle.abort();
    }

    // Write the continuous recording before returning, so the session
    // directory is complete when the session is.
    if let Some(recorder) = session_recorder {
        recorder.finish().await;
    }

    let result = match loop_err {
        Some(e) => Err(e),
        None => Ok(Vec::new()),
    };
    (Some(session_id), result)
}
