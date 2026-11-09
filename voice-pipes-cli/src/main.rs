#![expect(
    clippy::doc_markdown,
    reason = "doc comments on the clap types are the --help text, where backticks would print literally"
)]

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Result;
use voice_pipes::audit;
use voice_pipes::config;
use voice_pipes::telemetry;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing::error;

use audit::AuditSink;
use config::PipelineConfig;

/// Voice Pipes -- local voice pipeline
#[derive(Parser)]
#[command(name = "voice-pipes", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Log format. Without it, `[pipeline] log_format` in the config
    /// decides, and compact lines when that is unset.
    #[arg(long, value_enum, global = true)]
    log_format: Option<LogFormat>,

    /// Enable audit trail and write to this directory
    #[arg(long, global = true)]
    captures_dir: Option<PathBuf>,

    /// Record audio around resampling stages for auditing.
    /// Values: before, after, both
    #[arg(long, global = true, value_name = "SIDE")]
    audit_tap: Option<String>,

    /// Directory for audit tap WAV files (default: audit_tap/)
    #[arg(long, global = true, default_value = "audit_tap")]
    audit_tap_dir: PathBuf,

    /// Path to a TOML configuration file. Auto-discovers voice-pipes.toml
    /// in the current directory when not specified.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Synthesize text to speech (text -> WAV)
    Tts {
        /// Text to synthesize
        #[arg(short, long)]
        text: String,

        /// Output WAV file path
        #[arg(short, long, default_value = "output.wav")]
        output: PathBuf,
    },

    /// Run every component of a pipeline against known fixtures and report
    /// whether each did its job.
    ///
    /// Builds the components from the config file, so nothing needs to be
    /// running. Exits non-zero when a stage fails, so it can gate a deploy.
    /// Reports functional failures as well as measurement faults
    /// (`measurement.zero_with_work`, `measurement.invalid_numeric`).
    Check {
        /// Fixture to use, by name. Defaults to one with speech and one
        /// without, which test different failures.
        #[arg(long, value_name = "NAME")]
        fixture: Option<String>,
    },

    /// Transcribe speech to text (WAV -> text)
    Stt {
        /// Input WAV file path
        #[arg(short, long)]
        input: PathBuf,
    },

    /// Round-trip pipeline: STT -> TTS (WAV -> text -> WAV)
    Pipe {
        /// Input WAV file path (audio to transcribe)
        #[arg(short, long)]
        input: PathBuf,

        /// Output WAV file path (re-synthesized audio)
        #[arg(short, long, default_value = "output.wav")]
        output: PathBuf,

        /// Resample TTS output to this sample rate (e.g. 16000, 48000).
        /// By default, keeps the TTS model's native rate.
        #[arg(long)]
        output_rate: Option<u32>,
    },

    /// Run pipeline from a config file (WAV -> STT -> TTS -> WAV)
    Run {
        /// Input WAV file path
        #[arg(short, long)]
        input: PathBuf,

        /// Output WAV file path (default: output.wav)
        #[arg(short, long, default_value = "output.wav")]
        output: PathBuf,
    },

    /// Start as a worker node (serves the pipeline + debug API)
    Serve {
        /// Path to worker config TOML
        #[arg(long, value_name = "PATH")]
        worker_config: Option<PathBuf>,
    },

    /// Start as the orchestrator (admin API + internal API)
    Orchestrate {
        /// Path to orchestrator config TOML
        #[arg(long, value_name = "PATH")]
        orchestrator_config: Option<PathBuf>,
    },

    /// Live mic->STT->TTS->speaker loop (Ctrl+C to stop)
    #[cfg(feature = "audio-device")]
    Live {
        /// Silence duration (ms) to detect end of utterance
        /// Silence duration (ms) to detect end of utterance.
        /// If omitted, falls back to [vad].min_silence_ms from config.
        #[arg(long)]
        silence_ms: Option<u64>,

        /// Save captured audio to this directory: one WAV per utterance, plus
        /// a continuous stereo session_audio.wav when `[pipeline.audit]
        /// record_session_audio` is set
        #[arg(long)]
        save_input: Option<PathBuf>,

        /// Save TTS output to this directory (one WAV per utterance)
        #[arg(long)]
        save_output: Option<PathBuf>,
    },

    /// Listen: mic->STT only -- prints transcriptions (no LLM/TTS)
    #[cfg(feature = "audio-device")]
    Listen {
        /// Silence duration (ms) to detect end of utterance
        /// Silence duration (ms) to detect end of utterance.
        /// If omitted, falls back to [vad].min_silence_ms from config.
        #[arg(long)]
        silence_ms: Option<u64>,

        /// Save captured mic audio to this directory
        #[arg(long)]
        save_input: Option<PathBuf>,
    },

    /// Interactive real-time S2S voice loop client
    #[cfg(all(feature = "realtime", feature = "audio-device"))]
    Interactive {
        /// Speech-to-speech provider, by name or alias (gemini, openai,
        /// elevenlabs, or any other this binary registers). If omitted,
        /// falls back to [realtime_provider].provider from the configuration file.
        #[arg(long)]
        provider: Option<String>,

        /// Silence duration (ms) to detect end of utterance
        #[arg(long)]
        silence_ms: Option<u64>,

        /// Disable Echo Gating (audio-out gates audio-in is disabled).
        /// Default is `true` (gating enabled). Use this flag if testing with headphones
        /// and you want voice-based interruptions (server-side VAD barge-in).
        #[arg(long = "no-echo-gating", action = clap::ArgAction::SetFalse)]
        echo_gating: bool,
    },

    /// Run text through the text-processor chain (for testing LLM / echo / custom processors)
    Text {
        /// Input text (reads from stdin if omitted)
        #[arg(short, long)]
        input: Option<String>,
    },

    /// List compiled-in providers, or print one provider's parameter schema.
    Inspect {
        /// Filter by kind
        #[arg(long, value_enum, default_value_t = Kind::All)]
        kind: Kind,

        /// Show details + parameter schema for a single provider by name
        #[arg(long, value_name = "NAME")]
        provider: Option<String>,

        /// Print canonical metric registry descriptors (alias for --kind metrics)
        #[arg(long)]
        metrics: bool,

        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },

    /// Validate a TOML config file: load it, run the same validation pass
    /// every other subcommand uses, and print a report. Exits non-zero on
    /// any validation error so it can be wired into CI.
    ValidateConfig {
        /// Path to the config file to validate.
        #[arg(value_name = "PATH")]
        path: PathBuf,

        /// Output format
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },

    /// Probe a health endpoint and exit 0 only on a success answer: the
    /// container images' `HEALTHCHECK`, which has no other HTTP client.
    Healthz {
        /// The URL to probe.
        #[arg(long, default_value = voice_pipes::commands::DEFAULT_HEALTHZ_URL)]
        url: String,

        /// Give up after this many seconds.
        #[arg(long, default_value_t = 3)]
        timeout_s: u64,
    },

    /// Launch interactive fleet configurator wizard to generate TOML configurations
    #[cfg(feature = "wizard")]
    ConfigWizard,

    /// Download local model files, or list them, without the wizard
    #[cfg(feature = "wizard")]
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
}

