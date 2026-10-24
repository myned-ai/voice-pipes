//! Data model for benchmark experiments, test suites, and results.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Experiment ──────────────────────────────────────────────

/// A benchmark experiment comparing multiple pipeline variants against
/// a common test suite of audio scenarios.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub status: ExperimentStatus,
    pub test_suite_id: String,
    pub variants: Vec<Variant>,
    /// Number of times to repeat each scenario (for statistical robustness).
    #[serde(default = "default_repetitions")]
    pub repetitions: u32,
}

fn default_repetitions() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Draft,
    Running,
    Complete,
    Failed,
}

/// A pipeline variant within an experiment -- a frozen configuration snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub id: String,
    pub name: String,
    /// Relative path to the frozen TOML config file.
    pub config_path: PathBuf,
    /// SHA-256 hash of the pipeline-relevant config.
    pub config_hash: String,
    /// Tags for dispatch routing to the correct worker.
    #[serde(default)]
    pub tags: Vec<String>,
}

// ── Test Suite ──────────────────────────────────────────────

/// A collection of audio scenarios used as input for benchmark experiments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSuite {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub scenarios: Vec<TestScenario>,
}

/// A single test scenario: an audio file and optional expected outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestScenario {
    pub id: String,
    pub name: String,
    pub input: ScenarioInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<ScenarioExpected>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScenarioInput {
    /// Pre-recorded WAV file for deterministic replay.
    AudioFile { path: PathBuf },
    /// Text-only input (bypasses STT).
    TextPrompt { text: String },
    /// TTS-only corpus: each entry drives one rep against the variant's
    /// TTS provider directly, bypassing STT and LLM.  Used to compare
    /// TTS providers in isolation.
    TtsOnlyCorpus {
        /// Stratified entries -- one rep per entry; the runner ignores the
        /// scenario-level `repetitions` count for this variant.
        entries: Vec<TtsCorpusEntry>,
    },
    /// Multi-turn scripted conversation. Each turn drives the pipeline,
    /// waits for the assistant's `TurnCompleted`, then advances. Runs as
    /// a single WebSocket session so server-side conversation history is
    /// preserved across turns.
    Conversation { turns: Vec<ConversationTurn> },
    /// Barge-in scenario: speak an initial prompt, interrupt the
    /// pipeline mid-response, then optionally continue with a follow-up
    /// turn to verify recovery.
    BargeIn {
        initial: ConversationTurn,
        interrupt: InterruptSpec,
        #[serde(skip_serializing_if = "Option::is_none")]
        follow_up: Option<ConversationTurn>,
    },
    /// Synthetic-caller scenario: drive the pipeline by piping its
    /// audio output into a separate S2S model (the "caller") and piping
    /// the caller's audio back as user input. The caller and pipeline
    /// hold a free-form conversation until a stop condition fires.
    /// Gated behind cargo feature `synthetic-caller`.
    SyntheticCaller {
        #[serde(rename = "config")]
        caller: SyntheticCallerConfig,
    },
    /// Gym task scenario: connects to the pipeline, advertises the task's
    /// client-side tools, drives caller dialogue (with disfluencies and
    /// corrections), handles in-flight tool calls, and computes the composite
    /// RL reward.
    GymTask {
        #[serde(rename = "config")]
        task: GymTaskConfig,
    },
}

/// Configuration for a `GymTask` scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GymTaskConfig {
    /// Task domain: "banking" (or others in future).
    pub domain: String,
    /// Expected goal specification for the scenario.
    pub goal: serde_json::Value,
    /// Initial prompt spoken by the user to start the scenario.
    pub initial_prompt: String,
    /// Optional follow-up turns or corrections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_up_prompts: Vec<String>,
    /// Adversary configuration for hesitations and barge-in.
    #[serde(default)]
    pub adversary: voice_pipes_gym::AdversaryConfig,
    /// Reward weights.
    #[serde(default)]
    pub reward_weights: voice_pipes_gym::GymRewardWeights,
}

/// Outcome of a `GymTask` rollout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GymResult {
    pub domain: String,
    pub evaluation: voice_pipes_gym::TaskEvaluation,
    pub reward: voice_pipes_gym::GymRewardBreakdown,
    pub tool_calls_executed: u32,
}

