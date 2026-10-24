//! Per-turn metrics registry -- single source of truth for what happened
//! in one turn of the pipeline.
//!
//! Components push spans through [`TurnRecorder`] (one per turn) using
//! the RAII [`ComponentSpan`] helper. The recorder produces a
//! [`TurnMetricsSnapshot`] at turn-end which is shipped to the client,
//! broadcast on `PipelineEvent::TurnMetricsReady`, and persisted on the
//! audit `TurnRecord`. There are no parallel typed fields -- every
//! per-component value lives in `TurnComponentMetrics::extras`, with
//! co-located typed `*Extras` structs guarding key drift.

use parking_lot::Mutex;
use std::ops::Not;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

// ── Wire types ──────────────────────────────────────────────────────────

/// Snapshot of all component activity for a single turn.
///
/// Emitted exactly once per turn at turn-end. Sent to the client via
/// `ServerControl::TurnMetrics`, broadcast on
/// `PipelineEvent::TurnMetricsReady`, and persisted on the audit
/// `TurnRecord` as `latency_breakdown`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TurnMetricsSnapshot {
    pub utterance_id: u32,
    pub total_ms: u64,
    pub interrupted: bool,
    /// Who opened this turn. Agent-initiated turns have no user
    /// utterance: their latency anchor is turn-open (not end of user
    /// speech), they carry no STT component, and consumers must not
    /// fold them into user-perceived latency aggregates. Serde default
    /// keeps old captures and the admin BFF compatible.
    #[serde(default, skip_serializing_if = "TurnInitiator::is_user")]
    pub initiator: TurnInitiator,
    /// Components in begin-time (== pipeline) order.
    pub components: Vec<TurnComponentMetrics>,
    /// LLM response text accumulated during this turn (when available).
    /// Used for multi-turn synthesis-caller scenarios and dashboard display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_response_text: Option<String>,
}

/// Who opened a turn.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnInitiator {
    /// A user utterance (audio or typed text) committed the turn.
    #[default]
    User,
    /// The agent opened the turn (out-of-turn speech, task completion).
    Agent,
}

impl TurnInitiator {
    /// Serde skip helper: omit the field for the common user case.
    pub fn is_user(&self) -> bool {
        matches!(self, TurnInitiator::User)
    }
}

/// One component's slice of a turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TurnComponentMetrics {
    /// Stable identifier -- matches `Component::id()`
    /// (e.g. `"stt:parakeet"`, `"llm:openai"`, `"tts:piper"`).
    pub component_id: String,
    /// Free-form `snake_case` kind (`"stt"`, `"llm"`, `"tts"`, `"vad"`,
    /// `"audio_post"`, `"text_post"`, `"translator"`, ...). Free-form on
    /// the wire so a new kind never requires a wire-type change.
    pub kind: String,
    /// Begin time relative to turn start (ms).
    pub started_at_ms: u64,
    /// `None` while the span is still open at snapshot time
    /// (component started but never finished -- e.g. interrupted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// `finished_at_ms - started_at_ms`, denormalised for cheap reads.
    /// Producers never set this directly -- `TurnRecorder` computes it
    /// on `finish()` / `Drop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Not::not")]
    pub cold_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extras: serde_json::Map<String, serde_json::Value>,
}

/// Frontend-facing description of a component's metric layout.
///
/// Assembled once at session build time (the stage defaults, the output
/// stage, one per audio post-processor and annotator) and shipped on `ServerControl::SessionReady::metric_descriptors`. The
/// frontend renders one generic table driven by these descriptors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComponentMetricDescriptor {
    pub component_id: String,
    pub kind: String,
    /// Human-readable component label (e.g. `"Translator (en->es)"`).
    pub label: String,
    pub fields: Vec<MetricFieldDescriptor>,
}

/// Describes one field inside a component's `extras` map.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricFieldDescriptor {
    /// Matches a key in `TurnComponentMetrics::extras`.
    pub key: String,
    /// Display label (e.g. `"Time to first byte"`).
    pub label: String,
    pub unit: MetricUnit,
    /// Optional numeric format hint (e.g. `"0.00"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Optional provider-declared health semantics for the generic health
    /// engine (role/direction/budget/critical-path/severity). Absent on
    /// display-only metrics and on captures predating this field — the engine
    /// then falls back to its built-in defaults. Additive & optional so old
    /// wire payloads deserialize unchanged. See [`MetricHealth`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<MetricHealth>,
    /// Start anchor describing when this metric begins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_anchor: Option<String>,
    /// End anchor describing when this metric finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_anchor: Option<String>,
    /// Detailed description of what is included (and excluded) in this metric.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub includes: Option<String>,
    /// Who measures/owns this metric: `"engine"` or `"provider"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// For provider breakdowns, the stage key that contains this breakdown (e.g. `"llm_wait_ms"`, `"tts_wait_ms"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contained_in: Option<String>,
}

