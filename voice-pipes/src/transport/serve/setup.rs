//! Session initialization -- handshake, negotiation, audit sink, audio pipeline.
//!
//! Extracts the sequential setup steps from `run_session_inner` into
//! focused helper functions, each returning the state needed by the
//! orchestration loop.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::audit::StreamingAuditSink;
use crate::component::{DeviceInfo, ModelIdentity};
use crate::pipeline::{self, AudioChunk, SharedTurnState};
use crate::text::TextProcessor;
use crate::transport::adaptive::AdaptiveController;
use crate::transport::pacer::{PacerConfig, spawn_pacer};
use crate::transport::session::TransportSession;
use crate::transport::{
    AudioFormat, ClientControl, ClientMessage, ErrorCode, NegotiatedConfig, ServerControl,
    ServerMessage, SessionConfig, SessionInfo, TransportType,
};

use super::state::TransportState;

// ── Buffer sizes (shared with runner.rs) ────────────────────

pub(super) const SESSION_CHANNEL_BUF: usize = 64;
// Increase audio input buffer so slow inference passes don't starve the VAD queue.
pub(super) const AUDIO_IN_BUF: usize = 256;
pub(super) const UTTERANCE_BUF: usize = 4;
pub(super) const AUDIO_OUT_BUF: usize = 32;

// ── Session Bootstrapper ────────────────────────────────────

pub(super) struct BootstrappedSession {
    pub session_id: String,
    pub negotiated: NegotiatedConfig,
    pub client_config: Option<SessionConfig>,
    pub streaming_audit: Option<StreamingAuditSink>,
    pub in_rx: mpsc::Receiver<ClientMessage>,
    pub out_tx: mpsc::Sender<ServerMessage>,
    pub suppress_audio: Arc<AtomicBool>,
    pub pump_cancel: CancellationToken,
    pub pump_handle: tokio::task::JoinHandle<()>,
    /// Feed link signals in from anywhere (runner, WS write path, client
    /// frames); the controller task fuses them into a
    /// [`QualityTier`](crate::transport::QualityTier).
    pub observations: crate::transport::adaptive::ObservationSink,
    /// Live view of the link, for consumers that act on the tier (the
    /// annotator hub, the client via `quality_update`).
    pub conditions: watch::Receiver<crate::transport::NetworkConditions>,
}