/// Configuration for a synthetic-caller scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticCallerConfig {
    /// Which caller backend to use.
    pub caller: CallerKind,
    /// Persona / system prompt fed to the caller.
    pub persona: String,
    /// Stop conditions evaluated after every caller or pipeline turn.
    pub stop: SyntheticStopConditions,
    /// Optional barge-in policy: force the caller to interrupt the
    /// pipeline every N turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barge_in: Option<BargeInPolicy>,
    /// Optional persona template name (resolved by
    /// [`synthetic_caller::prompts::lookup`]). When set, the resolved
    /// prompt is concatenated with `persona` (template first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_template: Option<String>,
    /// How the runner bridges pipeline <-> caller. Defaults to
    /// [`BridgeMode::Text`] for cost stability; [`BridgeMode::Audio`]
    /// exercises the pipeline's STT path end-to-end.
    #[serde(default)]
    pub bridge: BridgeMode,
}

/// Selects how the runner connects the pipeline and the synthetic
/// caller (T7-Phase B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BridgeMode {
    /// Forward the pipeline's transcript to the caller as text and
    /// the caller's transcript to the pipeline as text. Bypasses
    /// pipeline STT. Cheaper, more deterministic -- used by default.
    #[default]
    Text,
    /// Forward raw PCM audio in both directions. Exercises the full
    /// VAD / STT / LLM / TTS chain and matches real production paths.
    /// Requires a caller backend that actually emits audio (i.e. the
    /// live `OpenAI` Realtime / Gemini Live providers).
    Audio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallerKind {
    /// Mock caller: replays a canned WAV / transcript script. Used by
    /// unit tests and as a no-API-cost smoke test.
    Mock { script: PathBuf },
    /// `OpenAI` Realtime API S2S caller.
    OpenaiRealtime {
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        voice: Option<String>,
    },
    /// Gemini Live S2S caller.
    GeminiLive {
        #[serde(default)]
        model: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticStopConditions {
    /// Hard cap on completed pipeline replies (i.e. conversational
    /// exchanges where the caller spoke and the pipeline answered).
    /// `max_turns: 4` therefore yields up to 4 user-bot exchanges
    /// (8 transcript entries: 4 caller + 4 pipeline). This matches
    /// the common-sense reading of "turn" used by scenario authors;
    /// counting caller and pipeline entries separately would halve
    /// the effective conversation length.
    pub max_turns: u32,
    /// Hard wall-clock cap in seconds.
    pub max_duration_s: u64,
    /// Stop early if the caller stays silent for this long after the
    /// pipeline completes a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_caller_silence_s: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BargeInPolicy {
    /// Force the caller to barge in (cancel + speak again) every N
    /// pipeline turns. `0` disables forced barge-in.
    pub every_n_turns: u32,
    /// Cap on consecutive failed barge-in attempts before the scenario
    /// is marked failed.
    #[serde(default)]
    pub max_consecutive_failures: u32,
}

/// Per-scenario result of a synthetic-caller run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyntheticCallerResult {
    pub turns: Vec<SyntheticTurn>,
    pub stop_reason: Option<SyntheticStopReason>,
    pub total_duration_ms: Option<u64>,
    pub barge_in_attempts: u32,
    pub barge_in_successes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticTurn {
    /// `"caller"` or `"pipeline"`.
    pub speaker: String,
    pub turn_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<f64>,
    #[serde(default)]
    pub barge_in: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticStopReason {
    MaxTurns,
    MaxDuration,
    CallerSilence,
    CallerError,
    PipelineError,
    BargeInFailureCap,
}

/// One turn in a [`ScenarioInput::Conversation`] or
/// [`ScenarioInput::BargeIn`] scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    /// Stable identifier within the scenario, e.g. `"t1"`. Used in
    /// per-turn results and assertion error messages.
    pub id: String,
    pub input: TurnInput,
    /// Optional per-turn assertions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<TurnExpected>,
    /// After this turn completes, wait for this many **agent-initiated**
    /// turns (a filler's late result, proactive speech, an idle prompt)
    /// before advancing to the next scripted turn. Their spoken text is
    /// appended to `response_text` so `transcript_contains` can assert
    /// against filler + follow-up combined. 0 (default) = don't wait.
    #[serde(default)]
    pub expect_followups: u32,
    /// How long to wait for the expected follow-ups (default 15 000 ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub followup_timeout_ms: Option<u64>,
}

/// What this turn sends to the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnInput {
    /// Inject a pre-recorded WAV (drives STT -> LLM -> TTS).
    Audio { path: PathBuf },
    /// Send a text message (bypasses STT).
    Text { text: String },
}

/// How a barge-in scenario interrupts the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptSpec {
    pub mode: InterruptMode,
    pub fire_on: InterruptFireOn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InterruptMode {
    /// Send a `{action:"cancel"}` control message.
    ClientCancel,
    /// Inject an audio clip while the pipeline is speaking; relies on
    /// server-side VAD / barge-in to fire `Interrupted`.
    InjectAudio { path: PathBuf },
    /// Send a new text turn while the pipeline is speaking.
    InjectText { text: String },
}

/// Which lifecycle event the interrupt waits for before firing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InterruptFireOn {
    /// Wait for `TtsStarted` before firing.
    TtsStarted,
    /// Wait for the first audio chunk on the wire.
    FirstAudio,
    /// Fire after `offset_ms` have elapsed past `FirstAudio`.
    PlaybackOffset { offset_ms: u64 },
}

