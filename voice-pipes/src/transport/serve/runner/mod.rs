//! Session runner -- orchestrates a single client through the full voice pipeline.
//!
//! Wires transport sessions into the voice pipeline, providing the
//! full audio round-trip: client audio -> STT -> LLM -> TTS -> client audio.

pub mod context;
pub mod utils;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::audit::streaming::AuditEvent;
use crate::component::HealthStatus;
use crate::pipeline;
use crate::pipeline::events::PipelineEvent;
use crate::transport::session::TransportSession;
use crate::transport::{
    ClientControl, ClientMessage, ErrorCode, ErrorComponent, ErrorSeverity, ServerControl,
    ServerMessage,
};

use super::setup;
use super::state::TransportState;
use super::turn::{self, TurnTracker};

use context::ClassicSessionContext;
use utils::effective_output_chunking;

/// RAII guard that decrements `TransportState::active_sessions` on drop, so the
/// live-session count is corrected even if the runner panics.
struct ActiveSessionGuard {
    state: Arc<TransportState>,
}

impl Drop for ActiveSessionGuard {
    fn drop(&mut self) {
        self.state.active_sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Run a single client session through the full voice pipeline.
///
/// Spawns a session pump (transport <-> channels), STT task, and an
/// orchestrator loop that manages per-utterance processing.
/// Exits when the transport connection closes or an unrecoverable
/// error occurs.
pub async fn run_session(session: Box<dyn TransportSession + Send>, state: Arc<TransportState>) {
    // Pre-handshake transport-layer id, used only for early diagnostics
    // before we've negotiated the canonical session_id from the session
    // store.  The orchestrator and capture artefacts both key off the
    // post-handshake id (see `handshake.session_id` inside
    // `run_session_inner`), so we *must not* report lifecycle events
    // using this transport id -- they would never link up to the
    // captures dir on disk.
    let transport_id = session.metadata().session_id.clone();
    // RAII guard: increment now, decrement on drop -- including a panic unwind
    // inside the runner. Previously a manual `fetch_sub` after the await was
    // skipped on any panic (an `.expect()` or a poisoned ledger lock), which
    // permanently burned a capacity slot until `/readyz` rejected everything.
    state.active_sessions.fetch_add(1, Ordering::Relaxed);
    let _active_guard = ActiveSessionGuard {
        state: Arc::clone(&state),
    };

    // ── Dispatch: classic STT/LLM/TTS vs. realtime S2S ──────
    //
    // When the loaded config has `[realtime_provider]`, hand the session
    // off to the realtime runner. We build the provider protocol per
    // session (not at server startup) because the factory shape is cheap
    // and per-session voice / instructions may diverge from the static
    // config in a future revision.
    let (handshake_session_id, result) = {
        #[cfg(feature = "realtime")]
        {
            if state.config.realtime_provider.is_some() {
                let store_arc = state.param_registry.store("realtime");
                let store_ref = store_arc.as_ref().and_then(|s| {
                    s.as_any()
                        .downcast_ref::<crate::api::param_store::AtomicParamStore>()
                });
                match crate::config::build_realtime_protocol(&state.config, store_ref) {
                    Ok(Some(proto)) => {
                        // Boxed: the realtime and classic runner futures are
                        // sibling arms of one state machine, so their sizes
                        // add up; keeping the (larger) realtime one on the
                        // heap keeps `run_session` under clippy's
                        // `large_futures` threshold at one allocation per
                        // session.
                        Box::pin(super::realtime_runner::run_realtime_session_inner(
                            session, &state, proto,
                        ))
                        .await
                    }
                    Ok(None) => {
                        // realtime_provider was Some at the check above but
                        // factory returned None -- treat as an internal
                        // mismatch and fall back to the classic path so the
                        // client at least gets a SESSION_CREATE_FAILED.
                        warn!(
                            transport_id = %transport_id,
                            "build_realtime_protocol returned None despite realtime_provider config; falling back to classic"
                        );
                        run_session_inner(session, &state).await
                    }
                    Err(e) => {
                        warn!(
                            transport_id = %transport_id,
                            error = %e,
                            "build_realtime_protocol failed; closing session"
                        );
                        (None, Err(e))
                    }
                }
            } else {
                run_session_inner(session, &state).await
            }
        }
        #[cfg(not(feature = "realtime"))]
        {
            run_session_inner(session, &state).await
        }
    };

    // `_active_guard` decrements `active_sessions` when it drops at the end of
    // this function (or on a panic unwind).

    // Mark the session disconnected so it enters the reconnect grace window and
    // is later reclaimed by the reaper (or evicted under capacity pressure).
    // Without this the store slot leaks forever and the worker stops accepting
    // new connections after `max_sessions` handshakes. A no-op if the handshake
    // never created a store session. This is also what makes Resume reachable:
    // the token stays valid for the grace period after the transport drops.
    if let Some(sid) = handshake_session_id.as_ref() {
        state.session_store.disconnect(sid);
    }

    // Push session-ended event to the orchestrator using the same id
    // that the captures sink + client `session_ready` used.  Skip when
    // the handshake never completed (no session was ever created in
    // the session store).
    if let (Some(sid), Some(tx)) = (
        handshake_session_id.as_ref(),
        state.session_event_tx.as_ref(),
    ) {
        let (final_status, tags) = match &result {
            Ok(tags) => (crate::api::types::SessionStatus::Completed, tags.clone()),
            Err(_) => (crate::api::types::SessionStatus::Error, Vec::new()),
        };
        let rec = turn::make_session_record(
            sid,
            final_status,
            Some(chrono::Utc::now()),
            tags,
            None,
            &state,
            None,
        );
        turn::emit_record(tx, rec, "session");
    }

    let log_id = handshake_session_id
        .as_deref()
        .unwrap_or(transport_id.as_str());
    match result {
        Ok(_) => info!(session_id = %log_id, "session ended normally"),
        Err(e) => warn!(session_id = %log_id, error = %e, "session ended with error"),
    }
}

// ── Orchestrator ────────────────────────────────────────────

// The replay helper lives in `setup.rs` with the shared `SetupDrain`; its
// unit tests are among this module's.
#[cfg(test)]
use super::setup::replay_held_frames;

/// Why a session is closing, as its `Closing` frame says it.
pub(crate) fn closing_reason(state: &TransportState) -> &'static str {
    if state.shutdown.is_cancelled() {
        "worker_shutdown"
    } else {
        "session_ended"
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the session orchestration loop: setup + one select arm per \
              event source; splitting would scatter the loop's shared \
              mutable state behind artificial helpers"
)]
async fn run_session_inner(
    session: Box<dyn TransportSession + Send>,
    state: &TransportState,
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

    // Precondition: what the session needs depends on its mode, not on the
    // engine. `SessionMode::TextInVoiceOut` and `Text` take no audio input, so
    // demanding an STT of them made a text-only worker impossible to configure
    // even though the mode exists for exactly that (`setup_audio_pipeline`
    // already builds no VAD/STT chain when `accepts_audio()` is false).
    //
    // TTS is still required unconditionally: the mirror case (`VoiceInTextOut`,
    // no TTS) needs the stateful-session and synthesis paths below to become
    // optional too, which is a larger change than this one.
    //
    // Both are `Option` (None for realtime configs), so a realtime config
    // served by a build without the `realtime` feature also lands here.
    let needs_stt = negotiated.mode.accepts_audio();
    let stt_root = state.stt.as_ref().map(Arc::clone);
    // `kind` is the structured log field; `phrase` and `remedy` build a message
    // the operator can act on without reading the source.
    let missing: Option<(&str, &str, &str)> = if needs_stt && stt_root.is_none() {
        Some((
            "stt",
            "an STT provider",
            "add an [stt] block to the pipeline config, or connect with a \
             text-input mode (text, text_in_voice_out)",
        ))
    } else if state.tts.is_none() {
        Some((
            "tts",
            "a TTS provider",
            "add a [tts] block to the pipeline config",
        ))
    } else {
        None
    };
    let tts_root = match (missing, state.tts.as_ref()) {
        (None, Some(tts)) => Arc::clone(tts),
        (missing, _) => {
            let (kind, phrase, remedy) =
                missing.unwrap_or(("tts", "a TTS provider", "add a [tts] block"));
            warn!(
                session_id = %session_id,
                mode = ?negotiated.mode,
                has_stt = state.stt.is_some(),
                has_tts = state.tts.is_some(),
                missing = kind,
                "classic pipeline is missing a provider this session mode requires; \
                 closing session (a realtime config needs a build with the \
                 `realtime` feature)"
            );
            let _ = out_tx
                .send(ServerMessage::Control(ServerControl::Error {
                    code: ErrorCode::SessionCreateFailed,
                    message: format!(
                        "server misconfigured: session mode \"{}\" requires {phrase}, \
                         but none is configured — {remedy}",
                        negotiated.mode.as_wire_str()
                    ),
                    severity: ErrorSeverity::Error,
                    turn_id: None,
                    component: Some(ErrorComponent::Transport),
                }))
                .await;
            // Drain gracefully: dropping out_tx lets the pacer/pump flush the error
            // to the client before we cancel (aborting first would race the flush).
            drop(out_tx);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), pump_handle).await;
            pump_cancel.cancel();
            return (
                Some(session_id),
                Err(anyhow::anyhow!(
                    "session mode \"{}\" requires {phrase}",
                    negotiated.mode.as_wire_str()
                )),
            );
        }
    };

    // Tell the client when the tier moves. Ends with the session.
    crate::transport::adaptive::spawn_quality_reporter(conditions.clone(), out_tx.clone());

    let session_output_chunking = effective_output_chunking(&state.config, &client_config);

    // ── Keep the inbound channel flowing during heavy setup ──
    // Per-session component construction below (local LLM weights, VAD
    // and turn-detector models) can take tens of seconds. The WS read
    // pump forwards frames into a bounded channel; if nothing drains it
    // while the client already streams mic audio, the pump blocks on the
    // full channel and stops reading the socket — the client's pong
    // frames go unread and the heartbeat watchdog closes the connection
    // (`missed_pongs`) before setup ever finishes. Drain and discard
    // pre-session *audio* (loss-tolerant; the pipeline isn't listening
    // yet), answer app-level pings, and HOLD everything else for replay:
    // `session_ready` has already gone out, so a client that sends a
    // typed message or a `tts_only_synthesize` right after it (the
    // benchmark runner does; so does any script) must not lose it to the
    // setup window — that showed up as a worker "not listening at all"
    // and a benchmark that timed out after 10 minutes on its first entry.
    // A closed transport surfaces as `None` from the returned receiver,
    // which the loop's own recv-arm handles.
    let session_recorder = setup::setup_session_recorder(state, &session_id);
    let drain = setup::SetupDrain::spawn(
        in_rx,
        out_tx.clone(),
        session_id.clone(),
        session_recorder
            .clone()
            .map(|r| (r, negotiated.input_sample_rate)),
    );

    // ── Per-session text processors ─────────────────────────
    // `block_in_place`: local-model construction is long synchronous CPU
    // work; let the runtime migrate other tasks off this worker thread.
    // (Only valid on the multi-thread runtime — current-thread test
    // runtimes call the builder directly.)
    let session_processors = match tokio::runtime::Handle::current().runtime_flavor() {
        tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| setup::build_session_processors(state, &streaming_audit))
        }
        _ => setup::build_session_processors(state, &streaming_audit),
    };

    // ── Agent channel ──
    // The session's out-of-turn entry point. `bind_session` hands the
    // handle to every text processor so an agentic backend can push
    // speech (`Say`/`Generate`), late tool results, and rich client
    // messages. Events are applied on this loop (single-writer rule);
    // speech events queue in `pending_agent` while a turn is active
    // and drain once the state machine returns to Listening.
    let (agent_tx, mut agent_rx) = mpsc::channel::<crate::agent::AgentEvent>(32);
    let agent_handle =
        crate::agent::AgentSessionHandle::new(agent_tx.clone()).with_session_id(&session_id);
    for p in session_processors.iter() {
        p.bind_session(agent_handle.session_handle());
    }
    let mut pending_agent: std::collections::VecDeque<crate::agent::AgentEvent> =
        std::collections::VecDeque::new();

    // ── Client-side tools ───────
    // Per-session allowlist: worker-declared tools (inline + catalog
    // files, resolved at boot) merged with `Configure.options.client_tools`
    // (later wins on name collision). Handed to the text processors so a
    // function-calling LLM advertises them and surfaces recognized calls
    // as `AgentEvent::ClientToolCall` — the pipeline routes, never executes.
    let session_client_tools = {
        let empty_opts = std::collections::HashMap::new();
        let opts = client_config.as_ref().map_or(&empty_opts, |c| &c.options);
        let runtime_tools = crate::agent::client_tools::parse_options_tools(opts);
        crate::agent::client_tools::ClientToolIndex::merge([
            state.client_tools.as_slice(),
            runtime_tools.as_slice(),
        ])
    };
    if !session_client_tools.is_empty() {
        for p in session_processors.iter() {
            p.configure_client_tools(
                session_client_tools
                    .configs()
                    .iter()
                    .map(crate::agent::client_tools::ClientToolConfig::spec)
                    .collect(),
            );
        }
    }
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();

    // ── Idle re-engagement ───────────────────
    // While the session sits in Listening with no active turn, a
    // configured timeout fires an "are you still there?" prompt through
    // the agent channel (full agent-turn lifecycle). The prompt counter
    // caps per silence stretch and resets when the user speaks — the
    // knobs every surveyed platform ships (Vapi/Retell/Pipecat).
    let idle_timeout = state
        .config
        .agent
        .as_ref()
        .and_then(|a| a.idle_timeout_ms)
        .map(std::time::Duration::from_millis);
    let idle_max_prompts = state
        .config
        .agent
        .as_ref()
        .map_or(0, |a| a.idle_max_prompts);
    let idle_message = state
        .config
        .agent
        .as_ref()
        .map(|a| a.idle_message.clone())
        .unwrap_or_default();
    let mut idle_deadline: Option<tokio::time::Instant> = None;
    let mut idle_prompts_sent: u32 = 0;
    // The idle offer in flight, if any; it runs off this loop (see below).
    let mut idle_offer: Option<tokio::task::JoinHandle<()>> = None;
    let idle_stall_logged = Arc::new(AtomicBool::new(false));

    // ── Per-session audio post-processors ───────────────────
    // For now these are shared across sessions -- same instances run for
    // every connection.  A future revision can build per-session if any
    // processor needs session-scoped state.
    let session_post_processors = Arc::clone(&state.audio_post_processors);
    // Off-path annotators get one hub task each per session; dropping
    // `annotators` at session end closes their streams.
    let annotators = pipeline::AnnotatorHubs::spawn(
        &state.annotators,
        &session_id,
        out_tx.clone(),
        negotiated.output_sample_rate,
    );

    // ── Audio pipeline (VAD -> barge-in -> STT) ──────────────
    // A turn detector that cannot be built (a model file gone missing
    // since validation) ends the session rather than running it with
    // different turn semantics.
    // This session's own event channel, recorded into its capture. Not the
    // worker-wide one: see `session_events` for what sharing it cost. Made
    // before the pipeline, which announces dropped utterances on it.
    let pe_tx = super::session_events::session_event_channel(
        streaming_audit.as_ref(),
        &state.pipeline_event_tx,
    );
    let mut pe_rx = pe_tx.subscribe();
    let ap = match setup::setup_audio_pipeline(
        state,
        &negotiated,
        &session_id,
        &streaming_audit,
        &pe_tx,
    ) {
        Ok(ap) => ap,
        Err(e) => return (Some(session_id), Err(e)),
    };
    let audio_tx = ap.audio_tx;
    let mut utt_rx = ap.utt_rx;
    let proc_audio_tx = ap.proc_audio_tx;
    let mut proc_audio_rx = ap.proc_audio_rx;
    let text_utt_tx = ap.text_utt_tx;
    let stt_handle = ap.stt_handle;
    let turn_detector = ap.turn_detector;
    let turn_state = ap.turn_state;
    let cancel_watch_tx = ap.cancel_watch_tx;
    let user_speaking = ap.user_speaking;

    // ── Turn tracking ───────────────────────────────────────
    let mut tracker = TurnTracker::new();
    let session_mode = negotiated.mode;
    let client_sample_rate = negotiated.input_sample_rate;
    let negotiated_output = Some(negotiated.output_sample_rate);
    let (metrics_tx, mut metrics_rx) = mpsc::channel::<pipeline::TurnMetricsSnapshot>(4);
    let stt_name = negotiated.stt_provider.clone().unwrap_or_default();
    let tts_name = negotiated.tts_provider.clone().unwrap_or_default();
    let session_sanitize_cfg = Some(voice_pipes_sdk::SanitizeConfig::from_tts(
        state.config.tts.as_ref(),
        &tts_name,
        tts_root.supported_inline_tags(),
    ));

    // Per-utterance audio ledgers -- kept so a late PlaybackReport can
    // look up the text<->audio mapping for precise history truncation.
    let mut audio_ledgers: std::collections::HashMap<
        u32,
        std::sync::Arc<parking_lot::Mutex<pipeline::AudioLedger>>,
    > = std::collections::HashMap::default();

    // TTS cold-start flag -- first successful synthesis in this session is cold.
    // Per-session granularity matches benchmark scenario reps (each rep is a
    // fresh session, so the first turn of each rep is the cold one).
    let tts_cold_start: std::sync::Arc<std::sync::atomic::AtomicBool> =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

    // ── Open stateful TTS session ───────────────────────────
    let tts_provider = Arc::clone(&tts_root);

    let tts_session_config = crate::tts::TtsSessionConfig::new(session_id.clone())
        .with_voice(negotiated.voice.clone())
        .with_language(negotiated.language.clone())
        .with_input_sample_rate(negotiated.input_sample_rate)
        .with_output_sample_rate(negotiated.output_sample_rate);

    let tts_session = match tts_provider.open_tts_session(&tts_session_config).await {
        Ok(Some(s)) => {
            info!(session_id = %session_id, "opened stateful TTS session");
            Arc::new(tokio::sync::Mutex::new(Some(s)))
        }
        Ok(None) => Arc::new(tokio::sync::Mutex::new(None)),
        Err(e) => {
            warn!(session_id = %session_id, error = %e, "failed to open stateful TTS session; falling back to stateless");
            Arc::new(tokio::sync::Mutex::new(None))
        }
    };

    // ── Component warm-ups ──────────────────────────────────
    // Spawn STT / LLM / TTS warm-ups in parallel as background tasks.
    // Goal: amortise the cold-start tax (TLS handshake + provider-side
    // model load + voice-model load) off the user's first turn.  Each
    // task is fire-and-forget -- the orchestration loop starts immediately
    // and races against the first audio frame.  Per-component elapsed
    // time is emitted as a `warmup_completed` audit trace event so the
    // diagnostic report can show whether utt-0 actually overlapped a
    // pending warm-up.  Default no-op trait impls mean providers without
    // a meaningful warm-up cost effectively nothing.
    fn emit_warmup(
        audit: &Option<crate::audit::StreamingAuditSink>,
        component: &str,
        provider: &str,
        elapsed_ms: u64,
        ok: bool,
        err: Option<String>,
    ) {
        if let Some(a) = audit {
            a.record(crate::audit::AuditEvent::Trace(
                crate::audit::TraceEvent::new(
                    component,
                    "warmup_completed",
                    serde_json::json!({
                        "component": component,
                        "provider": provider,
                        "elapsed_ms": elapsed_ms,
                        "ok": ok,
                        "error": err,
                    }),
                ),
            ));
        }
    }

    // Skipped entirely for text-input modes, which have no STT to warm.
    if let Some(stt_root) = stt_root.as_ref() {
        // STT warm-up. A provider whose warm-up is per process was warmed by
        // the worker at start and must not be warmed here, where it would
        // hold an inference set against the caller's first utterance.
        let stt_provider = Arc::clone(stt_root);
        if stt_provider.warm_up_scope() == crate::stt::WarmUpScope::PerProcess {
            emit_warmup(&streaming_audit, "stt", &stt_name, 0, true, None);
        } else {
            let audit = streaming_audit.clone();
            let stt_label = stt_name.clone();
            let warm_session_id = session_id.clone();
            let out_tx_warm = out_tx.clone();
            tokio::spawn(async move {
                let t0 = std::time::Instant::now();
                let res = stt_provider.warm_up().await;
                let elapsed = t0.elapsed().as_millis() as u64;
                let (ok, err) = match res {
                    Ok(()) => (true, None),
                    Err(e) => (false, Some(e.to_string())),
                };
                tracing::debug!(
                    session_id = %warm_session_id,
                    component = "stt",
                    provider = %stt_label,
                    elapsed_ms = elapsed,
                    ok,
                    "stt warm-up done"
                );
                if let Some(reason) = err.as_deref() {
                    // A failed warm-up means the backend is unreachable/unloaded.
                    // Flip the component Unhealthy (so /health reports it) and warn
                    // the client at connect time instead of waiting for the first
                    // turn to fail.
                    if let Some(component) = stt_provider.as_component() {
                        component.set_health(HealthStatus::unhealthy(format!(
                            "warm-up failed: {reason}"
                        )));
                    }
                    let _ = out_tx_warm
                        .send(ServerMessage::Control(ServerControl::Error {
                            code: ErrorCode::SttWarmupFailed,
                            message: format!("STT provider '{stt_label}' is unavailable: {reason}"),
                            severity: ErrorSeverity::Warning,
                            turn_id: None,
                            component: Some(ErrorComponent::Stt),
                        }))
                        .await;
                } else if let Some(component) = stt_provider.as_component() {
                    // Warm-up succeeded: clear a prior warm-up/connection Unhealthy
                    // mark so a recovered backend is reflected in /health. Leave
                    // Degraded/Healthy (owned by the auto-degrader) untouched.
                    if component.health().state == crate::component::HealthState::Unhealthy {
                        component.set_health(HealthStatus::healthy());
                    }
                }
                emit_warmup(&audit, "stt", &stt_label, elapsed, ok, err);
            });
        }
    }
    {
        // TTS warm-up
        let tts_provider = Arc::clone(&tts_root);
        let audit = streaming_audit.clone();
        let tts_label = tts_name.clone();
        let warm_session_id = session_id.clone();
        let out_tx_warm = out_tx.clone();
        tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            let res = tts_provider.warm_up().await;
            let elapsed = t0.elapsed().as_millis() as u64;
            let (ok, err) = match res {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e.to_string())),
            };
            tracing::debug!(
                session_id = %warm_session_id,
                component = "tts",
                provider = %tts_label,
                elapsed_ms = elapsed,
                ok,
                "tts warm-up done"
            );
            if let Some(reason) = err.as_deref() {
                // See the STT warm-up above: surface an unreachable TTS backend
                // (e.g. the VibeVoice sidecar not running) at connect time.
                if let Some(component) = tts_provider.as_component() {
                    component
                        .set_health(HealthStatus::unhealthy(format!("warm-up failed: {reason}")));
                }
                let _ = out_tx_warm
                    .send(ServerMessage::Control(ServerControl::Error {
                        code: ErrorCode::TtsWarmupFailed,
                        message: format!("TTS provider '{tts_label}' is unavailable: {reason}"),
                        severity: ErrorSeverity::Warning,
                        turn_id: None,
                        component: Some(ErrorComponent::Tts),
                    }))
                    .await;
            } else if let Some(component) = tts_provider.as_component() {
                // See the STT warm-up above: clear a prior warm-up Unhealthy
                // mark on recovery.
                if component.health().state == crate::component::HealthState::Unhealthy {
                    component.set_health(HealthStatus::healthy());
                }
            }
            emit_warmup(&audit, "tts", &tts_label, elapsed, ok, err);
        })
    };
    {
        // Text-processor / LLM chain warm-up.  session_processors is an
        // Arc<Vec<...>>, so we can share it with the background task.
        let procs = Arc::clone(&session_processors);
        let audit = streaming_audit.clone();
        let warm_session_id = session_id.clone();
        let out_tx_warm = out_tx.clone();
        tokio::spawn(async move {
            for proc in procs.iter() {
                let name = proc.name().to_string();
                let t0 = std::time::Instant::now();
                let res = proc.warm_up().await;
                let elapsed = t0.elapsed().as_millis() as u64;
                let (ok, err) = match res {
                    Ok(()) => (true, None),
                    Err(e) => (false, Some(e.to_string())),
                };
                tracing::debug!(
                    session_id = %warm_session_id,
                    component = "llm",
                    processor = %name,
                    elapsed_ms = elapsed,
                    ok,
                    "llm warm-up done"
                );
                if let Some(reason) = err.as_deref() {
                    // See the STT warm-up above: surface an unreachable LLM
                    // backend at connect time rather than on the first turn.
                    if let Some(component) = proc.as_component() {
                        component.set_health(HealthStatus::unhealthy(format!(
                            "warm-up failed: {reason}"
                        )));
                    }
                    let _ = out_tx_warm
                        .send(ServerMessage::Control(ServerControl::Error {
                            code: ErrorCode::LlmWarmupFailed,
                            message: format!("text processor '{name}' is unavailable: {reason}"),
                            severity: ErrorSeverity::Warning,
                            turn_id: None,
                            component: Some(ErrorComponent::Llm),
                        }))
                        .await;
                } else if let Some(component) = proc.as_component() {
                    // See the STT warm-up above: clear a prior warm-up Unhealthy
                    // mark on recovery.
                    if component.health().state == crate::component::HealthState::Unhealthy {
                        component.set_health(HealthStatus::healthy());
                    }
                }
                emit_warmup(&audit, "llm", &name, elapsed, ok, err);
            }
        })
    };

    // Tracks when the last ClearAudio was sent for barge-in RTT measurement.
    let mut barge_in_sent_at: Option<std::time::Instant> = None;

    // Instantiate SpeculativeSessionManager at session startup if speculative execution is enabled in configuration.
    let spec_manager = state.config.speculative.enabled.then(|| {
        let manager = Arc::new(pipeline::SpeculativeSessionManager::new(
            Arc::clone(&session_processors),
            Arc::clone(&tts_provider),
            state.config.speculative.clone(),
        ));

        // Spawn a task routing ap.spec_rx messages to spec_manager.handle_partial(...)
        if let Some(mut rx) = ap.spec_rx {
            let mgr = Arc::clone(&manager);
            tokio::spawn(async move {
                while let Some(utt) = rx.recv().await {
                    mgr.handle_partial(&utt.text).await;
                }
            });
        }
        manager
    });

    if let Some(ref manager) = spec_manager {
        manager.start_turn().await;
    }

    // Setup complete: reclaim the inbound receiver from the drain task,
    // with any messages the client sent during setup put back in front.
    let mut in_rx = match drain.finish().await {
        Ok(rx) => rx,
        Err(join_err) => {
            warn!(
                session_id = %session_id,
                error = %join_err,
                "setup drain task panicked; closing session"
            );
            drop(out_tx);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), pump_handle).await;
            pump_cancel.cancel();
            return (
                Some(session_id),
                Err(anyhow::anyhow!("setup drain task panicked: {join_err}")),
            );
        }
    };

    // ── Orchestration loop ──────────────────────────────────
    loop {
        // Arm/disarm the idle countdown from the current turn state
        // each iteration: armed only while Listening with no active
        // turn and prompts remaining; anything else clears it. The
        // `user_speaking` probe covers speech the VAD has confirmed but
        // that hasn't committed as a turn yet (endpointing still
        // counting down) — without it the prompt can fire mid-sentence.
        if idle_timeout.is_some() {
            let session_idle = tracker.current_cancel.is_none()
                && pipeline::get_turn_state(&turn_state) == pipeline::TurnState::Listening
                && !user_speaking.load(Ordering::Acquire);
            if session_idle && idle_prompts_sent < idle_max_prompts {
                if idle_deadline.is_none() {
                    idle_deadline = idle_timeout.map(|d| tokio::time::Instant::now() + d);
                }
            } else {
                idle_deadline = None;
            }
        }

        let cancel_fut = async {
            if let Some(ref cancel) = tracker.current_cancel {
                cancel.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        macro_rules! make_ctx {
            () => {
                ClassicSessionContext {
                    session_id: &session_id,
                    tracker: &mut tracker,
                    suppress_audio: &suppress_audio,
                    out_tx: &out_tx,
                    annotators: &annotators,
                    turn_detector: turn_detector.as_ref(),
                    session_recorder: session_recorder.as_ref(),
                    audio_tx: &audio_tx,
                    metrics_rx: &mut metrics_rx,
                    proc_audio_rx: &mut proc_audio_rx,
                    turn_state: &turn_state,
                    session_processors: &session_processors,
                    streaming_audit: &streaming_audit,
                    pe_tx: &pe_tx,
                    stt_name: &stt_name,
                    tts_name: &tts_name,
                    client_sample_rate,
                    negotiated_output,
                    audio_ledgers: &mut audio_ledgers,
                    barge_in_sent_at: &mut barge_in_sent_at,
                    tts_session: Arc::clone(&tts_session),
                    spec_manager: spec_manager.clone(),
                    sanitize_cfg: session_sanitize_cfg.clone(),
                    observations: observations.clone(),
                    client_tools: &session_client_tools,
                    client_calls: &mut client_calls,
                }
            };
        }

        tokio::select! {
            biased;

            // ── Worker shutting down ────────────────────────
            () = state.shutdown.cancelled() => {
                info!(session_id = %session_id, "worker shutting down; ending the session");
                break;
            }

            // ── Server-side barge-in / Interruption ─────────
            () = cancel_fut, if tracker.current_cancel.is_some() => {
                let mut ctx = make_ctx!();
                ctx.handle_server_barge_in().await;
            }

            // ── Inbound from transport ──────────────────────
            msg = in_rx.recv() => {
                let mut ctx = make_ctx!();
                match msg {
                    Some(ClientMessage::Audio(chunk)) => {
                        if !ctx.handle_audio(chunk, session_mode).await {
                            break;
                        }
                    }
                    Some(ClientMessage::Control(ClientControl::Cancel)) => {
                        ctx.handle_cancel().await;
                    }
                    Some(ClientMessage::Control(ClientControl::Ping { timestamp_ms })) => {
                        ctx.handle_ping(timestamp_ms).await;
                    }
                    Some(ClientMessage::Control(ClientControl::PlaybackComplete { utterance_id })) => {
                        ctx.handle_playback_complete(utterance_id).await;
                    }
                    Some(ClientMessage::Control(ClientControl::PlaybackReport {
                        utterance_id, last_played_offset_ms, last_played_text,
                    })) => {
                        ctx.handle_playback_report(utterance_id, last_played_offset_ms, last_played_text).await;
                    }
                    Some(ClientMessage::Control(ClientControl::PlaybackQuality {
                        utterance_id, underrun_count, total_silence_ms, audio_ms, heard_ms, wall_ms, gaps,
                        arrival_wall_ms, interrupted, annotation_late_count, annotation_late_ms_max,
                    })) => {
                        ctx.handle_playback_quality(utterance_id, crate::api::types::PlaybackHealth {
                            underrun_count, total_silence_ms, audio_ms, heard_ms, wall_ms, gaps,
                            arrival_wall_ms, interrupted, annotation_late_count, annotation_late_ms_max,
                        });
                    }
                    Some(ClientMessage::Control(ClientControl::TimingReport {
                        utterance_id, event, client_timestamp_ms, ..
                    })) => {
                        ctx.handle_timing_report(utterance_id, event, client_timestamp_ms);
                    }
                    Some(ClientMessage::Message(pipeline_msg)) => {
                        if !ctx.handle_pipeline_message(pipeline_msg, &text_utt_tx).await {
                            break;
                        }
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
                    Some(ClientMessage::Control(ClientControl::Configure(cfg))) => {
                        ctx.handle_configure(cfg);
                    }
                    Some(ClientMessage::Control(ClientControl::Resume { .. })) => {}
                    Some(ClientMessage::Control(ClientControl::ClientToolResult {
                        call_id, phase, result, speak,
                    })) => {
                        // A resolved Completed/Failed becomes a Generate
                        // event: Silent = context-only injection into the
                        // LLM history, WhenIdle (opt-in speak) = a spoken
                        // confirmation turn through the standard
                        // agent-turn lifecycle.
                        if let Some(ev) = ctx.resolve_client_tool_result(
                            &call_id, phase, result.as_ref(), speak,
                        ) {
                            ctx.handle_agent_event(
                                ev,
                                &mut pending_agent,
                                &cancel_watch_tx,
                                &proc_audio_tx,
                                &session_post_processors,
                                &session_output_chunking,
                                &metrics_tx,
                                &tts_cold_start,
                                state,
                            ).await;
                        }
                    }
                    Some(ClientMessage::Control(ClientControl::InjectAudio {
                        source, scenario_id, experiment_id, speed,
                    })) => {
                        ctx.handle_inject_audio(source, scenario_id, experiment_id, speed);
                    }
                    Some(ClientMessage::Control(ClientControl::TtsOnlySynthesize {
                        id, text, language, voice,
                    })) => {
                        ctx.handle_tts_only_synthesize(id, text, language, voice, &tts_cold_start, state);
                    }
                    None => {
                        debug!(session_id = %session_id, "transport closed");
                        break;
                    }
                }
            }

            // ── New utterance from STT ──────────────────────
            utt = utt_rx.recv() => {
                match utt {
                    Some(utterance) => {
                        // User activity: a fresh silence stretch gets a
                        // fresh idle-prompt budget.
                        if !utterance.source.is_agent() {
                            idle_prompts_sent = 0;
                        }
                        let mut ctx = make_ctx!();
                        ctx.handle_utterance(
                            utterance,
                            &cancel_watch_tx,
                            &proc_audio_tx,
                            &session_post_processors,
                            &session_output_chunking,
                            &metrics_tx,
                            &tts_cold_start,
                            state,
                        ).await;
                    }
                    None => break,
                }
            }

            // ── Speech that made no turn ────────────────────
            // The STT stage announces it on the session's event channel;
            // the client is told, since from its side speech ended and
            // nothing followed.
            // Every other event is the runner's own. Falling behind is
            // harmless, and the channel cannot close while the runner
            // holds a sender.
            ev = pe_rx.recv() => {
                if let Ok(PipelineEvent::UtteranceDropped { utterance_id, reason, audio_ms, .. }) = ev {
                    let _ = out_tx
                        .send(ServerMessage::Control(ServerControl::UtteranceDropped {
                            utterance_id,
                            reason,
                            audio_ms,
                        }))
                        .await;
                }
            }

            // ── Pipeline audio output (TTS -> client) ───────
            chunk = proc_audio_rx.recv() => {
                match chunk {
                    Some(c) => {
                        let mut ctx = make_ctx!();
                        ctx.handle_audio_output(c, &client_config, state).await;
                    }
                    None => break,
                }
            }

            // ── Agent events (agentic system via AgentSessionHandle) ──
            agent_ev = agent_rx.recv() => {
                // `agent_tx` is held by this function, so None only at
                // teardown.
                let Some(agent_ev) = agent_ev else {
                    debug!(session_id = %session_id, "agent channel closed");
                    break;
                };
                let mut ctx = make_ctx!();
                ctx.handle_agent_event(
                    agent_ev,
                    &mut pending_agent,
                    &cancel_watch_tx,
                    &proc_audio_tx,
                    &session_post_processors,
                    &session_output_chunking,
                    &metrics_tx,
                    &tts_cold_start,
                    state,
                ).await;
            }

            // ── Drain queued agent speech once idle ─────────
            // Fires only when a WhenIdle event is waiting and the state
            // machine is back in Listening with no active turn. Handling
            // a speech event immediately opens a turn (Processing), so
            // the guard goes false and the queue drains one per idle
            // period — no busy spin.
            () = std::future::ready(()), if !pending_agent.is_empty()
                && tracker.current_cancel.is_none()
                && pipeline::get_turn_state(&turn_state) == pipeline::TurnState::Listening => {
                if let Some(agent_ev) = pending_agent.pop_front() {
                    let mut ctx = make_ctx!();
                    ctx.handle_agent_event(
                        agent_ev,
                        &mut pending_agent,
                        &cancel_watch_tx,
                        &proc_audio_tx,
                        &session_post_processors,
                        &session_output_chunking,
                        &metrics_tx,
                        &tts_cold_start,
                        state,
                    ).await;
                }
            }

            // ── Idle re-engagement countdown ─────────
            // Pending unless armed; the deadline is absolute, so
            // re-creating the sleep future each loop iteration is fine.
            () = async {
                match idle_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                idle_deadline = None;
                if idle_prompts_sent < idle_max_prompts {
                    idle_prompts_sent += 1;
                    info!(
                        session_id = %session_id,
                        prompt = idle_prompts_sent,
                        of = idle_max_prompts,
                        "idle re-engagement prompt"
                    );
                    // Bound agents get first refusal (they phrase the
                    // re-engagement themselves — Retell's
                    // reminder_required precedent); the static message
                    // is the no-agent fallback. An agent may never answer,
                    // so the offer runs on its own task, bounded by the
                    // idle interval (at most a few seconds), and this loop
                    // keeps carrying audio and barge-ins meanwhile.
                    let idle_ms = idle_timeout.map_or(0, |d| d.as_millis() as u64);
                    let within = idle_timeout
                        .unwrap_or(IDLE_OFFER_MAX_WAIT)
                        .min(IDLE_OFFER_MAX_WAIT);
                    if let Some(previous) = idle_offer.take() {
                        previous.abort();
                    }
                    idle_offer = Some(agent_handle.spawn_idle_offer(
                        idle_ms,
                        idle_prompts_sent,
                        within,
                        idle_message.clone(),
                        Arc::clone(&idle_stall_logged),
                    ));
                }
            }
        }
    }

    // ── Cleanup ─────────────────────────────────────────────
    // If a turn was in-flight, write a terminated summary so the UI
    // can distinguish it from completed turns.
    if tracker.utterance_start.is_some()
        && let Some(sink) = &streaming_audit
    {
        sink.record(AuditEvent::UtteranceDone {
            utterance_id: tracker.current_utterance_id,
            interrupted: false,
            terminated: true,
            stt_provider: stt_name.clone(),
            tts_provider: tts_name.clone(),
        });
    }
    if let Some(cancel) = tracker.current_cancel.take() {
        cancel.cancel();
    }
    if let Some(offer) = idle_offer.take() {
        offer.abort();
    }

    // Notify the client of the aggregate session metrics, then signal
    // closure. Best-effort: if the transport is already gone these
    // sends are dropped silently.
    let _ = out_tx
        .send(ServerMessage::Control(
            tracker.session_aggregator.build_summary(),
        ))
        .await;
    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::Closing {
            reason: closing_reason(state).to_string(),
        }))
        .await;

    stt_handle.as_ref().inspect(|h| h.abort());
    // Give the pump a bounded window to write the summary and
    // `Closing` (it exits by itself after `Closing`, see `session_pump`)
    // before cancelling — cancelling first raced the flush and the client
    // usually saw a bare socket close on server-initiated endings.
    let mut pump_handle = pump_handle;
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut pump_handle)
        .await
        .is_err()
    {
        pump_cancel.cancel();
        pump_handle.abort();
        let _ = pump_handle.await;
    }
    if let Some(h) = stt_handle {
        let _ = h.await;
    }

    // Close stateful TTS session cleanly if opened
    if let Some(s) = tts_session.lock().await.take() {
        let _ = s.close().await;
    }

    // Write the continuous recording. Waited on rather than left to the
    // writer task: the session directory is complete when the session ends,
    // and a tool reading it should not race the last file into place.
    if let Some(recorder) = session_recorder {
        recorder.finish().await;
    }

    (
        Some(session_id),
        Ok(client_config.map(|c| c.tags).unwrap_or_default()),
    )
}

