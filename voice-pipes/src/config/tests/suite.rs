use super::*;

#[test]
fn parse_minimal_toml() {
    let toml_str = r#"
[stt]
provider = "parakeet-tdt"

[tts]
provider = "piper"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.stt.as_ref().unwrap().provider, "parakeet-tdt");
    assert_eq!(cfg.tts.as_ref().unwrap().provider, "piper");
    assert_eq!(cfg.pipeline.log_format, "compact");
    assert!(!cfg.pipeline.audit.enabled);
    assert!(!cfg.pipeline.audit_tap.enabled);
    assert!(cfg.output.is_none());
}

/// Every committed `config/worker-realtime-*.toml` must parse against the
/// engine's schema (other `worker-*` files serve non-engine binaries with
/// their own schemas). Their speech-to-speech providers are plugins, so
/// whether each resolves is checked where they are registered: the CLI
/// crate's `committed_realtime_configs_resolve`.
#[test]
fn committed_worker_configs_parse() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("config dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("worker-realtime-")
            || path.extension().and_then(|e| e.to_str()) != Some("toml")
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read config");
        let _: PipelineConfig =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{name}: does not parse: {e}"));
        checked += 1;
    }
    assert!(checked > 0, "no worker-*.toml found in {}", dir.display());
}

#[test]
fn parse_full_toml() {
    let toml_str = r#"
[pipeline]
name = "test-run"
log_format = "json"

[pipeline.audit]
enabled = true
captures_dir = "audit/"
buffer_size = 512

[pipeline.audit_tap]
enabled = true
side = "both"
dir = "tap_out/"

[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt"

[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"

[output]
sample_rate = 48000
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.pipeline.name.as_deref(), Some("test-run"));
    assert_eq!(cfg.pipeline.log_format, "json");
    assert!(cfg.pipeline.audit.enabled);
    assert_eq!(
        cfg.pipeline.audit.captures_dir.as_deref(),
        Some(Path::new("audit/"))
    );
    assert_eq!(cfg.pipeline.audit.buffer_size, 512);
    assert!(cfg.pipeline.audit_tap.enabled);
    assert_eq!(cfg.pipeline.audit_tap.side.as_deref(), Some("both"));
    assert_eq!(cfg.pipeline.audit_tap.dir, PathBuf::from("tap_out/"));
    assert_eq!(cfg.output.unwrap().sample_rate, Some(48000));
}

#[test]
fn reject_unknown_fields_on_pipeline() {
    // PipelineConfig still uses deny_unknown_fields at top level
    let toml_str = r"
[pipeline]
bogus_field = true
";
    let result: Result<PipelineConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unknown field"),
        "expected unknown field error, got: {err}"
    );
}

