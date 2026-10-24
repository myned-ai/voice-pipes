//! Provider factory builders -- construct STT, TTS, VAD, turn-detection,
//! and text-processor instances from pipeline configuration.

use tracing::info;

use crate::stt::SttProvider;
use crate::stt::turn::{SilenceTimeoutDetector, TurnDetector};
use crate::text::TextProcessor;
use crate::tts::TtsProvider;
use crate::vad::segmenter::VadBackend;

use super::types::{PipelineConfig, SttConfig, TtsConfig, VadSettings};

/// Build a [`RealtimeProtocol`](crate::realtime::RealtimeProtocol) instance
/// from the pipeline configuration. Returns `Ok(None)` when no
/// `[realtime_provider]` section is present.
///
/// Available only when compiled with the `realtime` feature.
#[cfg(feature = "realtime")]
pub fn build_realtime_protocol(
    cfg: &PipelineConfig,
    param_store: Option<&crate::api::param_store::AtomicParamStore>,
) -> Result<Option<Box<dyn crate::realtime::RealtimeProtocol>>, anyhow::Error> {
    let Some(mut rt_cfg) = cfg.realtime_provider.clone() else {
        return Ok(None);
    };
    let factory = crate::realtime::lookup_realtime_factory(&rt_cfg.provider).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown or disabled realtime provider: \"{}\"",
            rt_cfg.provider
        )
    })?;

    if let Some(store) = param_store {
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

    let provider_cfg = crate::realtime::realtime_config(&rt_cfg, factory.default_api_key_env());
    let errors = factory.validate(&provider_cfg);
    if !errors.is_empty() {
        let messages: Vec<String> = errors
            .iter()
            .map(|e| format!("{}: {}", e.field, e.message))
            .collect();
        return Err(anyhow::anyhow!(
            "realtime_provider config errors: {}",
            messages.join("; ")
        ));
    }
    // Built before the session's audit sink exists; the protocol is a pure
    // wire mapper and records nothing itself.
    let ctx = voice_pipes_sdk::BuildContext::new().with_params(param_store);
    let proto = factory.build(&provider_cfg, &ctx)?;
    info!(
        provider = %rt_cfg.provider,
        model = ?rt_cfg.model,
        "built realtime S2S protocol"
    );
    Ok(Some(proto))
}

/// The recogniser a pipeline without `[stt]` falls back to: the one the
/// binary registered as the stage's fallback
/// ([`plugins::register_fallback`](crate::plugins::register_fallback)).
/// `None` in a build with none, so a missing section is reported as
/// missing, not as a provider the user never named.
pub fn default_stt_provider() -> Option<String> {
    crate::plugins::fallback_provider(crate::plugins::Stage::Stt)
}

/// The voice a pipeline without `[tts]` falls back to: the one the binary
/// registered as the stage's fallback. `None` in a build with none.
pub fn default_tts_provider() -> Option<String> {
    crate::plugins::fallback_provider(crate::plugins::Stage::Tts)
}

/// The STT provider `cfg` names, or this build's default when it names none.
pub fn stt_provider_name(cfg: &PipelineConfig) -> Option<String> {
    cfg.stt
        .as_ref()
        .map(|s| s.provider.clone())
        .or_else(default_stt_provider)
}

/// The TTS provider `cfg` names, or this build's default when it names none.
pub fn tts_provider_name(cfg: &PipelineConfig) -> Option<String> {
    cfg.tts
        .as_ref()
        .map(|t| t.provider.clone())
        .or_else(default_tts_provider)
}

/// Build an [`SttProvider`] from the pipeline configuration.
pub fn build_stt_provider(
    cfg: &PipelineConfig,
    audit_tx: Option<tokio::sync::mpsc::Sender<crate::audit::TraceEvent>>,
) -> Result<Box<dyn SttProvider>, anyhow::Error> {
    let stt_cfg = cfg.stt.as_ref();
    let provider_name = stt_provider_name(cfg).ok_or_else(|| {
        anyhow::anyhow!(
            "the pipeline has no [stt] section, and this build has no default speech recogniser"
        )
    })?;

    let factories = crate::stt::stt_factories();
    let factory = factories
        .iter()
        .find(|f| f.matches(&provider_name))
        .ok_or_else(|| anyhow::anyhow!("unknown or disabled STT provider: \"{provider_name}\""))?;

    // Build a temporary SttConfig with defaults if none was provided.
    let default_cfg = SttConfig::new(provider_name.clone());
    let effective = super::credentials::stt(
        stt_cfg.unwrap_or(&default_cfg),
        factory.as_ref(),
        &super::credentials::process_env,
    );

    Ok(factory.build(
        &effective,
        &voice_pipes_sdk::BuildContext::new().with_audit(audit_tx),
    )?)
}