pub(super) async fn bootstrap_session(
    session: Box<dyn TransportSession + Send>,
    state: &TransportState,
) -> Option<BootstrappedSession> {
    let session_id = session.metadata().session_id.clone();
    // Taken before the session moves into its pump: the negotiated input
    // rate is published through this handle after the handshake so the
    // transport's inbound audio rate limiter is sized for the client's
    // actual stream.
    let negotiated_input_rate = session.negotiated_input_rate();

    // ── Split session into channels via a pump task ─────────
    let (in_tx, mut in_rx) = mpsc::channel::<ClientMessage>(SESSION_CHANNEL_BUF);
    let (out_tx, out_rx) = mpsc::channel::<ServerMessage>(SESSION_CHANNEL_BUF);

    let pump_cancel = CancellationToken::new();
    let suppress_audio = Arc::new(AtomicBool::new(false));

    // Built before the handshake so no observation can arrive before there is
    // something to receive it. Disabled config still publishes diagnostics --
    // an operator can watch what it *would* do before switching it on.
    let (observations, conditions) =
        AdaptiveController::spawn(state.config.adaptive_quality.clone());
    // The ping/pong bucket and smoothed RTT: subscribed here, before the
    // session moves into its pump, so the controller sees the coarse
    // heartbeat signal alongside the finer ones.
    if let Some(monitor) = session.health_monitor() {
        crate::transport::adaptive::spawn_health_feed(&monitor, observations.clone());
    }

    // ── Audio pacer ─────────────────────────────────────────
    let pacer_config = match session.metadata().transport_type {
        TransportType::WebSocket => PacerConfig::websocket(),
        TransportType::WebTransport | TransportType::Direct => PacerConfig::independent_streams(),
    };
    let paced_rx = spawn_pacer(pacer_config, out_rx, Arc::clone(&suppress_audio));

    let pump_handle = tokio::spawn(super::runner::session_pump(
        session,
        in_tx,
        paced_rx,
        pump_cancel.clone(),
        Arc::clone(&suppress_audio),
        observations.clone(),
    ));

    // ── Handshake ───────────────────────────────────────────
    let Some(handshake) = perform_handshake(&mut in_rx, &out_tx, state, &session_id).await else {
        drop(out_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), pump_handle).await;
        pump_cancel.cancel();
        return None;
    };
    let client_config = handshake.client_config;
    let session_id = handshake.session_id.clone();

    // ── Negotiate + announce ────────────────────────────────
    let negotiated = negotiate_and_announce(
        &client_config,
        state,
        &handshake.session_id,
        &handshake.session_token,
        handshake.resumed,
        &handshake.replay_messages,
        &out_tx,
    )
    .await;

    // Push session-started event to the orchestrator using the
    // post-handshake session_id. Sent after negotiation so the record
    // carries the negotiated sample rates; the end-of-session record only
    // fills fields the start record left empty.
    if let Some(ref tx) = state.session_event_tx {
        let rec = super::turn::make_session_record(
            &session_id,
            crate::api::types::SessionStatus::InProgress,
            None,
            Vec::new(),
            Some((negotiated.input_sample_rate, negotiated.output_sample_rate)),
            state,
        );
        super::turn::emit_record(tx, rec, "session");
    }

    // Publish the negotiated input rate to the transport: the WS
    // read pump sizes its inbound audio rate limiter from this.
    if let Some(rate) = &negotiated_input_rate {
        rate.store(
            negotiated.input_sample_rate,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    // ── Streaming audit sink ────────────────────────────────
    let streaming_audit =
        setup_audit_sink(state, &handshake.session_id, &negotiated, &client_config).await;

    Some(BootstrappedSession {
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
    })
}

// ── Handshake ───────────────────────────────────────────────

/// Result of the client handshake phase.
pub(super) struct HandshakeResult {
    pub session_id: String,
    pub session_token: String,
    pub resumed: bool,
    pub client_config: Option<SessionConfig>,
    pub replay_messages: Vec<ServerMessage>,
}

/// Wait for the client's first control message (up to 5 s).
///
/// The first message must be either `Configure` or `Resume`.
///
/// Returns `None` if the transport closes during the handshake,
/// indicating the caller should exit cleanly.
pub(super) async fn perform_handshake(
    in_rx: &mut mpsc::Receiver<ClientMessage>,
    out_tx: &mpsc::Sender<ServerMessage>,
    state: &TransportState,
    session_id: &str,
) -> Option<HandshakeResult> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

    tokio::select! {
        biased;
        () = tokio::time::sleep_until(deadline) => {
            let _ = out_tx.send(ServerMessage::Control(ServerControl::Error {
                code: ErrorCode::HandshakeTimeout,
                message: "expected Configure or Resume as the first control message".to_string(),
                severity: crate::transport::ErrorSeverity::Error,
                turn_id: None,
                component: Some(crate::transport::ErrorComponent::Transport),
            })).await;
            debug!(session_id = %session_id, "handshake timeout");
            None
        }
        msg = in_rx.recv() => {
            match msg {
                Some(ClientMessage::Control(ClientControl::Configure(mut cfg))) => {
                    cap_tags(&mut cfg.tags, session_id);
                    info!(
                        session_id = %session_id,
                        input_rate = cfg.sample_rate,
                        output_rate = ?cfg.output_sample_rate,
                        language = ?cfg.language,
                        voice = ?cfg.voice,
                        has_app_data = cfg.app_data.is_some(),
                        "client Configure received"
                    );
                    // The whole Configure is retained per session
                    // slot for the reconnect grace period, so cap the
                    // free-form parts well below the 1 MiB frame limit.
                    let extra = handshake_extra_bytes(&cfg);
                    if extra > MAX_HANDSHAKE_EXTRA_BYTES {
                        warn!(
                            session_id = %session_id,
                            bytes = extra,
                            limit = MAX_HANDSHAKE_EXTRA_BYTES,
                            "Configure app_data/options/tags too large; rejecting handshake"
                        );
                        let _ = out_tx.send(ServerMessage::Control(ServerControl::Error {
                            code: ErrorCode::InvalidHandshake,
                            message: format!(
                                "Configure app_data + options + tags exceed {MAX_HANDSHAKE_EXTRA_BYTES} bytes ({extra})"
                            ),
                            severity: crate::transport::ErrorSeverity::Error,
                            turn_id: None,
                            component: Some(crate::transport::ErrorComponent::Transport),
                        })).await;
                        return None;
                    }
                    let (session_id, session_token) = match state.session_store.create_session(cfg.clone()) {
                        Ok(result) => result,
                        Err(err) => {
                            let _ = out_tx.send(ServerMessage::Control(ServerControl::Error {
                                code: ErrorCode::SessionCreateFailed,
                                message: err.to_string(),
                                severity: crate::transport::ErrorSeverity::Error,
                                turn_id: None,
                                component: Some(crate::transport::ErrorComponent::Transport),
                            })).await;
                            return None;
                        }
                    };
                    Some(HandshakeResult {
                        session_id,
                        session_token,
                        resumed: false,
                        client_config: Some(cfg),
                        replay_messages: Vec::new(),
                    })
                }
                Some(ClientMessage::Control(ClientControl::Resume { session_token })) => {
                    info!(session_id = %session_id, "client Resume received");
                    match state.session_store.reconnect(&session_token) {
                        Ok((session_id, new_token, replay_messages)) => {
                            let client_config = state.session_store.get_config(&session_id);
                            Some(HandshakeResult {
                                session_id,
                                session_token: new_token,
                                resumed: true,
                                client_config,
                                replay_messages,
                            })
                        }
                        Err(err) => {
                            let _ = out_tx.send(ServerMessage::Control(ServerControl::Error {
                                code: ErrorCode::SessionResumeFailed,
                                message: err.to_string(),
                                severity: crate::transport::ErrorSeverity::Error,
                                turn_id: None,
                                component: Some(crate::transport::ErrorComponent::Transport),
                            })).await;
                            None
                        }
                    }
                }
                Some(other) => {
                    // Name the variant only. `{other:?}` serialised
                    // the whole frame (up to 1 MiB of samples) into the error.
                    let _ = out_tx.send(ServerMessage::Control(ServerControl::Error {
                        code: ErrorCode::InvalidHandshake,
                        message: format!(
                            "expected Configure or Resume first, got {}",
                            client_message_kind(&other)
                        ),
                        severity: crate::transport::ErrorSeverity::Error,
                        turn_id: None,
                        component: Some(crate::transport::ErrorComponent::Transport),
                    })).await;
                    None
                }
                None => None, // transport closed
            }
        }
    }
}

/// Upper bound on the serialised size of the free-form parts of a
/// `Configure` (`app_data`, `options`, `tags`). Everything else in the
/// struct is fixed-size. 16 KiB is far above any real client (the
/// Playground sends well under 1 KiB) and bounds what a slot retains.
pub(crate) const MAX_HANDSHAKE_EXTRA_BYTES: usize = 16 * 1024;

/// Most tags kept from a `Configure`; tags are written into every
/// session record and capture metadata, and listed by the admin UI.
pub(crate) const MAX_TAGS: usize = 16;
/// Longest tag kept, in bytes; longer ones are cut at a character
/// boundary.
pub(crate) const MAX_TAG_BYTES: usize = 64;