#[test]
fn stt_config_captures_extra_options() {
    // Provider-specific options live under the dedicated `[stt.options]`
    // sub-table; flat keys directly under `[stt]` are reserved for fields
    // declared on `SttConfig`.
    let toml_str = r#"
[stt]
provider = "deepgram"
api_key = "sk-test"

[stt.options]
language = "en-US"
diarize = true
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let stt = cfg.stt.unwrap();
    assert_eq!(stt.provider, "deepgram");
    assert_eq!(
        stt.api_key
            .as_ref()
            .map(voice_pipes_sdk::secrets::Secret::expose),
        Some("sk-test")
    );
    assert_eq!(
        stt.options.get("language").and_then(|v| v.as_str()),
        Some("en-US")
    );
    assert_eq!(
        stt.options
            .get("diarize")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[test]
fn defaults_applied() {
    let cfg: PipelineConfig = toml::from_str("").unwrap();
    assert_eq!(cfg.pipeline.log_format, "compact");
    assert_eq!(cfg.pipeline.audit.buffer_size, 256);
    assert_eq!(cfg.pipeline.audit_tap.dir, PathBuf::from("audit_tap"));
    assert!(!cfg.pipeline.audit.enabled);
    assert!(!cfg.pipeline.audit_tap.enabled);
}

/// `auto` runs the auto backend only when it can: with its model on disk.
/// The test's own executable stands in for a model file that exists
/// (nothing is loaded).
#[test]
fn auto_runs_the_auto_backend_only_when_it_can() {
    crate::vad::segmenter::stand_in::register();
    let with_model = |model: std::path::PathBuf| {
        let mut v: VadSettings = toml::from_str("mode = \"auto\"\n").expect("parses");
        v.model = Some(model);
        v
    };
    let present = std::env::current_exe().expect("the test binary");
    assert_eq!(
        crate::config::factories::auto_vad_mode(&with_model(present)),
        crate::vad::segmenter::stand_in::NAME
    );
    let missing = std::path::PathBuf::from("no/such/model.onnx");
    assert_eq!(
        crate::config::factories::auto_vad_mode(&with_model(missing)),
        "energy"
    );
}

/// A config the engine checks on its own validates; the recogniser and the
/// voice are plugins, whose configs the CLI's shipped-config test validates
/// with them linked.
#[test]
fn validate_good_config() {
    let toml_str = "[output]\nsample_rate = 16000\n";
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

// An unknown provider name is now a hard error (was only a `warn!`),
// reported against `stt.provider`/`tts.provider`.
#[test]
fn validate_unknown_stt_provider() {
    let toml_str = r#"
[stt]
provider = "whisper"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert_eq!(errors.len(), 1, "got: {errors:?}");
    assert_eq!(errors[0].field, "stt.provider");
    assert_eq!(errors[0].kind, ConfigErrorKind::UnknownName);
}

// The factory's own validation errors are surfaced by `validate`
// instead of being deferred to a `build()`-time panic. The gRPC voice client
// requires an endpoint; omitting it is caught here.
#[cfg(feature = "tts-grpc")]
#[test]
fn validate_surfaces_factory_validation_error() {
    let toml_str = r#"
[tts]
provider = "grpc"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert!(
        errors.iter().any(|e| e.field == "endpoint"),
        "the gRPC voice without an endpoint must surface it, got: {errors:?}"
    );
}

#[test]
fn validate_unknown_tts_provider() {
    let toml_str = r#"
[tts]
provider = "coqui"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert_eq!(errors.len(), 1, "got: {errors:?}");
    assert_eq!(errors[0].field, "tts.provider");
    assert_eq!(errors[0].kind, ConfigErrorKind::UnknownName);
}

// An unknown provider is rejected even when an api_key is supplied —
// a key can't make an unbuildable provider valid. (This test originally used
// "elevenlabs" as its unknown example; that's a real TTS provider now.)
#[test]
fn validate_unknown_provider_rejected_even_with_api_key() {
    let toml_str = r#"
[tts]
provider = "not-a-real-provider"
api_key = "sk-test"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert_eq!(errors.len(), 1, "got: {errors:?}");
    assert_eq!(errors[0].field, "tts.provider");
}

#[test]
fn validate_bad_sample_rate_zero() {
    let toml_str = r"
[output]
sample_rate = 0
";
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].field.contains("output.sample_rate"));
}

#[test]
fn validate_bad_sample_rate_too_high() {
    let toml_str = r"
[output]
sample_rate = 999999
";
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("192000"));
}

#[test]
fn validate_bad_audit_tap_side() {
    let toml_str = r#"
[pipeline.audit_tap]
enabled = true
side = "left"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("left"));
}

#[test]
fn merge_cli_overrides_config() {
    let toml_str = r#"
[pipeline]
log_format = "json"

[stt]
provider = "parakeet-tdt"
model_dir = "models/old"

[tts]
provider = "piper"
model = "models/old.onnx"

[output]
sample_rate = 22050
"#;
    let base: PipelineConfig = toml::from_str(toml_str).unwrap();
    let overrides = CliOverrides {
        log_format: Some("compact".to_string()),
        captures_dir: Some(PathBuf::from("my_audit/")),
        audit_tap: Some("before".to_string()),
        audit_tap_dir: Some(PathBuf::from("my_tap/")),
        output_rate: Some(48000),
    };

    let merged = merge(Some(base), overrides);

    assert_eq!(merged.pipeline.log_format, "compact");
    assert!(merged.pipeline.audit.enabled);
    assert_eq!(
        merged.pipeline.audit.captures_dir,
        Some(std::env::current_dir().unwrap().join("my_audit/"))
    );
    assert!(merged.pipeline.audit_tap.enabled);
    assert_eq!(merged.pipeline.audit_tap.side.as_deref(), Some("before"));
    assert_eq!(merged.pipeline.audit_tap.dir, PathBuf::from("my_tap/"));
    // STT/TTS stay as configured in the base config (no CLI model overrides)
    assert_eq!(
        merged.stt.as_ref().unwrap().model_dir.as_deref(),
        Some(Path::new("models/old"))
    );
    assert_eq!(
        merged.tts.as_ref().unwrap().model.as_deref(),
        Some(Path::new("models/old.onnx"))
    );
    assert_eq!(merged.output.unwrap().sample_rate, Some(48000));
}

