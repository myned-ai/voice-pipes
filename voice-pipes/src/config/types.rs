//! Configuration structs, defaults, and error types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audit::TapSide;

// ── Top-level config ────────────────────────────────────────

/// Top-level pipeline configuration, parsed from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    #[serde(default)]
    pub pipeline: PipelineSettings,
    #[serde(default)]
    pub speculative: SpeculativeConfig,
    pub stt: Option<SttConfig>,
    pub tts: Option<TtsConfig>,
    pub output: Option<OutputConfig>,
    pub vad: Option<VadSettings>,
    pub turn_detection: Option<TurnDetectionConfig>,
    /// `[grading]` — how this pipeline's sessions are graded. Optional, and
    /// for operators who have decided a default target does not fit them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grading: Option<GradingConfig>,
    pub barge_in: Option<BargeInConfig>,
    /// `[adaptive_quality]` — network-aware degradation of what the session
    /// sends. Absent
    /// section behaves exactly like `enabled = false`: the controller still
    /// publishes diagnostics but the tier never leaves `Full`.
    #[serde(default)]
    pub adaptive_quality: AdaptiveQualityConfig,
    pub transport: Option<TransportConfig>,
    #[serde(default)]
    pub text_processor: Vec<TextProcessorConfig>,
    #[serde(default)]
    pub audio_filter: Vec<FilterStageSettings>,
    #[serde(default)]
    pub audio_post_processor: Vec<AudioPostProcessorConfig>,
    /// Optional Speech-to-Speech (Realtime) provider. When set, the
    /// runner bypasses the STT → LLM → TTS chain and routes audio
    /// through a single audio-in/audio-out remote model (`OpenAI`
    /// Realtime, Gemini Live, …).
    pub realtime_provider: Option<RealtimeProviderConfig>,
    /// Runtime auth for the surfaces this process serves
    /// (WS transport, debug API, gRPC components) and the internal
    /// credential it presents to the orchestrator. Absent section =
    /// env-var defaults (`VP_CLIENT_TOKEN` / `VP_INTERNAL_TOKEN`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<crate::api::auth::AuthSettings>,
    /// Agentic-system integration: idle re-engagement and, later, the gRPC agent link.
    /// Absent section = no idle prompts, no external agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentSettings>,
    /// Inline client-side tool declarations (`[[client_tool]]`) —
    /// server-declared commands the driving LLM can invoke that are
    /// fulfilled by the *client*. Merged
    /// with `client_tools_path` catalogs and per-session
    /// `Configure.options.client_tools`; later sources win on name
    /// collision.
    #[serde(default, rename = "client_tool", skip_serializing_if = "Vec::is_empty")]
    pub client_tool: Vec<crate::agent::client_tools::ClientToolConfig>,
    /// External client-tool catalog file(s) — JSON (an array of tool
    /// definitions) or TOML (`[[client_tool]]` tables). A single path
    /// or a list, resolved relative to this config file's directory.
    /// A missing or invalid catalog is a config-validation error
    /// (fail fast); catalogs are read once at worker boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_tools_path: Option<ClientToolsPaths>,
}

/// `client_tools_path` — one path or a list of paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClientToolsPaths {
    One(PathBuf),
    Many(Vec<PathBuf>),
}

impl ClientToolsPaths {
    /// The declared paths, in order.
    pub fn paths(&self) -> &[PathBuf] {
        match self {
            Self::One(p) => std::slice::from_ref(p),
            Self::Many(v) => v.as_slice(),
        }
    }

    /// Mutable access for load-time path absolutisation.
    pub fn paths_mut(&mut self) -> &mut [PathBuf] {
        match self {
            Self::One(p) => std::slice::from_mut(p),
            Self::Many(v) => v.as_mut_slice(),
        }
    }
}

/// `[agent]` — agentic-system integration knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSettings {
    /// gRPC endpoint of the remote agent service. On S2S sessions this
    /// wires the agent in as the **tool backend** (the provider model
    /// stays the brain): its `ListTools` populate the provider session
    /// and `FunctionCall`s are forwarded over the link. Classic
    /// sessions instead mount the agent as the brain via
    /// `[[text_processor]] type = "agent-grpc"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Silence duration (while Listening with no active turn) before an
    /// idle re-engagement prompt fires. `None`/absent disables idle
    /// prompts. Industry defaults sit at 7.5–15 s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_ms: Option<u64>,
    /// Maximum idle prompts per silence stretch; the counter resets when
    /// the user speaks. Default 2. (Every surveyed platform caps this —
    /// Vapi default 3, Retell default 1.)
    #[serde(default = "default_idle_max_prompts")]
    pub idle_max_prompts: u32,
    /// Static prompt spoken when idle fires and no agent is bound to
    /// decide the wording itself.
    #[serde(default = "default_idle_message")]
    pub idle_message: String,
}

fn default_idle_max_prompts() -> u32 {
    2
}

fn default_idle_message() -> String {
    "Are you still there?".to_string()
}

/// Speculative execution configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeculativeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub speculative_tts: bool,
    #[serde(default = "default_min_speculative_partial_interval_ms")]
    pub min_partial_interval_ms: u64,
}