/// Optional per-turn assertions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnExpected {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript_contains: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub golden_transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_latency_ms: Option<u64>,
}

/// A single text input for [`ScenarioInput::TtsOnlyCorpus`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsCorpusEntry {
    /// Stable identifier for cross-run join.
    pub id: String,
    /// Text to synthesise.
    pub text: String,
    /// Optional BCP-47 language tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Length stratum.
    pub bucket: TtsLengthBucket,
    /// Content class stratum.
    pub class: TtsContentClass,
}

/// Length stratum for a [`TtsCorpusEntry`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TtsLengthBucket {
    /// <=10 words.
    Short,
    /// ~=30 words.
    Medium,
    /// ~=100 words.
    Long,
}

/// Content class stratum for a [`TtsCorpusEntry`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TtsContentClass {
    /// Plain prose.
    Plain,
    /// Numbers, dates, currencies.
    Numerics,
    /// Heavy punctuation, parenthetical remarks.
    Punctuation,
    /// Mixed content (prose + numerics + punctuation).
    Mixed,
}

/// Optional expected outcomes for automated scoring.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScenarioExpected {
    /// Substrings that must appear in the STT transcript.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript_contains: Vec<String>,
    /// Expected intent / behaviour for LLM-as-judge scoring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_intent: Option<String>,
    /// Maximum acceptable end-to-end latency in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_latency_ms: Option<u64>,
    /// Golden transcript for WER/CER computation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub golden_transcript: Option<String>,
    /// Golden response text for exact/fuzzy comparison.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub golden_response: Option<String>,

    // ── Multi-turn / barge-in additions ──────────
    /// For `Conversation` scenarios: number of assistant turns that
    /// must complete (`TurnCompleted` observed) for the scenario to pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns_completed: Option<u32>,
    /// For `Conversation` scenarios: per-turn end-to-end latency cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_latency_ms_per_turn: Option<u64>,
    /// For `Conversation` scenarios: maximum tolerated latency drift
    /// across turns, expressed as a percentage of the first-turn
    /// latency (e.g. `25.0` allows turn N to be 25% slower than turn 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_latency_drift_pct: Option<f64>,
    /// For `BargeIn` scenarios: the interrupt must fire and an
    /// `Interrupted` event must be observed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub must_interrupt: bool,
    /// For `BargeIn` scenarios: the observed interruption trigger must
    /// match this label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interruption_trigger: Option<ExpectedTrigger>,
    /// For `BargeIn` scenarios: maximum acceptable round-trip from
    /// firing the interrupt to receiving `Interrupted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_barge_in_rtt_ms: Option<u64>,
    /// For `BargeIn` scenarios: maximum time from `Interrupted` until
    /// the pipeline returns to `Listening`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_recovery_ms: Option<u64>,
    /// For `BargeIn` scenarios with a `follow_up` turn: that turn must
    /// reach `TurnCompleted`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub follow_up_must_complete: bool,

    // ── Synthetic-caller additions ──────────────
    /// For `SyntheticCaller` scenarios: the conversation must reach at
    /// least this many completed turns before the stop condition fires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_completed_turns: Option<u32>,
    /// For `SyntheticCaller` scenarios: substrings that must appear in
    /// at least one pipeline transcript across the conversation
    /// (case-insensitive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipeline_must_say: Vec<String>,
    /// For `SyntheticCaller` scenarios: per-pipeline-turn latency cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_pipeline_turn_latency_ms: Option<u64>,
    /// For `SyntheticCaller` scenarios with a `BargeInPolicy`: maximum
    /// fraction of failed barge-in attempts (`0.0..=1.0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_barge_in_failure_rate: Option<f64>,
}

/// Allowed values for [`ScenarioExpected::interruption_trigger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedTrigger {
    ServerVad,
    ClientCancel,
    EarlyBarge,
    Any,
}