#[test]
fn merge_cli_only_no_config_file() {
    let overrides = CliOverrides {
        log_format: None,
        captures_dir: None,
        audit_tap: Some("both".to_string()),
        audit_tap_dir: None,
        output_rate: None,
    };

    let merged = merge(None, overrides);

    assert_eq!(merged.pipeline.log_format, "compact");
    assert!(merged.pipeline.audit_tap.enabled);
    assert_eq!(merged.pipeline.audit_tap.side.as_deref(), Some("both"));
    assert!(merged.stt.is_none());
    assert!(merged.tts.is_none());
}

#[test]
fn merge_config_file_only_no_cli_overrides() {
    let toml_str = r#"
[pipeline]
log_format = "json"

[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt"
"#;
    let base: PipelineConfig = toml::from_str(toml_str).unwrap();
    let overrides = CliOverrides {
        log_format: None,
        captures_dir: None,
        audit_tap: None,
        audit_tap_dir: None,
        output_rate: None,
    };

    let merged = merge(Some(base), overrides);

    assert_eq!(merged.pipeline.log_format, "json");
    assert_eq!(
        merged.stt.as_ref().unwrap().model_dir.as_deref(),
        Some(Path::new("models/parakeet-tdt"))
    );
}

#[test]
fn tap_side_disabled_when_not_enabled() {
    let cfg: PipelineConfig = toml::from_str("").unwrap();
    assert!(cfg.tap_side().is_none());
}

#[test]
fn tap_side_resolves_correctly() {
    let toml_str = r#"
[pipeline.audit_tap]
enabled = true
side = "before"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.tap_side(), Some(crate::audit::TapSide::Before));
}

/// An unknown or disabled turn-detection mode is an error at build time,
/// not a silent substitution of the silence detector.
#[test]
fn unknown_turn_mode_fails_closed() {
    let cfg: PipelineConfig =
        toml::from_str("[turn_detection]\nmode = \"telepathy\"\nsilence_timeout_ms = 450\n")
            .unwrap();
    let err = build_turn_detector(&cfg, None)
        .err()
        .expect("must not fall back");
    assert!(err.to_string().contains("telepathy"), "{err}");
    // Absent section: the documented silence default.
    let cfg: PipelineConfig = toml::from_str("").unwrap();
    assert!(build_turn_detector(&cfg, None).is_ok());
    let cfg: PipelineConfig =
        toml::from_str("[turn_detection]\nmode = \"silence\"\nsilence_timeout_ms = 450\n").unwrap();
    assert!(build_turn_detector(&cfg, None).is_ok());
}

/// The knowledge base resolves against the config file's directory,
/// like the client-tool catalogs, not against the working directory.
#[test]
fn knowledge_base_path_resolves_against_the_config_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pipeline.toml");
    std::fs::write(
        &path,
        "[realtime_provider]\nprovider = \"openai-realtime\"\nknowledge_base_path = \"kb/facts.md\"\n",
    )
    .unwrap();
    let cfg = load(Some(&path)).unwrap().unwrap();
    let kb = cfg.realtime_provider.unwrap().knowledge_base_path.unwrap();
    assert!(kb.is_absolute(), "{}", kb.display());
    assert_eq!(kb, dir.join("kb/facts.md"));
}