/// What a metric measures, so the generic health engine can judge any
/// component without hardcoding provider names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricRole {
    /// A latency; contributes to user-perceived wait when `on_critical_path`.
    Latency,
    /// Higher-is-better rate (e.g. tokens/sec); off the critical path.
    Throughput,
    /// Output-quality signal (e.g. RTF); advisory, not a latency.
    Quality,
    /// Internal/compute metric; never drives the verdict.
    Diagnostic,
}

/// Direction of "better" for a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricDirection {
    LowerIsBetter,
    HigherIsBetter,
}

/// Multiplicative severity thresholds applied to `budget`.
///
/// For a lower-is-better metric the ratio is `observed / budget`; for
/// higher-is-better it is `budget / observed`. `ratio >= warn` → minor,
/// `>= moderate` → moderate, `>= severe` → severe. Below `warn` is negligible.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SeverityBands {
    pub warn: f64,
    pub moderate: f64,
    pub severe: f64,
}

impl Default for SeverityBands {
    fn default() -> Self {
        Self {
            warn: 1.15,
            moderate: 1.5,
            severe: 2.5,
        }
    }
}

/// Provider-declared health semantics for a single metric field. Every field
/// is optional so a component can declare as much or as little as it knows;
/// the generic engine fills the rest from role/unit defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MetricHealth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<MetricRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<MetricDirection>,
    /// Target/budget in the metric's unit. `None` = not judged against a budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<f64>,
    /// Whether this metric contributes to user-perceived latency. `None` means
    /// "conditional — the engine decides from the timeline/overlap" (e.g. STT
    /// `processing_ms`, which overlaps speech for incremental models). This is
    /// the generic replacement for the hardcoded streaming-STT skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_critical_path: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_bands: Option<SeverityBands>,
}

impl MetricFieldDescriptor {
    /// Display-only field (no health semantics). Health-bearing fields chain a
    /// role helper: `MetricFieldDescriptor::new(k, l, u).latency(400.0, true)`.
    pub fn new(key: impl Into<String>, label: impl Into<String>, unit: MetricUnit) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            unit,
            format: None,
            health: None,
            start_anchor: None,
            end_anchor: None,
            includes: None,
            owner: None,
            contained_in: None,
        }
    }

    /// Set the start and end anchors for the metric.
    #[must_use]
    pub fn anchors(mut self, start: impl Into<String>, end: impl Into<String>) -> Self {
        self.start_anchor = Some(start.into());
        self.end_anchor = Some(end.into());
        self
    }

    /// Set what is included in the metric measurement.
    #[must_use]
    pub fn includes(mut self, includes: impl Into<String>) -> Self {
        self.includes = Some(includes.into());
        self
    }

    /// Set metric owner ("engine" or "provider").
    #[must_use]
    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// Set containing stage key for provider breakdowns.
    #[must_use]
    pub fn contained_in(mut self, stage: impl Into<String>) -> Self {
        self.contained_in = Some(stage.into());
        self
    }

    /// Set the numeric format hint (e.g. `"0.00"`).
    #[must_use]
    pub fn fmt(mut self, format: impl Into<String>) -> Self {
        self.format = Some(format.into());
        self
    }

    fn with_role(
        mut self,
        role: MetricRole,
        direction: MetricDirection,
        budget: Option<f64>,
        on_critical_path: Option<bool>,
    ) -> Self {
        self.health = Some(MetricHealth {
            role: Some(role),
            direction: Some(direction),
            budget,
            on_critical_path,
            severity_bands: None,
        });
        self
    }

    /// A lower-is-better latency with a budget. `on_critical_path` may be
    /// `None` to defer to the engine's overlap logic (e.g. STT processing).
    #[must_use]
    pub fn latency(self, budget: f64, on_critical_path: Option<bool>) -> Self {
        self.with_role(
            MetricRole::Latency,
            MetricDirection::LowerIsBetter,
            Some(budget),
            on_critical_path,
        )
    }

    /// A higher-is-better throughput rate (off the critical path).
    #[must_use]
    pub fn throughput(self, budget: f64) -> Self {
        self.with_role(
            MetricRole::Throughput,
            MetricDirection::HigherIsBetter,
            Some(budget),
            Some(false),
        )
    }

    /// A lower-is-better quality signal (off the critical path).
    #[must_use]
    pub fn quality(self, budget: f64) -> Self {
        self.with_role(
            MetricRole::Quality,
            MetricDirection::LowerIsBetter,
            Some(budget),
            Some(false),
        )
    }

    /// An internal/compute metric that never drives the verdict.
    #[must_use]
    pub fn diagnostic(self) -> Self {
        self.with_role(
            MetricRole::Diagnostic,
            MetricDirection::LowerIsBetter,
            None,
            Some(false),
        )
    }
}

