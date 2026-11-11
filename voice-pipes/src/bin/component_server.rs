//! Component server -- hosts STT, TTS, and/or text-processing as gRPC services.
//!
//! Reads a standard `PipelineConfig` TOML file, builds whichever providers are
//! configured, wraps them in gRPC server wrappers, and listens on a configurable
//! address. This enables split-mode and hybrid topologies where each component
//! runs in its own process or container.
//!
//! # Usage
//!
//! ```text
//! voice-pipes component-server --config worker-stt.toml --listen 0.0.0.0:50051
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tonic::transport::Server;
use tracing::info;

use voice_pipes::config::{self, PipelineConfig};

/// Voice Pipes component server -- serve pipeline stages over gRPC.
#[derive(Parser)]
#[command(name = "voice-pipes-component", version)]
struct Cli {
    /// Path to the pipeline TOML config.
    #[arg(long, value_name = "PATH")]
    config: PathBuf,

    /// Listen address for the gRPC server (default: 0.0.0.0:50051).
    #[arg(long, default_value = "0.0.0.0:50051")]
    listen: SocketAddr,

    /// Log format: compact (default) or json.
    #[arg(long, default_value = "compact")]
    log_format: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialise tracing.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Load config.
    let toml_str = std::fs::read_to_string(&cli.config)
        .map_err(|e| anyhow::anyhow!("failed to read config {}: {e}", cli.config.display()))?;
    let cfg: PipelineConfig = toml::from_str(&toml_str)?;

    info!(listen = %cli.listen, "starting component server");

    // Runtime auth: gRPC components enforce the internal
    // credential when its env var is set; open otherwise (dev mode).
    let auth_cfg = cfg.auth.clone().unwrap_or_default();
    let internal_auth = voice_pipes::api::auth::SurfaceAuth::resolve(
        &auth_cfg.internal_token_env,
        "gRPC component server",
    );
    internal_auth.enforce_required(auth_cfg.required)?;
    internal_auth.log_status();
    internal_auth.warn_if_open(&cli.listen.to_string());
    let bearer_check = voice_pipes::grpc::auth::ServerBearerCheck::new(internal_auth);

    // Single ParamRegistry shared across all hosted components and the
    // ParamService that exposes it over gRPC. Components register their
    // local AtomicParamStore here so an `UpdateParams` RPC mutates the same
    // atomic cells that the hot-path reads from.
    let param_registry = Arc::new(voice_pipes::api::param_store::ParamRegistry::new());

    let mut services_added = 0u32;

    // Collect services into a Routes builder so we can add them dynamically.
    let mut routes = tonic::service::Routes::builder();

    // ── STT ─────────────────────────────────────────────────
    if let Some(stt_cfg) = cfg.stt.as_ref() {
        use voice_pipes::api::param_schema::ComponentParamSchema;
        info!("building STT provider for component server");
        let provider = config::build_stt_provider(&cfg, None)?;
        let provider = Arc::from(provider);

        let svc = voice_pipes::grpc::stt_server::SttServiceImpl::new(
            provider,
            stt_cfg.param_schema(),
            stt_cfg.current_values(),
        );
        let grpc_svc = voice_pipes::grpc::stt_server::SttServiceServer::new(svc);
        routes.add_service(tonic::service::interceptor::InterceptedService::new(
            grpc_svc,
            bearer_check.clone(),
        ));
        services_added += 1;
        info!("STT service registered");
    }