/// A misspelt key under the worker file's `[orchestrator]` table is an
/// error rather than a worker that never registers.
#[test]
fn worker_orchestrator_table_rejects_unknown_keys() {
    let err = toml::from_str::<crate::api::types::WorkerConfig>(
        "[orchestrator]\nendpont = \"http://orchestrator:9090\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("endpont"), "{err}");
    assert!(
        toml::from_str::<crate::api::types::WorkerConfig>(
            "[orchestrator]\nendpoint = \"http://orchestrator:9090\"\n"
        )
        .is_ok()
    );
    let err = toml::from_str::<crate::api::types::OrchestratorConfig>("prot = 9090\n").unwrap_err();
    assert!(err.to_string().contains("prot"), "{err}");
}

#[test]
fn load_nonexistent_explicit_path_errors() {
    let result = load(Some(Path::new("nonexistent_config_file.toml")));
    assert!(result.is_err());
}

#[test]
fn load_no_file_returns_none() {
    // In a test directory without voice-pipes.toml, should return None
    let result = load(None).unwrap();
    assert!(result.is_none());
}

#[test]
fn validate_multiple_errors() {
    let toml_str = r#"
[pipeline.audit_tap]
side = "invalid"

[stt]
provider = "unknown"

[tts]
provider = "unknown"

[output]
sample_rate = 0
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert_eq!(errors.len(), 4, "expected 4 errors, got: {errors:?}");
}

// ── build_vad tests ─────────────────────────────────────

#[test]
fn build_vad_defaults_to_passthrough() {
    let cfg: PipelineConfig = toml::from_str("").unwrap();
    let vad = build_vad(&cfg, None).unwrap();
    assert_eq!(vad.name(), "passthrough");
}

#[test]
fn build_vad_explicit_energy() {
    let toml_str = r#"
[vad]
mode = "energy"
threshold = 300.0
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let vad = build_vad(&cfg, None).unwrap();
    assert_eq!(vad.name(), "energy");
}

#[test]
fn build_vad_auto_falls_back_to_energy() {
    let toml_str = r#"
[vad]
mode = "auto"
model = "nonexistent/silero_vad.onnx"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let vad = build_vad(&cfg, None).unwrap();
    assert_eq!(vad.name(), "energy");
}

#[test]
fn build_vad_unknown_mode_errors() {
    let toml_str = r#"
[vad]
mode = "webrtc"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let result = build_vad(&cfg, None);
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(
        err.to_string().contains("webrtc"),
        "expected unknown mode error: {err}"
    );
}

// ── VAD threshold validation ───────────────────────────

#[test]
fn validate_energy_threshold_above_one_is_ok() {
    let toml_str = r#"
[vad]
mode = "energy"
energy_threshold = 300.0
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    let vad_errors: Vec<_> = errors
        .iter()
        .filter(|e| e.field.contains("energy_threshold"))
        .collect();
    assert!(
        vad_errors.is_empty(),
        "energy_threshold 300.0 should be valid, got: {vad_errors:?}"
    );
}

#[test]
fn validate_energy_threshold_rejects_negative() {
    let toml_str = r#"
[vad]
mode = "energy"
energy_threshold = -1.0
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert!(
        errors.iter().any(|e| e.field.contains("energy_threshold")),
        "energy_threshold -1.0 should be rejected, got: {errors:?}"
    );
}

#[test]
fn validate_energy_threshold_rejects_over_max() {
    let toml_str = r#"
[vad]
mode = "energy"
energy_threshold = 20000.0
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert!(
        errors.iter().any(|e| e.field.contains("energy_threshold")),
        "energy_threshold 20000.0 should be rejected, got: {errors:?}"
    );
}

#[test]
fn validate_speech_prob_rejects_above_one() {
    let toml_str = r#"
[vad]
mode = "silero"
model = "models/silero_vad.onnx"
speech_prob = 1.5
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    assert!(
        errors.iter().any(|e| e.field == "vad.speech_prob"),
        "speech_prob 1.5 should be rejected, got: {errors:?}"
    );
}

#[test]
fn validate_speech_prob_at_point_eight_is_ok() {
    let toml_str = r#"
[vad]
mode = "silero"
model = "models/silero_vad.onnx"
speech_prob = 0.8
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    let vad_errors: Vec<_> = errors
        .iter()
        .filter(|e| e.field == "vad.speech_prob")
        .collect();
    assert!(
        vad_errors.is_empty(),
        "speech_prob 0.8 should be valid, got: {vad_errors:?}"
    );
}

#[test]
fn validate_legacy_threshold_still_works() {
    let toml_str = r#"
[vad]
mode = "energy"
threshold = 300.0
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errors = validate(&cfg);
    let vad_errors: Vec<_> = errors
        .iter()
        .filter(|e| e.field.contains("threshold"))
        .collect();
    assert!(
        vad_errors.is_empty(),
        "legacy threshold 300.0 for energy should be valid, got: {vad_errors:?}"
    );
}

#[test]
fn effective_energy_threshold_prefers_specific() {
    let vad = VadSettings {
        mode: "energy".to_string(),
        model: None,
        energy_threshold: Some(500.0),
        speech_prob: None,
        threshold: Some(42.0),
        neg_threshold: None,
        min_speech_ms: None,
        min_silence_ms: None,
        pre_roll_ms: None,
        options: std::collections::HashMap::default(),
    };
    assert!((vad.effective_energy_threshold() - 500.0).abs() < f64::EPSILON);
}

#[test]
fn effective_energy_threshold_falls_back_to_threshold() {
    let vad = VadSettings {
        mode: "energy".to_string(),
        model: None,
        energy_threshold: None,
        speech_prob: None,
        threshold: Some(42.0),
        neg_threshold: None,
        min_speech_ms: None,
        min_silence_ms: None,
        pre_roll_ms: None,
        options: std::collections::HashMap::default(),
    };
    assert!((vad.effective_energy_threshold() - 42.0).abs() < f64::EPSILON);
}

#[test]
fn effective_speech_prob_prefers_specific() {
    let vad = VadSettings {
        mode: "silero".to_string(),
        model: None,
        energy_threshold: None,
        speech_prob: Some(0.7),
        threshold: Some(0.3),
        neg_threshold: None,
        min_speech_ms: None,
        min_silence_ms: None,
        pre_roll_ms: None,
        options: std::collections::HashMap::default(),
    };
    assert!((vad.effective_speech_prob() - 0.7).abs() < f32::EPSILON);
}

#[test]
fn effective_speech_prob_falls_back_to_threshold() {
    let vad = VadSettings {
        mode: "silero".to_string(),
        model: None,
        energy_threshold: None,
        speech_prob: None,
        threshold: Some(0.3),
        neg_threshold: None,
        min_speech_ms: None,
        min_silence_ms: None,
        pre_roll_ms: None,
        options: std::collections::HashMap::default(),
    };
    assert!((vad.effective_speech_prob() - 0.3).abs() < f32::EPSILON);
}

// ── config_hash tests ───────────────────────────────────

#[test]
fn config_hash_is_deterministic() {
    let toml_str = r#"
[stt]
provider = "parakeet-tdt"

[tts]
provider = "piper"
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let h1 = config_hash(&cfg);
    let h2 = config_hash(&cfg);
    assert_eq!(h1, h2, "same config must produce the same hash");
    assert_eq!(
        h1.len(),
        HASH_HEX_LEN,
        "hash must be {HASH_HEX_LEN} hex chars"
    );
}

/// The hash names a pipeline's shape, not its credentials: a key, whether a
/// stage's `api_key` or a processor's option, never goes into it, so rotating
/// one leaves the hash alone and the hash cannot be used to test a guess.
#[test]
fn config_hash_ignores_credentials() {
    let with_key = |key: &str| -> PipelineConfig {
        toml::from_str(&format!(
            "[[text_processor]]\ntype = \"openai-compat\"\nbase_url = \"http://llm:8081/v1\"\n\
             model = \"m\"\napi_key = \"{key}\"\n\n[tts]\nprovider = \"piper\"\napi_key = \"{key}\"\n"
        ))
        .unwrap()
    };
    assert_eq!(
        config_hash(&with_key("sk-one")),
        config_hash(&with_key("sk-two"))
    );
}

#[test]
fn config_hash_ignores_transport() {
    let base = r#"
[stt]
provider = "parakeet-tdt"

[tts]
provider = "piper"
"#;
    let with_transport = r#"
[stt]
provider = "parakeet-tdt"

[tts]
provider = "piper"

[transport]
enabled = true
bind = "0.0.0.0"
port = 9090
max_sessions = 10
"#;
    let cfg1: PipelineConfig = toml::from_str(base).unwrap();
    let cfg2: PipelineConfig = toml::from_str(with_transport).unwrap();
    assert_eq!(
        config_hash(&cfg1),
        config_hash(&cfg2),
        "transport settings must not affect hash"
    );
}

#[test]
fn config_hash_ignores_audit_and_log_format() {
    let base = r#"
[stt]
provider = "parakeet-tdt"
"#;
    let with_audit = r#"
[pipeline]
log_format = "json"

[pipeline.audit]
enabled = true
captures_dir = "audit/"

[stt]
provider = "parakeet-tdt"
"#;
    let cfg1: PipelineConfig = toml::from_str(base).unwrap();
    let cfg2: PipelineConfig = toml::from_str(with_audit).unwrap();
    assert_eq!(
        config_hash(&cfg1),
        config_hash(&cfg2),
        "audit/log_format settings must not affect hash"
    );
}

#[test]
fn config_hash_differs_for_different_providers() {
    let toml_a = r#"
[stt]
provider = "parakeet-tdt"
"#;
    let toml_b = r#"
[stt]
provider = "deepgram"
api_key = "sk-test"
"#;
    let cfg_a: PipelineConfig = toml::from_str(toml_a).unwrap();
    let cfg_b: PipelineConfig = toml::from_str(toml_b).unwrap();
    assert_ne!(
        config_hash(&cfg_a),
        config_hash(&cfg_b),
        "different providers must produce different hashes"
    );
}

// The hand-written "wizard-generated" config fixtures that used to live here
// had drifted from the wizard's real output. They are superseded by
// `commands::config_wizard::tests::every_choice_yields_a_parseable_valid_config`,
// which validates the *actually generated* TOML for every menu path.

// ── Client-side tools config ──

#[test]
fn parse_client_tool_inline_and_path_variants() {
    let toml_str = r#"
client_tools_path = "commands/client-tools.json"

[[client_tool]]
name = "navigate"
description = "Navigate the avatar UI to a named screen."
speak_on_result = true
[client_tool.parameters]
type = "object"
properties = { screen = { type = "string" } }
required = ["screen"]
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.client_tool.len(), 1);
    assert_eq!(cfg.client_tool[0].name, "navigate");
    assert!(cfg.client_tool[0].speak_on_result);
    assert_eq!(cfg.client_tool[0].parameters["required"][0], "screen");
    let paths = cfg.client_tools_path.as_ref().unwrap().paths().to_vec();
    assert_eq!(paths, vec![PathBuf::from("commands/client-tools.json")]);

    // The list form parses too.
    let toml_str = r#"client_tools_path = ["a.json", "b.toml"]"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(cfg.client_tools_path.unwrap().paths().len(), 2);

    // `parameters` defaults to the empty object schema.
    let toml_str = r#"
[[client_tool]]
name = "ping"
description = "Ping the client."
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(
        cfg.client_tool[0].parameters,
        serde_json::json!({"type": "object"})
    );
    assert!(!cfg.client_tool[0].speak_on_result);
}