impl Default for SpeculativeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            speculative_tts: false,
            min_partial_interval_ms: default_min_speculative_partial_interval_ms(),
        }
    }
}

fn default_min_speculative_partial_interval_ms() -> u64 {
    150
}

/// Configuration for an ingress audio filter stage.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FilterStageSettings {
    /// Provider name: e.g. "deepfilter".
    pub provider: String,
    /// Path to model file or directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<PathBuf>,
    /// Filter-specific options from the table.
    #[serde(default, flatten)]
    pub options: HashMap<String, serde_json::Value>,
}

/// Configuration for a single audio post-processor in the chain.
///
/// Same TOML shape as [`TextProcessorConfig`]: a flat struct with `type`
/// as the discriminator and remaining fields captured into a generic
/// `options` map.  Each factory in
/// [`crate::pipeline::post_audio::audio_post_processor_factories`] extracts
/// the fields it needs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioPostProcessorConfig {
    /// Processor type: e.g. "noop", "level", "grpc".
    #[serde(rename = "type")]
    pub processor_type: String,
    /// `inline` (default): awaited per chunk before the chunk is forwarded
    /// -- for processors that mutate samples. `annotate`: run off the audio
    /// path by an [`AnnotatorHub`](crate::pipeline::annotate::AnnotatorHubs)
    /// -- for processors that derive metadata (blendshapes, visemes, VU).
    #[serde(default)]
    pub mode: PostProcessorMode,
    /// Annotate only: coalesce audio into segments of this many ms (160).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_ms: Option<u64>,
    /// Annotate only: the first segment of an utterance is released after
    /// this many ms (80) so annotations exist before the client's prebuffer
    /// elapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_batch_ms: Option<u64>,
    /// Annotate only: segments outstanding towards the annotator (2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight: Option<u64>,
    /// Per segment (annotate, default 250) / per chunk (inline, default 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Annotate only: send a synthetic warm-up segment at session start (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup: Option<bool>,
    /// Annotate only: `[audio_post_processor.circuit_breaker]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_breaker: Option<CircuitBreakerConfig>,
    /// All other fields -- each factory interprets its own subset.
    #[serde(flatten)]
    pub options: std::collections::HashMap<String, serde_json::Value>,
}

impl AudioPostProcessorConfig {
    /// Hub tuning for `mode = "annotate"`, with the plan's defaults filled in.
    pub fn annotator_config(&self) -> AnnotatorConfig {
        let d = AnnotatorConfig::default();
        let breaker = self.circuit_breaker.unwrap_or_default();
        AnnotatorConfig {
            batch_ms: self.batch_ms.unwrap_or(d.batch_ms).max(1),
            first_batch_ms: self.first_batch_ms.unwrap_or(d.first_batch_ms).max(1),
            max_in_flight: usize::try_from(self.max_in_flight.unwrap_or(d.max_in_flight as u64))
                .unwrap_or(d.max_in_flight)
                .max(1),
            timeout_ms: self.timeout_ms.unwrap_or(d.timeout_ms).max(1),
            warmup: self.warmup.unwrap_or(d.warmup),
            breaker_failures: breaker.failures.max(1),
            breaker_cooldown_ms: breaker.cooldown_ms,
        }
    }
}

/// Where an `[[audio_post_processor]]` runs relative to the audio path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PostProcessorMode {
    /// Awaited per chunk, before the chunk is forwarded (today's behaviour).
    #[default]
    Inline,
    /// Off the audio path: teed, batched, async; results arrive as
    /// `audio_annotation` frames.
    Annotate,
}

/// Circuit-breaker settings for an annotate-mode processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerConfig {
    /// Consecutive failed segments (timeout / send failure) that open the breaker.
    #[serde(default = "default_breaker_failures")]
    pub failures: u32,
    /// How long the annotator is skipped before a probe segment is tried.
    #[serde(default = "default_breaker_cooldown_ms")]
    pub cooldown_ms: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failures: default_breaker_failures(),
            cooldown_ms: default_breaker_cooldown_ms(),
        }
    }
}

fn default_breaker_failures() -> u32 {
    3
}

fn default_breaker_cooldown_ms() -> u64 {
    5000
}

/// Resolved hub tuning for one annotate-mode processor (defaults applied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotatorConfig {
    pub batch_ms: u64,
    pub first_batch_ms: u64,
    pub max_in_flight: usize,
    pub timeout_ms: u64,
    pub warmup: bool,
    pub breaker_failures: u32,
    pub breaker_cooldown_ms: u64,
}

impl Default for AnnotatorConfig {
    fn default() -> Self {
        Self {
            batch_ms: 160,
            first_batch_ms: 80,
            max_in_flight: 2,
            timeout_ms: 250,
            warmup: true,
            breaker_failures: default_breaker_failures(),
            breaker_cooldown_ms: default_breaker_cooldown_ms(),
        }
    }
}

