//! Configuration validation.

use super::credentials;
use super::types::{ConfigError, ConfigErrorKind, PipelineConfig, RealtimeProviderConfig};

/// Longest system prompt, persona instruction or idle message accepted
/// from the config. These strings go into every request or session; a
/// longer body of material belongs in `knowledge_base_path`.
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;
/// Caps on repeated tables that are instantiated per session.
pub const MAX_TEXT_PROCESSORS: usize = 16;
pub const MAX_POST_PROCESSORS: usize = 16;
pub const MAX_AUDIO_FILTERS: usize = 16;
pub const MAX_STATIC_CLIENT_TOOLS: usize = 64;

/// Validate a merged config. Returns a list of errors (empty = valid).
pub fn validate(cfg: &PipelineConfig) -> Vec<ConfigError> {
    let mut errors = Vec::new();

    check_audit_tap(cfg, &mut errors);
    check_stt(cfg, &mut errors);
    check_tts(cfg, &mut errors);
    check_output(cfg, &mut errors);
    check_vad(cfg, &mut errors);
    check_turn_detection(cfg, &mut errors);
    check_grading(cfg, &mut errors);
    check_barge_in(cfg, &mut errors);
    check_realtime_session(cfg, &mut errors);
    check_realtime_provider(cfg, &mut errors);
    check_runtime_ranges(cfg, &mut errors);
    check_adaptive_quality(cfg, &mut errors);
    check_audio_post_processors(cfg, &mut errors);
    check_realtime_extras(cfg, &mut errors);
    check_vad_timing(cfg, &mut errors);
    check_shape_caps(cfg, &mut errors);
    check_text_processors(cfg, &mut errors);
    check_audio_filters(cfg, &mut errors);
    check_client_tools(cfg, &mut errors);

    errors
}

/// `pipeline.audit_tap`.
fn check_audit_tap(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate audit_tap.side
    if let Some(ref side) = cfg.pipeline.audit_tap.side
        && !matches!(side.as_str(), "before" | "after" | "both")
    {
        errors.push(ConfigError::new(
            "pipeline.audit_tap.side",
            format!("must be \"before\", \"after\", or \"both\", got \"{side}\""),
        ));
    }
}

/// `[stt]`, through its factory.
fn check_stt(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate STT provider via its factory: an unknown provider name
    // is a hard error, and the factory's own validation
    // (api_key/env-var, model paths, etc.) is surfaced instead of discarded.
    // Mirrors the realtime_provider path below.
    if let Some(ref stt) = cfg.stt {
        let stt_factories = crate::stt::stt_factories();
        match stt_factories.iter().find(|f| f.matches(&stt.provider)) {
            None => errors
                .push(crate::plugins::Stage::Stt.unknown_provider("stt.provider", &stt.provider)),
            Some(factory) => errors.extend(factory.validate(&credentials::stt(
                stt,
                factory.as_ref(),
                &credentials::process_env,
            ))),
        }
    }
}

/// `[tts]`, through its factory.
fn check_tts(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate TTS provider via its factory; see STT above.
    if let Some(ref tts) = cfg.tts {
        let tts_factories = crate::tts::tts_factories();
        match tts_factories.iter().find(|f| f.matches(&tts.provider)) {
            None => errors
                .push(crate::plugins::Stage::Tts.unknown_provider("tts.provider", &tts.provider)),
            Some(factory) => errors.extend(factory.validate(&credentials::tts(
                tts,
                factory.as_ref(),
                &credentials::process_env,
            ))),
        }

        for key in tts.lexicon.keys() {
            if key.trim().is_empty() {
                errors.push(ConfigError::new(
                    "tts.lexicon",
                    "tts.lexicon key cannot be empty or whitespace-only",
                ));
            }
        }

        for tag in &tts.passthrough_tags {
            if tag.trim().is_empty() {
                errors.push(ConfigError::new(
                    "tts.passthrough_tags",
                    "tts.passthrough_tags entry cannot be empty or whitespace-only",
                ));
            }
        }
    }
}

