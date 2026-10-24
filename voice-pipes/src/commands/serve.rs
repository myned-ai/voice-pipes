//! Worker node command: serve the pipeline over HTTP + WebSocket.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tracing::{error, info, warn};

use crate::api::types::{VadInfo, WorkerConfig, WorkerState};
use crate::config::PipelineConfig;
use crate::transport::session_store::{SessionStore, SessionStoreConfig};
use crate::{api, config, stt, text, tts};

/// Maximum number of consecutive ports to try when the configured port is busy.
const PORT_SCAN_LIMIT: u16 = 16;

/// Bound for the turn/session telemetry channels forwarded to the orchestrator.
/// Records are low-rate (one per turn / per session lifecycle event),
/// so this is generous headroom; if the orchestrator forwarder stalls, senders
/// drop records with a warning rather than growing memory without bound.
const TELEMETRY_CHANNEL_BUF: usize = 1024;

/// If `provider_or_type == "grpc"` and an endpoint is set, connect a
/// [`RemoteParamStore`] and register it under `component_id`. Returns
/// `true` when a remote store was registered, `false` otherwise (so the
/// caller can fall back to a local `AtomicParamStore`).
///
/// Only meaningful when at least one client-side gRPC feature is enabled.
#[allow(
    clippy::unused_async,
    reason = "the await is only exercised when a client-side gRPC feature is compiled in"
)]
async fn register_remote_if_grpc(
    registry: &Arc<crate::api::param_store::ParamRegistry>,
    component_id: &str,
    provider_or_type: &str,
    endpoint: Option<&str>,
    schema: Vec<crate::api::param_schema::ParamDescriptor>,
    initial_values: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> bool {
    if provider_or_type != "grpc" {
        return false;
    }
    #[cfg(any(feature = "stt-grpc", feature = "tts-grpc", feature = "text-grpc"))]
    {
        let Some(endpoint) = endpoint else {
            warn!(
                component = component_id,
                "grpc-backed component declared but no endpoint configured; skipping remote ParamStore"
            );
            return false;
        };
        match crate::grpc::params::RemoteParamStore::connect(endpoint, component_id).await {
            Ok(store) => {
                info!(
                    component = component_id,
                    endpoint = endpoint,
                    "registered RemoteParamStore proxy for grpc-backed component"
                );

                if let Some(opts) = initial_values {
                    use crate::api::param_store::MutableParamStore;
                    for param in &schema {
                        if let Some(val) = opts.get(&param.key) {
                            match param.validate(val.clone()) {
                                Ok(typed_val) => {
                                    if let Err(e) = store.set_param(&param.key, typed_val).await {
                                        warn!(component = component_id, key = %param.key, error = %e, "failed to apply initial config value to remote component");
                                    } else {
                                        info!(component = component_id, key = %param.key, "applied initial config value to remote component");
                                    }
                                }
                                Err(e) => {
                                    warn!(component = component_id, key = %param.key, error = %e, "initial config value validation failed");
                                }
                            }
                        }
                    }
                }

                registry.register(component_id, schema, Arc::new(store));
                true
            }
            Err(e) => {
                warn!(
                    component = component_id,
                    endpoint = endpoint,
                    error = %e,
                    "failed to connect RemoteParamStore; falling back to local AtomicParamStore (mutations will be ignored by remote)"
                );
                false
            }
        }
    }
    #[cfg(not(any(feature = "stt-grpc", feature = "tts-grpc", feature = "text-grpc")))]
    {
        let _ = (registry, component_id, endpoint, schema, initial_values);
        false
    }
}

/// The local address something already answers on at `port`, if any.
///
/// A bind can succeed while another process serves the same port: Windows
/// lets `127.0.0.1:9090` be bound beside another process's `0.0.0.0:9090`
/// or `[::1]:9090`, and clients that resolve `localhost` to `::1` then reach
/// the other process. A Docker orchestrator left running did exactly that to
/// a native one. Loopback refuses an unserved port at once, so the probe
/// costs nothing when the port is free.
async fn port_served_locally(port: u16) -> Option<std::net::SocketAddr> {
    const PROBE: std::time::Duration = std::time::Duration::from_millis(300);
    for ip in [
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
    ] {
        let addr = std::net::SocketAddr::new(ip, port);
        if let Ok(Ok(_)) = tokio::time::timeout(PROBE, tokio::net::TcpStream::connect(addr)).await {
            return Some(addr);
        }
    }
    None
}

/// Try to bind a TCP listener on `host:port`.  If the port is already taken,
/// on this address or by another process answering on loopback, try up to
/// [`PORT_SCAN_LIMIT`] successive ports.  Returns the listener and the port
/// it actually bound to.
pub(crate) async fn try_bind(
    host: &str,
    port: u16,
    label: &str,
) -> Result<(tokio::net::TcpListener, u16), anyhow::Error> {
    for offset in 0..PORT_SCAN_LIMIT {
        let candidate = port
            .checked_add(offset)
            .ok_or_else(|| anyhow::anyhow!("{label}: port overflow while scanning from {port}"))?;
        if let Some(served) = port_served_locally(candidate).await {
            warn!(
                "{label}: port {candidate} is already served on {served} by another process \
                 (a Docker container left running?), trying next..."
            );
            continue;
        }
        let addr = format!("{host}:{candidate}");
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                if offset > 0 {
                    warn!(
                        configured_port = port,
                        actual_port = candidate,
                        "{label}: configured port {port} was unavailable, bound to {candidate} instead"
                    );
                }
                return Ok((listener, candidate));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                warn!("{label}: port {candidate} already in use, trying next...");
            }
            Err(e) => {
                return Err(anyhow::anyhow!("{label}: failed to bind {addr}: {e}"));
            }
        }
    }
    Err(anyhow::anyhow!(
        "{label}: ports {port}-{} are all in use -- free a port or choose a different range",
        port + PORT_SCAN_LIMIT - 1
    ))
}