// ── Session pump ────────────────────────────────────────────

/// The longest the idle offer waits for a bound agent to take the prompt
/// before the configured message is spoken instead.
const IDLE_OFFER_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Smoothing for the send-latency EMA. Deliberately slow: this signal is
/// sampled on every outbound frame, so a single slow write must not be able
/// to move the tier by itself.
const SEND_LATENCY_ALPHA: f64 = 0.1;

/// How often the smoothed send latency is reported to the adaptive
/// controller. The EMA updates on every frame; reporting it that often
/// would make the controller's streak for this signal count frames rather
/// than seconds.
const SEND_LATENCY_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Pump task that exclusively owns the transport session.
///
/// Reads inbound messages via `session.recv()` and forwards them to
/// `in_tx`.  Reads outbound messages from `out_rx` and sends them
/// via `session.send()`.  Exits when the session closes, the cancel
/// token fires, or either channel is dropped. The `realtime_runner`
/// sibling reuses it, so both paths share the same pump semantics
/// (including barge-in audio suppression).
pub(super) async fn session_pump(
    mut session: Box<dyn TransportSession + Send>,
    in_tx: mpsc::Sender<ClientMessage>,
    mut out_rx: mpsc::Receiver<ServerMessage>,
    cancel: CancellationToken,
    suppress_audio: Arc<AtomicBool>,
    observations: crate::transport::adaptive::ObservationSink,
) {
    let mut suppressed_count: u64 = 0;
    // S4: how long the socket write itself takes, smoothed. Axum's sink
    // exposes no buffered-byte count, so this stands in for kernel
    // send-buffer pressure -- when the buffer is full, `send` blocks. It is a
    // proxy, but a sub-second one that needs nothing from the client.
    let mut send_latency_ema: Option<f64> = None;
    let mut send_latency_reported_at: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            biased;

            () = cancel.cancelled() => break,

            msg = out_rx.recv() => {
                match msg {
                    Some(m) => {
                        // Drop stale audio messages when barge-in is in progress.
                        if matches!(m, ServerMessage::Audio(_))
                            && suppress_audio.load(Ordering::Acquire)
                        {
                            suppressed_count += 1;
                            if suppressed_count == 1 || suppressed_count.is_multiple_of(10) {
                                debug!(
                                    suppressed_count,
                                    "session_pump: suppressing audio frame (barge-in)"
                                );
                            }
                            continue;
                        }
                        if suppressed_count > 0 && !matches!(m, ServerMessage::Audio(_)) {
                            debug!(
                                total_suppressed = suppressed_count,
                                "session_pump: finished suppressing audio"
                            );
                            suppressed_count = 0;
                        }
                        let started = std::time::Instant::now();
                        // `Closing` is by contract the last frame of
                        // a session. Exiting right after it is written lets
                        // the runner *await* this pump (bounded) instead of
                        // cancelling it, so `SessionSummary` and `Closing`
                        // reach the client on server-initiated endings too.
                        let is_closing =
                            matches!(m, ServerMessage::Control(ServerControl::Closing { .. }));
                        if let Err(e) = session.send(m).await {
                            debug!(error = %e, "session send failed");
                            break;
                        }
                        if is_closing {
                            break;
                        }
                        // Smoothed so one slow write (a GC pause, a scheduler
                        // hiccup) cannot move the tier on its own.
                        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                        let ema = send_latency_ema
                            .map_or(elapsed, |prev| SEND_LATENCY_ALPHA * elapsed
                                + (1.0 - SEND_LATENCY_ALPHA) * prev);
                        send_latency_ema = Some(ema);
                        let due = send_latency_reported_at
                            .is_none_or(|at| at.elapsed() >= SEND_LATENCY_REPORT_INTERVAL);
                        if due {
                            send_latency_reported_at = Some(std::time::Instant::now());
                            observations.observe(crate::transport::adaptive::Observation {
                                send_latency_ms: Some(ema as u64),
                                ..Default::default()
                            });
                        }
                    }
                    None => break,
                }
            }

            result = session.recv() => {
                match result {
                    Ok(Some(m)) => {
                        if in_tx.send(m).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        debug!(error = %e, "session recv failed");
                        break;
                    }
                }
            }
        }
    }

    let _ = session.close("session ending").await;
}

#[cfg(test)]
#[path = "tests/runner.rs"]
mod tests;