/// What `models` does.
#[cfg(feature = "wizard")]
#[derive(Subcommand)]
enum ModelsAction {
    /// Download missing files to their default locations. With no names,
    /// fetches what examples/minimal.toml needs (Parakeet, Silero VAD, Piper,
    /// SmolLM2-360M).
    Fetch {
        /// Model names as `models list` prints them, e.g. smollm2-1.7b
        #[arg(value_name = "NAME")]
        names: Vec<String>,
    },
    /// List every known model, where it goes and whether it is on disk
    List,
}

/// How log lines are written.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum LogFormat {
    Compact,
    Json,
}

impl LogFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Json => "json",
        }
    }
}

/// How a report is printed.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

/// Which providers `inspect` lists.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Kind {
    All,
    Stt,
    Tts,
    Vad,
    #[value(alias = "text_processor")]
    Text,
    #[value(alias = "turn_detection")]
    Turn,
    #[value(alias = "s2s")]
    Realtime,
    Metrics,
}

impl From<Kind> for voice_pipes::commands::InspectKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::All => Self::All,
            Kind::Stt => Self::Stt,
            Kind::Tts => Self::Tts,
            Kind::Vad => Self::Vad,
            Kind::Text => Self::Text,
            Kind::Turn => Self::Turn,
            Kind::Realtime => Self::Realtime,
            Kind::Metrics => Self::Metrics,
        }
    }
}

/// Plugin providers are registered here, before the engine runs, so its
/// factory lists carry them like the built-in ones, and their setup-wizard
/// entries with them. The engine never names a plugin; the binary that
/// links it does. Safe to call more than once: the first call registers,
/// later ones return its outcome. The wizard orders entries by their own
/// `menu_rank`, not by the order here.
///
/// # Errors
/// A plugin whose provider name is already taken, or a second entry that
/// claims to be its stage's default: registering it would silently shadow
/// the other.
fn register_plugins() -> Result<(), String> {
    static OUTCOME: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    OUTCOME
        .get_or_init(|| {
            register_each_plugin()
                .map_err(|e: voice_pipes::plugins::RegistrationError| e.to_string())
        })
        .clone()
}

/// For each stage, the features of this crate that give it something to
/// offer, which the setup wizard names when a build has none. A test holds
/// every name to a feature this crate has.
const BUILD_HINTS: &[(voice_pipes::plugins::Stage, &str)] = {
    use voice_pipes::plugins::Stage;
    &[
        (
            Stage::Stt,
            "stt-parakeet, stt-moonshine, stt-nemotron35, stt-assemblyai or stt-google",
        ),
        (Stage::Llm, "llm-gguf or llm-openai-compat"),
        (
            Stage::Tts,
            "tts-piper, tts-kokoro, tts-openai, tts-elevenlabs, tts-gemini or tts-pocket",
        ),
        (Stage::Realtime, "s2s-gemini, s2s-openai or s2s-elevenlabs"),
        (Stage::Turn, "turn-smart or turn-vap"),
    ]
};