// ── Serve: worker node ──────────────────────────────────────
//
// `run_serve` orchestrates four largely independent bootstrapping
// concerns:
//
//   1. **Worker bootstrap** -- load the worker config, probe VAD/STT
//      capabilities, build the [`WorkerState`], and register
//      per-component parameter stores.
//   2. **Transport bootstrap** -- when transport is enabled, build the
//      STT/TTS/text/audio-post providers, the [`TransportState`], the
//      router, and bind its listener.
//   3. **Orchestrator client lifecycle** -- register with the
//      orchestrator and run the heartbeat loop with re-registration
//      on failure.
//   4. **Event forwarders** -- record completed turn records in the
//      worker's local history (served by `GET /debug/sessions`) and, when
//      an orchestrator is configured, relay turn and session lifecycle
//      events from the transport runner back to it.
//
// Each concern is extracted into its own helper below so the
// top-level `run_serve` reads as a wiring routine and each piece can
// evolve (new providers, auth, lifecycle hooks, deployment behaviors)
// without touching the others.

/// Plumbing produced by [`bootstrap_transport`] when transport is
/// enabled -- owned by `run_serve` and threaded into the various
/// downstream spawn helpers.
struct TransportSetup {
    listener: tokio::net::TcpListener,
    app: axum::Router,
    turn_rx: tokio::sync::mpsc::Receiver<crate::api::types::TurnRecord>,
    session_rx: tokio::sync::mpsc::Receiver<crate::api::types::SessionRecord>,
    /// Actual bound port (after [`try_bind`] port-scanning).
    port: u16,
}

/// 1) Worker bootstrap -- load worker config, probe VAD/STT capabilities,
///    and build the shared [`WorkerState`].  Component parameter stores are
///    registered separately by [`register_param_stores`].
fn build_worker_state(
    pipeline_config: PipelineConfig,
    worker_config_path: Option<PathBuf>,
) -> Result<Arc<WorkerState>, anyhow::Error> {
    let worker_config: WorkerConfig = if let Some(path) = worker_config_path {
        let text = std::fs::read_to_string(&path)?;
        toml::from_str(&text)?
    } else {
        WorkerConfig::default()
    };

    let worker_id = worker_config
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let captures_dir = pipeline_config.pipeline.audit.captures_dir.clone();
    let audit_tap_dir = pipeline_config.pipeline.audit_tap.dir.clone();
    let audit_store: Option<Arc<dyn crate::audit::store::AuditStore>> = captures_dir
        .as_ref()
        .map(|d| Arc::new(crate::audit::store::LocalFsAuditStore::new(d.clone())) as _);
    let tap_store: Arc<dyn crate::audit::store::AuditStore> =
        Arc::new(crate::audit::store::LocalFsAuditStore::new(audit_tap_dir));

    // Resolve VAD and STT info for the debug API.
    let vad = config::build_vad(&pipeline_config, None)?;
    let vad_backend_name = vad.name().to_string();
    let vad_cfg = pipeline_config.vad.as_ref();
    let vad_info = VadInfo {
        mode: vad_cfg.map_or_else(|| "auto".to_string(), |v| v.mode.clone()),
        backend: vad_backend_name.clone(),
        model: vad_cfg
            .and_then(|v| v.model.as_ref())
            .map(|p| p.display().to_string()),
        // The threshold the selected mode applies: RMS energy for the
        // energy backend, a speech probability for a model.
        threshold: vad_cfg.map_or(0.5, |v| {
            if v.mode == "energy" {
                v.effective_energy_threshold() as f32
            } else {
                v.effective_speech_prob()
            }
        }),
        neg_threshold: vad_cfg.map_or(
            crate::config::DEFAULT_VAD_NEG_THRESHOLD,
            crate::config::VadSettings::effective_neg_threshold,
        ),
        min_speech_ms: vad_cfg.and_then(|v| v.min_speech_ms).unwrap_or(250),
        min_silence_ms: vad_cfg.and_then(|v| v.min_silence_ms).unwrap_or(100),
    };

    Ok(Arc::new(WorkerState {
        worker_id,
        config: pipeline_config,
        worker_config,
        audit_store,
        tap_store,
        start_time: Instant::now(),
        local_sessions: Arc::new(parking_lot::RwLock::new(VecDeque::new())),
        // Handed to `TransportState` in `bootstrap_transport` so both read one
        // counter. Stays 0 when transport is disabled -- nothing can open a
        // session, which is the honest answer rather than a stale one.
        live_sessions: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        vad_backend: vad_backend_name,
        vad_info,
        provider_facts: std::sync::OnceLock::new(),
        param_registry: Arc::new(crate::api::param_store::ParamRegistry::new()),
        component_registry: Arc::new(crate::component::ComponentRegistry::new()),
    }))
}