// ── Results ─────────────────────────────────────────────────

/// Aggregated results for a completed experiment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentResults {
    pub experiment_id: String,
    pub completed_at: DateTime<Utc>,
    pub variant_results: Vec<VariantResults>,
}

/// Per-variant aggregated results across all scenarios.
///
/// When the variant could not run at all (e.g. no matching worker), every
/// scenario carries `status: Error` and [`error_reason`] describes the
/// variant-level failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantResults {
    pub variant_id: String,
    pub variant_name: String,
    pub scenarios: Vec<ScenarioResult>,
    pub summary: VariantSummary,
    /// Parsed summary of the pipeline config used for this variant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_summary: Option<PipelineConfigSummary>,
    /// Variant-level error reason (e.g. "no available worker matching tags [...]").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
}

/// Structured summary of a pipeline configuration, parsed from the TOML.
/// Used to display key differentiators in the UI without showing the full config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfigSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_detection_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub barge_in_enabled: Option<bool>,
}

/// Aggregate summary metrics for a variant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VariantSummary {
    pub scenarios_run: u32,
    pub scenarios_passed: u32,
    pub scenarios_failed: u32,
    pub avg_e2e_latency_ms: Option<f64>,
    pub p50_e2e_latency_ms: Option<u64>,
    pub p90_e2e_latency_ms: Option<u64>,
    pub p99_e2e_latency_ms: Option<u64>,
    pub avg_ttft_ms: Option<f64>,
    pub avg_ttfb_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_wer: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_quality_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_estimate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_stt_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_llm_generation_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tts_ttfb_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_tts_ttfb_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90_tts_ttfb_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p99_tts_ttfb_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tts_synthesis_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tts_audio_duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tts_rtf: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tts_chars_per_second: Option<f64>,
    /// Fraction of repetitions where TTS produced no audio (and the turn
    /// was not user-cancelled). 0.0 = all good; 1.0 = every rep failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_failure_rate: Option<f64>,
    /// Mean synthesis time of the cold-start reps only (typically 1 per
    /// session). `None` when no cold reps were observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_tts_synthesis_ms: Option<f64>,
    /// Mean synthesis time of warm reps only (everything that was not the
    /// first rep of its session).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_tts_synthesis_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tokens_per_second: Option<f64>,
    // ── Multi-turn / barge-in aggregates ──
    /// Number of conversation scenarios in this variant (any scenario with
    /// `turns` populated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_scenarios: Option<u32>,
    /// Average count of completed turns per conversation scenario.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_turns_completed: Option<f64>,
    /// Mean per-turn end-to-end latency across all turns of all
    /// conversation scenarios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_turn_latency_ms: Option<f64>,
    /// Number of barge-in scenarios in this variant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub barge_in_scenarios: Option<u32>,
    /// Fraction of barge-in scenarios where the pipeline interrupted as
    /// expected (`interrupted == true`). Range 0.0..=1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub barge_in_success_rate: Option<f64>,
    /// p50 of `BargeInResult.barge_in_rtt_ms` across barge-in scenarios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_barge_in_rtt_ms: Option<u64>,
    /// p95 of `BargeInResult.barge_in_rtt_ms` across barge-in scenarios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_barge_in_rtt_ms: Option<u64>,
    /// p50 of `BargeInResult.recovery_ms` across barge-in scenarios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_recovery_ms: Option<u64>,
    /// p95 of `BargeInResult.recovery_ms` across barge-in scenarios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_recovery_ms: Option<u64>,
}

/// Result of a single scenario run for a single variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario_id: String,
    pub scenario_name: String,
    /// The capture session IDs for each repetition.
    pub session_ids: Vec<String>,
    pub status: ScenarioRunStatus,
    pub metrics: ScenarioMetrics,
    /// Raw per-repetition E2E latencies (ms), used for percentile computation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latency_samples: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoring: Option<ScenarioScoring>,
    /// Human-readable reason when status is `Error` or `Timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// How many repetitions failed (out of the total requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetitions_failed: Option<u32>,
    /// Per-entry results for `TtsOnlyCorpus` scenarios. `None` for other
    /// scenario kinds. Empty `Vec` is suppressed from JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_entries: Option<Vec<TtsEntryResult>>,
    /// Per-turn results for `Conversation` scenarios.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<Vec<TurnResult>>,
    /// Barge-in measurements for `BargeIn` scenarios.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barge_in: Option<BargeInResult>,
    /// Synthetic-caller transcript and stop reason for
    /// `SyntheticCaller` scenarios.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthetic_caller: Option<SyntheticCallerResult>,
    /// RL evaluation and composite reward for `GymTask` scenarios.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gym: Option<GymResult>,
}