/// `[output]`: the rate and the chunking.
fn check_output(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate output sample rate
    if let Some(ref out) = cfg.output {
        if let Some(rate) = out.sample_rate
            && (rate == 0 || rate > 192_000)
        {
            errors.push(ConfigError::new(
                "output.sample_rate",
                format!("must be > 0 and <= 192000, got {rate}"),
            ));
        }
        if let Some(ref chunking) = out.chunking
            && chunking.enabled
        {
            if chunking.frame_ms == 0 {
                errors.push(ConfigError::new(
                    "output.chunking.frame_ms",
                    "must be > 0 when output.chunking.enabled = true",
                ));
            }
            if !(10..=250).contains(&chunking.frame_ms) {
                errors.push(ConfigError::new(
                    "output.chunking.frame_ms",
                    format!(
                        "recommended range is 10..=250 ms, got {}",
                        chunking.frame_ms
                    ),
                ));
            }
            if let Some(rate) = out.sample_rate {
                let frame_samples = (rate as u64 * chunking.frame_ms as u64) / 1000;
                if frame_samples == 0 {
                    errors.push(ConfigError::new(
                        "output.chunking.frame_ms",
                        "computed chunk size is < 1 sample at the configured output rate",
                    ));
                }
            }
        }
    }
}

/// `[vad]`: the mode, the backend's own settings, the thresholds.
fn check_vad(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate VAD settings
    if let Some(ref vad) = cfg.vad {
        let vad_factories = crate::vad::segmenter::vad_factories();
        let known_modes: Vec<&str> = vad_factories.iter().map(|f| f.name()).collect();
        if vad.mode != "auto" && !vad_factories.iter().any(|f| f.matches(&vad.mode)) {
            errors.push(
                ConfigError::new(
                    "vad.mode",
                    format!(
                        "must be \"auto\" or one of [{}], got \"{}\"",
                        known_modes
                            .iter()
                            .map(|m| format!("\"{m}\""))
                            .collect::<Vec<_>>()
                            .join(", "),
                        vad.mode,
                    ),
                )
                .with_kind(ConfigErrorKind::UnknownName),
            );
        }
        // The selected backend checks its own settings (a model path for
        // Silero, a plugin's options), as every other stage's factory does.
        // `auto` picks a backend at build time from what is on disk, so it
        // has nothing to check here.
        if let Some(factory) = vad_factories.iter().find(|f| f.matches(&vad.mode)) {
            errors.extend(factory.validate(&vad.backend_config()));
        }
        // Validate energy_threshold
        if let Some(t) = vad.energy_threshold
            && !(0.0..=10000.0).contains(&t)
        {
            errors.push(ConfigError::new(
                "vad.energy_threshold",
                format!("must be in [0.0, 10000.0] (RMS amplitude), got {t}"),
            ));
        }
        // Validate speech_prob
        if let Some(t) = vad.speech_prob
            && !(0.0..=1.0).contains(&t)
        {
            errors.push(ConfigError::new(
                "vad.speech_prob",
                format!("must be in [0.0, 1.0] (speech probability), got {t}"),
            ));
        }
        // Validate legacy threshold (mode-aware)
        if let Some(t) = vad.threshold {
            let (max, label) = if vad.mode == "energy" {
                (10000.0_f32, "[0.0, 10000.0] (RMS energy)")
            } else {
                (1.0_f32, "[0.0, 1.0] (speech probability)")
            };
            if !(0.0..=max).contains(&t) {
                errors.push(ConfigError::new(
                    "vad.threshold",
                    format!("must be in {label}, got {t}"),
                ));
            }
        }
    }
}

/// `[turn_detection]`, through the detector's factory.
fn check_turn_detection(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate turn detection settings
    if let Some(ref td) = cfg.turn_detection {
        let td_factories = crate::stt::turn::turn_detector_factories();
        match td_factories.iter().find(|f| f.matches(&td.mode)) {
            // Each detector knows what its own mode requires -- a model file
            // that exists, a threshold in range. Without this the factories'
            // `validate` never ran, so a model-based mode with no model path
            // passed here and failed at session start instead.
            Some(factory) => errors.extend(factory.validate(&td.turn_config())),
            None => errors.push(
                crate::plugins::Stage::Turn.unknown_provider("turn_detection.mode", &td.mode),
            ),
        }
        if td.silence_timeout_ms == 0 {
            errors.push(ConfigError::new(
                "turn_detection.silence_timeout_ms",
                "must be > 0",
            ));
        }
    }
}