/// Build a [`TtsProvider`] from the pipeline configuration.
///
/// When `param_store` is provided, the provider may adopt cells from it
/// for runtime-mutable parameters (e.g. default voice).
pub fn build_tts_provider(
    cfg: &PipelineConfig,
    audit_tx: Option<tokio::sync::mpsc::Sender<crate::audit::TraceEvent>>,
    param_store: Option<&crate::api::param_store::AtomicParamStore>,
) -> Result<Box<dyn TtsProvider>, anyhow::Error> {
    let tts_cfg = cfg.tts.as_ref();
    let provider_name = tts_provider_name(cfg).ok_or_else(|| {
        anyhow::anyhow!("the pipeline has no [tts] section, and this build has no default voice")
    })?;

    let factories = crate::tts::tts_factories();
    let factory = factories
        .iter()
        .find(|f| f.matches(&provider_name))
        .ok_or_else(|| anyhow::anyhow!("unknown or disabled TTS provider: \"{provider_name}\""))?;

    let default_cfg = TtsConfig::new(provider_name.clone());
    let effective = super::credentials::tts(
        tts_cfg.unwrap_or(&default_cfg),
        factory.as_ref(),
        &super::credentials::process_env,
    );

    Ok(factory.build(
        &effective,
        &voice_pipes_sdk::BuildContext::new()
            .with_audit(audit_tx)
            .with_params(param_store)
            .with_text_chunking(cfg.pipeline.text_chunking.clone()),
    )?)
}

/// Build the canonical [`AtomicParamStore`] for the TTS component, populated
/// from the provider's mutable schema. Currently exposes the default `voice`
/// for providers that support runtime voice swapping.
pub fn build_tts_param_store(
    cfg: &PipelineConfig,
) -> std::sync::Arc<crate::api::param_store::AtomicParamStore> {
    let voice = cfg
        .tts
        .as_ref()
        .and_then(|t| t.voice.clone())
        .unwrap_or_default();
    std::sync::Arc::new(
        crate::api::param_store::AtomicParamStore::builder()
            .string("voice", voice)
            .build(),
    )
}

/// Build a [`VadBackend`] from the pipeline configuration.
///
/// When `param_store` is provided, the VAD adopts cells from it for
/// runtime-mutable parameters (energy threshold, speech probability, etc.)
/// so PATCH /debug/params updates take effect on the next chunk.
pub fn build_vad(
    cfg: &PipelineConfig,
    param_store: Option<&crate::api::param_store::AtomicParamStore>,
) -> Result<Box<dyn VadBackend>, anyhow::Error> {
    let vad_cfg = cfg.vad.as_ref();
    // When no [vad] section is present, default to passthrough so that
    // providers with built-in VAD (e.g. AssemblyAI) get all audio.
    let mode = vad_cfg.map_or("passthrough", |v| v.mode.as_str());

    let factories = crate::vad::segmenter::vad_factories();

    let auto = vad_cfg.map(auto_vad_mode);
    let resolved_mode = if mode == "auto" {
        auto.as_deref().unwrap_or("energy")
    } else {
        mode
    };

    let factory = factories
        .iter()
        .find(|f| f.matches(resolved_mode))
        .ok_or_else(|| anyhow::anyhow!("unknown vad.mode: \"{resolved_mode}\""))?;

    let default_settings = VadSettings {
        mode: resolved_mode.to_string(),
        model: None,
        energy_threshold: None,
        speech_prob: None,
        threshold: None,
        neg_threshold: None,
        min_speech_ms: None,
        min_silence_ms: None,
        pre_roll_ms: None,
        options: std::collections::HashMap::default(),
    };
    let effective = vad_cfg.unwrap_or(&default_settings);

    info!(mode = resolved_mode, "building VAD via factory");
    // The backend sees the mode that chose it, not "auto".
    let mut backend_cfg = effective.backend_config();
    backend_cfg.mode = resolved_mode.to_string();
    Ok(factory.build(
        &backend_cfg,
        &voice_pipes_sdk::BuildContext::new().with_params(param_store),
    )?)
}