/// Per-turn result inside a `Conversation` scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResult {
    pub turn_id: String,
    /// Pipeline-side utterance id when known (assigned by the
    /// orchestrator).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utterance_id: Option<u32>,
    /// STT transcript when this turn used `TurnInput::Audio`, or the
    /// echoed text for `TurnInput::Text`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_text: Option<String>,
    /// Assistant response text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_text: Option<String>,
    /// End-to-end latency for this turn (user input committed -> first
    /// assistant audio).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e2e_latency_ms: Option<f64>,
    /// True when `TurnCompleted` was observed for this turn.
    #[serde(default)]
    pub completed: bool,
    /// Agent-initiated turns observed during this turn's window
    /// (including its `expect_followups` wait).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub agent_turns_seen: u32,
    /// Per-turn assertion failures (empty when the turn passed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expectation_failures: Vec<String>,
    /// Optional error message if this turn errored before completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's `skip_serializing_if` calls the predicate with a reference"
)]
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// Measurements specific to a `BargeIn` scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BargeInResult {
    /// True when `Interrupted` was observed.
    #[serde(default)]
    pub interrupted: bool,
    /// Wire-level trigger reported by the pipeline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Time from firing the interrupt to receiving `Interrupted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub barge_in_rtt_ms: Option<u64>,
    /// Time from `Interrupted` until the pipeline returned to
    /// `Listening`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_ms: Option<u64>,
    /// True when a `follow_up` turn was provided and reached
    /// `TurnCompleted`.
    #[serde(default)]
    pub follow_up_completed: bool,
}

/// Per-entry result for a single `TtsCorpusEntry` within a
/// `TtsOnlyCorpus` scenario. Surfaces the raw measurements + audio
/// pointer + scorer output so the UI can render per-entry tables and
/// play back individual clips.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsEntryResult {
    pub entry_id: String,
    pub text: String,
    pub bucket: TtsLengthBucket,
    pub class: TtsContentClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtf: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cold_start: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Absolute path to the persisted WAV on the worker host. Resolved
    /// by the audio-streaming endpoint; never exposed verbatim to the
    /// browser.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_path: Option<String>,
    /// `true` when the orchestrator (or any future post-processor) has
    /// confirmed the WAV is readable. The frontend uses this to decide
    /// whether to render a Play button.
    #[serde(default)]
    pub audio_available: bool,
    /// Provider error string when synthesis failed for this entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Peak absolute amplitude (int16 scale) reported by the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_amplitude: Option<u16>,
    /// Fraction (0–1) of 100 ms windows with speech-level energy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_ratio: Option<f64>,
    /// `true` when the worker reported audio but it carried no speech:
    /// the provider "succeeded" with silence (see [`TTS_SILENT_SPEECH_RATIO`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
    /// Round-trip transcript from the configured `TtsScorer`, when
    /// scoring was enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wer: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cer: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioRunStatus {
    Passed,
    Failed,
    Timeout,
    Error,
}

/// Raw latency metrics for a scenario run (averaged across repetitions).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScenarioMetrics {
    pub e2e_latency_ms: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub ttfb_ms: Option<f64>,
    pub stt_text: Option<String>,
    pub response_text: Option<String>,
    pub audio_duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_generation_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_ttfb_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_synthesis_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_audio_duration_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_chars: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_rtf: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_chars_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_provider: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tts_cold_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_error: Option<String>,
    /// `tts_only_corpus`: entries whose audio carried no speech (the
    /// provider returned silence and called it success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_silent_entries: Option<u32>,
}

/// A `tts_only_corpus` entry whose speech ratio is below this, or whose
/// peak amplitude is below [`TTS_SILENT_PEAK`], is counted as silent. The
/// thresholds come from measured runs of a misfiring checkpoint (silent
/// output: peak < 300, ratio 0; speech: peak > 3000, ratio > 0.3).
pub const TTS_SILENT_SPEECH_RATIO: f64 = 0.05;
/// See [`TTS_SILENT_SPEECH_RATIO`].
pub const TTS_SILENT_PEAK: u16 = 500;

/// Quality scoring results for a scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioScoring {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wer: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cer: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_reasoning: Option<String>,
    /// Whether all expected-outcome checks passed.
    #[serde(default)]
    pub expectations_met: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expectation_failures: Vec<String>,
}