/// `[grading]`.
fn check_grading(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate grading rules. Strict on purpose: a misspelt target would
    // otherwise never apply, and the operator would go on being graded by the
    // default they meant to replace with nothing to say so.
    if let Some(ref g) = cfg.grading {
        for (key, value) in &g.targets {
            if !super::types::GRADING_TARGET_KEYS.contains(&key.as_str()) {
                errors.push(
                    ConfigError::new(
                        format!("grading.targets.{key}"),
                        format!(
                            "unknown target; expected one of: {}",
                            super::types::GRADING_TARGET_KEYS.join(", ")
                        ),
                    )
                    .with_kind(ConfigErrorKind::UnknownKey),
                );
            } else if !value.is_finite() || *value <= 0.0 {
                errors.push(ConfigError::new(
                    format!("grading.targets.{key}"),
                    format!("must be a positive number, got {value}"),
                ));
            }
        }
        // An endpointing target at or under the silence wait can never be met:
        // no turn commits sooner than the timeout, so every turn would breach
        // a target the operator wrote down themselves. Only checked when they
        // set it -- the built-in default is not theirs to be told off for.
        if let Some(&target) = g.targets.get("endpointing_ms") {
            let timeout = cfg.silence_timeout_ms() as f64;
            if target.is_finite() && target <= timeout {
                errors.push(ConfigError::new(
                    "grading.targets.endpointing_ms",
                    format!(
                        "{target} ms can never be met: turns wait \
                         turn_detection.silence_timeout_ms = {timeout} ms of silence \
                         before committing, so every turn would breach it. Set it \
                         above {timeout}, or shorten the timeout"
                    ),
                ));
            }
        }
        if let Some(levels) = g.levels {
            let named = [
                ("good", levels.good),
                ("poor", levels.poor),
                ("unusable", levels.unusable),
            ];
            for (name, v) in named {
                if let Some(v) = v
                    && (!v.is_finite() || v <= 0.0)
                {
                    errors.push(ConfigError::new(
                        format!("grading.levels.{name}"),
                        format!("must be a positive number of ms, got {v}"),
                    ));
                }
            }
            // Whatever is set must keep its order; a band above the one that
            // follows it would make a level unreachable.
            let set: Vec<(&str, f64)> = named
                .iter()
                .filter_map(|(n, v)| v.map(|v| (*n, v)))
                .collect();
            for pair in set.windows(2) {
                if pair[0].1 >= pair[1].1 {
                    errors.push(ConfigError::new(
                        format!("grading.levels.{}", pair[1].0),
                        format!(
                            "must be greater than `{}` ({}), got {}",
                            pair[0].0, pair[0].1, pair[1].1
                        ),
                    ));
                }
            }
        }
    }
}

/// `[barge_in]`.
fn check_barge_in(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate barge-in settings
    if let Some(ref bi) = cfg.barge_in {
        if bi.min_chunks == 0 {
            errors.push(ConfigError::new("barge_in.min_chunks", "must be > 0"));
        }
        if bi.speech_threshold <= 0.0 {
            errors.push(ConfigError::new(
                "barge_in.speech_threshold",
                "must be > 0.0",
            ));
        }
    }
}

/// The speech-to-speech session settings the engine itself reads.
fn check_realtime_session(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // The session settings the engine maps for every provider, bounded
    // here; the provider refuses the keys neither of them reads.
    if let Some(rt) = &cfg.realtime_provider {
        let mut o = voice_pipes_sdk::options::ProviderOptions::new(
            "realtime_provider.options",
            &rt.options,
        );
        let _ = o.string("instructions");
        let _ = o.optional_f64_in("vad_threshold", 0.0..=1.0);
        let _ = o.optional_u64_in("vad_silence_ms", 0..=10_000);
        let _ = o.optional_u64_in("vad_prefix_padding_ms", 0..=10_000);
        let _ = o.one_of("vad_eagerness", &["low", "medium", "high", "auto"], "auto");
        errors.extend(o.into_value_errors());
    }

    // The engine resamples the model's audio to this rate, so it is checked
    // whichever provider is configured.
    if let Some(Err(e)) = cfg
        .realtime_provider
        .as_ref()
        .map(RealtimeProviderConfig::output_emit_rate)
    {
        errors.push(e);
    }
}