/// Trim a client's tag list to [`MAX_TAGS`] entries of at most
/// [`MAX_TAG_BYTES`] each, logging once when anything was cut.
pub(crate) fn cap_tags(tags: &mut Vec<String>, session_id: &str) {
    let before = tags.len();
    tags.truncate(MAX_TAGS);
    let mut cut = before > MAX_TAGS;
    for tag in tags.iter_mut() {
        if tag.len() > MAX_TAG_BYTES {
            let mut end = MAX_TAG_BYTES;
            while !tag.is_char_boundary(end) {
                end -= 1;
            }
            tag.truncate(end);
            cut = true;
        }
    }
    if cut {
        warn!(
            session_id = %session_id,
            max_tags = MAX_TAGS,
            max_tag_bytes = MAX_TAG_BYTES,
            "Configure tags trimmed to the cap"
        );
    }
}

/// Serialised size of the free-form parts of a `Configure`.
pub(crate) fn handshake_extra_bytes(cfg: &crate::transport::SessionConfig) -> usize {
    let app_data = cfg
        .app_data
        .as_ref()
        .and_then(|v| serde_json::to_vec(v).ok())
        .map_or(0, |b| b.len());
    let options = serde_json::to_vec(&cfg.options).map_or(0, |b| b.len());
    let tags: usize = cfg.tags.iter().map(String::len).sum();
    app_data + options + tags
}

/// Variant name for error messages — never the payload.
fn client_message_kind(m: &ClientMessage) -> &'static str {
    match m {
        ClientMessage::Audio(_) => "audio",
        ClientMessage::Message(_) => "message",
        ClientMessage::Control(_) => "control",
    }
}

// ── Model identity resolution ───────────────────────────────

/// Resolve the model identity for each component once at session setup.
///
/// Prefers the provider's own [`model_identity`](crate::tts::TtsProvider::model_identity)
/// (which may probe the running process, e.g. `/v1/models` on an
/// OpenAI-compatible sidecar); falls back to the configured model name.
/// Realtime (S2S) sessions report the single fused model for all three slots.
///
/// Returns `(stt, llm, tts)`.
async fn resolve_model_identities(
    state: &TransportState,
) -> (
    Option<ModelIdentity>,
    Option<ModelIdentity>,
    Option<ModelIdentity>,
) {
    if let Some(rt) = &state.config.realtime_provider {
        let id = rt.model.clone().map(ModelIdentity::from_config);
        return (id.clone(), id.clone(), id);
    }

    let stt = match &state.stt {
        Some(p) => p.model_identity().await,
        None => None,
    }
    .or_else(|| state.config.stt.as_ref().and_then(stt_config_identity));

    let tts = match &state.tts {
        Some(p) => p.model_identity().await,
        None => None,
    }
    .or_else(|| state.config.tts.as_ref().and_then(tts_config_identity));

    let mut llm = None;
    for p in state.processors.iter() {
        if let Some(id) = p.model_identity().await {
            llm = Some(id);
            break;
        }
    }
    if llm.is_none() {
        llm = state
            .config
            .text_processor
            .iter()
            .find_map(llm_config_identity);
    }

    (stt, llm, tts)
}

/// Resolve the compute device (gpu/cpu) for each component once at session
/// setup, mirroring [`resolve_model_identities`]. Returns `(stt, llm, tts)`;
/// `None` per slot when the provider doesn't report a device (the diagnostic
/// then shows no device badge rather than guessing). Realtime (S2S) sessions
/// run against cloud providers with no local device, so all three are `None`.
/// How the session's STT provider delivers transcripts: `"streaming"` when
/// it declares [`SttCapability::Streaming`](crate::stt::SttCapability),
/// `"batch"` otherwise. `None` on realtime sessions and sessions without STT.
pub(super) fn stt_mode(state: &TransportState) -> Option<&'static str> {
    if state.config.realtime_provider.is_some() {
        return None;
    }
    state.stt.as_ref().map(|p| stt_mode_of(p.as_ref()))
}

/// [`stt_mode`] for one provider.
pub(super) fn stt_mode_of(provider: &dyn crate::stt::SttProvider) -> &'static str {
    if provider
        .capabilities()
        .contains(&crate::stt::SttCapability::Streaming)
    {
        "streaming"
    } else {
        "batch"
    }
}

async fn resolve_device_infos(
    state: &TransportState,
) -> (Option<DeviceInfo>, Option<DeviceInfo>, Option<DeviceInfo>) {
    if state.config.realtime_provider.is_some() {
        return (None, None, None);
    }

    let stt = match &state.stt {
        Some(p) => p.device_info().await,
        None => None,
    };
    let tts = match &state.tts {
        Some(p) => p.device_info().await,
        None => None,
    };
    let mut llm = None;
    for p in state.processors.iter() {
        if let Some(d) = p.device_info().await {
            llm = Some(d);
            break;
        }
    }

    (stt, llm, tts)
}