/// 1b) Register per-component parameter stores on the worker's
/// [`ParamRegistry`].  For grpc-backed components this proxies to the
/// remote `ParamStore`; otherwise a local `AtomicParamStore` is used.
async fn register_param_stores(state: &Arc<WorkerState>) {
    use crate::api::param_schema::ComponentParamSchema;

    // VAD -- single shared instance; per-session VADs adopt its cells so
    // PATCH /debug/params propagates worker-wide.
    let vad_store = config::build_vad_param_store(&state.config);
    let vad_schema = state
        .config
        .vad
        .as_ref()
        .map(super::super::api::param_schema::ComponentParamSchema::param_schema)
        .unwrap_or_default();
    state.param_registry.register("vad", vad_schema, vad_store);

    // Barge-in monitor.
    if let Some(bi) = state.config.barge_in.as_ref() {
        let bi_store = config::build_barge_in_param_store(&state.config);
        state
            .param_registry
            .register("barge_in", bi.param_schema(), bi_store);
    }

    // Realtime provider
    #[cfg(feature = "realtime")]
    if let Some(rt) = state.config.realtime_provider.as_ref() {
        let rt_store = config::build_realtime_param_store(&state.config);
        state
            .param_registry
            .register("realtime", rt.param_schema(), rt_store);
    }

    // TTS -- grpc -> RemoteParamStore proxy; otherwise local AtomicParamStore.
    if let Some(tts_cfg) = state.config.tts.as_ref() {
        let registered_remote = register_remote_if_grpc(
            &state.param_registry,
            "tts",
            tts_cfg.provider.as_str(),
            tts_cfg.endpoint.as_deref(),
            tts_cfg.param_schema(),
            Some(&tts_cfg.options),
        )
        .await;
        if !registered_remote {
            let tts_store = config::build_tts_param_store(&state.config);
            state
                .param_registry
                .register("tts", tts_cfg.param_schema(), tts_store);
        }
    }

    // Text processors -- one store per processor under the `pipeline_graph` id.
    let total_tps = state.config.text_processor.len();
    for (idx, tp_cfg) in state.config.text_processor.iter().enumerate() {
        let id = config::text_processor_id(&tp_cfg.processor_type, idx, total_tps);
        let endpoint = tp_cfg.options.get("endpoint").and_then(|v| v.as_str());
        let registered_remote = register_remote_if_grpc(
            &state.param_registry,
            &id,
            tp_cfg.processor_type.as_str(),
            endpoint,
            tp_cfg.param_schema(),
            Some(&tp_cfg.options),
        )
        .await;
        if !registered_remote {
            let tp_store = config::build_text_processor_param_store(tp_cfg);
            state
                .param_registry
                .register(&id, tp_cfg.param_schema(), tp_store);
        }
    }
}