/// `[realtime_provider]`, through its factory. A build without the
/// `realtime` feature refuses the section: it would otherwise load and
/// serve sessions that never hear the caller.
fn check_realtime_provider(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    if let Some(ref rt) = cfg.realtime_provider {
        #[cfg(feature = "realtime")]
        match crate::realtime::lookup_realtime_factory(&rt.provider) {
            None => errors.push(
                crate::plugins::Stage::Realtime
                    .unknown_provider("realtime_provider.provider", &rt.provider),
            ),
            Some(factory) => errors.extend(factory.validate(&crate::realtime::realtime_config(
                rt,
                factory.default_api_key_env(),
            ))),
        }
        #[cfg(not(feature = "realtime"))]
        errors.push(
            ConfigError::new(
                "realtime_provider.provider",
                format!(
                    "speech-to-speech provider \"{}\" needs a build with the `realtime` feature; this one has none",
                    rt.provider
                ),
            )
            .with_kind(voice_pipes_sdk::ConfigErrorKind::UnknownName),
        );

        // S2S Realtime is mutually exclusive with the classic
        // STT → LLM → TTS chain. We don't auto-disable -- we surface
        // the misconfiguration loudly so operators don't pay for both.
        if cfg.stt.is_some() {
            errors.push(ConfigError::new("stt", "[stt] is incompatible with [realtime_provider]; remove one. Realtime providers handle speech-to-text internally."));
        }
        if cfg.tts.is_some() {
            errors.push(ConfigError::new("tts", "[tts] is incompatible with [realtime_provider]; remove one. Realtime providers handle text-to-speech internally."));
        }
    }
}

/// Pool sizes, the transport, the audit buffer and the agent's idle settings.
fn check_runtime_ranges(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    if !matches!(cfg.pipeline.log_format.as_str(), "compact" | "json") {
        errors.push(ConfigError::new(
            "pipeline.log_format",
            format!(
                "must be \"compact\" or \"json\", got \"{}\"",
                cfg.pipeline.log_format
            ),
        ));
    }
    // Ranges the runtime relies on. Each of these is either divided by,
    // used as a channel or pool size, or compared as a threshold; a zero
    // or an inverted ordering would not fail at load and would misbehave
    // at the first session instead.
    if let Some(ref t) = cfg.transport {
        if t.enabled && t.port == 0 {
            errors.push(ConfigError::new(
                "transport.port",
                "must be > 0 when the transport is enabled",
            ));
        }
        if t.max_sessions == 0 {
            errors.push(ConfigError::new(
                "transport.max_sessions",
                "must be >= 1 (0 refuses every session)",
            ));
        }
        if t.write_timeout_ms == 0 {
            errors.push(ConfigError::new(
                "transport.write_timeout_ms",
                "must be > 0",
            ));
        }
    }
    if cfg.pipeline.audit.buffer_size == 0 {
        errors.push(ConfigError::new(
            "pipeline.audit.buffer_size",
            "must be >= 1",
        ));
    }
    if let Some(ref agent) = cfg.agent {
        if agent.idle_timeout_ms == Some(0) {
            errors.push(ConfigError::new(
                "agent.idle_timeout_ms",
                "must be > 0 (omit the key to disable idle prompts)",
            ));
        }
        if agent.idle_message.len() > MAX_PROMPT_BYTES {
            errors.push(ConfigError::new(
                "agent.idle_message",
                format!("must be at most {MAX_PROMPT_BYTES} bytes"),
            ));
        }
    }
}

/// `[adaptive_quality]`: the tiers keep their order.
fn check_adaptive_quality(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    let a = &cfg.adaptive_quality;
    if a.enabled {
        if a.downgrade_observations == 0 {
            errors.push(ConfigError::new(
                "adaptive_quality.downgrade_observations",
                "must be >= 1",
            ));
        }
        if !(a.reduced_arrival_rtf < a.minimal_arrival_rtf
            && a.minimal_arrival_rtf < a.critical_arrival_rtf)
        {
            errors.push(ConfigError::new(
                "adaptive_quality.*_arrival_rtf",
                "tiers must be ordered reduced < minimal < critical",
            ));
        }
        if !(a.reduced_send_latency_ms < a.minimal_send_latency_ms
            && a.minimal_send_latency_ms < a.critical_send_latency_ms)
        {
            errors.push(ConfigError::new(
                "adaptive_quality.*_send_latency_ms",
                "tiers must be ordered reduced < minimal < critical",
            ));
        }
        if a.reduced_underruns >= a.minimal_underruns {
            errors.push(ConfigError::new(
                "adaptive_quality.*_underruns",
                "tiers must be ordered reduced < minimal",
            ));
        }
    }
}