// Re-exported from voice-pipes-sdk — the canonical definition lives there
// so external plugin crates can use it without depending on the binary.
pub use voice_pipes_sdk::TextProcessorConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineSettings {
    pub name: Option<String>,
    #[serde(default = "default_log_format")]
    pub log_format: String,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub audit_tap: AuditTapConfig,
    /// Minimum number of Unicode characters in a committed utterance.
    /// Shorter transcriptions are discarded as noise (default: 2).
    #[serde(default = "default_min_utterance_chars")]
    pub min_utterance_chars: usize,
    /// How often an idle worker runs its recogniser once, in seconds, so
    /// the model stays resident: left alone on a host that is short of
    /// memory, its pages are evicted and the first utterance after the
    /// quiet spell waits for them longer than the pipeline waits for a
    /// final. Only recognisers warmed once per process take part; 0
    /// disables it (default: 300).
    #[serde(default = "default_keep_warm_interval_s")]
    pub keep_warm_interval_s: u64,
    #[serde(default)]
    pub text_chunking: TextChunkingConfig,
}

// Re-exported from voice-pipes-sdk — the canonical definition lives there
// so external plugin crates can use it without depending on the binary.
pub use voice_pipes_sdk::TextChunkingConfig;

fn default_min_utterance_chars() -> usize {
    2
}

fn default_keep_warm_interval_s() -> u64 {
    300
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self {
            name: None,
            log_format: default_log_format(),
            audit: AuditConfig::default(),
            audit_tap: AuditTapConfig::default(),
            min_utterance_chars: default_min_utterance_chars(),
            keep_warm_interval_s: default_keep_warm_interval_s(),
            text_chunking: TextChunkingConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    #[serde(default)]
    pub enabled: bool,
    pub captures_dir: Option<PathBuf>,
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
    /// Session capture directories whose newest file is older than
    /// this many days are deleted when a worker starts. Captures hold raw
    /// audio and transcripts of both sides, so unbounded retention is a
    /// privacy liability by default. `0` keeps everything forever.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    /// Disk budget for one session's capture. Once the session has
    /// written this many bytes, audio artefacts are dropped (with one
    /// warning) while transcripts and events keep being recorded. `0`
    /// removes the budget.
    #[serde(default = "default_max_session_bytes")]
    pub max_session_bytes: u64,
    /// Record one continuous stereo WAV per session (channel 0 the user,
    /// channel 1 the agent) alongside the per-utterance audio. Off by
    /// default: it records continuously, including silence and including
    /// audio no turn ever committed.
    ///
    /// The per-utterance files are post-VAD and cut into utterances, so the
    /// silence between turns and the relative timing of the two speakers are
    /// not recoverable from them. Turn-taking models and any evaluation of
    /// interruption handling need exactly that structure.
    #[serde(default)]
    pub record_session_audio: bool,
    /// Cap on one session's continuous recording, in seconds. Past it audio
    /// is dropped and the recording is marked truncated. At 16 kHz stereo a
    /// minute costs about 3.8 MB.
    #[serde(default = "default_session_audio_max_seconds")]
    pub session_audio_max_seconds: u32,
    /// Record every prompt in full in the trace log. Off by default:
    /// the `llm_prompt` trace event carries the message count and a
    /// hash of the prompt, which is enough to correlate turns without
    /// duplicating the whole conversation on disk on every turn.
    #[serde(default)]
    pub full_prompts: bool,
}

fn default_retention_days() -> u32 {
    7
}

fn default_session_audio_max_seconds() -> u32 {
    900
}

fn default_max_session_bytes() -> u64 {
    512 * 1024 * 1024
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            captures_dir: None,
            buffer_size: default_buffer_size(),
            retention_days: default_retention_days(),
            max_session_bytes: default_max_session_bytes(),
            record_session_audio: false,
            session_audio_max_seconds: default_session_audio_max_seconds(),
            full_prompts: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditTapConfig {
    #[serde(default)]
    pub enabled: bool,
    pub side: Option<String>,
    #[serde(default = "default_audit_tap_dir")]
    pub dir: PathBuf,
}

impl Default for AuditTapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            side: None,
            dir: PathBuf::from("audit_tap"),
        }
    }
}

// ── SttConfig — re-exported from voice-pipes-sdk ────────────
//
// The canonical definition (including the custom Serialize/Deserialize impl)
// lives in the SDK so plugin crates can use it
// without depending on voice-pipes.
pub use voice_pipes_sdk::SttConfig;

// Re-exported from voice-pipes-sdk — the canonical definition lives there
// so external plugin crates can use it without depending on the binary.
pub use voice_pipes_sdk::TtsConfig;