/// Every linked plugin's factory, then its wizard entries under the name the
/// factory answers to.
#[allow(
    clippy::unnecessary_wraps,
    reason = "a build that links no plugin has nothing that can fail"
)]
fn register_each_plugin() -> Result<(), voice_pipes::plugins::RegistrationError> {
    /// Register a linked plugin's factory and its wizard entries.
    macro_rules! plugin {
        ($feature:literal, $stage:ident, $krate:ident) => {
            #[cfg(feature = $feature)]
            voice_pipes::plugins::register_plugin(
                voice_pipes::plugins::Plugin::$stage($krate::factory),
                $krate::wizard_choices(),
            )?;
        };
        ($feature:literal, $stage:ident, $krate:ident, no wizard entries) => {
            #[cfg(feature = $feature)]
            voice_pipes::plugins::register_plugin(
                voice_pipes::plugins::Plugin::$stage($krate::factory),
                Vec::new(),
            )?;
        };
    }
    plugin!("stt-parakeet", Stt, voice_pipes_stt_parakeet);
    plugin!("stt-moonshine", Stt, voice_pipes_stt_moonshine);
    plugin!("stt-google", Stt, voice_pipes_stt_google);
    plugin!("stt-nemotron", Stt, voice_pipes_stt_nemotron);
    plugin!("stt-nemotron35", Stt, voice_pipes_stt_nemotron35);
    plugin!("stt-assemblyai", Stt, voice_pipes_stt_assemblyai);
    plugin!("tts-piper", Tts, voice_pipes_tts_piper);
    plugin!("tts-openai", Tts, voice_pipes_tts_openai);
    plugin!("tts-kokoro", Tts, voice_pipes_tts_kokoro);
    plugin!("tts-pocket", Tts, voice_pipes_tts_pocket);
    plugin!("tts-gemini", Tts, voice_pipes_tts_gemini);
    plugin!("tts-elevenlabs", Tts, voice_pipes_tts_elevenlabs);
    plugin!("vad-silero", Vad, voice_pipes_vad_silero);
    plugin!("llm-gguf", Text, voice_pipes_llm_gguf);
    plugin!("llm-openai-compat", Text, voice_pipes_llm_openai_compat);
    plugin!("s2s-gemini", Realtime, voice_pipes_s2s_gemini);
    plugin!("s2s-openai", Realtime, voice_pipes_s2s_openai);
    plugin!("s2s-elevenlabs", Realtime, voice_pipes_s2s_elevenlabs);
    plugin!("turn-smart", Turn, voice_pipes_turn_smart);
    plugin!("turn-vap", Turn, voice_pipes_turn_vap, no wizard entries);
    // What a pipeline without `[stt]` or `[tts]` runs: the recogniser and
    // voice this binary ships as its defaults, on the machine, needing no
    // key.
    #[cfg(feature = "stt-parakeet")]
    voice_pipes::plugins::register_fallback(
        voice_pipes::plugins::Stage::Stt,
        voice_pipes_stt_parakeet::factory().name(),
    )?;
    #[cfg(feature = "tts-piper")]
    voice_pipes::plugins::register_fallback(
        voice_pipes::plugins::Stage::Tts,
        voice_pipes_tts_piper::factory().name(),
    )?;
    for (stage, hint) in BUILD_HINTS {
        voice_pipes::plugins::register_build_hint(*stage, *hint);
    }
    // What `[vad] mode = "auto"` runs when its model is on disk.
    #[cfg(feature = "vad-silero")]
    voice_pipes::vad::segmenter::register_auto_backend(voice_pipes_vad_silero::factory().name())?;
    Ok(())
}

#[tokio::main]
async fn main() {
    // Parsed first, so `--help` and `--version` answer whatever the plugins do.
    let cli = Cli::parse();
    // One TLS stack for the whole process, chosen before anything connects.
    voice_pipes_net::install_crypto_provider();
    if let Err(e) = register_plugins() {
        eprintln!("error: {e}");
        std::process::exit(2);
    }

    let json_logs = match cli.log_format {
        Some(format) => format == LogFormat::Json,
        None => config::log_format_hint(cli.config.as_deref()).as_deref() == Some("json"),
    };
    #[cfg_attr(
        not(feature = "telemetry"),
        allow(
            clippy::let_unit_value,
            reason = "without OpenTelemetry export the provider handle is the unit type"
        )
    )]
    let otel_provider = telemetry::init(json_logs);

    let result = run(cli).await;
    if let Err(e) = &result {
        // The whole chain: the outermost context alone rarely says why.
        error!("{e:#}");
    }
    // Spans buffered for export go out before the process ends, also on
    // the error path.
    telemetry::shutdown(otel_provider);
    if result.is_err() {
        std::process::exit(1);
    }
}