/// What `[vad] mode = "auto"` runs with `settings`: the binary's auto backend
/// ([`crate::vad::segmenter::auto_backend`]) when it can run with them,
/// else energy.
pub(crate) fn auto_vad_mode(settings: &VadSettings) -> String {
    let candidate = crate::vad::segmenter::auto_backend()
        .and_then(|name| crate::vad::segmenter::lookup_vad_factory(&name));
    if let Some(factory) = candidate {
        let mut cfg = settings.backend_config();
        cfg.mode = factory.name().to_string();
        if factory.can_run(&cfg) {
            return factory.name().to_string();
        }
    }
    "energy".to_string()
}

/// Build the canonical [`AtomicParamStore`] for the VAD component, populated
/// from the resolved [`VadSettings`] (or defaults when absent). All keys
/// declared as mutable in [`VadSettings::param_schema`] are present so the
/// PATCH endpoint can update them.
pub fn build_vad_param_store(
    cfg: &PipelineConfig,
) -> std::sync::Arc<crate::api::param_store::AtomicParamStore> {
    let default_settings = VadSettings {
        mode: cfg
            .vad
            .as_ref()
            .map_or_else(|| "passthrough".to_string(), |v| v.mode.clone()),
        model: None,
        energy_threshold: None,
        speech_prob: None,
        threshold: None,
        neg_threshold: None,
        min_speech_ms: None,
        min_silence_ms: None,
        pre_roll_ms: None,
        options: std::collections::HashMap::default(),
    };
    let v = cfg.vad.as_ref().unwrap_or(&default_settings);
    std::sync::Arc::new(
        crate::api::param_store::AtomicParamStore::builder()
            .float("energy_threshold", v.effective_energy_threshold())
            .float("speech_prob", v.effective_speech_prob() as f64)
            .float("neg_threshold", f64::from(v.effective_neg_threshold()))
            // min_speech_ms / min_silence_ms are not on the live param surface:
            // the VAD does not read them, so editing them would do nothing.
            // They remain accepted TOML fields for backward compatibility.
            .build(),
    )
}

/// Build the canonical [`AtomicParamStore`] for the barge-in component.
/// Keys mirror [`BargeInConfig::param_schema`].
pub fn build_barge_in_param_store(
    cfg: &PipelineConfig,
) -> std::sync::Arc<crate::api::param_store::AtomicParamStore> {
    let bi = cfg.barge_in.as_ref();
    let speech_threshold = bi.map_or(200.0, |b| b.speech_threshold);
    let min_chunks = bi.map_or(3, |b| b.min_chunks) as i64;
    std::sync::Arc::new(
        crate::api::param_store::AtomicParamStore::builder()
            .float("speech_threshold", speech_threshold)
            .int("min_chunks", min_chunks)
            .bool("barge_in_during_processing", false)
            .build(),
    )
}

/// Build a [`TurnDetector`] from the pipeline config.
///
/// Defaults to `SilenceTimeoutDetector(1000ms)` when `[turn_detection]`
/// is absent. A mode that is not compiled in, or a factory that fails to
/// build, is an error: turn semantics are what the operator configured,
/// and silently substituting a different detector would change how every
/// conversation ends without anything in the log saying so.
///
/// Shared rather than owned: the detector lives in the STT task, but a
/// detector that reads both sides of the conversation also needs the audio
/// the agent is playing, which only the runner sees.
pub fn build_turn_detector(
    cfg: &PipelineConfig,
    audit_tx: Option<tokio::sync::mpsc::Sender<crate::audit::TraceEvent>>,
) -> Result<std::sync::Arc<dyn TurnDetector>, anyhow::Error> {
    let Some(td) = cfg.turn_detection.as_ref() else {
        let timeout_ms = cfg.silence_timeout_ms();
        info!(
            timeout_ms,
            "using silence-timeout turn detector (no [turn_detection])"
        );
        return Ok(std::sync::Arc::new(SilenceTimeoutDetector::new(timeout_ms)));
    };

    let factories = crate::stt::turn::turn_detector_factories();
    let factory = factories
        .iter()
        .find(|f| f.matches(&td.mode))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown or disabled turn_detection.mode \"{}\"; expected one of {:?}",
                td.mode,
                factories.iter().map(|f| f.name()).collect::<Vec<_>>()
            )
        })?;
    factory
        .build(
            &td.turn_config(),
            &voice_pipes_sdk::BuildContext::new().with_audit(audit_tx),
        )
        .map(std::sync::Arc::from)
        .map_err(|e| anyhow::anyhow!("turn_detection.mode \"{}\": {e}", td.mode))
}