/// Configuration for a Speech-to-Speech (Realtime) provider.
///
/// When present on [`PipelineConfig::realtime_provider`], the runner
/// bypasses the STT → LLM → TTS chain and instead routes audio
/// through a single audio-in / audio-out remote model (`OpenAI`
/// Realtime, Gemini Live, …).
///
/// ```toml
/// [realtime_provider]
/// provider     = "openai-realtime"     # or "gemini-live" / "elevenlabs"
/// model        = "gpt-realtime"         # provider-specific, optional
/// voice        = "alloy"                # provider-specific, optional
/// vad          = "server_vad"           # "server_vad" | "none"
/// instructions = "You are a helpful assistant."
/// api_key_env  = "OPENAI_API_KEY"       # defaults per-provider
///
/// [realtime_provider.options]
/// output_emit_rate = 16000              # the rate the model's audio enters the pipeline at
/// # agent_id = "agent_..."             # elevenlabs-only: required agent id
/// # agent_id_env = "ELEVENLABS_AGENT_ID" # elevenlabs-only: env fallback when
/// #                                     # agent_id is unset (this is the default)
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeProviderConfig {
    /// Factory name or alias (`"openai-realtime"`, `"gemini-live"`, …).
    pub provider: String,
    /// Provider-specific model identifier.
    pub model: Option<String>,
    /// Provider-specific voice / speaker ID.
    pub voice: Option<String>,
    /// Turn-detection mode hint: `"server_vad"` (provider handles VAD) or
    /// `"none"` (caller drives turn boundaries explicitly).
    pub vad: Option<String>,
    /// System / persona instructions passed at session-start time.
    pub instructions: Option<String>,
    /// Optional path to a file (UTF-8) whose contents are appended to
    /// `instructions` at session-start. Convenient for shipping a
    /// domain knowledge base without inlining a giant TOML string.
    /// Resolved relative to the worker.toml directory. Failures to
    /// read are logged once and treated as "no KB" (non-fatal).
    pub knowledge_base_path: Option<std::path::PathBuf>,
    /// Environment variable name carrying the API key. Defaults are
    /// per-provider (`OPENAI_API_KEY`, `GEMINI_API_KEY`).
    pub api_key_env: Option<String>,
    /// Provider-specific key-value options.
    ///
    /// Populated from the nested `[realtime_provider.options]` TOML sub-table.
    #[serde(default)]
    pub options: std::collections::HashMap<String, serde_json::Value>,
    /// `Configure.options` keys a connecting client may override.
    /// Anything else the client sends is dropped, logged, and echoed in
    /// `session_ready.negotiated.ignored_options`. `instructions`, when
    /// listed, is appended to the operator's prompt, never substituted.
    /// Default: the VAD tuning knobs (`vad_threshold`, `vad_silence_ms`,
    /// `vad_prefix_padding_ms`, `vad_eagerness`). Keys voice-pipes consumes
    /// itself (`client_tools`, `output_chunking_*`) are never filtered.
    #[serde(default = "crate::transport::serve::client_overrides::default_allowlist")]
    pub client_overrides: Vec<String>,
    /// vp-side end-of-speech observer on the uplink
    /// (`[realtime_provider.endpoint_observer]`).
    /// Absent = defaults (enabled, energy backend).
    #[serde(default)]
    pub endpoint_observer: Option<EndpointObserverConfig>,
}

/// The rate the model's audio enters the pipeline at when
/// `options.output_emit_rate` is not set.
pub const DEFAULT_OUTPUT_EMIT_RATE: u32 = 16_000;

impl RealtimeProviderConfig {
    /// `options.output_emit_rate`: the rate the model's audio is resampled
    /// to, as one stream, before it enters the pipeline; 16 kHz when unset.
    ///
    /// # Errors
    /// When it is set to anything but an integer from 8000 to 48000.
    pub fn output_emit_rate(&self) -> Result<u32, ConfigError> {
        let Some(v) = self.options.get("output_emit_rate") else {
            return Ok(DEFAULT_OUTPUT_EMIT_RATE);
        };
        v.as_u64()
            .filter(|n| (8_000..=48_000).contains(n))
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                ConfigError::new(
                    "realtime_provider.options.output_emit_rate",
                    "must be an integer between 8000 and 48000",
                )
            })
    }
}

/// vp's own end-of-speech observer on the realtime uplink. Measurement
/// only: it never gates, delays or interrupts audio. Gives every S2S turn
/// `provider_endpointing_ms` (how late the provider noticed the user
/// stopped) and an honest TTFB anchor when the provider offers none.
///
/// ```toml
/// [realtime_provider.endpoint_observer]
/// enabled          = true
/// backend          = "energy"     # "energy", or a model backend such as "silero" (needs `model`)
/// energy_threshold = 200.0        # RMS on the i16 scale (energy backend)
/// speech_prob      = 0.5          # silero backend
/// hangover_ms      = 300          # silence after speech before "end" is declared
/// min_speech_ms    = 150          # shorter bursts (clicks) are ignored
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointObserverConfig {
    #[serde(default = "default_endpoint_observer_enabled")]
    pub enabled: bool,
    #[serde(default = "default_endpoint_observer_backend")]
    pub backend: String,
    /// Silero ONNX model path (backend = "silero").
    pub model: Option<PathBuf>,
    #[serde(default = "default_endpoint_observer_energy_threshold")]
    pub energy_threshold: f64,
    #[serde(default = "default_endpoint_observer_speech_prob")]
    pub speech_prob: f32,
    #[serde(default = "default_endpoint_observer_hangover_ms")]
    pub hangover_ms: u64,
    #[serde(default = "default_endpoint_observer_min_speech_ms")]
    pub min_speech_ms: u64,
}