/// 2) Transport bootstrap -- when transport is enabled, build providers,
///    the shared [`TransportState`], the router, and bind the WebSocket
///    listener.  Returns `Ok(None)` when transport is disabled in config.
async fn bootstrap_transport(
    state: &Arc<WorkerState>,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<Option<TransportSetup>, anyhow::Error> {
    let transport_cfg = state.config.transport.clone().unwrap_or_default();
    if !transport_cfg.enabled {
        return Ok(None);
    }

    // Realtime S2S sessions own the full speech-to-speech round-trip
    // inside the provider (OpenAI Realtime, Gemini Live, …), so there is
    // no local STT/TTS to construct. Validation already guarantees that
    // `[realtime_provider]` is mutually exclusive with `[stt]` / `[tts]`
    // (see `crate::config::validate`). We still warm-build the protocol
    // once at startup so credential / model-name typos surface before
    // the first client connects.
    #[cfg(feature = "realtime")]
    let realtime_active = state.config.realtime_provider.is_some();
    #[cfg(not(feature = "realtime"))]
    let realtime_active = false;

    #[cfg(feature = "realtime")]
    if realtime_active {
        // Build once and drop -- per-session protocols are constructed
        // inside `run_session`, but failing the startup validation here
        // catches typos in `provider`/`api_key_env` early.
        let _ = config::build_realtime_protocol(&state.config, None)?;
    }

    // No `[stt]` block means this worker serves no speech input -- a text-only
    // or text-in/voice-out deployment (`SessionMode::Text`,
    // `SessionMode::TextInVoiceOut`). Build nothing rather than falling back to
    // the build's default recogniser (`config::default_stt_provider`), which would fail
    // worker startup on a missing model the operator never asked for. The
    // capability probe above already assumes exactly this (`stt.is_some()`).
    //
    // `live` and `batch` keep the implicit default: they cannot do anything
    // without an STT, so guessing is helpful there and misleading here.
    let stt: Option<Arc<dyn stt::SttProvider>> = if realtime_active || state.config.stt.is_none() {
        None
    } else {
        Some(Arc::from(config::build_stt_provider(&state.config, None)?))
    };
    // Warm the recogniser once, here, where no session exists yet to
    // contend for its inference sets. The per-session warm-up the runner
    // also starts covers providers whose cost is per connection; a local
    // model's cost is per process, and paid here instead of by the first
    // caller. Failure is logged, not fatal: the first turn then just pays.
    if let Some(stt) = &stt {
        if let Some(capacity) = stt.concurrent_capacity() {
            let cap = state.worker_config.max_concurrent;
            if cap as usize > capacity {
                tracing::warn!(
                    provider = stt.name(),
                    pool_size = capacity,
                    max_concurrent = cap,
                    "the recogniser can decode {capacity} utterance(s) at once but the worker admits {cap} sessions: callers who stop speaking at the same moment will wait for a free set and may fail; raise `[stt.options] pool_size` or lower `max_concurrent`"
                );
            }
        }
        let t0 = std::time::Instant::now();
        match stt.warm_up().await {
            Ok(()) => info!(
                provider = stt.name(),
                elapsed_ms = t0.elapsed().as_millis() as u64,
                "recogniser warmed up"
            ),
            Err(e) => {
                tracing::warn!(provider = stt.name(), error = %e, "recogniser warm-up failed; the first turn will pay for it");
            }
        }
        if stt.warm_up_scope() == crate::stt::WarmUpScope::PerProcess {
            tokio::spawn(keep_warm(
                Arc::clone(stt),
                Arc::clone(&state.live_sessions),
                std::time::Duration::from_secs(state.config.pipeline.keep_warm_interval_s),
                shutdown.clone(),
            ));
        }
    }
    // Adopt the registered TTS param store so the provider sees runtime
    // mutations to e.g. default voice.
    let tts: Option<Arc<dyn tts::TtsProvider>> = if realtime_active {
        None
    } else {
        let store_arc = state.param_registry.store("tts");
        let store_ref = store_arc.as_ref().and_then(|s| {
            s.as_any()
                .downcast_ref::<crate::api::param_store::AtomicParamStore>()
        });
        Some(Arc::from(config::build_tts_provider(
            &state.config,
            None,
            store_ref,
        )?))
    };
    // What `/debug/providers` reports, read off the providers that serve
    // sessions rather than off a second copy built for the purpose.
    let _ = state.provider_facts.set(api::types::ProviderFacts {
        stt_capabilities: stt.as_ref().map_or_else(Vec::new, |s| {
            s.capabilities().iter().map(|c| format!("{c:?}")).collect()
        }),
        stt_sample_rate: stt.as_ref().map(|s| s.expected_sample_rate()),
        tts_sample_rate: tts.as_ref().map(|t| t.sample_rate()),
    });
    let processors: Arc<Vec<Box<dyn text::TextProcessor>>> = Arc::new(
        config::build_text_processors(&state.config, None, Some(&state.param_registry))?,
    );
    let audio_post_processors: Arc<Vec<Box<dyn crate::pipeline::AudioPostProcessor>>> =
        Arc::new(crate::pipeline::build_audio_post_processors(&state.config)?);
    let annotators: Arc<Vec<crate::pipeline::AnnotatorSpec>> =
        Arc::new(crate::pipeline::build_audio_annotators(&state.config)?);
    if !annotators.is_empty() {
        info!(
            annotators = ?annotators.iter().map(|a| a.annotator.name()).collect::<Vec<_>>(),
            "audio annotators configured (off-path, mode = \"annotate\")"
        );
    }

    // Register every provider that exposes a Component into the
    // worker-wide registry so `/debug/components` and aggregate health can
    // see them. Audio I/O gets a minimal always-healthy stub.
    if let Some(stt_arc) = stt.as_ref()
        && let Some(c) = stt_arc.as_component()
    {
        state.component_registry.register(std::sync::Arc::clone(&c));
        if let Some(dynamic) = c.dynamic_schema()
            && let Some(store) = state.param_registry.store("stt")
        {
            state.param_registry.register("stt", dynamic, store);
        }
    }
    if let Some(tts_arc) = tts.as_ref()
        && let Some(c) = tts_arc.as_component()
    {
        state.component_registry.register(std::sync::Arc::clone(&c));
        if let Some(dynamic) = c.dynamic_schema()
            && let Some(store) = state.param_registry.store("tts")
        {
            state.param_registry.register("tts", dynamic, store);
        }
    }
    let total_tps = state.config.text_processor.len();
    for (idx, p) in processors.iter().enumerate() {
        if let Some(c) = p.as_component() {
            state.component_registry.register(std::sync::Arc::clone(&c));
            if let Some(dynamic) = c.dynamic_schema() {
                let id = config::text_processor_id(
                    &state.config.text_processor[idx].processor_type,
                    idx,
                    total_tps,
                );
                if let Some(store) = state.param_registry.store(&id) {
                    state.param_registry.register(&id, dynamic, store);
                }
            }
        }
    }
    state
        .component_registry
        .register(crate::audio::build_io_component("audio:io"));

    // Spawn the periodic auto-degrade task. It walks the registry
    // every interval and flips Healthy <-> Degraded based on rolling
    // telemetry (error rate + p99 latency). Defaults are conservative; see
    // `AutoDegradeConfig`.
    let degrader = std::sync::Arc::new(crate::component::AutoDegrader::new(
        (*state.component_registry).clone(),
        crate::component::AutoDegradeConfig::default(),
    ));
    degrader.spawn();

    let (turn_tx, turn_rx) =
        tokio::sync::mpsc::channel::<crate::api::types::TurnRecord>(TELEMETRY_CHANNEL_BUF);
    let (session_tx, session_rx) =
        tokio::sync::mpsc::channel::<crate::api::types::SessionRecord>(TELEMETRY_CHANNEL_BUF);

    // Client-side tool catalogs are startup-only I/O: read and merged
    // once here, never per connection.
    let client_tools = Arc::new(crate::config::load_client_tools(&state.config)?);
    if !client_tools.is_empty() {
        info!(tools = client_tools.len(), "client-side tools declared");
        // A speculative run answers a partial transcript and may be thrown
        // away, but a tool call it makes has already gone to the client.
        if state.config.speculative.enabled {
            warn!(
                "speculative execution is on with client-side tools declared: a tool call \
                 from a speculative run that is later discarded still reaches the client"
            );
        }
    }

    let transport_state = Arc::new(crate::transport::serve::TransportState {
        config: Arc::new(state.config.clone()),
        stt,
        tts,
        processors,
        audio_post_processors,
        annotators,
        active_sessions: Arc::clone(&state.live_sessions),
        max_sessions: transport_cfg.max_sessions,
        ready: std::sync::atomic::AtomicBool::new(true),
        shutdown: shutdown.clone(),
        turn_complete_tx: Some(turn_tx),
        session_event_tx: Some(session_tx),
        session_store: Arc::new(SessionStore::new(SessionStoreConfig {
            max_sessions: transport_cfg.max_sessions,
            ..SessionStoreConfig::default()
        })),
        pipeline_event_tx: tokio::sync::broadcast::channel(256).0,
        param_registry: Arc::clone(&state.param_registry),
        component_registry: Arc::clone(&state.component_registry),
        client_tools,
    });

    // Reclaim disconnected sessions past their grace period. Without this the
    // session store grows unbounded and the worker refuses new connections once
    // `max_sessions` slots are used. The handle is detached -- the task lives
    // for the server's lifetime.
    transport_state.session_store.spawn_reaper();

    let app = crate::transport::serve::transport_router(transport_state);
    let (listener, port) = try_bind(
        &transport_cfg.bind,
        transport_cfg.port,
        "transport (WebSocket)",
    )
    .await?;
    info!(
        bind = %format!("{}:{}", transport_cfg.bind, port),
        max_sessions = transport_cfg.max_sessions,
        "transport listener started (WebSocket)"
    );

    Ok(Some(TransportSetup {
        listener,
        app,
        turn_rx,
        session_rx,
        port,
    }))
}

/// The debug-API address the worker advertises to the orchestrator.
///
/// Prefers the explicit `advertise_address` — required whenever the
/// orchestrator cannot reach the worker's *bind* address, e.g. separate
/// Docker containers, where a registered `0.0.0.0` gets rewritten to the
/// orchestrator's own loopback and every proxied live edit 502s. A bare
/// host (no `:`) defaults to the actual debug port. Falls back to
/// `{debug_bind}:{actual_port}`.
fn registered_debug_address(cfg: &api::types::WorkerConfig, actual_debug_port: u16) -> String {
    match cfg.advertise_address.as_deref().map(str::trim) {
        Some(addr) if !addr.is_empty() => {
            if addr.contains(':') {
                addr.to_string()
            } else {
                format!("{addr}:{actual_debug_port}")
            }
        }
        _ => format!("{}:{}", cfg.debug_bind, actual_debug_port),
    }
}

/// The *browser-reachable* client-transport address the worker registers
/// (what `GET /api/v1/dispatch` returns as `ws_url`), when configured.
///
/// `transport_advertise_address` verbatim; a bare host (no `:`) defaults to
/// the actual transport port. `None` when unset: dispatch then derives the
/// URL from the internal `{transport.bind}:{port}` address, which the
/// orchestrator rewrites from `0.0.0.0` to `127.0.0.1` — so two
/// containerised workers on different host ports must set this or every
/// dispatch targets the same port.
fn registered_public_transport_address(
    cfg: &api::types::WorkerConfig,
    actual_port: u16,
) -> Option<String> {
    match cfg.transport_advertise_address.as_deref().map(str::trim) {
        Some(addr) if !addr.is_empty() => Some(if addr.contains(':') {
            addr.to_string()
        } else {
            format!("{addr}:{actual_port}")
        }),
        _ => None,
    }
}

/// Build the capacity block the orchestrator uses for dispatch admission.
///
/// Takes the live counter directly rather than a whole `WorkerState` to make
/// the dependency explicit: capacity is a function of **live sessions**, never
/// of turn history. Deriving it from `local_sessions` -- which only ever holds
/// `Completed` turns -- pinned it at 0, so the orchestrator's
/// `active_sessions >= max_concurrent` check never excluded a worker and
/// dispatch kept loading saturated ones.
fn worker_capacity(
    max_concurrent: u32,
    live: &std::sync::atomic::AtomicUsize,
) -> api::types::WorkerCapacity {
    api::types::WorkerCapacity {
        max_concurrent,
        active_sessions: live.load(std::sync::atomic::Ordering::Relaxed) as u32,
    }
}

/// 3) Orchestrator client lifecycle -- registers the worker and runs the
///    heartbeat loop with re-registration on failure.  No-op when no
///    orchestrator endpoint is configured.
///
/// Returns an error if the registration payload cannot be serialised
/// (a config-shape bug); transient HTTP failures are handled inside the
/// retry loop and never bubble up.
fn spawn_orchestrator_client(
    state: Arc<WorkerState>,
    actual_debug_port: u16,
    actual_transport_address: Option<String>,
    public_transport_address: Option<String>,
    internal_auth: &api::auth::SurfaceAuth,
) -> Result<(), anyhow::Error> {
    let Some(endpoint) = state.worker_config.orchestrator.endpoint.clone() else {
        info!("worker node ready (standalone, no orchestrator endpoint)");
        return Ok(());
    };
    // Registration/heartbeat present the internal credential;
    // the orchestrator's `/internal/*` surface rejects them without it.
    let http_client = api::auth::http_client_with_bearer(internal_auth)?;
    let interval = state.worker_config.orchestrator.heartbeat_interval_s;

    // Serialise the pipeline config once, up-front, so a malformed
    // config fails the worker fast instead of panicking inside the
    // background task. Secrets are blanked: the orchestrator
    // serves this payload from several endpoints and never needs the
    // raw keys.
    let pipeline_config_json =
        crate::config::redact::to_redacted_json(&state.config).map_err(|e| {
            anyhow::anyhow!("failed to serialise pipeline config for registration: {e}")
        })?;

    // Scan the worker's CWD `examples/` directory so the orchestrator
    // can surface only configs this worker can actually load. Best-effort:
    // an empty result is fine (orchestrator falls back to its own scan).
    let available_configs =
        crate::config::discover::scan_configs(std::path::Path::new("examples"), 2);

    tokio::spawn(async move {
        let client = http_client;
        let reg_url = format!("{endpoint}/internal/workers/register");
        let hb_url = format!("{endpoint}/internal/workers/heartbeat");

        let reg_payload = api::types::WorkerRegistration {
            worker_id: state.worker_id.clone(),
            address: registered_debug_address(&state.worker_config, actual_debug_port),
            capacity: api::types::WorkerCapacity {
                max_concurrent: state.worker_config.max_concurrent,
                active_sessions: 0,
            },
            pipeline_config_hash: config::config_hash(&state.config),
            pipeline_name: state.config.pipeline.name.clone(),
            pipeline_config: Some(pipeline_config_json),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tags: state.worker_config.tags.clone(),
            transport_address: actual_transport_address,
            public_transport_address,
            available_configs,
        };

        // Initial registration (retry until success).
        loop {
            match client.post(&reg_url).json(&reg_payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!("worker node ready & registered with orchestrator");
                    break;
                }
                Ok(resp) => error!("Failed to register: HTTP {}", resp.status()),
                Err(e) => error!("Failed to connect to orchestrator: {}", e),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }

        // Heartbeat loop with re-registration-on-failure semantics.
        let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval));
        let mut registered = true;
        loop {
            ticker.tick().await;

            if !registered {
                match client.post(&reg_url).json(&reg_payload).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        info!("re-registered with orchestrator after connection loss");
                        registered = true;
                    }
                    Ok(resp) => {
                        warn!(
                            "re-registration failed: HTTP {} - retrying next tick",
                            resp.status()
                        );
                        continue;
                    }
                    Err(e) => {
                        warn!("orchestrator unreachable, will retry: {}", e);
                        continue;
                    }
                }
            }

            // Snapshot component registry so the orchestrator can surface
            // per-stage health/telemetry without scraping `/debug/components`
            // on every worker. Cheap (atomics + clones) and bounded by the
            // small number of registered components per worker.
            let (components, aggregate_health) = {
                let snap = state.component_registry.snapshot();
                let agg = state.component_registry.aggregate_health();
                let entries: Vec<api::types::ComponentSnapshot> = snap
                    .into_iter()
                    .map(|c| api::types::ComponentSnapshot {
                        id: c.id().to_string(),
                        kind: c.kind(),
                        health: c.health(),
                        telemetry: c.telemetry(),
                        param_schema: c.dynamic_schema(),
                    })
                    .collect();
                let agg_opt = if entries.is_empty() { None } else { Some(agg) };
                (entries, agg_opt)
            };
            let hb_payload = api::types::WorkerHeartbeat {
                worker_id: state.worker_id.clone(),
                capacity: worker_capacity(state.worker_config.max_concurrent, &state.live_sessions),
                components,
                aggregate_health,
            };
            match client.post(&hb_url).json(&hb_payload).send().await {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => {
                    warn!(
                        "Heartbeat rejected: HTTP {} - will re-register",
                        resp.status()
                    );
                    registered = false;
                }
                Err(e) => {
                    error!("Heartbeat failed: {} - will re-register on next tick", e);
                    registered = false;
                }
            }
        }
    });

    Ok(())
}