/// Validate a merged [`PipelineConfig`] and convert any validation errors
/// into a structured [`anyhow::Error`].  This lets every CLI subcommand
/// fail through the single `Err` path in [`main`] instead of calling
/// `std::process::exit(1)` inline.
fn require_valid_config(cfg: &PipelineConfig) -> Result<(), anyhow::Error> {
    let errors = config::validate(cfg);
    if errors.is_empty() {
        return Ok(());
    }
    let joined = errors
        .iter()
        .map(|e| format!("  - {e}"))
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::bail!("config validation failed:\n{joined}")
}

async fn run(cli: Cli) -> Result<(), anyhow::Error> {
    // The pipeline config (explicit or auto-discovered) is read only by the
    // commands that run a pipeline: one that does not parse must not break
    // `inspect`, or `validate-config` on another file.
    let config_path = cli.config.clone();
    let pipeline_config =
        |overrides: config::CliOverrides| -> Result<PipelineConfig, anyhow::Error> {
            let cfg = config::merge(config::load(config_path.as_deref())?, overrides);
            require_valid_config(&cfg)?;
            Ok(cfg)
        };

    // Build CLI overrides -- only populate fields the user actually passed on
    // the command line. For pipe/run we'll add stt/tts/output-rate below.
    let mut cli_overrides = config::CliOverrides {
        log_format: cli.log_format.map(|f| f.as_str().to_string()),
        captures_dir: cli.captures_dir.clone(),
        audit_tap: cli.audit_tap.clone(),
        audit_tap_dir: None, // only override if user explicitly passed it
        output_rate: None,
    };

    // If the user changed audit_tap_dir from the default, treat it as an override
    if cli.audit_tap_dir != *"audit_tap" {
        cli_overrides.audit_tap_dir = Some(cli.audit_tap_dir.clone());
    }

    match cli.command {
        Command::Tts { text, output } => {
            let cfg = pipeline_config(cli_overrides)?;
            let audit_sink = build_audit_sink(&cfg);
            voice_pipes::commands::run_tts(&text, &output, &cfg, audit_sink.as_ref()).await
        }
        Command::Check { fixture } => {
            let cfg = pipeline_config(cli_overrides)?;
            voice_pipes::commands::run_check(&cfg, fixture.as_deref()).await
        }
        Command::Stt { input } => {
            let cfg = pipeline_config(cli_overrides)?;
            voice_pipes::commands::run_stt(&input, &cfg).await
        }
        Command::Pipe {
            input,
            output,
            output_rate,
        } => {
            cli_overrides.output_rate = output_rate;
            let cfg = pipeline_config(cli_overrides)?;
            let audit_sink = build_audit_sink(&cfg);
            voice_pipes::commands::run_pipe(&input, &output, &cfg, audit_sink.as_ref()).await
        }
        Command::Run { input, output } => {
            let cfg = pipeline_config(cli_overrides)?;
            let audit_sink = build_audit_sink(&cfg);
            voice_pipes::commands::run_pipe(&input, &output, &cfg, audit_sink.as_ref()).await
        }
        Command::Serve { worker_config } => {
            // The long-running path validates like every other command: a
            // worker must not come up on a config that fails the checks.
            let cfg = pipeline_config(cli_overrides)?;
            voice_pipes::commands::run_serve(cfg, worker_config).await
        }
        Command::Orchestrate {
            orchestrator_config,
        } => {
            // Composition root: attach the benchmarking host extension's routes
            // through the orchestrator's neutral `extensions` seam. Dropping the
            // `voice-pipes-bench` dependency (and this closure) yields a
            // benchmark-free build.
            voice_pipes::commands::run_orchestrate(orchestrator_config, |cfg| {
                vec![voice_pipes_bench::router(&cfg.benchmarks_dir)]
            })
            .await
        }
        #[cfg(feature = "audio-device")]
        Command::Live {
            silence_ms,
            save_input,
            save_output,
        } => {
            let cfg = pipeline_config(cli_overrides)?;
            let silence_ms = silence_ms
                .or_else(|| {
                    cfg.vad
                        .as_ref()
                        .and_then(|v| v.min_silence_ms.map(|x| x as u64))
                })
                .unwrap_or(1500);
            let audit_sink = build_audit_sink(&cfg);
            voice_pipes::commands::run_live(
                silence_ms,
                save_input,
                save_output,
                &cfg,
                audit_sink.as_ref(),
            )
            .await
        }
        #[cfg(feature = "audio-device")]
        Command::Listen {
            silence_ms,
            save_input,
        } => {
            let cfg = pipeline_config(cli_overrides)?;
            let silence_ms = silence_ms
                .or_else(|| {
                    cfg.vad
                        .as_ref()
                        .and_then(|v| v.min_silence_ms.map(|x| x as u64))
                })
                .unwrap_or(1500);
            voice_pipes::commands::run_listen(silence_ms, save_input, &cfg).await
        }
        #[cfg(all(feature = "realtime", feature = "audio-device"))]
        Command::Interactive {
            provider,
            silence_ms,
            echo_gating,
        } => {
            let cfg = pipeline_config(cli_overrides)?;
            let provider = provider
                .or_else(|| cfg.realtime_provider.as_ref().map(|rt| rt.provider.clone()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "speech-to-speech provider must be specified via `--provider <name>` or `[realtime_provider].provider` in the configuration file"
                    )
                })?;
            let silence_ms = silence_ms
                .or_else(|| {
                    cfg.vad
                        .as_ref()
                        .and_then(|v| v.min_silence_ms.map(|x| x as u64))
                })
                .unwrap_or(1500);
            voice_pipes::commands::run_interactive(provider, silence_ms, echo_gating, &cfg).await
        }
        Command::Text { input } => {
            let cfg = pipeline_config(cli_overrides)?;
            voice_pipes::commands::run_text(input, &cfg).await
        }
        Command::Inspect {
            kind,
            provider,
            metrics,
            format,
        } => {
            // Inspect doesn't need a config file -- it operates on the
            // static factory registries. Run synchronously inside the
            // tokio runtime.
            let effective_kind = if metrics {
                voice_pipes::commands::InspectKind::Metrics
            } else {
                kind.into()
            };
            let format = match format {
                OutputFormat::Text => voice_pipes::commands::InspectFormat::Text,
                OutputFormat::Json => voice_pipes::commands::InspectFormat::Json,
            };
            voice_pipes::commands::run_inspect(effective_kind, provider, format)
        }
        Command::ValidateConfig { path, format } => {
            // Standalone validation: ignore the global --config flag and
            // operate on the explicit path the user passed, so CI can
            // validate arbitrary files.
            let format = match format {
                OutputFormat::Text => voice_pipes::commands::ValidateConfigFormat::Text,
                OutputFormat::Json => voice_pipes::commands::ValidateConfigFormat::Json,
            };
            voice_pipes::commands::run_validate_config(&path, format)
        }
        Command::Healthz { url, timeout_s } => {
            voice_pipes::commands::run_healthz(&url, std::time::Duration::from_secs(timeout_s))
                .await
        }
        #[cfg(feature = "wizard")]
        Command::ConfigWizard => voice_pipes::commands::run_config_wizard().await,
        #[cfg(feature = "wizard")]
        Command::Models { action } => match action {
            ModelsAction::Fetch { names } => voice_pipes::commands::run_models_fetch(&names).await,
            ModelsAction::List => voice_pipes::commands::run_models_list(),
        },
    }
}