impl EndpointObserverConfig {
    /// The observer's detector settings in the shape every VAD factory
    /// takes, so any registered backend can serve it, a plugin's included.
    /// The end threshold sits a little under the start one, as the main VAD
    /// sets it.
    pub fn vad_settings(&self) -> VadSettings {
        VadSettings {
            mode: self.backend.clone(),
            model: self.model.clone(),
            energy_threshold: Some(self.energy_threshold),
            speech_prob: Some(self.speech_prob),
            threshold: None,
            neg_threshold: Some((self.speech_prob - 0.15).max(0.05)),
            min_speech_ms: None,
            min_silence_ms: None,
            pre_roll_ms: None,
            options: std::collections::HashMap::default(),
        }
    }
}

impl Default for EndpointObserverConfig {
    fn default() -> Self {
        Self {
            enabled: default_endpoint_observer_enabled(),
            backend: default_endpoint_observer_backend(),
            model: None,
            energy_threshold: default_endpoint_observer_energy_threshold(),
            speech_prob: default_endpoint_observer_speech_prob(),
            hangover_ms: default_endpoint_observer_hangover_ms(),
            min_speech_ms: default_endpoint_observer_min_speech_ms(),
        }
    }
}

fn default_endpoint_observer_enabled() -> bool {
    true
}

fn default_endpoint_observer_backend() -> String {
    "energy".to_string()
}

fn default_endpoint_observer_energy_threshold() -> f64 {
    200.0
}

fn default_endpoint_observer_speech_prob() -> f32 {
    0.5
}

fn default_endpoint_observer_hangover_ms() -> u64 {
    300
}

fn default_endpoint_observer_min_speech_ms() -> u64 {
    150
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub chunking: Option<OutputChunkingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputChunkingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_output_chunk_frame_ms")]
    pub frame_ms: u32,
}

impl Default for OutputChunkingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frame_ms: default_output_chunk_frame_ms(),
        }
    }
}

fn default_output_chunk_frame_ms() -> u32 {
    50
}

/// Voice activity detection settings.
///
/// ```toml
/// [vad]
/// mode = "auto"                       # "auto" | "energy" | "silero"
/// model = "models/silero_vad.onnx"    # path to Silero ONNX model
/// energy_threshold = 200.0            # RMS amplitude for energy mode
/// speech_prob = 0.5                   # speech probability for silero mode
/// neg_threshold = 0.35
/// min_speech_ms = 250
/// min_silence_ms = 100
/// ```
/// Target names an operator may set under `[grading.targets]`.
///
/// The session report's own list, kept here so a typo is a config error at
/// startup rather than a target that silently never applies. A test in the
/// console's backend reads this list out of the source and fails if the two
/// drift apart.
pub const GRADING_TARGET_KEYS: &[&str] = &[
    "ttft_ms",
    "ttfb_ms",
    "endpointing_ms",
    "stt_processing_ms",
    "draining_handshake_ms",
    "rtf_max",
    "tokens_per_sec_min",
    "barge_in_rtt_ms",
    "self_barge_played_ms_max",
    "outlier_total_ms_factor",
    "tool_roundtrip_ms",
    "emit_rtf_max",
    "post_budget_ratio_max",
    "annotation_lag_budget_ms",
    "unanswered_wait_ms",
];

/// `[grading]` — the operator's own rules for judging a session.
///
/// The defaults describe a good conversational agent in general. A deployment
/// may have chosen otherwise on purpose: a 700 ms silence timeout so callers
/// are never cut off mid-thought, a larger model that answers more slowly.
/// Graded against the defaults, that pipeline reports `fair` on every session
/// for ever, for a decision its owners made deliberately -- and a verdict
/// that is always amber stops being read.
///
/// What is set here is written into each session's capture, so a session is
/// graded by the rules in force when it ran, and editing this section never
/// re-grades history. A report graded by custom rules says so, naming every
/// value that differs from its default; relaxing a target changes the grade,
/// never the honesty of the report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GradingConfig {
    /// Per-metric targets, by the names in [`GRADING_TARGET_KEYS`]. Any
    /// subset; what is omitted keeps its default.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub targets: std::collections::BTreeMap<String, f64>,
    /// Bands on the response time a caller feels, which set the level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<GradingLevels>,
}

/// Bands on critical-path p90 -- the wait from the end of the caller's
/// speech to the first audio of the reply -- in ms. At or under `good` a
/// session can be `healthy`; over `poor` it is `degraded`; over `unusable`,
/// `broken`. Any subset; what is omitted keeps its default.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GradingLevels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub good: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unusable: Option<f64>,
}

/// Speech-end probability a model VAD uses when `neg_threshold` is unset.
pub const DEFAULT_VAD_NEG_THRESHOLD: f32 = 0.35;