/// Units understood by the generic frontend formatter.
///
/// Wire form is the `snake_case` discriminant (e.g. `"ms"`,
/// `"tokens_per_sec"`). Adding a new unit is one Rust + one TypeScript
/// switch arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricUnit {
    Ms,
    Percent,
    Ratio,
    Count,
    TokensPerSec,
    Bytes,
    Raw,
    /// Decibels: dBFS for levels, plain dB for level differences (SNR).
    Db,
}

// ── Quick-access derived helpers ────────────────────────────────────────

impl TurnMetricsSnapshot {
    /// Look up a component by stable identifier.
    pub fn component(&self, id: &str) -> Option<&TurnComponentMetrics> {
        self.components.iter().find(|c| c.component_id == id)
    }

    /// Look up the first component matching a kind. There may be more
    /// than one -- callers that care iterate `components` directly.
    pub fn component_by_kind(&self, kind: &str) -> Option<&TurnComponentMetrics> {
        self.components.iter().find(|c| c.kind == kind)
    }

    /// Time to first LLM token, derived from `llm.extras["ttft_ms"]`.
    pub fn ttft_ms(&self) -> Option<u64> {
        self.component_by_kind("llm")?
            .extras
            .get("ttft_ms")?
            .as_u64()
    }

    /// Time to first TTS audio byte, derived from `tts.extras["ttfb_ms"]`.
    /// Realtime S2S turns emit a single `realtime` component instead of
    /// `stt`/`llm`/`tts` slices; fall back to it so consumers (bench
    /// collector, session summary) see S2S TTFB too.
    pub fn ttfb_ms(&self) -> Option<u64> {
        self.component_by_kind("tts")
            .or_else(|| self.component_by_kind("realtime"))?
            .extras
            .get("ttfb_ms")?
            .as_u64()
    }

    /// Total STT latency, taken from `stt.extras["processing_ms"]` (preferred)
    /// or the STT component's `latency_ms`.
    pub fn stt_latency_ms(&self) -> Option<u64> {
        let stt = self.component_by_kind("stt")?;
        stt.extras
            .get("processing_ms")
            .and_then(serde_json::Value::as_u64)
            .or(stt.latency_ms)
    }

    /// LLM generation latency, derived from `llm.extras["generation_ms"]`
    /// (preferred) or the LLM component's `latency_ms`.
    pub fn llm_generation_ms(&self) -> Option<u64> {
        let llm = self.component_by_kind("llm")?;
        llm.extras
            .get("generation_ms")
            .and_then(serde_json::Value::as_u64)
            .or(llm.latency_ms)
    }
}

// ── Canonical stt/llm/tts descriptors ───────────────────────────────────