#[test]
fn validate_client_tool_errors() {
    // Invalid inline definition.
    let toml_str = r#"
[[client_tool]]
name = "has spaces"
description = ""
"#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errs = validate(&cfg);
    assert!(
        errs.iter().any(|e| e.field == "client_tool[0]"),
        "expected client_tool errors, got {errs:?}"
    );

    // Missing catalog file fails fast.
    let toml_str = r#"client_tools_path = "does/not/exist.json""#;
    let cfg: PipelineConfig = toml::from_str(toml_str).unwrap();
    let errs = validate(&cfg);
    assert!(
        errs.iter().any(|e| e.field == "client_tools_path"),
        "expected client_tools_path error, got {errs:?}"
    );
}

/// The shipped example configs must keep parsing and their shared
/// catalog must keep loading (merged with the inline tools, later
/// source wins).
#[test]
fn example_clienttools_configs_parse_and_load() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/avatar");
    for file in [
        "avatar-cascade-clienttools.toml",
        "avatar-realtime-clienttools.toml",
    ] {
        let raw = std::fs::read_to_string(root.join(file)).unwrap();
        let mut cfg: PipelineConfig =
            toml::from_str(&raw).unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        // Mimic load(): resolve catalog paths relative to the config dir.
        if let Some(paths) = cfg.client_tools_path.as_mut() {
            for p in paths.paths_mut() {
                if p.is_relative() {
                    *p = root.join(&*p);
                }
            }
        }
        let tools = load_client_tools(&cfg).unwrap_or_else(|e| panic!("{file} catalog: {e}"));
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"navigate"), "{file}: {names:?}");
        assert!(names.contains(&"highlight"), "{file}: {names:?}");
        assert!(names.contains(&"play_media"), "{file}: {names:?}");
    }
}