/// How many completed turns `/debug/sessions` keeps in memory.
///
/// Oldest are evicted first; the endpoint paginates over them in
/// chronological order.
const LOCAL_TURN_HISTORY: usize = 200;

/// Append one turn to the bounded local history.
///
/// A free function rather than an inline block so the `parking_lot` guard is
/// provably dropped before the caller's next `.await` -- holding it across one
/// would risk a deadlock and is not caught by the compiler.
fn record_turn_locally(
    store: &parking_lot::RwLock<VecDeque<crate::api::types::TurnRecord>>,
    record: crate::api::types::TurnRecord,
) {
    let mut store = store.write();
    while store.len() >= LOCAL_TURN_HISTORY {
        store.pop_front();
    }
    store.push_back(record);
}

/// 4) Event forwarders -- record completed turns in the worker's local
///    history (served by `GET /debug/sessions`) and, when an orchestrator is
///    configured, relay turn and session lifecycle events to it.
fn spawn_event_forwarders(
    state: &Arc<WorkerState>,
    turn_rx: tokio::sync::mpsc::Receiver<crate::api::types::TurnRecord>,
    session_rx: tokio::sync::mpsc::Receiver<crate::api::types::SessionRecord>,
    internal_auth: &api::auth::SurfaceAuth,
) -> Result<(), anyhow::Error> {
    // Turn records are recorded locally on EVERY worker, orchestrator or not.
    // Previously this function returned early when no orchestrator was
    // configured, so on a standalone worker nothing drained `turn_rx`:
    // `/debug/sessions` stayed permanently empty (despite
    // examples/orchestrate/worker-standalone.toml advertising it as "local turn
    // history with latency metrics") and, once the 1024-slot channel filled,
    // every completed turn logged "telemetry channel full; dropping record".
    let endpoint = state.worker_config.orchestrator.endpoint.clone();
    let worker_id = state.worker_id.clone();
    let client = endpoint
        .as_ref()
        .map(|_| api::auth::http_client_with_bearer(internal_auth))
        .transpose()?;

    spawn_turn_recorder(
        Arc::clone(&state.local_sessions),
        endpoint.clone(),
        worker_id.clone(),
        turn_rx,
        client.clone(),
    );

    match (endpoint, client) {
        (Some(endpoint), Some(client)) => {
            spawn_session_forwarder(endpoint, worker_id, session_rx, client);
        }
        // Nothing consumes session records without an orchestrator, but the
        // channel still has to be drained or it fills and warns per session.
        _ => {
            tokio::spawn(async move {
                let mut session_rx = session_rx;
                while session_rx.recv().await.is_some() {}
            });
        }
    }
    Ok(())
}