fn build_audit_sink(cfg: &PipelineConfig) -> Option<AuditSink> {
    cfg.pipeline.audit.enabled.then(|| {
        let dir = cfg
            .pipeline
            .audit
            .captures_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(config::DEFAULT_CAPTURES_DIR));
        let store: std::sync::Arc<dyn voice_pipes::audit::store::AuditStore> =
            std::sync::Arc::new(voice_pipes::audit::store::LocalFsAuditStore::new(dir));
        AuditSink::spawn(store, cfg.pipeline.audit.buffer_size)
    })
}

#[cfg(test)]
mod plugin_tests {
    /// Every shipped example and pipeline config resolves with the plugins
    /// linked: its providers (speech-to-speech ones too) and turn mode are
    /// registered and its plugin settings are ones the plugin reads. The
    /// engine's own tests cannot check these (`mode = "smart"` is a plugin
    /// now). Each file is read as the schema its name says it holds, and
    /// one that does not parse fails; missing keys and model files are
    /// ignored. Run with the images' features, as CI's image-features job
    /// does.
    #[cfg(all(
        feature = "turn-smart",
        feature = "stt-nemotron",
        feature = "stt-assemblyai",
        feature = "tts-kokoro",
        feature = "tts-openai",
        feature = "tts-pocket",
        feature = "llm-openai-compat",
        feature = "llm-gguf",
        feature = "s2s-gemini",
        feature = "s2s-openai",
        feature = "s2s-elevenlabs",
    ))]
    #[test]
    fn shipped_configs_resolve_with_their_plugins() {
        fn tomls(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("readable dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    tomls(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                    out.push(path);
                }
            }
        }
        super::register_plugins().expect("the linked plugins register");
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../voice-pipes"));
        let mut files = Vec::new();
        tomls(&root.join("examples"), &mut files);
        tomls(&root.join("config"), &mut files);
        // The compose stack's own pipelines: a worker image refuses to start
        // on one that fails validation, in a restart loop.
        tomls(&root.join("deploy/docker/config"), &mut files);

        let mut checked = Vec::new();
        let mut problems = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path).expect("read config");
            let name = path
                .strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string();
            let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            let dir = path
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|d| d.to_str())
                .unwrap_or("");
            // Orchestrator and worker-process configs hold their own
            // schemas: the orchestrator's file, the fleet workers' in
            // `config/` and `examples/orchestrate/` (a speech-to-speech
            // worker's file there is a pipeline), and `*.worker.toml`.
            // The gRPC examples' `worker-*` files are component-server
            // pipelines.
            let worker = file.ends_with(".worker.toml")
                || (matches!(dir, "config" | "orchestrate")
                    && file.starts_with("worker")
                    && !file.starts_with("worker-realtime-"));
            let parsed = if file == "orchestrator.toml" {
                toml::from_str::<voice_pipes::api::types::OrchestratorConfig>(&text).map(|_| None)
            } else if worker {
                toml::from_str::<voice_pipes::api::types::WorkerConfig>(&text).map(|_| None)
            } else {
                toml::from_str::<voice_pipes::config::PipelineConfig>(&text).map(Some)
            };
            let cfg = match parsed {
                Ok(Some(cfg)) => cfg,
                Ok(None) => continue,
                Err(e) => {
                    problems.push(format!("{name}: does not parse: {e}"));
                    continue;
                }
            };
            let provider = |p: Option<&str>| p == Some("grpc");
            for e in voice_pipes::config::validate(&cfg) {
                use voice_pipes::config::ConfigErrorKind;
                let unresolved = matches!(
                    e.kind,
                    ConfigErrorKind::UnknownName | ConfigErrorKind::UnknownKey
                );
                // The gRPC client providers are engine features the images
                // do not build; a config naming one needs that feature.
                let grpc_not_built = (e.field == "stt.provider"
                    && provider(cfg.stt.as_ref().map(|s| s.provider.as_str()))
                    && !cfg!(feature = "stt-grpc"))
                    || (e.field == "tts.provider"
                        && provider(cfg.tts.as_ref().map(|t| t.provider.as_str()))
                        && !cfg!(feature = "tts-grpc"));
                if unresolved && !grpc_not_built {
                    problems.push(format!("{name}: {}: {}", e.field, e.message));
                }
            }
            checked.push((name, cfg));
        }
        assert!(problems.is_empty(), "{problems:#?}");
        assert!(
            checked.len() >= 10,
            "only {} configs parsed as pipelines",
            checked.len()
        );
        assert!(
            checked
                .iter()
                .any(|(_, c)| c.turn_detection.as_ref().is_some_and(|t| t.mode == "smart")),
            "no shipped config uses the Smart Turn plugin, so this checked none"
        );
        assert!(
            checked
                .iter()
                .filter(|(_, c)| c.realtime_provider.is_some())
                .count()
                >= 3,
            "the speech-to-speech configs were not checked"
        );
    }

    /// Every choice a plugin offers in the setup wizard names a feature this
    /// crate declares and every worker image is built with. The engine's
    /// wizard tests hold the built-in choices to the same rule but cannot see
    /// a plugin; this crate links them. Run with the images' features (CI's
    /// `image-features` job does) so the plugins are compiled in.
    #[test]
    fn every_plugin_wizard_choice_names_a_feature_the_images_build() {
        super::register_plugins().expect("the linked plugins register");
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let compose = std::fs::read_to_string(format!(
            "{root}/voice-pipes/deploy/docker/docker-compose.yml"
        ))
        .expect("docker-compose.yml");
        let manifest = std::fs::read_to_string(format!("{root}/voice-pipes-cli/Cargo.toml"))
            .expect("CLI Cargo.toml");
        let defaults = manifest
            .lines()
            .find_map(|l| l.strip_prefix("default = ["))
            .expect("CLI default features")
            .split(',')
            .map(|f| f.trim().trim_matches(|c| c == '"' || c == ']'))
            .collect::<Vec<_>>();
        // The workers' list, defined once (`- &name FEATURES=...`), and the
        // other images' lists. The CUDA image builds the workers' list with
        // the GPU switches on top, so a feature in it is in both images.
        let images: Vec<Vec<&str>> = compose
            .lines()
            .filter_map(|l| {
                let item = l.trim().strip_prefix("- ")?;
                let item = item
                    .strip_prefix('&')
                    .and_then(|a| a.split_once(' ').map(|(_, rest)| rest))
                    .unwrap_or(item);
                item.strip_prefix("FEATURES=")
            })
            .map(|l| l.split(',').map(str::trim).collect::<Vec<_>>())
            .collect();

        use voice_pipes::plugins::{Stage, wizard_choices};
        let choices = Stage::ALL
            .into_iter()
            .flat_map(wizard_choices)
            .map(|(_, choice)| choice)
            .collect::<Vec<_>>();
        for choice in &choices {
            let Some(feature) = choice.feature.as_deref() else {
                continue;
            };
            assert!(
                manifest.contains(&format!("\n{feature} = [")),
                "{:?}: feature `{feature}` is not declared in voice-pipes-cli/Cargo.toml",
                choice.label
            );
            // Every image builds this crate's default features on top of its
            // FEATURES, so a default one is in all of them.
            if defaults.contains(&feature) {
                continue;
            }
            // Some images build only what a speech-to-speech worker needs,
            // so, as the engine's own coverage test does, the feature must be
            // in one image's list, not in every one.
            assert!(
                images.iter().any(|list| list.contains(&feature)),
                "{:?}: no worker image's FEATURES has `{feature}`",
                choice.label
            );
        }
        #[cfg(feature = "stt-nemotron35")]
        assert!(
            choices
                .iter()
                .any(|c| c.feature.as_deref() == Some("stt-nemotron35")),
            "the Nemotron 3.5 plugin is compiled in but offers no wizard choice"
        );
        #[cfg(feature = "tts-kokoro")]
        assert!(
            choices
                .iter()
                .any(|c| c.feature.as_deref() == Some("tts-kokoro")),
            "the Kokoro plugin is compiled in but offers no wizard choice"
        );
    }

    /// Every sidecar a plugin's wizard entry declares exists in the compose
    /// file under the declared profile, and the variable that hands it the
    /// model is consumed by both launcher scripts, so the wizard never
    /// declares something nothing starts. The engine checks its own manifest
    /// keys; only this crate sees the plugins' sidecars.
    #[test]
    fn every_plugin_sidecar_is_in_compose_and_the_launchers() {
        use voice_pipes_sdk::WizardDeploy;
        super::register_plugins().expect("the linked plugins register");
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let read = |rel: &str| {
            std::fs::read_to_string(format!("{root}/{rel}"))
                .unwrap_or_else(|e| panic!("{rel}: {e}"))
        };
        let compose = read("voice-pipes/deploy/docker/docker-compose.yml");
        let launchers = [read("launch-stack.ps1"), read("launch-stack.sh")];

        use voice_pipes::plugins::{Stage, wizard_choices};
        for (_, choice) in Stage::ALL.into_iter().flat_map(wizard_choices) {
            let (profile, service, model_env) = match &choice.deploy {
                WizardDeploy::DockerSidecar {
                    profile,
                    service,
                    model_env,
                    ..
                } => (profile, service, model_env.clone()),
                WizardDeploy::DockerSidecarModel {
                    profile,
                    service,
                    model_env,
                    ..
                } => (profile, service, Some(model_env.clone())),
                _ => continue,
            };
            assert!(
                compose.contains(&format!("{service}:")),
                "{:?}: compose service {service:?} not found",
                choice.label
            );
            assert!(
                compose.contains(&format!("profiles: [\"{profile}\"]")),
                "{:?}: compose profile {profile:?} not found",
                choice.label
            );
            if let Some(env) = model_env {
                for launcher in &launchers {
                    assert!(
                        launcher.contains(&env),
                        "{:?}: a launcher does not consume {env}",
                        choice.label
                    );
                }
            }
        }
    }

    /// Each stage's menu opens with its out-of-the-box entry, with every
    /// plugin this binary links in the menu: the built-in voice and turn
    /// mode, and Parakeet for recognition, ahead of every plugin.
    #[cfg(feature = "wizard")]
    #[test]
    fn each_wizard_menu_opens_with_its_out_of_the_box_entry() {
        use voice_pipes::commands::config_wizard::menu;
        use voice_pipes::plugins::Stage;
        super::register_plugins().expect("the linked plugins register");
        let first = |stage| menu(stage).first().map(|(provider, _)| provider.clone());
        #[cfg(feature = "stt-parakeet")]
        assert_eq!(first(Stage::Stt).as_deref(), Some("parakeet-tdt"));
        #[cfg(feature = "tts-piper")]
        assert_eq!(first(Stage::Tts).as_deref(), Some("piper"));
        assert_eq!(first(Stage::Turn).as_deref(), Some("layered"));
    }

    /// A pipeline without `[stt]` or `[tts]` runs Parakeet and Piper, which
    /// this binary registers as the fallbacks, whatever else it links
    /// (`AssemblyAI`'s wizard entry claims the preselection, not the
    /// fallback).
    #[cfg(all(feature = "stt-parakeet", feature = "tts-piper"))]
    #[test]
    fn a_pipeline_without_stt_or_tts_runs_parakeet_and_piper() {
        super::register_plugins().expect("the linked plugins register");
        let bare: voice_pipes::config::PipelineConfig =
            toml::from_str("[pipeline]\nname = \"bare\"\n").expect("parses");
        assert_eq!(
            voice_pipes::config::stt_provider_name(&bare).as_deref(),
            Some("parakeet-tdt")
        );
        assert_eq!(
            voice_pipes::config::tts_provider_name(&bare).as_deref(),
            Some("piper")
        );
    }

    /// Every factory this binary registers, plugin and built-in, refuses an
    /// option it does not read at build as at validate, as a config refusal
    /// naming the same fields: a config that does not validate never starts
    /// a pipeline, and a refused build says why as validation does. The
    /// option needs nothing on disk or on the network to refuse, so a build
    /// that fails on a missing model or key instead has skipped a check.
    #[test]
    fn every_factory_builds_only_what_validates() {
        use voice_pipes_sdk::testing::build_refuses_what_validate_refuses;
        use voice_pipes_sdk::{
            BuildContext, SttConfig, TextProcessorConfig, TtsConfig, VadConfig, turn::TurnConfig,
        };
        super::register_plugins().expect("the linked plugins register");
        let unread = || {
            std::collections::HashMap::from([(
                "an_option_no_factory_reads".to_string(),
                serde_json::json!(1),
            )])
        };
        let mut failures = Vec::new();
        let mut check = |what: String, run: &dyn Fn()| {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
                let why = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_default();
                failures.push(format!("{what}: {why}"));
            }
        };
        let ctx = BuildContext::new();
        for f in voice_pipes::stt::stt_factories() {
            let cfg = SttConfig::new(f.name()).with_options(unread());
            check(format!("STT {}", f.name()), &|| {
                build_refuses_what_validate_refuses(
                    std::slice::from_ref(&cfg),
                    |c| f.validate(c),
                    |c| f.build(c, &ctx),
                );
            });
        }
        for f in voice_pipes::tts::tts_factories() {
            let cfg = TtsConfig::new(f.name()).with_options(unread());
            check(format!("TTS {}", f.name()), &|| {
                build_refuses_what_validate_refuses(
                    std::slice::from_ref(&cfg),
                    |c| f.validate(c),
                    |c| f.build(c, &ctx),
                );
            });
        }
        for f in voice_pipes::text::text_processor_factories() {
            let cfg = TextProcessorConfig::new(f.name()).with_options(unread());
            check(format!("text processor {}", f.name()), &|| {
                build_refuses_what_validate_refuses(
                    std::slice::from_ref(&cfg),
                    |c| f.validate(c),
                    |c| f.build(c, &ctx),
                );
            });
        }
        for f in voice_pipes::vad::segmenter::vad_factories() {
            let cfg = VadConfig::new(f.name()).with_options(unread());
            check(format!("VAD {}", f.name()), &|| {
                build_refuses_what_validate_refuses(
                    std::slice::from_ref(&cfg),
                    |c| f.validate(c),
                    |c| f.build(c, &ctx),
                );
            });
        }
        for f in voice_pipes::stt::turn::turn_detector_factories() {
            let cfg = TurnConfig::new(f.name(), 700).with_options(unread());
            check(format!("turn detector {}", f.name()), &|| {
                build_refuses_what_validate_refuses(
                    std::slice::from_ref(&cfg),
                    |c| f.validate(c),
                    |c| f.build(c, &ctx),
                );
            });
        }
        #[cfg(feature = "realtime")]
        for f in voice_pipes::realtime::realtime_factories() {
            let cfg = voice_pipes::realtime::RealtimeConfig::new(f.name()).with_options(unread());
            check(format!("speech-to-speech {}", f.name()), &|| {
                build_refuses_what_validate_refuses(
                    std::slice::from_ref(&cfg),
                    |c| f.validate(c),
                    |c| f.build(c, &ctx),
                );
            });
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Each feature the wizard's build hints name is a feature of this crate,
    /// and every stage has a hint.
    #[test]
    fn the_build_hints_name_this_crates_features() {
        let manifest: toml::Table = toml::from_str(include_str!("../Cargo.toml")).expect("parses");
        let features = manifest["features"].as_table().expect("a features table");
        for (stage, hint) in super::BUILD_HINTS {
            for name in hint
                .split([',', ' '])
                .filter(|w| !w.is_empty() && *w != "or")
            {
                assert!(
                    features.contains_key(name),
                    "{stage:?}: no feature {name:?}"
                );
            }
        }
        for stage in voice_pipes::plugins::Stage::ALL {
            assert!(
                super::BUILD_HINTS.iter().any(|(s, _)| *s == stage),
                "{stage:?} has no build hint"
            );
        }
    }

    /// `[vad] mode = "auto"` runs Silero, which this binary names auto's
    /// backend, once its model is on disk; the wizard writes Silero's model
    /// under `[vad]` and fetches it.
    #[cfg(feature = "vad-silero")]
    #[test]
    fn auto_vad_is_silero() {
        super::register_plugins().expect("the linked plugins register");
        assert_eq!(
            voice_pipes::vad::segmenter::auto_backend().as_deref(),
            Some("silero")
        );
        let entry = voice_pipes::plugins::vad_wizard_entry("silero").expect("Silero's settings");
        assert!(matches!(
            entry.deploy,
            voice_pipes_sdk::WizardDeploy::Registry { ref key } if key == "silero"
        ));
    }
}
