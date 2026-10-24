//! Pipeline configuration -- loading, merging, and validation.
//!
//! Reads TOML config files, merges CLI overrides, and builds
//! concrete provider instances (STT, TTS, VAD, text processors).

pub mod credentials;
pub mod discover;
mod factories;
mod param_schema_impls;
pub mod redact;
mod types;
mod validate;

#[cfg(test)]
#[path = "tests/suite.rs"]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sha2::{Digest, Sha256};
use tracing::info;

// Re-export all public types so callers keep using `crate::config::Foo`.
pub use factories::{
    build_barge_in_param_store, build_stt_provider, build_text_processor_param_store,
    build_text_processors, build_tts_param_store, build_tts_provider, build_turn_detector,
    build_vad, build_vad_param_store, default_stt_provider, default_tts_provider,
    stt_provider_name, text_processor_id, tts_provider_name,
};
#[cfg(feature = "realtime")]
pub use factories::{build_realtime_param_store, build_realtime_protocol};
pub use types::{
    AdaptiveQualityConfig, AgentSettings, AnnotatorConfig, AudioPostProcessorConfig, AuditConfig,
    AuditTapConfig, BargeInConfig, CircuitBreakerConfig, CliOverrides, ConfigError,
    ConfigErrorKind, DEFAULT_OUTPUT_EMIT_RATE, DEFAULT_VAD_NEG_THRESHOLD, DEFAULT_VAD_PRE_ROLL_MS,
    EndpointObserverConfig, OutputChunkingConfig, OutputConfig, PipelineConfig, PipelineSettings,
    PostProcessorMode, RealtimeProviderConfig, SpeculativeConfig, SttConfig, TextChunkingConfig,
    TextProcessorConfig, TransportConfig, TtsConfig, TurnDetectionConfig, VadSettings,
};
pub use validate::validate;

pub use voice_pipes_sdk::text::AUDIT_FULL_PROMPTS_OPTION;

// ── Load ────────────────────────────────────────────────────

const AUTO_DISCOVER_FILENAME: &str = "voice-pipes.toml";

/// `[pipeline] log_format` from the config [`load`] would read, before
/// logging is set up (so before `load` can log). Tolerant: a missing or
/// unreadable file, or one that does not parse, gives `None`, and the real
/// load reports the problem once logging exists.
pub fn log_format_hint(explicit_path: Option<&Path>) -> Option<String> {
    let path =
        explicit_path.map_or_else(|| PathBuf::from(AUTO_DISCOVER_FILENAME), Path::to_path_buf);
    let text = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    table
        .get("pipeline")?
        .get("log_format")?
        .as_str()
        .map(str::to_string)
}

/// Load pipeline config from an explicit path, or auto-discover
/// `voice-pipes.toml` in the current directory. Returns `None` if no
/// config file exists (pure CLI mode).
pub fn load(explicit_path: Option<&Path>) -> Result<Option<PipelineConfig>, anyhow::Error> {
    let path = if let Some(p) = explicit_path {
        if !p.exists() {
            return Err(ConfigError::new(
                "--config",
                format!("config file not found: {}", p.display()),
            )
            .into());
        }
        p.to_path_buf()
    } else {
        let auto = PathBuf::from(AUTO_DISCOVER_FILENAME);
        if !auto.exists() {
            return Ok(None);
        }
        // The working directory is whatever the process was started from;
        // say so, since a worker started from the wrong place would
        // otherwise pick up someone else's file without a trace.
        tracing::warn!(
            path = %std::env::current_dir().map_or_else(|_| auto.clone(), |d| d.join(&auto)).display(),
            "no --config given; using the config found in the working directory"
        );
        auto
    };

    info!(path = %path.display(), "loading pipeline config");
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading pipeline config {}", path.display()))?;
    let mut config: PipelineConfig = toml::from_str(&contents)
        .with_context(|| format!("parsing pipeline config {}", path.display()))?;

    // Absolute-ize audit paths relative to the process's current working directory
    // so that captures are always saved to the same folder regardless of where the config file lives.
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(dir) = config.pipeline.audit.captures_dir.as_ref()
            && dir.is_relative()
        {
            config.pipeline.audit.captures_dir = Some(cwd.join(dir));
        }
        if config.pipeline.audit_tap.dir.is_relative() {
            config.pipeline.audit_tap.dir = cwd.join(&config.pipeline.audit_tap.dir);
        }
    }

    // Client-tool catalogs and the realtime knowledge base resolve relative
    // to the config file's own directory, so they can be version-controlled
    // next to the config that names them and do not depend on where the
    // process was started.
    let config_dir = path.parent().map(std::path::Path::to_path_buf);
    if let Some(dir) = config_dir {
        if let Some(paths) = config.client_tools_path.as_mut() {
            for p in paths.paths_mut() {
                if p.is_relative() {
                    *p = dir.join(&*p);
                }
            }
        }
        if let Some(rt) = config.realtime_provider.as_mut()
            && let Some(kb) = rt.knowledge_base_path.as_mut()
            && kb.is_relative()
        {
            *kb = dir.join(&*kb);
        }
    }

    Ok(Some(config))
}