fn file_stem(p: Option<&std::path::Path>) -> Option<String> {
    p.and_then(std::path::Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
}

fn stt_config_identity(cfg: &crate::config::SttConfig) -> Option<ModelIdentity> {
    cfg.model_name
        .clone()
        .or_else(|| file_stem(cfg.model.as_deref()))
        .or_else(|| {
            cfg.model_dir
                .as_deref()
                .and_then(std::path::Path::file_name)
                .map(|s| s.to_string_lossy().into_owned())
        })
        .map(ModelIdentity::from_config)
}

fn tts_config_identity(cfg: &crate::config::TtsConfig) -> Option<ModelIdentity> {
    cfg.model_name
        .clone()
        .or_else(|| file_stem(cfg.model.as_deref()))
        .map(ModelIdentity::from_config)
}

fn llm_config_identity(cfg: &crate::config::TextProcessorConfig) -> Option<ModelIdentity> {
    cfg.options
        .get("model")
        .or_else(|| cfg.options.get("model_name"))
        .and_then(|v| v.as_str())
        .map(ModelIdentity::from_config)
}

// ── Negotiation ─────────────────────────────────────────────

/// Negotiate the session configuration from client preferences and
/// server config, then send `SessionReady` to the client.
pub(super) async fn negotiate_and_announce(
    client_config: &Option<SessionConfig>,
    state: &TransportState,
    session_id: &str,
    session_token: &str,
    resumed: bool,
    replay_messages: &[ServerMessage],
    out_tx: &mpsc::Sender<ServerMessage>,
) -> NegotiatedConfig {
    // For classic sessions, the local TTS provider's native sample rate is
    // the ultimate fallback when neither server config nor client preference
    // specifies an output rate. For realtime sessions, there is no local TTS
    // provider and the realtime runner is responsible for plumbing the
    // configured `output_emit_rate`; that rate (its default when unset) is
    // the fallback, so negotiation offers what the session will emit.
    let tts_native_rate = state.tts.as_ref().map_or_else(
        || {
            state
                .config
                .realtime_provider
                .as_ref()
                .and_then(|r| r.output_emit_rate().ok())
                .unwrap_or(crate::config::DEFAULT_OUTPUT_EMIT_RATE)
        },
        |t| t.sample_rate(),
    );
    let client_cfg = client_config.clone().unwrap_or_default();
    let client_output_pref = client_cfg.output_sample_rate;

    // ── Input rate ──────────────────────────────────────────
    //
    // The client proposes; the server may lower. Browsers capture at 48 kHz
    // while every STT here works at 16 kHz, so the default path ships 3x the
    // bytes the server then throws away — 768 kbps up where 256 would do, on
    // the leg of a consumer link most likely to be narrow.
    //
    // Lowering is gated on `accepts_negotiated_input_rate` because the rate is
    // bound once at the handshake: a client that ignores the negotiated value
    // and keeps sending 48 kHz would have its audio read at 16 kHz, and
    // nothing downstream could detect it. Old clients therefore keep exactly
    // today's behaviour.
    //
    // Never raise: if a client proposes 8 kHz it presumably means it, and
    // upsampling adds no information.
    let client_input_rate = {
        let proposed = client_cfg.input_rate();
        let stt_rate = state.stt.as_ref().map(|s| s.expected_sample_rate());
        match stt_rate {
            Some(rate) if client_cfg.accepts_negotiated_input_rate && rate < proposed => {
                info!(
                    session_id = %session_id,
                    proposed,
                    negotiated = rate,
                    "client accepts a negotiated input rate; lowering to the STT's native rate"
                );
                rate
            }
            _ => proposed,
        }
    };

    let negotiated_output_rate = state
        .config
        .output_rate()
        .or(client_output_pref)
        .unwrap_or(tts_native_rate);

    let (negotiated_audio_format, mut negotiated_options) = match client_cfg.audio_format {
        AudioFormat::Opus => {
            warn!(
                session_id = %session_id,
                "client requested opus audio_format but this worker transport path currently serves PCM; downgrading to pcm_i16_le"
            );
            let mut opts = std::collections::HashMap::new();
            opts.insert(
                "audio_format_fallback".to_string(),
                serde_json::Value::String("opus_to_pcm_i16_le".to_string()),
            );
            (AudioFormat::PcmI16Le, opts)
        }
        other => (other, std::collections::HashMap::new()),
    };
    if !client_cfg.options.is_empty() {
        negotiated_options.extend(client_cfg.options.clone());
    }

    // Resolve the model actually running for each component (once per session).
    let (stt_model, llm_model, tts_model) = resolve_model_identities(state).await;

    let negotiated = NegotiatedConfig {
        audio_format: negotiated_audio_format,
        input_sample_rate: client_input_rate,
        output_sample_rate: negotiated_output_rate,
        channels: client_cfg.channels,
        language: client_cfg.language.clone(),
        voice: client_cfg
            .voice
            .clone()
            .or_else(|| state.config.tts.as_ref().and_then(|t| t.voice.clone())),
        stt_provider: Some(
            state
                .config
                .stt
                .as_ref()
                .map_or_else(|| "unknown".into(), |s| s.provider.clone()),
        ),
        tts_provider: Some(
            state
                .config
                .tts
                .as_ref()
                .map_or_else(|| "unknown".into(), |t| t.provider.clone()),
        ),
        app_data: None,
        options: negotiated_options,
        mode: client_cfg.mode,
        protocol_version: crate::transport::PROTOCOL_VERSION,
        // Canonical stt/llm/tts descriptors for the built-in pipeline, or
        // the single-component Realtime descriptor when the session is
        // backed by an S2S provider (OpenAI Realtime / Gemini Live).
        metric_descriptors: {
            let mut descriptors = if state.config.realtime_provider.is_some() {
                crate::pipeline::realtime_descriptors()
            } else {
                crate::pipeline::default_descriptors()
            };
            // Streaming-phase throughput (`output`) and one
            // `audio_post:<name>` per inline processor.
            descriptors.push(crate::pipeline::ComponentMetricDescriptor::output());
            descriptors.extend(
                state
                    .audio_post_processors
                    .iter()
                    .map(|p| p.turn_descriptor()),
            );
            // One `annotator:<name>` component per annotate-mode processor.
            descriptors.extend(state.annotators.iter().map(|spec| {
                crate::pipeline::ComponentMetricDescriptor::annotator(
                    spec.annotator.name(),
                    spec.config.batch_ms,
                )
            }));
            descriptors
        },
        stt_model,
        llm_model,
        tts_model,
        first_utterance_id: crate::transport::FIRST_UTTERANCE_ID,
        text_utterance_id_base: crate::transport::TEXT_UTTERANCE_ID_BASE,
        // Tell the client which of its options the realtime runner
        // will not honour (same allowlist the runner applies).
        ignored_options: state
            .config
            .realtime_provider
            .as_ref()
            .map(|rt| {
                super::client_overrides::filter(&client_cfg.options, &rt.client_overrides).ignored
            })
            .unwrap_or_default(),
    };

    if let Some(client_version) = client_cfg.protocol_version
        && client_version != crate::transport::PROTOCOL_VERSION
    {
        warn!(
            session_id = %session_id,
            client_protocol_version = client_version,
            server_protocol_version = crate::transport::PROTOCOL_VERSION,
            "client requested protocol version different from server; negotiating server version"
        );
    }

    info!(
        session_id = %session_id,
        input_rate = negotiated.input_sample_rate,
        output_rate = negotiated.output_sample_rate,
        stt = ?negotiated.stt_provider,
        tts = ?negotiated.tts_provider,
        protocol_version = negotiated.protocol_version,
        "session negotiated"
    );

    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::SessionReady(
            SessionInfo {
                session_id: session_id.to_string(),
                session_token: session_token.to_string(),
                resumed,
                client_config: client_config.clone(),
                negotiated: negotiated.clone(),
            },
        )))
        .await;

    for msg in replay_messages {
        let _ = out_tx.send(msg.clone()).await;
    }

    negotiated
}