/// Audio held back from before speech is detected, in ms.
///
/// 300 ms is the common figure across voice stacks (it is what `OpenAI`'s
/// realtime API calls `prefix_padding_ms`) and covers the gap between a word
/// starting and a detector noticing it. 100 ms is too short: it costs the
/// first syllable of every utterance.
pub const DEFAULT_VAD_PRE_ROLL_MS: u32 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VadSettings {
    /// `"auto"` -- the backend the binary names (Silero, in the voice-pipes
    /// binary) when it can run, i.e. its model is on disk; else energy.
    /// `"energy"` -- RMS energy threshold.
    /// `"passthrough"` -- every chunk is speech.
    /// Any other mode names a registered backend, such as `"silero"`.
    #[serde(default = "default_vad_mode")]
    pub mode: String,
    /// Model file, for a model-based backend such as Silero (which requires it).
    pub model: Option<PathBuf>,
    /// RMS amplitude threshold for energy VAD (default 200.0).
    /// Only used when mode = "energy".
    pub energy_threshold: Option<f64>,
    /// Speech probability threshold for a model-based backend (default 0.5,
    /// range 0.0-1.0), `auto` included when it runs one.
    pub speech_prob: Option<f32>,
    /// Deprecated: use `energy_threshold` or `speech_prob` instead.
    /// If set, acts as fallback when the mode-specific field is absent.
    pub threshold: Option<f32>,
    /// Probability below which speech is considered ended (model-based
    /// backends).
    pub neg_threshold: Option<f32>,
    /// Minimum speech duration in ms.
    pub min_speech_ms: Option<u32>,
    /// Minimum silence duration in ms.
    pub min_silence_ms: Option<u32>,
    /// Audio kept from before speech was detected, in ms (default 300).
    ///
    /// A detector recognises a word a little after it starts: the onset of
    /// "hello" is quiet and unvoiced, and by the time the probability crosses
    /// the threshold the first syllable is already past. Whatever is not held
    /// back is gone, and the recogniser is handed a word with its beginning
    /// missing -- "hello, how are you" arriving as "o, how are you", which
    /// reads as a recogniser fault and is not one.
    ///
    /// Costs a little leading room tone on every utterance, which recognisers
    /// handle; too small a value costs syllables, which they cannot.
    pub pre_roll_ms: Option<u32>,
    /// Backend-specific settings for a plugin backend (`[vad.options]`).
    /// The built-in backends read none.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub options: std::collections::HashMap<String, serde_json::Value>,
}

impl VadSettings {
    /// The configuration a backend receives: this section with its
    /// thresholds resolved, and without the utterance timing the pipeline
    /// keeps for itself. Built-in and plugin backends read the same one.
    pub fn backend_config(&self) -> voice_pipes_sdk::VadConfig {
        voice_pipes_sdk::VadConfig::new(self.mode.clone())
            .with_model(self.model.clone())
            .with_speech_prob(self.effective_speech_prob())
            .with_neg_threshold(self.neg_threshold)
            .with_energy_threshold(self.effective_energy_threshold())
            .with_options(self.options.clone())
    }

    /// Resolve the effective energy threshold (`energy_threshold` > threshold fallback > 200.0).
    pub fn effective_energy_threshold(&self) -> f64 {
        self.energy_threshold
            .unwrap_or_else(|| self.threshold.map_or(200.0, |t| t as f64))
    }

    /// Resolve the effective speech probability threshold (`speech_prob` > threshold fallback > 0.5).
    pub fn effective_speech_prob(&self) -> f32 {
        self.speech_prob
            .unwrap_or_else(|| self.threshold.unwrap_or(0.5))
    }

    /// Resolve the effective speech-end threshold (`neg_threshold` >
    /// [`DEFAULT_VAD_NEG_THRESHOLD`]).
    pub fn effective_neg_threshold(&self) -> f32 {
        self.neg_threshold.unwrap_or(DEFAULT_VAD_NEG_THRESHOLD)
    }

    /// Resolve the effective pre-roll (`pre_roll_ms` > 300).
    pub fn effective_pre_roll_ms(&self) -> u32 {
        self.pre_roll_ms.unwrap_or(DEFAULT_VAD_PRE_ROLL_MS)
    }

    /// Returns JSON descriptions of every VAD configuration field.
    pub fn metadata_descriptions() -> serde_json::Value {
        serde_json::json!({
            "mode": "VAD backend: auto, energy, passthrough, or a registered model backend",
            "model": "The underlying AI model file or version being used",
            "energy_threshold": "RMS amplitude threshold for energy VAD (default 200.0)",
            "speech_prob": "Speech probability threshold for a model-based backend (default 0.5, range 0.0-1.0)",
            "threshold": "(deprecated) Fallback threshold -- use energy_threshold or speech_prob instead",
            "neg_threshold": "Probability below which speech is considered ended",
            "min_speech_ms": "Minimum duration in milliseconds to be considered valid speech",
            "min_silence_ms": "Minimum silence duration before ending speech segment"
        })
    }
}

fn default_vad_mode() -> String {
    "auto".to_string()
}

/// Turn detection configuration.
///
/// ```toml
/// [turn_detection]
/// mode = "layered"        # "silence" | "provider" | "layered", or a plugin's mode
/// silence_timeout_ms = 1000
///
/// # Any other key belongs to the selected mode's detector, which checks it
/// # (Smart Turn, for one, reads smart_model_path, smart_threshold, ...).
/// smart_model_path   = "models/smart-turn-v3.2-cpu.onnx"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnDetectionConfig {
    /// `"silence"` -- fixed silence timeout (default).
    /// `"provider"` -- use provider end-of-turn signal, silence fallback.
    /// `"layered"` -- provider signal first, then silence timeout.
    /// Or the name of a detector a plugin registered (`"smart"`, `"vap"`).
    #[serde(default = "default_turn_mode")]
    pub mode: String,
    /// Silence timeout in milliseconds (used by all modes as primary or fallback).
    #[serde(default = "default_silence_timeout_ms")]
    pub silence_timeout_ms: u64,
    /// Every other key of the section, for the selected mode's detector.
    /// Its `validate` refuses keys it does not read, so a misspelt setting
    /// is still an error.
    #[serde(flatten)]
    pub options: std::collections::HashMap<String, serde_json::Value>,
}