impl ComponentMetricDescriptor {
    /// Build a descriptor for the canonical STT component slice.
    ///
    /// Field key vocabulary mirrors `pipeline::extras::SttExtras`. The
    /// returned descriptor describes every key STT spans publish via the
    /// typed `extras_typed` helper -- adding a new STT extras field
    /// requires updating both `SttExtras` and this descriptor.
    pub fn stt_default() -> Self {
        use MetricUnit::{Count, Ms, Ratio};
        Self {
            component_id: "stt".into(),
            kind: "stt".into(),
            label: "Speech-to-Text".into(),
            fields: vec![
                // `on_critical_path: None` — conditional: for incremental /
                // streaming models processing overlaps the user's speech, so the
                // engine decides the contribution from the timeline / RTFx.
                MetricFieldDescriptor::new("processing_ms", "Processing", Ms)
                    .latency(800.0, None)
                    .anchors("speech_commit / last_chunk_in", "final_transcript")
                    .owner("provider")
                    .includes("Model inference wall time (batch: full utterance; streaming: last chunk inference)"),
                // VAD silence wait before commit — always on the critical path.
                MetricFieldDescriptor::new("endpointing_ms", "Endpointing", Ms)
                    .latency(500.0, Some(true))
                    .anchors("last_speech_frame", "turn_commit")
                    .owner("engine")
                    .includes("VAD hangover silence and configured silence_timeout_ms"),
                MetricFieldDescriptor::new("stt_audio_duration_ms", "Audio duration", Ms)
                    .diagnostic()
                    .anchors("turn_start", "turn_commit")
                    .owner("engine")
                    .includes("Total audio duration consumed by the STT stage"),
                MetricFieldDescriptor::new("stt_rtfx", "RTFx (audio/wall)", Ratio)
                    .diagnostic()
                    .anchors("turn_start", "final_transcript")
                    .owner("provider")
                    .includes("Audio duration divided by inference wall time (higher is faster)"),
                MetricFieldDescriptor::new("stt_algorithmic_delay_ms", "Algorithmic delay", Ms)
                    .diagnostic()
                    .anchors("audio_chunk_timestamp", "encoder_window_end")
                    .owner("provider")
                    .includes("Lookahead / chunking delay inherent to model architecture"),
                MetricFieldDescriptor::new("stt_effective_latency_ms", "Effective latency", Ms)
                    .diagnostic()
                    .anchors("last_speech_frame", "final_transcript")
                    .owner("engine")
                    .includes("What the user waits for: endpointing silence plus processing time"),
                MetricFieldDescriptor::new("stt_inference_count", "Inference count", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_commit")
                    .owner("provider")
                    .includes("Number of encoder passes or streaming inference steps"),
                MetricFieldDescriptor::new("stt_streaming_window_ms", "Streaming window", Ms)
                    .diagnostic()
                    .anchors("window_start", "window_end")
                    .owner("provider")
                    .includes("Buffered audio chunk length fed to streaming inference"),
            ],
        }
    }

    /// Build a descriptor for the canonical LLM component slice.
    /// Mirrors `pipeline::extras::LlmExtras`.
    pub fn llm_default() -> Self {
        use MetricUnit::{Count, Ms, Percent, Raw};
        Self {
            component_id: "llm".into(),
            kind: "llm".into(),
            label: "Language Model".into(),
            fields: vec![
                // First token — the LLM's contribution to perceived latency.
                MetricFieldDescriptor::new("ttft_ms", "Time to first token", Ms)
                    .latency(300.0, Some(true))
                    .anchors("prompt_submitted", "first_token")
                    .owner("provider")
                    .contained_in("llm_wait_ms")
                    .includes("Provider request dispatch to first non-empty generated token"),
                // Total generation streams out behind the first token; not the
                // perceived wait for a streaming pipeline.
                MetricFieldDescriptor::new("generation_ms", "Generation", Ms)
                    .diagnostic()
                    .anchors("first_token", "last_token")
                    .owner("provider")
                    .includes("Time from first generated token to stream completion"),
                MetricFieldDescriptor::new("token_count", "Tokens", Count)
                    .diagnostic()
                    .anchors("first_token", "last_token")
                    .owner("provider")
                    .includes("Total output tokens generated"),
                MetricFieldDescriptor::new("token_estimate", "Token estimate", Count)
                    .diagnostic()
                    .anchors("first_token", "last_token")
                    .owner("provider")
                    .includes("Estimated output token count when exact count unavailable"),
                MetricFieldDescriptor::new(
                    "llm_perceived_ttft_ms",
                    "Perceived TTFT (incl. scheduling)",
                    Ms,
                )
                .diagnostic()
                .anchors("turn_commit", "first_token")
                .owner("engine")
                .contained_in("llm_wait_ms")
                .includes("Turn commit to first LLM token, including scheduling and task queue wait"),
                MetricFieldDescriptor::new("llm_wait_ms", "LLM stage wait (commit -> frag)", Ms)
                    .diagnostic()
                    .anchors("turn_commit", "first_text_fragment")
                    .owner("engine")
                    .includes("Engine-measured contiguous wait from turn commit to first text fragment handed to TTS"),
                MetricFieldDescriptor::new("stage_skipped_reason", "Stage skipped reason", Raw)
                    .diagnostic()
                    .anchors("turn_commit", "turn_commit")
                    .owner("engine")
                    .includes("Why the LLM stage did not run (e.g. agent_say or speculative_accept)"),
                MetricFieldDescriptor::new("speculative_rollbacks", "Speculative Rollbacks", Count)
                    .diagnostic()
                    .anchors("turn_commit", "turn_end")
                    .owner("engine")
                    .includes("Count of speculative prediction rollbacks during turn"),
                MetricFieldDescriptor::new(
                    "speculative_tokens_generated",
                    "Speculative Tokens Generated",
                    Count,
                )
                .diagnostic()
                .anchors("turn_commit", "turn_end")
                .owner("engine")
                .includes("Total tokens speculatively generated ahead of time"),
                MetricFieldDescriptor::new(
                    "speculative_tokens_accepted",
                    "Speculative Tokens Accepted",
                    Count,
                )
                .diagnostic()
                .anchors("turn_commit", "turn_end")
                .owner("engine")
                .includes("Count of speculative tokens accepted by the verifier"),
                MetricFieldDescriptor::new(
                    "speculative_efficiency",
                    "Speculative Efficiency",
                    Percent,
                )
                .fmt("0.0")
                .diagnostic()
                .anchors("turn_commit", "turn_end")
                .owner("engine")
                .includes("Accepted speculative tokens divided by generated speculative tokens"),
            ],
        }
    }

    /// Build a descriptor for the canonical TTS component slice.
    /// Mirrors `pipeline::extras::TtsExtras`.
    pub fn tts_default() -> Self {
        use MetricUnit::{Count, Ms, Ratio, Raw};
        Self {
            component_id: "tts".into(),
            kind: "tts".into(),
            label: "Text-to-Speech".into(),
            fields: vec![
                // First audio byte — the TTS contribution to perceived latency.
                MetricFieldDescriptor::new("ttfb_ms", "Time to first byte", Ms)
                    .latency(400.0, Some(true))
                    .anchors("first_text_fragment", "first_audio_chunk")
                    .owner("provider")
                    .contained_in("tts_wait_ms")
                    .includes("First text fragment received by TTS to first raw audio chunk produced"),
                MetricFieldDescriptor::new("tts_wait_ms", "TTS stage wait (frag -> audio)", Ms)
                    .diagnostic()
                    .anchors("first_text_fragment", "first_audio_chunk")
                    .owner("engine")
                    .includes("Engine-measured contiguous TTS stage wait to first raw audio chunk"),
                MetricFieldDescriptor::new("output_wait_ms", "Output stage wait", Ms)
                    .diagnostic()
                    .anchors("first_audio_chunk", "first_sent_audio")
                    .owner("engine")
                    .includes("Engine-measured contiguous output stage wait from first audio chunk to first emitted audio frame"),
                MetricFieldDescriptor::new(
                    "commit_to_first_audio_ms",
                    "Commit to first audio",
                    Ms,
                )
                .diagnostic()
                .anchors("turn_commit", "first_sent_audio")
                .owner("engine")
                .includes("Engine-measured total post-commit wait (llm_wait_ms + tts_wait_ms + output_wait_ms)"),
                MetricFieldDescriptor::new("stage_skipped_reason", "Stage skipped reason", Raw)
                    .diagnostic()
                    .anchors("turn_commit", "turn_commit")
                    .owner("engine")
                    .includes("Why TTS stage did not run (e.g. speculative_accept)"),
                MetricFieldDescriptor::new("tts_internal_ttfb_ms", "TTS startup (pre-pacer)", Ms)
                    .diagnostic()
                    .anchors("synthesis_start", "first_audio_chunk")
                    .owner("provider")
                    .contained_in("tts_wait_ms")
                    .includes("TTS startup and first audio chunk generation before engine queueing"),
                MetricFieldDescriptor::new("synthesis_ms", "Synthesis", Ms)
                    .diagnostic()
                    .anchors("synthesis_start", "synthesis_end")
                    .owner("provider")
                    .includes("Active provider synthesis compute time excluding consumer backpressure"),
                MetricFieldDescriptor::new("audio_duration_ms", "Audio duration", Ms)
                    .diagnostic()
                    .anchors("synthesis_start", "synthesis_end")
                    .owner("provider")
                    .includes("Total duration of synthesized audio output"),
                // Synthesis slower than playback is a quality risk (underruns).
                MetricFieldDescriptor::new("rtf", "Realtime factor", Ratio)
                    .fmt("0.00")
                    .quality(0.6)
                    .anchors("synthesis_start", "synthesis_end")
                    .owner("provider")
                    .includes("Active synthesis compute time divided by generated audio duration (lower is faster)"),
                MetricFieldDescriptor::new("rtfx", "RTFx (audio/wall)", Ratio)
                    .fmt("0.00")
                    .diagnostic()
                    .anchors("synthesis_start", "synthesis_end")
                    .owner("provider")
                    .includes("Generated audio duration divided by synthesis compute time (higher is faster)"),
                MetricFieldDescriptor::new("chars", "Characters", Count)
                    .diagnostic()
                    .anchors("synthesis_start", "synthesis_end")
                    .owner("engine")
                    .includes("Characters of the agent reply fed to TTS (Unicode scalar values)"),
                MetricFieldDescriptor::new("sample_rate", "Sample rate", Count)
                    .diagnostic()
                    .anchors("synthesis_start", "synthesis_end")
                    .owner("provider")
                    .includes("Output sampling rate in Hz"),
            ],
        }
    }

    /// Build the canonical descriptor for a Realtime S2S provider slice
    /// (`OpenAI` Realtime, Gemini Live, …). Realtime sessions collapse
    /// the classic stt + llm + tts triple into a single component whose
    /// extras carry the only latencies the provider exposes: TTFB to the
    /// first audio delta, audio output duration, and accumulated input
    /// audio duration.
    pub fn realtime_default() -> Self {
        use MetricUnit::{Count, Db, Ms, Percent, Ratio, Raw};
        Self {
            component_id: "realtime".into(),
            kind: "realtime".into(),
            label: "Realtime S2S".into(),
            fields: vec![
                // Anchor → first audio delta (never the first transcript).
                MetricFieldDescriptor::new("ttfb_ms", "Time to first audio", Ms)
                    .latency(600.0, Some(true))
                    .anchors("speech_end_anchor", "first_audio_delta")
                    .owner("provider")
                    .includes("End of user speech anchor to first audio chunk from S2S provider"),
                // How late the provider noticed the user stopped, against
                // vp's own uplink observer — the S2S `endpointing_ms`.
                MetricFieldDescriptor::new("provider_endpointing_ms", "Provider endpointing", Ms)
                    .latency(500.0, Some(true))
                    .anchors("last_speech_frame", "provider_speech_stopped")
                    .owner("engine")
                    .includes("Lag between local VAD speech end and provider speech stopped notification"),
                MetricFieldDescriptor::new("first_text_ms", "Time to first transcript", Ms)
                    .diagnostic()
                    .anchors("speech_end_anchor", "first_transcript_delta")
                    .owner("provider")
                    .includes("End of speech anchor to first transcript received from provider"),
                MetricFieldDescriptor::new("ttfb_anchor_source", "TTFB anchor", Raw)
                    .diagnostic()
                    .anchors("speech_end_anchor", "speech_end_anchor")
                    .owner("engine")
                    .includes("Anchor name used for TTFB (e.g. provider_speech_stopped, vp_vad, client_speech_end)"),
                MetricFieldDescriptor::new("audio_output_ms", "Audio output", Ms)
                    .diagnostic()
                    .anchors("first_audio_delta", "audio_turn_end")
                    .owner("provider")
                    .includes("Total audio duration produced in this turn"),
                MetricFieldDescriptor::new("input_audio_ms", "Input audio", Ms)
                    .diagnostic()
                    .anchors("turn_start", "speech_end_anchor")
                    .owner("provider")
                    .includes("Total input audio duration processed for this turn"),
                MetricFieldDescriptor::new("sample_rate", "Sample rate", Count)
                    .diagnostic()
                    .anchors("turn_start", "audio_turn_end")
                    .owner("provider")
                    .includes("Output sampling rate in Hz"),
                // Quality of the user speech this turn answers, metered by
                // the uplink observer. Keys
                // match `audio::input_meter::EXTRAS_KEYS`. Diagnostic until
                // the calibration gives them budgets.
                MetricFieldDescriptor::new("input_speech_dbfs", "Input speech level", Db)
                    .diagnostic()
                    .anchors("speech_start", "speech_end")
                    .owner("engine")
                    .includes("RMS level of detected user speech in dBFS"),
                MetricFieldDescriptor::new("input_noise_dbfs", "Input noise floor", Db)
                    .diagnostic()
                    .anchors("speech_start", "speech_end")
                    .owner("engine")
                    .includes("Estimated background noise floor in dBFS"),
                MetricFieldDescriptor::new("input_snr_db", "Input SNR", Db)
                    .diagnostic()
                    .anchors("speech_start", "speech_end")
                    .owner("engine")
                    .includes("Estimated signal-to-noise ratio in dB"),
                MetricFieldDescriptor::new("input_peak_dbfs", "Input peak", Db)
                    .diagnostic()
                    .anchors("speech_start", "speech_end")
                    .owner("engine")
                    .includes("Maximum sample amplitude in dBFS"),
                MetricFieldDescriptor::new("input_clipped_ratio", "Input clipped", Percent)
                    .fmt("0.00")
                    .diagnostic()
                    .anchors("speech_start", "speech_end")
                    .owner("engine")
                    .includes("Fraction of input samples at clipping threshold"),
                MetricFieldDescriptor::new("input_speech_ratio", "Input speech ratio", Ratio)
                    .diagnostic()
                    .anchors("turn_start", "speech_end")
                    .owner("engine")
                    .includes("Proportion of audio classified as active speech"),
                MetricFieldDescriptor::new("input_speech_ms", "Input speech", Ms)
                    .diagnostic()
                    .anchors("speech_start", "speech_end")
                    .owner("engine")
                    .includes("Cumulative active speech duration metered on input"),
                MetricFieldDescriptor::new("input_level_source", "Input level source", Raw)
                    .diagnostic()
                    .anchors("speech_start", "speech_end")
                    .owner("engine")
                    .includes("Metering observer source identifier"),
            ],
        }
    }
}

impl ComponentMetricDescriptor {
    /// Descriptor for one annotate-mode audio post-processor
    /// (`component_id = "annotator:<name>"`, `kind = "annotator"`). Keys
    /// match [`crate::pipeline::annotate::AnnotatorExtras`]. Latencies are
    /// off the critical path by construction (the hub never delays audio):
    /// `latency_p50_ms` is judged against `batch_ms` (a batch that takes
    /// longer than its own duration cannot keep up) and `lag_max_ms`
    /// against the client's default prebuffer (200 ms), past which the
    /// metadata visibly trails the speech.
    pub fn annotator(name: &str, batch_ms: u64) -> Self {
        use MetricUnit::{Bytes, Count, Ms, Ratio, Raw};
        Self {
            component_id: format!("annotator:{name}"),
            kind: "annotator".into(),
            label: format!("Annotator ({name})"),
            fields: vec![
                MetricFieldDescriptor::new("latency_p50_ms", "Annotation latency (p50)", Ms)
                    .latency(batch_ms as f64, Some(false))
                    .anchors("batch_start", "batch_end")
                    .owner("engine")
                    .includes("Median batch processing latency of annotator"),
                MetricFieldDescriptor::new("lag_max_ms", "Lag behind audio (max)", Ms)
                    .latency(200.0, Some(false))
                    .anchors("chunk_timestamp", "annotation_emitted")
                    .owner("engine")
                    .includes("Maximum lag between audio chunk timestamp and annotation delivery"),
                MetricFieldDescriptor::new("latency_max_ms", "Annotation latency (max)", Ms)
                    .diagnostic()
                    .anchors("batch_start", "batch_end")
                    .owner("engine")
                    .includes("Maximum batch processing latency of annotator"),
                MetricFieldDescriptor::new("segments", "Segments", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Count of audio segments processed"),
                MetricFieldDescriptor::new("annotations", "Annotations", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Count of annotations emitted"),
                MetricFieldDescriptor::new("dropped_chunks", "Dropped chunks", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Count of audio chunks dropped by annotator queue"),
                MetricFieldDescriptor::new("timeouts", "Timeouts", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Count of batches that exceeded timeout"),
                MetricFieldDescriptor::new("skipped_segments", "Skipped (breaker)", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Count of segments skipped due to breaker trip"),
                MetricFieldDescriptor::new("annotation_kbps", "Annotation bitrate", Ratio)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Bitrate of annotation stream in kbps"),
                MetricFieldDescriptor::new("annotation_bytes", "Annotation bytes", Bytes)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Total bytes of annotation metadata produced"),
                MetricFieldDescriptor::new("breaker_open", "Breaker open", Raw)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Whether circuit breaker opened during turn"),
            ],
        }
    }
}

/// The canonical stt / llm / tts descriptor triple for the built-in
/// pipeline. Returned in pipeline order (stt -> llm -> tts).
///
/// Used by `setup.rs` to populate `SessionInfo.metric_descriptors` for
/// every session built from the standard pipeline; the post-processors'
/// and annotators' own descriptors are appended to them.
pub fn default_descriptors() -> Vec<ComponentMetricDescriptor> {
    vec![
        ComponentMetricDescriptor::stt_default(),
        ComponentMetricDescriptor::llm_default(),
        ComponentMetricDescriptor::tts_default(),
    ]
}

/// Descriptor list for sessions backed by a Realtime S2S provider.
///
/// Single-component (the provider owns STT+LLM+TTS), matched by
/// `crate::transport::serve::realtime_runner` when it emits per-turn
/// `TurnMetricsSnapshot` payloads.
pub fn realtime_descriptors() -> Vec<ComponentMetricDescriptor> {
    vec![ComponentMetricDescriptor::realtime_default()]
}

/// All canonical component metric descriptors across stages and utilities.
pub fn all_canonical_descriptors() -> Vec<ComponentMetricDescriptor> {
    vec![
        ComponentMetricDescriptor::stt_default(),
        ComponentMetricDescriptor::llm_default(),
        ComponentMetricDescriptor::tts_default(),
        ComponentMetricDescriptor::realtime_default(),
        ComponentMetricDescriptor::output(),
        ComponentMetricDescriptor::audio_post("post_processor"),
        ComponentMetricDescriptor::annotator("annotator", 100),
    ]
}

// ── TurnRecorder + ComponentSpan ────────────────────────────────────────

/// Per-turn registry of component spans. Owned by the orchestrator;
/// passed by reference into per-component task code so each can open
/// spans without re-implementing timing.
///
/// `Clone` is cheap (Arc) so the recorder can be shared across the STT,
/// LLM, and TTS tasks for a single utterance.
#[derive(Debug, Clone)]
pub struct TurnRecorder {
    inner: Arc<Mutex<TurnRecorderInner>>,
}

#[derive(Debug)]
struct TurnRecorderInner {
    turn_start: Instant,
    components: Vec<TurnComponentMetrics>,
}

impl Default for TurnRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnRecorder {
    /// Stamp the turn start to `Instant::now()`.
    pub fn new() -> Self {
        Self::new_at(Instant::now())
    }

    /// Stamp the turn start to an explicit anchor (`commit_start`).
    pub fn new_at(turn_start: Instant) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TurnRecorderInner {
                turn_start,
                components: Vec::new(),
            })),
        }
    }

    /// Open a new span for `component_id` of `kind`.
    ///
    /// Returns a RAII guard that finishes the span on `Drop` (or when
    /// `finish()` is called explicitly). Insertion order matches call
    /// order, which matches pipeline order.
    pub fn begin(&self, component_id: &str, kind: &str) -> ComponentSpan<'_> {
        let mut inner = self.inner.lock();
        let started_at_ms = inner.turn_start.elapsed().as_millis() as u64;
        let index = inner.components.len();
        inner.components.push(TurnComponentMetrics {
            component_id: component_id.to_string(),
            kind: kind.to_string(),
            started_at_ms,
            ..Default::default()
        });
        ComponentSpan {
            recorder: self,
            index,
            finished: false,
        }
    }