/// Resolve the worker's static client-tool declarations. Merge order
/// has later sources win on name collision: inline
/// `[[client_tool]]` entries, then external catalog files, with
/// per-session `Configure.options` merged last at session setup. Read
/// once at worker boot (startup-only I/O); a missing or invalid
/// catalog is a hard error.
pub fn load_client_tools(
    cfg: &PipelineConfig,
) -> Result<Vec<crate::agent::client_tools::ClientToolConfig>, anyhow::Error> {
    use crate::agent::client_tools::{ClientToolIndex, load_catalog_file};
    let mut file_tools = Vec::new();
    if let Some(paths) = cfg.client_tools_path.as_ref() {
        for path in paths.paths() {
            let tools = load_catalog_file(path)
                .map_err(|e| ConfigError::new("client_tools_path", format!("{e:#}")))?;
            file_tools.extend(tools);
        }
    }
    let index = ClientToolIndex::merge([cfg.client_tool.as_slice(), file_tools.as_slice()]);
    Ok(index.configs().to_vec())
}

// ── Merge ───────────────────────────────────────────────────

/// Where captures go when recording is on and no directory is named.
pub const DEFAULT_CAPTURES_DIR: &str = "captures";

/// Give recording one absolute directory: the one named (relative to the
/// working directory), or [`DEFAULT_CAPTURES_DIR`] there when recording is
/// on and none is named. The session recorder, the retention sweep and the
/// debug API's store all read this one value, so what is recorded is also
/// what is pruned and listed.
fn resolve_captures_dir(audit: &mut AuditConfig) {
    let named = audit.captures_dir.take();
    let dir = match named {
        Some(dir) => Some(dir),
        None if audit.enabled => Some(PathBuf::from(DEFAULT_CAPTURES_DIR)),
        None => None,
    };
    audit.captures_dir = dir.map(|dir| {
        if dir.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            cwd.join(dir)
        } else {
            dir
        }
    });
}

/// Build a final `PipelineConfig` by merging a (possibly absent) config file
/// with CLI overrides. CLI always wins.
pub fn merge(base: Option<PipelineConfig>, cli: CliOverrides) -> PipelineConfig {
    let mut cfg = base.unwrap_or_else(|| PipelineConfig {
        pipeline: PipelineSettings::default(),
        speculative: SpeculativeConfig::default(),
        stt: None,
        tts: None,
        output: None,
        vad: None,
        turn_detection: None,
        grading: None,
        barge_in: None,
        adaptive_quality: AdaptiveQualityConfig::default(),
        transport: None,
        text_processor: Vec::new(),
        audio_post_processor: Vec::new(),
        realtime_provider: None,
        auth: None,
        agent: None,
        client_tool: Vec::new(),
        client_tools_path: None,
    });

    // Global settings
    if let Some(fmt) = cli.log_format {
        cfg.pipeline.log_format = fmt;
    }

    // Audit
    if let Some(dir) = cli.captures_dir {
        cfg.pipeline.audit.enabled = true;
        cfg.pipeline.audit.captures_dir = Some(dir);
    }
    resolve_captures_dir(&mut cfg.pipeline.audit);

    // Audit tap
    if let Some(side) = cli.audit_tap {
        cfg.pipeline.audit_tap.enabled = true;
        cfg.pipeline.audit_tap.side = Some(side);
    }
    if let Some(dir) = cli.audit_tap_dir {
        cfg.pipeline.audit_tap.dir = dir;
    }

    // Output rate
    if let Some(rate) = cli.output_rate {
        let out = cfg.output.get_or_insert(OutputConfig {
            sample_rate: None,
            chunking: None,
        });
        out.sample_rate = Some(rate);
    }

    cfg
}

// ── Config hash ─────────────────────────────────────────────

/// Length of the hex-encoded hash prefix (32 hex chars = 16 bytes).
const HASH_HEX_LEN: usize = 32;

/// Compute a deterministic SHA-256 hash of the pipeline-relevant config.
///
/// Excludes transport settings (bind, port, `max_sessions`) and audit settings
/// since those don't affect pipeline behavior -- two workers with different
/// ports but identical STT/TTS/LLM/VAD config produce the same hash.
///
/// Returns a 32-character hex string (first 16 bytes of SHA-256).
pub fn config_hash(cfg: &PipelineConfig) -> String {
    // Serialize to JSON Value, then strip transient fields
    let mut val = serde_json::to_value(cfg).unwrap_or_default();
    if let Some(obj) = val.as_object_mut() {
        obj.remove("transport");
        // Remove audit settings from pipeline section (dir, buffer_size are deployment-specific)
        if let Some(pipeline) = obj.get_mut("pipeline").and_then(|p| p.as_object_mut()) {
            pipeline.remove("audit");
            pipeline.remove("audit_tap");
            pipeline.remove("log_format");
        }
    }
    // Convert to sorted BTreeMap for canonical ordering
    let canonical = to_canonical(&val);
    let json_str = serde_json::to_string(&canonical).unwrap_or_default();
    let hash = Sha256::digest(json_str.as_bytes());
    hash[..HASH_HEX_LEN / 2]
        .iter()
        .fold(String::with_capacity(HASH_HEX_LEN), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Recursively convert JSON values to use `BTreeMap` for deterministic key ordering.
fn to_canonical(val: &serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<&str, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.as_str(), to_canonical(v)))
                .collect();
            serde_json::to_value(sorted).unwrap_or_default()
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(to_canonical).collect())
        }
        other => other.clone(),
    }
}