/// Each `[[audio_post_processor]]`'s circuit breaker.
fn check_audio_post_processors(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    for (idx, p) in cfg.audio_post_processor.iter().enumerate() {
        if let Some(cb) = p.circuit_breaker.as_ref() {
            if cb.failures == 0 {
                errors.push(ConfigError::new(
                    format!("audio_post_processor[{idx}].circuit_breaker.failures"),
                    "must be >= 1",
                ));
            }
            if cb.cooldown_ms == 0 {
                errors.push(ConfigError::new(
                    format!("audio_post_processor[{idx}].circuit_breaker.cooldown_ms"),
                    "must be > 0",
                ));
            }
        }
    }
}

/// The speech-to-speech endpoint observer and instructions.
fn check_realtime_extras(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    if let Some(ref rt) = cfg.realtime_provider {
        if let Some(ref eo) = rt.endpoint_observer {
            // Any registered VAD backend can serve the observer; the chosen
            // one checks its own settings, reported under the observer's
            // fields.
            let vad_factories = crate::vad::segmenter::vad_factories();
            match vad_factories.iter().find(|f| f.matches(&eo.backend)) {
                Some(factory) => {
                    errors.extend(
                        factory
                            .validate(&eo.vad_settings().backend_config())
                            .into_iter()
                            .map(|e| {
                                ConfigError::new(
                                    format!(
                                        "realtime_provider.endpoint_observer.{}",
                                        e.field.trim_start_matches("vad.")
                                    ),
                                    e.message,
                                )
                            }),
                    );
                }
                None => errors.push(
                    ConfigError::new(
                        "realtime_provider.endpoint_observer.backend",
                        format!(
                            "must be one of [{}], got \"{}\"",
                            vad_factories
                                .iter()
                                .map(|f| format!("\"{}\"", f.name()))
                                .collect::<Vec<_>>()
                                .join(", "),
                            eo.backend
                        ),
                    )
                    .with_kind(ConfigErrorKind::UnknownName),
                ),
            }
            if !(0.0..=10_000.0).contains(&eo.energy_threshold) {
                errors.push(ConfigError::new(
                    "realtime_provider.endpoint_observer.energy_threshold",
                    format!("must be in [0.0, 10000.0], got {}", eo.energy_threshold),
                ));
            }
            if !(0.0..=1.0).contains(&eo.speech_prob) {
                errors.push(ConfigError::new(
                    "realtime_provider.endpoint_observer.speech_prob",
                    format!("must be in [0.0, 1.0], got {}", eo.speech_prob),
                ));
            }
        }
        if rt
            .instructions
            .as_ref()
            .is_some_and(|s| s.len() > MAX_PROMPT_BYTES)
        {
            errors.push(ConfigError::new("realtime_provider.instructions", format!(
                    "must be at most {MAX_PROMPT_BYTES} bytes (use knowledge_base_path for long material)"
                )));
        }
    }
}

/// `[vad]`'s timing and hysteresis.
fn check_vad_timing(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    if let Some(ref vad) = cfg.vad {
        if let Some(ms) = vad.min_speech_ms
            && ms == 0
        {
            errors.push(ConfigError::new("vad.min_speech_ms", "must be > 0"));
        }
        if let Some(t) = vad.neg_threshold
            && !(0.0..=1.0).contains(&t)
        {
            errors.push(ConfigError::new(
                "vad.neg_threshold",
                format!("must be in [0.0, 1.0], got {t}"),
            ));
        }
    }
}