/// Build the chain of [`TextProcessor`]s from the pipeline configuration.
///
/// When `param_registry` is provided, each processor is matched against an
/// already-registered store under the same id used by [`pipeline_graph`]
/// (`<type>` for a single processor, `<type>_<idx>` for multiples). The
/// matched [`AtomicParamStore`](crate::api::param_store::AtomicParamStore)
/// is forwarded to the factory so it can adopt cells for runtime-mutable
/// parameters.
pub fn build_text_processors(
    cfg: &PipelineConfig,
    audit_tx: Option<tokio::sync::mpsc::Sender<crate::audit::TraceEvent>>,
    param_registry: Option<&std::sync::Arc<crate::api::param_store::ParamRegistry>>,
) -> Result<Vec<Box<dyn TextProcessor>>, anyhow::Error> {
    let factories = crate::text::text_processor_factories();
    let mut processors: Vec<Box<dyn TextProcessor>> = Vec::new();
    let total = cfg.text_processor.len();

    for (idx, proc_cfg) in cfg.text_processor.iter().enumerate() {
        let factory = factories
            .iter()
            .find(|f| f.matches(&proc_cfg.processor_type))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown or disabled text processor type: \"{}\"",
                    proc_cfg.processor_type,
                )
            })?;

        // Resolve the per-processor param store via the registry, if any.
        let id = text_processor_id(&proc_cfg.processor_type, idx, total);
        let store_arc = param_registry.and_then(|r| r.store(&id));
        let store_ref = store_arc.as_ref().and_then(|s| {
            s.as_any()
                .downcast_ref::<crate::api::param_store::AtomicParamStore>()
        });

        // Pipeline-level audit settings a processor needs travel in its
        // option map, so a factory sees one config shape.
        let mut proc_cfg = super::credentials::text_processor(
            proc_cfg,
            factory.as_ref(),
            &super::credentials::process_env,
        );
        if cfg.pipeline.audit.full_prompts {
            proc_cfg.options.insert(
                super::AUDIT_FULL_PROMPTS_OPTION.to_string(),
                serde_json::Value::Bool(true),
            );
        }

        let processor = factory.build(
            &proc_cfg,
            &voice_pipes_sdk::BuildContext::new()
                .with_audit(audit_tx.clone())
                .with_params(store_ref)
                .with_text_chunking(cfg.pipeline.text_chunking.clone()),
        )?;
        processors.push(processor);
    }

    Ok(processors)
}

/// Stable id for the Nth text-processor of a given type. Mirrors the id
/// scheme used by [`crate::api::orchestrator::pipeline_graph`].
pub fn text_processor_id(processor_type: &str, idx: usize, total: usize) -> String {
    if total <= 1 {
        processor_type.to_string()
    } else {
        format!("{processor_type}_{idx}")
    }
}

/// Build the [`AtomicParamStore`] for a text-processor entry from the
/// processor's own schema: one cell per live field, with its current value.
/// The engine names no processor here; a plugin's live fields work the same.
pub fn build_text_processor_param_store(
    proc_cfg: &crate::config::TextProcessorConfig,
) -> std::sync::Arc<crate::api::param_store::AtomicParamStore> {
    let schema = match crate::text::lookup_text_processor_factory(&proc_cfg.processor_type) {
        Some(f) => f.provider_schema(proc_cfg),
        None => return std::sync::Arc::default(),
    };
    std::sync::Arc::new(crate::api::param_store::AtomicParamStore::from_schema(
        &schema,
    ))
}

/// Build the canonical [`AtomicParamStore`] for the realtime provider.
#[cfg(feature = "realtime")]
pub fn build_realtime_param_store(
    cfg: &PipelineConfig,
) -> std::sync::Arc<crate::api::param_store::AtomicParamStore> {
    use crate::api::param_store::AtomicParamStore;
    let mut builder = AtomicParamStore::builder();
    if let Some(rt_cfg) = &cfg.realtime_provider {
        if let Some(m) = &rt_cfg.model {
            builder = builder.string("model", m);
        }
        if let Some(v) = &rt_cfg.voice {
            builder = builder.string("voice", v);
        }
        if let Some(vad) = &rt_cfg.vad {
            builder = builder.string("vad", vad);
        }
        if let Some(inst) = &rt_cfg.instructions {
            builder = builder.string("instructions", inst);
        }
    }
    std::sync::Arc::new(builder.build())
}