/// Drain completed turn records into the worker's local history (served by
/// `GET /debug/sessions`), forwarding each to the orchestrator when one is
/// configured.
fn spawn_turn_recorder(
    store: Arc<parking_lot::RwLock<VecDeque<crate::api::types::TurnRecord>>>,
    endpoint: Option<String>,
    worker_id: String,
    mut turn_rx: tokio::sync::mpsc::Receiver<crate::api::types::TurnRecord>,
    client: Option<reqwest::Client>,
) {
    tokio::spawn(async move {
        let url = endpoint.map(|e| format!("{e}/internal/events/turn"));
        while let Some(record) = turn_rx.recv().await {
            record_turn_locally(&store, record.clone());
            // Recording is local and always succeeds; forwarding is
            // best-effort and must never hold up the next record, so each
            // forward runs on its own task (bounded by the client's request
            // timeout) instead of in this loop.
            if let (Some(url), Some(client)) = (url.clone(), client.clone()) {
                let event = crate::api::types::SessionCompleteEvent {
                    worker_id: worker_id.clone(),
                    session: record,
                };
                tokio::spawn(async move {
                    if let Err(e) = client.post(&url).json(&event).send().await {
                        tracing::warn!(error = %e, "failed to report turn to orchestrator");
                    }
                });
            }
        }
    });
}