// ── Audit sink ──────────────────────────────────────────────

/// Set up the streaming audit sink (if auditing is enabled).
///
/// Also writes `metadata.json` into the captures directory.
pub(super) async fn setup_audit_sink(
    state: &TransportState,
    session_id: &str,
    negotiated: &NegotiatedConfig,
    client_config: &Option<SessionConfig>,
) -> Option<StreamingAuditSink> {
    const AUDIT_CHAN_CAP: usize = 256;

    if !state.config.pipeline.audit.enabled {
        return None;
    }

    let captures_dir = state
        .config
        .pipeline
        .audit
        .captures_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from(crate::config::DEFAULT_CAPTURES_DIR));
    let audit_store: Arc<dyn crate::audit::store::AuditStore> =
        Arc::new(crate::audit::store::LocalFsAuditStore::new(captures_dir));

    // Write metadata.json before any artefact can land, so the admin UI
    // and the retention sweep recognise the directory as a session from
    // its first file on.
    let tags = client_config
        .as_ref()
        .map(|c| &c.tags)
        .cloned()
        .unwrap_or_default();
    // Resolve each component's compute device (gpu/cpu) once, alongside the
    // model identity already on `negotiated`. `null` per slot when unreported.
    let (stt_device, llm_device, tts_device) = resolve_device_infos(state).await;
    let meta = serde_json::json!({
        "id": session_id,
        "start_time": chrono::Utc::now().to_rfc3339(),
        "pipeline_name": state.config.pipeline.name,
        "pipeline_config_hash": crate::config::config_hash(&state.config),
        "stt_provider": negotiated.stt_provider,
        "tts_provider": negotiated.tts_provider,
        // How the STT provider delivers transcripts, from its declared
        // capabilities: "streaming" or "batch" (null on realtime sessions,
        // which have no separate STT). The BFF report uses it to decide
        // whether STT processing time is on the user's wait.
        "stt_mode": stt_mode(state),
        // How the end of a turn is decided, and the silence it waits for.
        //
        // The wait for silence is the floor under `endpointing_ms`: with a
        // 700 ms timeout no turn can commit sooner than 700 ms after the
        // speaker stops, however fast the recogniser is. Without this on
        // record the report saw 740 ms against a 500 ms target and named the
        // recogniser as the bottleneck for a delay that was configuration.
        // The fallback is the one the VAD filter applies when the section is
        // absent, so what is written is what was in force.
        "turn_detection": {
            "mode": state.config.turn_detection.as_ref().map_or("silence", |td| td.mode.as_str()),
            "silence_timeout_ms": state.config.silence_timeout_ms(),
        },
        // The operator's grading rules, when the pipeline sets any. Written
        // here so a session is graded by the rules in force when it ran, and
        // so editing `[grading]` later cannot re-grade it. Omitted entirely
        // when unset, which the report reads as "the defaults".
        "grading": state.config.grading,
        // The voice the session speaks with (client override, else the
        // configured `[tts] voice`).
        "tts_voice": negotiated.voice,
        // Resolved model identity per component (the model actually running,
        // even cross-process). The BFF report reads these as the primary
        // source for the providers block.
        "stt_model": negotiated.stt_model,
        "llm_model": negotiated.llm_model,
        "tts_model": negotiated.tts_model,
        // Resolved compute device per component (gpu/cpu), sibling to the
        // *_model blocks. The BFF report reads these for the providers block.
        "stt_device": stt_device,
        "llm_device": llm_device,
        "tts_device": tts_device,
        "input_sample_rate": negotiated.input_sample_rate,
        "output_sample_rate": negotiated.output_sample_rate,
        "tags": tags,
        // Persist the per-component metric descriptors so the
        // session-detail page can render the descriptor-driven
        // per-turn table without re-deriving them server-side.
        "metric_descriptors": negotiated.metric_descriptors,
    });
    let meta_key = format!("{session_id}/metadata.json");
    let meta_bytes = serde_json::to_string_pretty(&meta).unwrap_or_default();
    if let Err(e) = audit_store.put(&meta_key, meta_bytes.as_bytes()).await {
        warn!(error = %e, "failed to write session metadata.json");
    }

    let audit_cfg = &state.config.pipeline.audit;
    let sink = StreamingAuditSink::spawn_with_budget(
        std::sync::Arc::clone(&audit_store),
        session_id.to_string(),
        audit_cfg.buffer_size.max(AUDIT_CHAN_CAP),
        audit_cfg.max_session_bytes,
    );

    Some(sink)
}