/// How many repeated tables a config may list.
fn check_shape_caps(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Shape caps: every entry below is carried into each session (or each
    // request); a config that lists hundreds of them is a mistake, not a
    // deployment.
    if cfg.text_processor.len() > MAX_TEXT_PROCESSORS {
        errors.push(ConfigError::new(
            "text_processor",
            format!("at most {MAX_TEXT_PROCESSORS} entries"),
        ));
    }
    if cfg.audio_post_processor.len() > MAX_POST_PROCESSORS {
        errors.push(ConfigError::new(
            "audio_post_processor",
            format!("at most {MAX_POST_PROCESSORS} entries"),
        ));
    }
    if cfg.audio_filter.len() > MAX_AUDIO_FILTERS {
        errors.push(ConfigError::new(
            "audio_filter",
            format!("at most {MAX_AUDIO_FILTERS} entries"),
        ));
    }
    if cfg.client_tool.len() > MAX_STATIC_CLIENT_TOOLS {
        errors.push(ConfigError::new(
            "client_tool",
            format!("at most {MAX_STATIC_CLIENT_TOOLS} entries"),
        ));
    }
}

/// Each `[[text_processor]]`, through its factory.
fn check_text_processors(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Each text processor through its factory, as the other stages are: an
    // unknown type fails here rather than at build, and the factory's own
    // checks (ranges, endpoints) run at config load.
    let tp_factories = crate::text::text_processor_factories();
    for (idx, tp) in cfg.text_processor.iter().enumerate() {
        match tp_factories.iter().find(|f| f.matches(&tp.processor_type)) {
            None => errors.push(
                crate::plugins::Stage::Llm
                    .unknown_provider(format!("text_processor[{idx}].type"), &tp.processor_type),
            ),
            // A factory sees one entry and names its fields
            // `text_processor.<key>`; the entry's index is the engine's
            // to add, or two processors of one type are indistinguishable.
            Some(factory) => errors.extend(
                factory
                    .validate(&credentials::text_processor(
                        tp,
                        factory.as_ref(),
                        &credentials::process_env,
                    ))
                    .into_iter()
                    .map(|mut e| {
                        e.field = indexed_text_processor_field(&e.field, idx);
                        e
                    }),
            ),
        }
    }
    for (idx, tp) in cfg.text_processor.iter().enumerate() {
        if tp
            .options
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.len() > MAX_PROMPT_BYTES)
        {
            errors.push(ConfigError::new(
                format!("text_processor[{idx}].system_prompt"),
                format!("must be at most {MAX_PROMPT_BYTES} bytes"),
            ));
        }
    }
}

/// Client-side tools: inline definitions and catalogs.
fn check_client_tools(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    // Validate client-side tools: inline
    // definitions must be well-formed and every external catalog must
    // exist and parse — fail fast, unlike the best-effort realtime KB file.
    for (idx, tool) in cfg.client_tool.iter().enumerate() {
        for message in tool.validation_errors() {
            errors.push(ConfigError::new(format!("client_tool[{idx}]"), message));
        }
    }
    if let Some(ref paths) = cfg.client_tools_path {
        for path in paths.paths() {
            if let Err(e) = crate::agent::client_tools::load_catalog_file(path) {
                errors.push(ConfigError::new("client_tools_path", format!("{e:#}")));
            }
        }
    }
}

/// `field` as reported for `text_processor[idx]`: `text_processor.<key>`
/// becomes `text_processor[idx].<key>`, and a bare key is placed under it.
fn indexed_text_processor_field(field: &str, idx: usize) -> String {
    match field.strip_prefix("text_processor") {
        Some("") => format!("text_processor[{idx}]"),
        Some(rest) if rest.starts_with('.') => format!("text_processor[{idx}]{rest}"),
        _ => format!("text_processor[{idx}].{field}"),
    }
}

/// Each `[[audio_filter]]`, through its factory.
fn check_audio_filters(cfg: &PipelineConfig, errors: &mut Vec<ConfigError>) {
    let filter_factories = crate::pipeline::filter::filter_factories();
    let known_providers: Vec<String> = filter_factories
        .iter()
        .map(|f| format!("\"{}\"", f.name()))
        .collect();
    for (idx, af) in cfg.audio_filter.iter().enumerate() {
        match filter_factories.iter().find(|f| f.matches(&af.provider)) {
            None => errors.push(
                ConfigError::new(
                    format!("audio_filter[{idx}].provider"),
                    format!(
                        "unknown audio filter \"{}\"; this build has [{}]",
                        af.provider,
                        known_providers.join(", ")
                    ),
                )
                .with_kind(ConfigErrorKind::UnknownName),
            ),
            Some(factory) => {
                let stage_cfg = voice_pipes_sdk::FilterStageConfig::new(&af.provider)
                    .with_model(af.model.clone())
                    .with_options(af.options.clone());
                errors.extend(factory.validate(&stage_cfg).into_iter().map(|mut e| {
                    e.field = indexed_audio_filter_field(&e.field, idx);
                    e
                }));
            }
        }
    }
}

/// `field` as reported for `audio_filter[idx]`: `audio_filter.<key>`
/// becomes `audio_filter[idx].<key>`, and a bare key is placed under it.
fn indexed_audio_filter_field(field: &str, idx: usize) -> String {
    match field.strip_prefix("audio_filter") {
        Some("") => format!("audio_filter[{idx}]"),
        Some(rest) if rest.starts_with('.') => format!("audio_filter[{idx}]{rest}"),
        _ => format!("audio_filter[{idx}].{field}"),
    }
}

#[cfg(test)]
#[path = "tests/validate.rs"]
mod tests;