fn spawn_session_forwarder(
    endpoint: String,
    worker_id: String,
    mut session_rx: tokio::sync::mpsc::Receiver<crate::api::types::SessionRecord>,
    client: reqwest::Client,
) {
    tokio::spawn(async move {
        let url = format!("{endpoint}/internal/events/session");
        while let Some(mut record) = session_rx.recv().await {
            // Fill in worker_id (the runner leaves it empty).
            if record.worker_id.is_empty() {
                record.worker_id = worker_id.clone();
            }
            if let Err(e) = client.post(&url).json(&record).send().await {
                tracing::warn!(error = %e, "failed to report session event to orchestrator");
            }
        }
    });
}

/// Spawn the WebSocket transport server in a background task.  The debug
/// API server runs in the foreground in [`run_serve`].
/// Bind the worker debug API: its listener, the port it got, and its
/// router behind the internal credential.
async fn bind_debug_api(
    state: &Arc<WorkerState>,
    internal_auth: &api::auth::SurfaceAuth,
) -> Result<(tokio::net::TcpListener, u16, axum::Router), anyhow::Error> {
    let app =
        api::auth::protect_router(api::worker_debug::router(Arc::clone(state)), internal_auth);
    let (listener, port) = try_bind(
        &state.worker_config.debug_bind,
        state.worker_config.debug_port,
        "debug API",
    )
    .await?;
    let bind = format!("{}:{}", state.worker_config.debug_bind, port);
    internal_auth.warn_if_open(&bind);
    info!(bind = %bind, worker_id = %state.worker_id, "worker debug API listening");
    Ok((listener, port, app))
}

/// Serve the transport until `shutdown`. The handle ends early only when
/// the listener fails, which the caller treats as fatal: a worker that
/// heartbeats without a transport would be sent sessions it cannot take.
fn spawn_transport_server(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    shutdown: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<std::io::Result<()>> {
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
    })
}

/// How long a shutting-down worker waits for its sessions to end.
const SHUTDOWN_DRAIN: std::time::Duration = std::time::Duration::from_secs(10);

/// What the transport task's end means: nothing, during a requested
/// shutdown; otherwise the error that ends the worker.
fn transport_task_outcome(
    ended: Result<std::io::Result<()>, tokio::task::JoinError>,
    shutting_down: bool,
) -> Result<(), anyhow::Error> {
    match ended {
        Ok(Ok(())) if shutting_down => Ok(()),
        Ok(Ok(())) => Err(anyhow::anyhow!("the transport listener stopped")),
        Ok(Err(e)) => Err(anyhow::Error::new(e).context("the transport listener failed")),
        Err(e) => Err(anyhow::Error::new(e).context("the transport listener task ended")),
    }
}