/// Start the continuous two-sided recorder, when it is configured.
///
/// Separate from [`setup_audit_sink`] because it is separately opted into:
/// the per-utterance artefacts are useful on any audited session, while this
/// records everything including silence.
pub(super) fn setup_session_recorder(
    state: &TransportState,
    session_id: &str,
) -> Option<crate::audit::SessionRecorder> {
    let audit_cfg = &state.config.pipeline.audit;
    if !audit_cfg.enabled || !audit_cfg.record_session_audio {
        return None;
    }
    let captures_dir = audit_cfg
        .captures_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from(crate::config::DEFAULT_CAPTURES_DIR));
    let store: Arc<dyn crate::audit::store::AuditStore> =
        Arc::new(crate::audit::store::LocalFsAuditStore::new(captures_dir));
    Some(crate::audit::SessionRecorder::spawn(
        store,
        session_id.to_string(),
        audit_cfg.session_audio_max_seconds,
    ))
}

// ── Per-session text processors ─────────────────────────────

/// A channel whose trace events land in the session's audit sink, or `None`
/// when the session records nothing. A component that traces gets its own.
fn session_trace_tx(
    streaming_audit: &Option<StreamingAuditSink>,
) -> Option<mpsc::Sender<crate::audit::TraceEvent>> {
    let sink = streaming_audit.as_ref()?.clone();
    let (tx, mut rx) = mpsc::channel::<crate::audit::TraceEvent>(64);
    tokio::spawn(async move {
        while let Some(evt) = rx.recv().await {
            sink.record(crate::audit::AuditEvent::Trace(evt));
        }
    });
    Some(tx)
}

/// Build fresh text processors for this session.
///
/// Each session gets its own conversation history and trace forwarding.
pub(super) fn build_session_processors(
    state: &TransportState,
    streaming_audit: &Option<StreamingAuditSink>,
) -> Arc<Vec<Box<dyn TextProcessor>>> {
    let trace_tx = session_trace_tx(streaming_audit);

    match crate::config::build_text_processors(&state.config, trace_tx, Some(&state.param_registry))
    {
        Ok(p) => Arc::new(p),
        Err(e) => {
            tracing::error!(
                error = %e,
                "failed to build per-session text processors, using shared"
            );
            Arc::clone(&state.processors)
        }
    }
}

// ── Audio pipeline ──────────────────────────────────────────