    /// Render the snapshot. `total_ms` is the elapsed time from
    /// `Instant::now()` against the turn start; `interrupted` reflects
    /// whether the caller short-circuited the turn.
    pub fn snapshot(
        &self,
        utterance_id: u32,
        interrupted: bool,
        llm_response_text: Option<String>,
    ) -> TurnMetricsSnapshot {
        let inner = self.inner.lock();
        let total_ms = inner.turn_start.elapsed().as_millis() as u64;
        TurnMetricsSnapshot {
            utterance_id,
            total_ms,
            interrupted,
            initiator: TurnInitiator::default(),
            components: inner.components.clone(),
            llm_response_text,
        }
    }

    /// Read elapsed-from-turn-start in ms. Internal helper used by
    /// `ComponentSpan::finish()`.
    fn elapsed_ms(&self) -> u64 {
        let inner = self.inner.lock();
        inner.turn_start.elapsed().as_millis() as u64
    }

    fn with_component<R>(&self, index: usize, f: impl FnOnce(&mut TurnComponentMetrics) -> R) -> R {
        let mut inner = self.inner.lock();
        let component = inner
            .components
            .get_mut(index)
            .expect("ComponentSpan index out of range -- recorder corrupted");
        f(component)
    }
}

/// RAII handle for a component's per-turn span.
///
/// `finish()` is idempotent and runs from `Drop` if the caller forgot.
/// A span that goes out of scope without `finish()` and without a
/// matching update still records `started_at_ms`; `finished_at_ms`
/// will be populated from the Drop-time clock -- that signal lets the
/// snapshot show partial / interrupted activity rather than silently
/// truncating it.
#[derive(Debug)]
pub struct ComponentSpan<'a> {
    recorder: &'a TurnRecorder,
    index: usize,
    finished: bool,
}