impl TurnDetectionConfig {
    /// The detector's view of this section.
    pub fn turn_config(&self) -> voice_pipes_sdk::turn::TurnConfig {
        voice_pipes_sdk::turn::TurnConfig::new(self.mode.clone(), self.silence_timeout_ms)
            .with_options(self.options.clone())
    }
}

fn default_turn_mode() -> String {
    "silence".to_string()
}

/// The silence that ends a turn when `[turn_detection] silence_timeout_ms`
/// is not set, whether the section is there or not.
pub const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1000;

fn default_silence_timeout_ms() -> u64 {
    DEFAULT_SILENCE_TIMEOUT_MS
}

/// Barge-in / interruption detection configuration.
///
/// ```toml
/// [barge_in]
/// enabled = true
/// min_chunks = 3            # consecutive speech chunks to trigger (default 3)
/// speech_threshold = 200.0  # RMS threshold for speech detection
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BargeInConfig {
    /// Whether barge-in detection is enabled (default true).
    #[serde(default = "default_barge_in_enabled")]
    pub enabled: bool,
    /// Consecutive speech chunks required before triggering an interrupt.
    /// Each chunk is ~20ms, so 3 = ~60ms of sustained speech.
    #[serde(default = "default_barge_in_min_chunks")]
    pub min_chunks: u32,
    /// RMS amplitude threshold for speech detection during barge-in.
    /// Higher values reduce false positives from background noise.
    #[serde(default = "default_barge_in_threshold")]
    pub speech_threshold: f64,
}

impl Default for BargeInConfig {
    fn default() -> Self {
        Self {
            enabled: default_barge_in_enabled(),
            min_chunks: default_barge_in_min_chunks(),
            speech_threshold: default_barge_in_threshold(),
        }
    }
}

// ── Adaptive quality ────────────────────────────────────────

/// Thresholds and timings for the adaptive-quality controller.
///
/// **Disabled by default.** With `enabled = false` the controller still
/// observes and publishes diagnostics, but the tier never leaves
/// [`QualityTier::Full`](crate::transport::QualityTier::Full), so every byte
/// on the wire is identical to a build without this feature.
///
/// One entry per tier for each signal, rather than a shared scale, because the
/// signals are not comparable to each other and their thresholds were chosen
/// independently.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveQualityConfig {
    /// Master switch. `false` pins the tier at `Full`.
    #[serde(default)]
    pub enabled: bool,

    /// Consecutive degraded observations required before dropping a tier.
    /// Downgrades are fast but not instant: one bad sample is noise.
    #[serde(default = "default_downgrade_observations")]
    pub downgrade_observations: u32,

    /// `arrival_rtf` at or above which the link is considered degraded to the
    /// named tier. Above 1.0 means the client received audio slower than it
    /// plays it.
    #[serde(default = "default_reduced_arrival_rtf")]
    pub reduced_arrival_rtf: f32,
    #[serde(default = "default_minimal_arrival_rtf")]
    pub minimal_arrival_rtf: f32,
    #[serde(default = "default_critical_arrival_rtf")]
    pub critical_arrival_rtf: f32,

    /// Server socket-write latency (ms) at or above which the link is
    /// considered degraded to the named tier.
    #[serde(default = "default_reduced_send_latency_ms")]
    pub reduced_send_latency_ms: u64,
    #[serde(default = "default_minimal_send_latency_ms")]
    pub minimal_send_latency_ms: u64,
    #[serde(default = "default_critical_send_latency_ms")]
    pub critical_send_latency_ms: u64,

    /// Client playback underruns in one observation at or above which the
    /// link is considered degraded to the named tier.
    #[serde(default = "default_reduced_underruns")]
    pub reduced_underruns: u32,
    #[serde(default = "default_minimal_underruns")]
    pub minimal_underruns: u32,

    /// How long the link must stay better than the current tier before
    /// climbing one step out of it. Longer than the downgrade path on
    /// purpose: oscillation is worse than sitting one tier too low.
    #[serde(default = "default_reduced_clean_secs")]
    pub reduced_clean_secs: u64,
    #[serde(default = "default_minimal_clean_secs")]
    pub minimal_clean_secs: u64,
    #[serde(default = "default_critical_clean_secs")]
    pub critical_clean_secs: u64,
}

impl Default for AdaptiveQualityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            downgrade_observations: default_downgrade_observations(),
            reduced_arrival_rtf: default_reduced_arrival_rtf(),
            minimal_arrival_rtf: default_minimal_arrival_rtf(),
            critical_arrival_rtf: default_critical_arrival_rtf(),
            reduced_send_latency_ms: default_reduced_send_latency_ms(),
            minimal_send_latency_ms: default_minimal_send_latency_ms(),
            critical_send_latency_ms: default_critical_send_latency_ms(),
            reduced_underruns: default_reduced_underruns(),
            minimal_underruns: default_minimal_underruns(),
            reduced_clean_secs: default_reduced_clean_secs(),
            minimal_clean_secs: default_minimal_clean_secs(),
            critical_clean_secs: default_critical_clean_secs(),
        }
    }
}