/// Handles and channels returned by `setup_audio_pipeline`.
pub(super) struct AudioPipelineHandles {
    /// Send audio chunks into the STT pipeline.
    pub audio_tx: mpsc::Sender<AudioChunk>,
    /// Receive utterances from STT.
    pub utt_rx: mpsc::Receiver<pipeline::Utterance>,
    /// Send processed audio to the orchestration loop.
    pub proc_audio_tx: mpsc::Sender<AudioChunk>,
    /// Receive processed audio in the orchestration loop.
    pub proc_audio_rx: mpsc::Receiver<AudioChunk>,
    /// Clone of `utt_tx` for injecting text-input utterances.
    pub text_utt_tx: mpsc::Sender<pipeline::Utterance>,
    /// STT task join handle (None in text-only mode).
    pub stt_handle: Option<tokio::task::JoinHandle<()>>,
    /// The session's turn detector, shared with the STT task so the runner
    /// can tee it the agent's outbound audio. None in text-only mode.
    pub turn_detector: Option<std::sync::Arc<dyn crate::stt::turn::TurnDetector>>,
    /// Shared turn state (Listening / Processing / Speaking).
    pub turn_state: SharedTurnState,
    /// Watch channel to notify the interrupt monitor of new cancel tokens.
    pub cancel_watch_tx: watch::Sender<CancellationToken>,
    /// Speculative execution partial channel
    pub spec_rx: Option<mpsc::Receiver<pipeline::Utterance>>,
    /// True while the VAD filter has confirmed, uncommitted user speech
    /// in flight (speech onset seen, end-of-utterance sentinel not yet
    /// emitted). Stays false in text-only mode.
    pub user_speaking: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Set up the audio pipeline: VAD -> (barge-in) -> STT.
///
/// In text-only mode, the audio path is skipped and only the text
/// utterance injection channel is live.
pub(super) fn setup_audio_pipeline(
    state: &TransportState,
    negotiated: &NegotiatedConfig,
    session_id: &str,
    streaming_audit: &Option<StreamingAuditSink>,
    pe_tx: &tokio::sync::broadcast::Sender<pipeline::events::PipelineEvent>,
) -> Result<AudioPipelineHandles, anyhow::Error> {
    let session_mode = negotiated.mode;
    let (audio_tx, raw_audio_rx) = mpsc::channel::<AudioChunk>(AUDIO_IN_BUF);
    let (utt_tx, utt_rx) = mpsc::channel::<pipeline::Utterance>(UTTERANCE_BUF);
    let (proc_audio_tx, proc_audio_rx) = mpsc::channel::<AudioChunk>(AUDIO_OUT_BUF);

    let turn_state = pipeline::new_turn_state();
    let noop_token = CancellationToken::new();
    let (cancel_watch_tx, cancel_watch_rx) = watch::channel(noop_token);

    let text_utt_tx = utt_tx.clone();

    let barge_in_enabled =
        session_mode.accepts_audio() && state.config.barge_in.as_ref().is_some_and(|bi| bi.enabled);

    let mut spec_rx = None;
    let mut session_turn_detector: Option<std::sync::Arc<dyn crate::stt::turn::TurnDetector>> =
        None;
    let user_speaking = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let stt_handle = if session_mode.accepts_audio() {
        // VAD filter (utterance segmentation).
        let silence_ms = state.config.silence_timeout_ms();
        let pre_roll_ms = state
            .config
            .vad
            .as_ref()
            .map_or(crate::config::DEFAULT_VAD_PRE_ROLL_MS, |v| {
                v.effective_pre_roll_ms()
            });
        let vad = {
            // Fetch the shared VAD store (registered once at worker start).
            // When present, the VAD adopts its atomic cells so PATCH
            // /debug/params updates take effect on the next chunk.
            let store_arc = state.param_registry.store("vad");
            let store_ref = store_arc.as_ref().and_then(|s| {
                s.as_any()
                    .downcast_ref::<crate::api::param_store::AtomicParamStore>()
            });
            crate::config::build_vad(&state.config, store_ref).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to build VAD for transport, using passthrough");
                Box::new(crate::vad::segmenter::PassthroughVad::new())
            })
        };
        // Read before the VAD moves into its task: barge-in below needs to know
        // whether anything local is actually judging the audio.
        let local_vad_detects_speech = vad.name() != "passthrough";
        let filter_chain = pipeline::build_filter_chain(&state.config, None).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to build audio filter chain for transport, proceeding without filter");
            None
        });
        let (vad_out_tx, vad_out_rx) = mpsc::channel::<AudioChunk>(AUDIO_IN_BUF);
        tokio::spawn(pipeline::vad_filter_task_with_filter(
            raw_audio_rx,
            vad_out_tx,
            vad,
            filter_chain,
            silence_ms,
            pre_roll_ms,
            Some(std::sync::Arc::clone(&user_speaking)),
        ));

        // Barge-in.
        // The settings struct holds atomic cells; when the worker has
        // registered a "barge_in" param store, we adopt its cells so PATCH
        // /debug/params updates take effect on every active session.
        let barge_in_settings = state
            .config
            .barge_in
            .as_ref()
            .map(|bi| pipeline::BargeInSettings::new(bi.speech_threshold, bi.min_chunks, false))
            .unwrap_or_default();
        let barge_in_settings = {
            let store_arc = state.param_registry.store("barge_in");
            let store_ref = store_arc.as_ref().and_then(|s| {
                s.as_any()
                    .downcast_ref::<crate::api::param_store::AtomicParamStore>()
            });
            if let Some(store) = store_ref {
                let mut s = barge_in_settings;
                if let Some(c) = store.float_cell("speech_threshold") {
                    s = s.with_speech_threshold_cell(c);
                }
                if let Some(c) = store.int_cell("min_chunks") {
                    s = s.with_min_chunks_cell(c);
                }
                if let Some(c) = store.bool_cell("barge_in_during_processing") {
                    s = s.with_barge_in_during_processing_cell(c);
                }
                s
            } else {
                barge_in_settings
            }
        };

        let stt_input_rx = if barge_in_enabled {
            let (stt_tx, stt_rx) = mpsc::channel::<AudioChunk>(AUDIO_IN_BUF);
            let (monitor_tx, monitor_rx) = mpsc::channel::<AudioChunk>(AUDIO_IN_BUF);
            tokio::spawn(pipeline::mic_audio_splitter(vad_out_rx, stt_tx, monitor_tx));

            let (vad_tx, mut vad_rx) = mpsc::channel::<crate::vad::engine::VadEvent>(32);
            let rms_vad = crate::vad::engine::RmsVadEngine::new(
                monitor_rx,
                std::sync::Arc::clone(&turn_state),
                barge_in_settings.clone(),
            );

            let (stt_vad_hook_tx, stt_vad_hook_rx) =
                mpsc::channel::<crate::stt::TranscriptionResult>(32);
            let provider_vad = crate::vad::engine::ProviderVadEngine::from_source(
                Box::new(crate::vad::engine::SttResultSignalSource::new(
                    stt_vad_hook_rx,
                )),
                std::sync::Arc::clone(&turn_state),
                barge_in_settings,
            )
            .text_implies_speech(!local_vad_detects_speech);

            let mut composite_vad = crate::vad::engine::CompositeVadEngine::new(vec![
                Box::new(rms_vad),
                Box::new(provider_vad),
            ]);

            let vad_tx_clone = vad_tx.clone();
            tokio::spawn(async move {
                use crate::vad::engine::VadEngine;
                composite_vad.start(vad_tx_clone).await;
            });

            let vad_cancel_rx = cancel_watch_rx.clone();
            tokio::spawn(async move {
                use crate::vad::engine::VadEvent;
                while let Some(event) = vad_rx.recv().await {
                    if let VadEvent::SpeechStarted { .. } = event {
                        let token = vad_cancel_rx.borrow().clone();
                        if !token.is_cancelled() {
                            tracing::debug!("VAD triggered interruption, cancelling current token");
                            token.cancel();
                        }
                    }
                }
            });

            info!(session_id = %session_id, "server-side barge-in enabled via generic CompositeVadEngine");
            (stt_rx, Some(stt_vad_hook_tx))
        } else {
            (vad_out_rx, None)
        };

        let (speculative_tx, s_rx) = if state.config.speculative.enabled {
            let (tx, rx) = mpsc::channel::<pipeline::Utterance>(32);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        spec_rx = s_rx;

        // STT task (long-lived, one per session).
        let turn_detector =
            crate::config::build_turn_detector(&state.config, session_trace_tx(streaming_audit))?;
        session_turn_detector = Some(std::sync::Arc::clone(&turn_detector));
        let min_chars = state.config.pipeline.min_utterance_chars;
        let stt = Arc::clone(
            state
                .stt
                .as_ref()
                .expect("classic audio pipeline requires a local STT provider"),
        );
        let (rx, hook) = stt_input_rx;
        Some(tokio::spawn(pipeline::stt_task(
            rx,
            utt_tx,
            speculative_tx,
            stt,
            turn_detector,
            streaming_audit.clone(),
            Some(pe_tx.clone()),
            min_chars,
            hook,
            negotiated.language.clone(),
        )))
    } else {
        drop(cancel_watch_rx);
        drop(raw_audio_rx);
        drop(utt_tx);
        info!(session_id = %session_id, mode = ?session_mode, "audio pipeline skipped (text-only mode)");
        None
    };

    Ok(AudioPipelineHandles {
        audio_tx,
        utt_rx,
        proc_audio_tx,
        proc_audio_rx,
        text_utt_tx,
        stt_handle,
        turn_detector: session_turn_detector,
        turn_state,
        cancel_watch_tx,
        spec_rx,
        user_speaking,
    })
}