/// Resolves on Ctrl-C, or on SIGTERM where there is one (what `docker stop`
/// and Kubernetes send).
pub(crate) async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!(error = %e, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(e) => {
                warn!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

/// Wait until no session is live, or `bound` passes.
async fn drain_sessions(live: &std::sync::atomic::AtomicUsize, bound: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        let left = live.load(std::sync::atomic::Ordering::Relaxed);
        if left == 0 {
            info!("every session closed");
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(sessions = left, "shutting down with sessions still open");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Tell the orchestrator this worker is leaving, so nothing is dispatched
/// to it in the time the heartbeat timeout would still count it live.
async fn deregister(state: &WorkerState, internal_auth: &api::auth::SurfaceAuth) {
    let Some(endpoint) = state.worker_config.orchestrator.endpoint.as_deref() else {
        return;
    };
    let client = match api::auth::http_client_with_bearer_and_timeout(
        internal_auth,
        std::time::Duration::from_secs(2),
    ) {
        Ok(client) => client,
        Err(e) => {
            warn!(error = %e, "cannot deregister from the orchestrator");
            return;
        }
    };
    let body = api::types::WorkerDeregistration {
        worker_id: state.worker_id.clone(),
    };
    match client
        .post(format!("{endpoint}/internal/workers/deregister"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => info!("deregistered from the orchestrator"),
        Ok(resp) => warn!(status = %resp.status(), "the orchestrator refused the deregistration"),
        Err(e) => warn!(error = %e, "cannot reach the orchestrator to deregister"),
    }
}

pub async fn run_serve(
    pipeline_config: PipelineConfig,
    worker_config_path: Option<PathBuf>,
) -> Result<(), anyhow::Error> {
    // 1) Worker bootstrap.
    let state = build_worker_state(pipeline_config, worker_config_path)?;
    register_param_stores(&state).await;

    // Bounded retention for session captures. Off the startup path
    // (a large captures/ dir may take a while to walk) and repeated
    // periodically, so a worker that stays up for weeks keeps pruning;
    // the report is logged when a sweep removed something.
    {
        let audit = &state.config.pipeline.audit;
        if audit.enabled
            && audit.retention_days > 0
            && let Some(dir) = audit.captures_dir.clone()
        {
            let days = audit.retention_days;
            tokio::spawn(async move {
                loop {
                    let sweep_dir = dir.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let report =
                            crate::audit::retention::sweep_expired_captures(&sweep_dir, days);
                        if report.removed > 0 || !report.errors.is_empty() {
                            info!(
                                dir = %sweep_dir.display(),
                                retention_days = days,
                                removed = report.removed,
                                kept = report.kept,
                                bytes_freed = report.bytes_freed,
                                errors = report.errors.len(),
                                "capture retention sweep"
                            );
                        }
                        for (name, err) in &report.errors {
                            warn!(session = %name, error = %err, "capture retention: could not remove");
                        }
                    })
                    .await;
                    tokio::time::sleep(crate::audit::retention::SWEEP_INTERVAL).await;
                }
            });
        }
    }

    // Runtime auth. The debug API is machine-to-machine (the
    // orchestrator proxies to it), so it enforces the *internal*
    // credential; the WS transport enforces the *client* credential.
    // Unset env vars leave a surface open (dev mode); `[auth]
    // required = true` makes that a startup error instead.
    let auth_cfg = state.config.auth.clone().unwrap_or_default();
    let internal_auth =
        api::auth::SurfaceAuth::resolve(&auth_cfg.internal_token_env, "worker debug API");
    let client_auth = api::auth::SurfaceAuth::resolve(&auth_cfg.client_token_env, "WS transport");
    internal_auth.enforce_required(auth_cfg.required)?;
    if state.config.transport.as_ref().is_some_and(|t| t.enabled) {
        client_auth.enforce_required(auth_cfg.required)?;
    }
    internal_auth.log_status();
    client_auth.log_status();
    // WS audio injection policy: dev = allowed, client-auth
    // enforced = denied, VP_ENABLE_AUDIO_INJECTION overrides.
    crate::transport::inject::configure_injection(client_auth.enabled());

    // Debug API listener -- bound early so the actual port is known before
    // we hand it to the orchestrator client.
    let (debug_listener, actual_debug_port, debug_app) =
        bind_debug_api(&state, &internal_auth).await?;

    // 2) Transport bootstrap (optional).
    let shutdown = tokio_util::sync::CancellationToken::new();
    let transport_setup = bootstrap_transport(&state, &shutdown).await?;
    // Internal address (bind:port) for the orchestrator itself — the
    // benchmark runner connects through it, swapping a wildcard host for
    // the worker's service name. The browser-facing address is separate.
    let actual_transport_address = transport_setup.as_ref().map(|t| {
        let bind = state
            .config
            .transport
            .as_ref()
            .map(|c| c.bind.clone())
            .unwrap_or_default();
        format!("{}:{}", bind, t.port)
    });
    let public_transport_address = transport_setup
        .as_ref()
        .and_then(|t| registered_public_transport_address(&state.worker_config, t.port));

    // 3) Orchestrator client lifecycle.
    spawn_orchestrator_client(
        Arc::clone(&state),
        actual_debug_port,
        actual_transport_address,
        public_transport_address,
        &internal_auth,
    )?;

    // 4) Event forwarders + transport server (when transport is enabled).
    let mut transport_task = None;
    if let Some(setup) = transport_setup {
        spawn_event_forwarders(&state, setup.turn_rx, setup.session_rx, &internal_auth)?;
        if let Some(t) = state.config.transport.as_ref() {
            client_auth.warn_if_open(&format!("{}:{}", t.bind, setup.port));
        }
        let app = api::auth::protect_router(setup.app, &client_auth);
        transport_task = Some(spawn_transport_server(
            setup.listener,
            app,
            shutdown.clone(),
        ));
    }

    // 5) Serve until a shutdown signal, then drain and leave the fleet.
    serve_until_shutdown(
        &state,
        debug_listener,
        debug_app,
        transport_task,
        &shutdown,
        &internal_auth,
    )
    .await
}

/// Serve until a shutdown signal: sessions are told (each ends with a
/// `Closing` frame), the listeners stop accepting, open sessions get a
/// bounded time to finish, and the orchestrator is told the worker left. A
/// transport listener that fails ends the worker with an error, so a
/// supervisor restarts it.
async fn serve_until_shutdown(
    state: &WorkerState,
    debug_listener: tokio::net::TcpListener,
    debug_app: axum::Router,
    mut transport_task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    shutdown: &tokio_util::sync::CancellationToken,
    internal_auth: &api::auth::SurfaceAuth,
) -> Result<(), anyhow::Error> {
    let on_signal = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        info!("shutdown requested; closing sessions");
        on_signal.cancel();
    });
    let debug_server = axum::serve(debug_listener, debug_app)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned());
    let transport_ended = async {
        match transport_task.as_mut() {
            Some(task) => task.await,
            None => std::future::pending().await,
        }
    };
    let transport_ended = tokio::select! {
        served = debug_server.into_future() => {
            served?;
            None
        }
        ended = transport_ended => Some(ended),
    };
    if let Some(ended) = transport_ended {
        // Both listeners stop on the token, in either order: the transport
        // ending during a requested shutdown is the orderly case. Ending
        // at any other time is the failure that must not leave a worker
        // heartbeating without a transport.
        transport_task = None;
        transport_task_outcome(ended, shutdown.is_cancelled())?;
        shutdown.cancel();
    }
    drain_sessions(&state.live_sessions, SHUTDOWN_DRAIN).await;
    if let Some(task) = transport_task
        && tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .is_err()
    {
        warn!("the transport listener did not stop in time");
    }
    deregister(state, internal_auth).await;
    info!("worker stopped");
    Ok(())
}

/// Run the recogniser once every `interval` while no session is open, so
/// an idle worker's model stays resident. A worker left alone on a host
/// that is short of memory has its model pages evicted, and the first
/// utterance after the quiet spell then waits for them to come back longer
/// than the pipeline waits for a final; a decode of silence every few
/// minutes keeps the pages in use. Ends with the worker's shutdown.
pub(crate) async fn keep_warm(
    stt: Arc<dyn stt::SttProvider>,
    active_sessions: Arc<std::sync::atomic::AtomicUsize>,
    interval: std::time::Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if interval.is_zero() {
        return;
    }
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(interval) => {}
        }
        if active_sessions.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            continue;
        }
        let t0 = std::time::Instant::now();
        match stt.warm_up().await {
            Ok(()) => tracing::debug!(
                provider = stt.name(),
                elapsed_ms = t0.elapsed().as_millis() as u64,
                "idle recogniser kept warm"
            ),
            Err(e) => tracing::warn!(provider = stt.name(), error = %e, "idle keep-warm failed"),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/serve.rs"]
mod tests;