/// The shipped corpus example must actually turn on continuous recording.
///
/// Without this, the example can silently stop doing the one thing it exists
/// for: a run looks successful, writes all the usual per-utterance artefacts,
/// and leaves no `session_audio.wav` behind.
#[test]
fn the_turn_corpus_example_enables_session_recording() {
    let rel = std::path::Path::new("examples/live/english-turn-corpus.toml");
    let path = if rel.exists() {
        rel.to_path_buf()
    } else {
        std::path::Path::new("voice-pipes").join(rel)
    };
    let cfg = crate::config::load(Some(&path))
        .expect("example config loads")
        .expect("example config is present");
    assert!(cfg.pipeline.audit.enabled, "audit must be enabled");
    assert!(
        cfg.pipeline.audit.record_session_audio,
        "record_session_audio must survive loading, or the example records nothing"
    );
}

// ── [grading] ───────────────────────────────────────────────

fn grading_errors(section: &str) -> Vec<ConfigError> {
    let toml_str = format!(
        "[stt]\nprovider = \"parakeet-tdt\"\n\n[tts]\nprovider = \"piper\"\n\
         model = \"models/piper/en_US.onnx\"\n\n{section}"
    );
    let cfg: PipelineConfig = toml::from_str(&toml_str).expect("parses");
    validate(&cfg)
        .into_iter()
        .filter(|e| e.field.starts_with("grading"))
        .collect()
}