impl ComponentSpan<'_> {
    pub fn set_provider(&mut self, provider: impl Into<String>) {
        let provider = provider.into();
        self.recorder
            .with_component(self.index, |c| c.provider = Some(provider));
    }

    pub fn set_cold_start(&mut self, cold_start: bool) {
        self.recorder
            .with_component(self.index, |c| c.cold_start = cold_start);
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.recorder
            .with_component(self.index, |c| c.error = Some(error));
    }

    /// Mutable access to the free-form extras map for ad-hoc keys.
    /// Convention: `snake_case`, <= 32 bytes per key.
    pub fn extras_mut<R>(
        &mut self,
        f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> R,
    ) -> R {
        self.recorder
            .with_component(self.index, |c| f(&mut c.extras))
    }

    /// Serialise a typed extras struct into the component's extras map.
    /// Each key in the serialised value overwrites any existing entry
    /// with the same name.
    pub fn extras_typed<T: Serialize>(&mut self, value: &T) {
        let serialised =
            serde_json::to_value(value).expect("typed extras must serialise into a JSON object");
        let map = match serialised {
            serde_json::Value::Object(map) => map,
            other => {
                debug_assert!(false, "extras_typed expects a JSON object, got {other:?}");
                return;
            }
        };
        self.recorder.with_component(self.index, |c| {
            for (k, v) in map {
                c.extras.insert(k, v);
            }
        });
    }

    /// Stamp `finished_at_ms` and `latency_ms`. Idempotent -- calling
    /// twice (e.g. explicit `finish()` then `Drop`) is a no-op the
    /// second time.
    pub fn finish(mut self) {
        self.finish_in_place();
    }

    /// Stamp an explicit `(started_at_ms, finished_at_ms)` window and derive
    /// `latency_ms = finished_at_ms.map(|f| f.saturating_sub(started_at_ms))`.
    /// Prevents `Drop` from overwriting `finished_at_ms` with the teardown time.
    pub fn finish_with_window(mut self, started_at_ms: u64, finished_at_ms: Option<u64>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.recorder.with_component(self.index, |c| {
            c.started_at_ms = started_at_ms;
            c.finished_at_ms = finished_at_ms;
            c.latency_ms = finished_at_ms.map(|f| f.saturating_sub(started_at_ms));
        });
    }

    fn finish_in_place(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let now_ms = self.recorder.elapsed_ms();
        self.recorder.with_component(self.index, |c| {
            c.finished_at_ms = Some(now_ms);
            c.latency_ms = Some(now_ms.saturating_sub(c.started_at_ms));
        });
    }
}

impl Drop for ComponentSpan<'_> {
    fn drop(&mut self) {
        self.finish_in_place();
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/metrics.rs"]
mod tests;