fn default_downgrade_observations() -> u32 {
    2
}
fn default_reduced_arrival_rtf() -> f32 {
    1.15
}
fn default_minimal_arrival_rtf() -> f32 {
    1.4
}
fn default_critical_arrival_rtf() -> f32 {
    2.0
}
fn default_reduced_send_latency_ms() -> u64 {
    250
}
fn default_minimal_send_latency_ms() -> u64 {
    750
}
fn default_critical_send_latency_ms() -> u64 {
    2000
}
fn default_reduced_underruns() -> u32 {
    1
}
fn default_minimal_underruns() -> u32 {
    3
}
fn default_reduced_clean_secs() -> u64 {
    30
}
fn default_minimal_clean_secs() -> u64 {
    45
}
fn default_critical_clean_secs() -> u64 {
    60
}

fn default_barge_in_enabled() -> bool {
    true
}

fn default_barge_in_min_chunks() -> u32 {
    3
}

fn default_barge_in_threshold() -> f64 {
    2000.0
}

// ── Transport config ────────────────────────────────────────

/// Transport layer configuration.
///
/// ```toml
/// [transport]
/// enabled = true
/// bind = "0.0.0.0"
/// port = 8800
/// max_sessions = 16
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    /// Whether the transport layer is enabled (default false).
    #[serde(default)]
    pub enabled: bool,
    /// Bind address for the transport listener.
    #[serde(default = "default_transport_bind")]
    pub bind: String,
    /// Port for the transport listener.
    #[serde(default = "default_transport_port")]
    pub port: u16,
    /// Maximum concurrent transport sessions.
    #[serde(default = "default_transport_max_sessions")]
    pub max_sessions: usize,
    /// How long one WebSocket frame may take to be written before
    /// the peer is treated as gone. A client that stops reading (but keeps
    /// the TCP connection alive) otherwise stalls the write pump, which
    /// blocks the session's `send()` and the heartbeat behind a full
    /// channel and pins the session slot until the OS gives up.
    #[serde(default = "default_transport_write_timeout_ms")]
    pub write_timeout_ms: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_transport_bind(),
            port: default_transport_port(),
            max_sessions: default_transport_max_sessions(),
            write_timeout_ms: default_transport_write_timeout_ms(),
        }
    }
}

fn default_transport_write_timeout_ms() -> u64 {
    5_000
}

fn default_transport_bind() -> String {
    "0.0.0.0".to_string()
}

fn default_transport_port() -> u16 {
    8800
}

fn default_transport_max_sessions() -> usize {
    16
}

// ── Defaults ────────────────────────────────────────────────

fn default_log_format() -> String {
    "compact".to_string()
}

fn default_buffer_size() -> usize {
    256
}

fn default_audit_tap_dir() -> PathBuf {
    PathBuf::from("audit_tap")
}

// ── ConfigError — re-exported from voice-pipes-sdk ───────────
pub use voice_pipes_sdk::{ConfigError, ConfigErrorKind};

// ── CLI overrides ───────────────────────────────────────────

/// Values extracted from CLI flags that can override config file settings.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub log_format: Option<String>,
    pub captures_dir: Option<PathBuf>,
    pub audit_tap: Option<String>,
    pub audit_tap_dir: Option<PathBuf>,
    pub output_rate: Option<u32>,
}

// ── Resolve helpers ─────────────────────────────────────────

impl PipelineConfig {
    /// The silence that ends a turn, in ms: the section's setting, or the
    /// default when the section is absent. Everything that waits for the
    /// end of a turn (the VAD filter, the turn detector) or judges it (the
    /// grading rules, the session record) reads it here, so they agree.
    pub fn silence_timeout_ms(&self) -> u64 {
        self.turn_detection
            .as_ref()
            .map_or(DEFAULT_SILENCE_TIMEOUT_MS, |td| td.silence_timeout_ms)
    }

    /// Resolve the `TapSide` from the `audit_tap` config.
    pub fn tap_side(&self) -> Option<TapSide> {
        if !self.pipeline.audit_tap.enabled {
            return None;
        }
        self.pipeline.audit_tap.side.as_deref().map(|s| match s {
            "before" => TapSide::Before,
            "after" => TapSide::After,
            _ => TapSide::Both,
        })
    }

    /// Resolve the audit tap directory.
    pub fn tap_dir(&self) -> &Path {
        &self.pipeline.audit_tap.dir
    }

    /// Resolve the output sample rate (if any).
    pub fn output_rate(&self) -> Option<u32> {
        self.output.as_ref().and_then(|o| o.sample_rate)
    }

    /// Resolve optional output chunking configuration.
    pub fn output_chunking(&self) -> Option<OutputChunkingConfig> {
        self.output.as_ref().and_then(|o| o.chunking.clone())
    }
}