#[test]
fn grading_is_optional_and_any_subset_may_be_set() {
    // A timeout is stated because an endpointing target is checked against it.
    let td = "[turn_detection]\nsilence_timeout_ms = 700\n\n";
    assert!(grading_errors("").is_empty());
    assert!(grading_errors(&format!("{td}[grading.targets]\nendpointing_ms = 800\n")).is_empty());
    assert!(grading_errors("[grading.targets]\nttft_ms = 500\n").is_empty());
    assert!(grading_errors("[grading.levels]\npoor = 4000\n").is_empty());
    assert!(
        grading_errors(&format!(
            "{td}[grading.targets]\nendpointing_ms = 800\nttft_ms = 500\n\n\
             [grading.levels]\ngood = 2000\npoor = 4000\nunusable = 6000\n"
        ))
        .is_empty()
    );
}

/// A misspelt target would never apply, and the operator would go on being
/// graded by the default they meant to replace with nothing to tell them so.
#[test]
fn a_misspelt_grading_target_is_an_error_not_a_silent_no_op() {
    let errs = grading_errors("[grading.targets]\nendpointng_ms = 800\n");
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "grading.targets.endpointng_ms");
    assert!(
        errs[0].message.contains("endpointing_ms"),
        "should list the valid names"
    );
}

#[test]
fn grading_values_must_be_positive_numbers() {
    for bad in ["0", "-5", "nan", "inf"] {
        let errs = grading_errors(&format!("[grading.targets]\nttft_ms = {bad}\n"));
        assert_eq!(
            errs.len(),
            1,
            "ttft_ms = {bad} should be rejected: {errs:?}"
        );
    }
    assert_eq!(grading_errors("[grading.levels]\ngood = 0\n").len(), 1);
}

/// A band set above the one that follows it would make a level unreachable.
#[test]
fn grading_levels_must_stay_in_order() {
    let errs = grading_errors("[grading.levels]\ngood = 5000\npoor = 4000\n");
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "grading.levels.poor");
    // Order is checked across whatever is set, not only neighbours.
    assert_eq!(
        grading_errors("[grading.levels]\ngood = 7000\nunusable = 6000\n").len(),
        1
    );
}

#[test]
fn unknown_keys_under_grading_are_rejected() {
    let toml_str = "[grading]\nlevel = \"lenient\"\n";
    assert!(toml::from_str::<PipelineConfig>(toml_str).is_err());
    let toml_str = "[grading.levels]\nterrible = 9000\n";
    assert!(toml::from_str::<PipelineConfig>(toml_str).is_err());
}

/// What the report engine reads out of `metadata.json`: only what was set,
/// so an unset rule is absent rather than present as a null to be guessed at.
#[test]
fn grading_serializes_only_what_was_set() {
    let cfg: PipelineConfig = toml::from_str("[grading.targets]\nendpointing_ms = 800\n").unwrap();
    let json = serde_json::to_value(&cfg.grading).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "targets": { "endpointing_ms": 800.0 } })
    );
    let none: PipelineConfig = toml::from_str("").unwrap();
    assert_eq!(
        serde_json::to_value(&none.grading).unwrap(),
        serde_json::Value::Null
    );
}

/// A target the operator set that no turn can meet is their mistake to hear
/// about at startup, not something to discover as a wall of breaches.
#[test]
fn an_endpointing_target_under_the_silence_wait_is_unreachable() {
    let section = "[turn_detection]
silence_timeout_ms = 700

                   [grading.targets]
endpointing_ms = 600
";
    let errs = grading_errors(section);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "grading.targets.endpointing_ms");
    assert!(
        errs[0].message.contains("700"),
        "should name the timeout: {}",
        errs[0].message
    );

    // Above the wait is fine, and so is leaving it unset -- the built-in
    // default sits under a 700 ms timeout too, and that is not their doing.
    let ok = "[turn_detection]
silence_timeout_ms = 700

              [grading.targets]
endpointing_ms = 800
";
    assert!(grading_errors(ok).is_empty());
    assert!(
        grading_errors(
            "[turn_detection]
silence_timeout_ms = 700
"
        )
        .is_empty()
    );
}

// ── Credentials and plain-text endpoints ────────────────────

// ── Default providers ───────────────────────────────────────