    // ── TTS ─────────────────────────────────────────────────
    if let Some(tts_cfg) = cfg.tts.as_ref() {
        use voice_pipes::api::param_schema::ComponentParamSchema;
        info!("building TTS provider for component server");
        // Build the param store first so the factory can adopt its cells.
        let tts_store = config::build_tts_param_store(&cfg);
        // Explicit `T` so the clone stays concrete and the unsized coercion to
        // the registry's `Arc<dyn MutableParamStore>` happens at the call site.
        param_registry.register(
            "tts",
            tts_cfg.param_schema(),
            Arc::<voice_pipes::api::param_store::AtomicParamStore>::clone(&tts_store),
        );

        let provider = config::build_tts_provider(&cfg, None, Some(tts_store.as_ref()))?;
        let provider = Arc::from(provider);

        let svc = voice_pipes::grpc::tts_server::TtsServiceImpl::new(
            provider,
            tts_cfg.param_schema(),
            tts_cfg.current_values(),
        );
        let grpc_svc = voice_pipes::grpc::tts_server::TtsServiceServer::new(svc);
        routes.add_service(tonic::service::interceptor::InterceptedService::new(
            grpc_svc,
            bearer_check.clone(),
        ));
        services_added += 1;
        info!("TTS service registered (with ParamService cells)");
    }

    // ── Text processors ─────────────────────────────────────
    if !cfg.text_processor.is_empty()
        && register_text_service(&cfg, &param_registry, &bearer_check, &mut routes)?
    {
        services_added += 1;
    }

    if services_added == 0 {
        anyhow::bail!(
            "no STT, TTS, or text_processor sections found in config -- nothing to serve"
        );
    }

    // Always expose the ParamService so the orchestrator can forward
    // `PATCH /debug/params` updates. Cheap when no mutable params are
    // registered (the registry is just empty).
    let param_svc = voice_pipes::grpc::params::ParamServiceImpl::new(Arc::clone(&param_registry));
    let param_grpc = voice_pipes::grpc::params::ParamServiceServer::new(param_svc);
    routes.add_service(tonic::service::interceptor::InterceptedService::new(
        param_grpc,
        bearer_check.clone(),
    ));
    info!("ParamService registered");

    info!(services = services_added, listen = %cli.listen, "component server ready");
    Server::builder()
        .add_routes(routes.routes())
        .serve(cli.listen)
        .await?;

    Ok(())
}

fn register_text_service(
    cfg: &PipelineConfig,
    param_registry: &Arc<voice_pipes::api::param_store::ParamRegistry>,
    bearer_check: &voice_pipes::grpc::auth::ServerBearerCheck,
    routes: &mut tonic::service::RoutesBuilder,
) -> Result<bool> {
    use voice_pipes::api::param_schema::ComponentParamSchema;
    info!("building text processors for component server");
    let total = cfg.text_processor.len();
    for (idx, tp_cfg) in cfg.text_processor.iter().enumerate() {
        let id = config::text_processor_id(&tp_cfg.processor_type, idx, total);
        let store = config::build_text_processor_param_store(tp_cfg);
        param_registry.register(&id, tp_cfg.param_schema(), store);
    }

    let processors = config::build_text_processors(cfg, None, Some(param_registry))?;
    let Some(proc) = processors.into_iter().next() else {
        return Ok(false);
    };
    let proc = Arc::from(proc);
    let tp_cfg = cfg
        .text_processor
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing text_processor config"))?;
    let cfg_for_sessions = cfg.clone();
    let reg_for_sessions = Arc::clone(param_registry);
    let builder: voice_pipes::grpc::text_server::SessionProcessorBuilder = Arc::new(move || {
        let mut built =
            config::build_text_processors(&cfg_for_sessions, None, Some(&reg_for_sessions))
                .map_err(|e| e.to_string())?;
        built
            .drain(..)
            .next()
            .map(Arc::from)
            .ok_or_else(|| "no text processor built for session".to_string())
    });

    let svc = voice_pipes::grpc::text_server::TextProcessorServiceImpl::new(
        proc,
        tp_cfg.param_schema(),
        tp_cfg.current_values(),
    )
    .with_session_builder(builder);
    let grpc_svc = voice_pipes::grpc::text_server::TextProcessorServiceServer::new(svc);
    routes.add_service(tonic::service::interceptor::InterceptedService::new(
        grpc_svc,
        bearer_check.clone(),
    ));
    info!("TextProcessor service registered (with ParamService cells and per-session state)");
    Ok(true)
}