// ── Setup-window inbound drain (shared by both runners) ─────

/// Put client frames that arrived during component setup back in front of
/// the live inbound stream, in arrival order. With nothing held the
/// original receiver is returned untouched (no extra hop).
pub(super) fn replay_held_frames(
    mut live: mpsc::Receiver<ClientMessage>,
    held: Vec<ClientMessage>,
) -> mpsc::Receiver<ClientMessage> {
    if held.is_empty() {
        return live;
    }
    let (tx, rx) = mpsc::channel::<ClientMessage>(held.len() + 64);
    for msg in held {
        // Capacity covers every held frame; a full channel here is a bug.
        let _ = tx.try_send(msg);
    }
    tokio::spawn(async move {
        while let Some(msg) = live.recv().await {
            if tx.send(msg).await.is_err() {
                break; // the session loop is gone
            }
        }
    });
    rx
}

/// Cap on control/text frames held during setup. A client can only push
/// ~64 frames per 1.3 s past the WS pump anyway; this bounds memory if
/// setup is slow and the client is chatty.
const MAX_HELD_FRAMES: usize = 256;

/// Keeps the inbound channel drained while per-session setup runs.
///
/// `session_ready` has already gone out when setup starts, so a client may
/// stream mic audio or send a typed message right away. Nothing reads
/// `in_rx` during setup, the 64-slot channel fills in ~1.3 s of 20 ms
/// frames, the WS pump blocks on it and stops reading the socket, pongs go
/// unread and the heartbeat closes the connection before setup finishes.
/// This task discards pre-session audio (loss-tolerant; nothing is
/// listening yet) after teeing it into the session recorder, if one is
/// running, so the recording starts at `session_ready` rather than at the
/// end of setup. It answers app-level pings, and holds every other frame
/// for replay so a typed message or `tts_only_synthesize` sent during
/// setup is not lost (it showed up as a worker "not listening at all").
///
/// Used by the classic runner around component construction and by the
/// realtime runner around the provider/agent connects. Dropping
/// it before `finish` (an early-return error path) aborts the task, which
/// closes `in_rx` and lets the WS pump exit.
pub(super) struct SetupDrain {
    done_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<(mpsc::Receiver<ClientMessage>, Vec<ClientMessage>)>>,
}

impl SetupDrain {
    /// `recorder` carries the negotiated input rate: the session loop
    /// stamps it on every inbound chunk, and the recording must agree.
    pub(super) fn spawn(
        mut in_rx: mpsc::Receiver<ClientMessage>,
        out_tx: mpsc::Sender<ServerMessage>,
        session_id: String,
        recorder: Option<(crate::audit::SessionRecorder, u32)>,
    ) -> Self {
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let mut discarded_audio: u64 = 0;
            let mut dropped_held: u64 = 0;
            let mut held: Vec<ClientMessage> = Vec::new();
            loop {
                tokio::select! {
                    _ = &mut done_rx => break,
                    msg = in_rx.recv() => match msg {
                        Some(ClientMessage::Control(ClientControl::Ping { timestamp_ms })) => {
                            super::gateway::shared_handlers::handle_ping(&out_tx, timestamp_ms)
                                .await;
                        }
                        Some(ClientMessage::Audio(chunk)) => {
                            if let Some((recorder, rate)) = &recorder {
                                recorder.record(
                                    crate::audit::SessionAudioSide::User,
                                    &chunk.samples,
                                    *rate,
                                    chunk.channels,
                                );
                            }
                            discarded_audio += 1;
                        }
                        Some(other) => {
                            if held.len() < MAX_HELD_FRAMES {
                                held.push(other);
                            } else {
                                dropped_held += 1;
                            }
                        }
                        None => break,
                    }
                }
            }
            if discarded_audio > 0 || !held.is_empty() || dropped_held > 0 {
                debug!(
                    session_id = %session_id,
                    discarded_audio,
                    held = held.len(),
                    dropped_held,
                    "pre-session client frames during setup (audio dropped, the rest replayed)"
                );
            }
            (in_rx, held)
        });
        Self {
            done_tx: Some(done_tx),
            handle: Some(handle),
        }
    }

    /// Stop draining and hand back the inbound receiver with the held
    /// frames replayed in front of the live stream.
    pub(super) async fn finish(
        mut self,
    ) -> Result<mpsc::Receiver<ClientMessage>, tokio::task::JoinError> {
        if let Some(tx) = self.done_tx.take() {
            let _ = tx.send(());
        }
        let handle = self.handle.take().expect("finish is called at most once");
        let (rx, held) = handle.await?;
        Ok(replay_held_frames(rx, held))
    }
}

impl Drop for SetupDrain {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

#[cfg(test)]
#[path = "tests/setup.rs"]
mod tests;