/// A pipeline without `[stt]` or `[tts]` falls back to this build's default,
/// and a build without one says the section is missing instead of naming a
/// provider the user never wrote.
#[test]
fn a_missing_stage_section_falls_back_to_this_builds_default_or_says_so() {
    let bare: PipelineConfig = toml::from_str("[pipeline]\nname = \"bare\"\n").unwrap();
    assert_eq!(
        crate::config::stt_provider_name(&bare),
        crate::config::default_stt_provider()
    );
    assert_eq!(
        crate::config::tts_provider_name(&bare),
        crate::config::default_tts_provider()
    );
    if crate::config::default_stt_provider().is_none() {
        let err = crate::config::build_stt_provider(&bare, None)
            .err()
            .expect("no recogniser");
        assert!(err.to_string().contains("no [stt] section"), "{err}");
    }
    if crate::config::default_tts_provider().is_none() {
        let err = crate::config::build_tts_provider(&bare, None, None)
            .err()
            .expect("no voice");
        assert!(err.to_string().contains("no [tts] section"), "{err}");
    }

    let named: PipelineConfig =
        toml::from_str("[stt]\nprovider = \"custom-stt\"\n[tts]\nprovider = \"custom-tts\"\n")
            .unwrap();
    assert_eq!(
        crate::config::stt_provider_name(&named).as_deref(),
        Some("custom-stt")
    );
    assert_eq!(
        crate::config::tts_provider_name(&named).as_deref(),
        Some("custom-tts")
    );
}

/// A detector's own keys stay flat in `[turn_detection]` and reach it as
/// options with their TOML types.
#[test]
fn turn_detection_keys_reach_the_detector_as_options() {
    let cfg: PipelineConfig = toml::from_str(
        r#"
[turn_detection]
mode = "smart"
silence_timeout_ms = 1500
smart_model_path = "models/smart-turn.onnx"
smart_threshold = 0.5
smart_window_secs = 8
"#,
    )
    .unwrap();
    let td = cfg.turn_detection.unwrap().turn_config();
    assert_eq!(td.mode, "smart");
    assert_eq!(td.silence_timeout_ms, 1500);
    let mut o = td.options();
    assert_eq!(
        o.string("smart_model_path").as_deref(),
        Some("models/smart-turn.onnx")
    );
    assert_eq!(o.optional_f64_in("smart_threshold", 0.0..=1.0), Some(0.5));
    assert_eq!(o.optional_u64_in("smart_window_secs", 1..=8), Some(8));
    assert!(!td.options.contains_key("mode"));
}

/// A key the selected mode does not read is refused by validation, so a
/// typo or a leftover setting is still an error without a fixed schema.
#[test]
fn a_turn_key_the_mode_does_not_read_is_refused() {
    let cfg: PipelineConfig = toml::from_str(
        r#"
[turn_detection]
mode = "layered"
silence_timeout_ms = 700
smart_threshold = 0.5
"#,
    )
    .unwrap();
    let errors = super::validate(&cfg);
    assert!(
        errors
            .iter()
            .any(|e| e.field == "turn_detection.smart_threshold"
                && e.message.contains("\"layered\"")),
        "{errors:?}"
    );
}

/// What the setup wizard writes, `enabled = true` and no directory: the
/// merged config names one absolute directory, so the recorder, the
/// retention sweep and the debug API's store agree on it.
#[test]
fn recording_with_no_directory_named_gets_one_absolute_default() {
    let file: PipelineConfig = toml::from_str("[pipeline.audit]\nenabled = true\n").unwrap();
    let cfg = merge(Some(file), CliOverrides::default());
    let dir = cfg.pipeline.audit.captures_dir.expect("a directory");
    assert!(dir.is_absolute(), "{}", dir.display());
    assert!(dir.ends_with(DEFAULT_CAPTURES_DIR), "{}", dir.display());
}

#[test]
fn recording_off_names_no_directory() {
    let file: PipelineConfig = toml::from_str("[pipeline.audit]\nenabled = false\n").unwrap();
    let cfg = merge(Some(file), CliOverrides::default());
    assert!(cfg.pipeline.audit.captures_dir.is_none());
}

#[test]
fn a_relative_directory_from_the_command_line_is_made_absolute() {
    let cfg = merge(
        None,
        CliOverrides {
            captures_dir: Some(std::path::PathBuf::from("my-captures")),
            ..CliOverrides::default()
        },
    );
    let dir = cfg.pipeline.audit.captures_dir.expect("a directory");
    assert!(
        dir.is_absolute() && dir.ends_with("my-captures"),
        "{}",
        dir.display()
    );
}

#[test]
fn example_feature_configs_parse_cleanly() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/features");
    for file in [
        "turn-vap.toml",
        "speculative-execution.toml",
        "adaptive-quality.toml",
        "agent-grpc-idle.toml",
        "realtime-s2s-endpoint-observer.toml",
    ] {
        let raw = std::fs::read_to_string(root.join(file)).unwrap();
        let _cfg: PipelineConfig =
            toml::from_str(&raw).unwrap_or_else(|e| panic!("{file} must parse: {e}"));
    }
}
