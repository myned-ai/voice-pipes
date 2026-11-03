//! Streaming-phase throughput metrics -- makes "audio slower than real
//! time" and "post-processor over budget" visible.
//!
//! Latency (TTFB, TTFT) and failure metrics alone miss a chain that takes
//! 58 ms per 41 ms chunk: it never fails and starts on time, yet the
//! client starves after its prebuffer. Two per-turn recorders close that gap:
//!
//! * [`OutputThroughput`] -- the `output` turn component: wall time of the
//!   streaming phase ÷ audio duration, measured both where audio is
//!   *received* from the provider/TTS (`source_rtf`) and where it is
//!   *forwarded* to the transport after the inline chain (`emit_rtf`); the
//!   difference is what the post-audio chain added (`chain_overhead_ms`).
//! * [`ChainStats`] -- one `audio_post:<name>` component per inline
//!   processor: per-call latency (p50/max), timeouts, failures and
//!   `budget_ratio` (mean latency ÷ mean chunk audio duration; > 1 means the
//!   processor cannot keep up with its own input), plus the sustained-
//!   slowness `warn!` that was missing from the worker log.
//!
//! Naming follows vp's `rtf` convention: processing time ÷ audio duration,
//! > 1.0 = slower than real time, lower is better.

use std::collections::HashMap;
use std::time::Instant;

use super::metrics::{
    ComponentMetricDescriptor, MetricFieldDescriptor, MetricUnit, SeverityBands,
    TurnComponentMetrics,
};

/// Consecutive over-budget chunks before the "cannot keep up" warning.
pub const SLOWNESS_WARN_AFTER_CHUNKS: u32 = 10;
/// Recent-latency ring used for the p50 (bounded per turn).
const LATENCY_RING: usize = 128;

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ── Output throughput ───────────────────────────────────────

/// Streaming-phase throughput of one turn. Fed by the engine at two points
/// per chunk: [`on_source`](Self::on_source) when a chunk arrives from the
/// provider/TTS (pre-chain) and [`on_emit`](Self::on_emit) when it is handed
/// to the transport (post-chain).
#[derive(Debug, Default, Clone)]
pub struct OutputThroughput {
    first_source: Option<Instant>,
    last_source: Option<Instant>,
    first_emit: Option<Instant>,
    last_emit: Option<Instant>,
    audio_ms: u64,
}

impl OutputThroughput {
    /// A chunk of `chunk_audio_ms` audio was received (pre-chain). Chunks
    /// without audio (EOU sentinels) are ignored.
    pub fn on_source(&mut self, chunk_audio_ms: u64) {
        if chunk_audio_ms == 0 {
            return;
        }
        let now = Instant::now();
        self.first_source.get_or_insert(now);
        self.last_source = Some(now);
        self.audio_ms += chunk_audio_ms;
    }

    /// A chunk with audio was handed to the transport (post-chain).
    pub fn on_emit(&mut self) {
        let now = Instant::now();
        self.first_emit.get_or_insert(now);
        self.last_emit = Some(now);
    }

    /// Audio received so far, ms.
    pub fn audio_ms(&self) -> u64 {
        self.audio_ms
    }

    /// Timestamp of the first non-empty chunk received from the provider/TTS.
    pub fn first_source_instant(&self) -> Option<Instant> {
        self.first_source
    }

    /// Timestamp of the first non-empty chunk emitted after the inline chain.
    pub fn first_emit_instant(&self) -> Option<Instant> {
        self.first_emit
    }

    /// Engine-owned stage 4 (`t_first_emit - t_first_tts_audio`, ms, 0.1 ms
    /// resolution) measured across the inline post-audio chain and `audio_tx`
    /// send for the first non-empty audio chunk.
    pub fn output_wait_ms(&self) -> Option<f64> {
        self.first_source
            .zip(self.first_emit)
            .map(|(s, e)| super::extras::round_tenths_ms(e.saturating_duration_since(s)))
    }

    fn span_ms(first: Option<Instant>, last: Option<Instant>) -> u64 {
        match (first, last) {
            (Some(f), Some(l)) => l.saturating_duration_since(f).as_millis() as u64,
            _ => 0,
        }
    }

    /// Wall time first → last emitted chunk, ms.
    pub fn emit_wall_ms(&self) -> u64 {
        Self::span_ms(self.first_emit, self.last_emit)
    }

    /// Wall time first → last received chunk, ms.
    pub fn source_wall_ms(&self) -> u64 {
        Self::span_ms(self.first_source, self.last_source)
    }

    /// `emit_wall_ms ÷ audio_ms` (2 decimals). `None` without audio.
    pub fn emit_rtf(&self) -> Option<f64> {
        (self.audio_ms > 0).then(|| round2(self.emit_wall_ms() as f64 / self.audio_ms as f64))
    }

    /// `source_wall_ms ÷ audio_ms` (2 decimals). `None` without audio.
    pub fn source_rtf(&self) -> Option<f64> {
        (self.audio_ms > 0).then(|| round2(self.source_wall_ms() as f64 / self.audio_ms as f64))
    }

    /// Wall time the chain added between receiving and forwarding, ms (≥ 0).
    pub fn chain_overhead_ms(&self) -> u64 {
        self.emit_wall_ms().saturating_sub(self.source_wall_ms())
    }

    /// The `output` component (`emit_rtf`, `source_rtf`, `chain_overhead_ms`,
    /// `audio_output_ms`). `None` when the turn produced no audio. Emitted for
    /// interrupted turns too (over the span delivered so far).
    pub fn finish(&self) -> Option<TurnComponentMetrics> {
        self.finish_at(None, None)
    }

    /// Like [`Self::finish`], but stamps `started_at_ms` / `finished_at_ms`
    /// relative to `turn_start` when provided and attaches
    /// `commit_to_first_audio_ms`.
    pub fn finish_at(
        &self,
        turn_start: Option<Instant>,
        commit_to_first_audio_ms: Option<f64>,
    ) -> Option<TurnComponentMetrics> {
        let emit_rtf = self.emit_rtf()?;
        let source_rtf = self.source_rtf().unwrap_or(0.0);
        let mut extras = serde_json::Map::new();
        extras.insert("emit_rtf".into(), serde_json::json!(emit_rtf));
        extras.insert("source_rtf".into(), serde_json::json!(source_rtf));
        extras.insert(
            "chain_overhead_ms".into(),
            serde_json::json!(self.chain_overhead_ms()),
        );
        extras.insert("audio_output_ms".into(), serde_json::json!(self.audio_ms));
        if let Some(wait_ms) = self.output_wait_ms() {
            extras.insert("output_wait_ms".into(), serde_json::json!(wait_ms));
        }
        if let Some(commit_ms) = commit_to_first_audio_ms {
            extras.insert(
                "commit_to_first_audio_ms".into(),
                serde_json::json!(commit_ms),
            );
        }
        let started_at_ms = match (turn_start, self.first_source) {
            (Some(ts), Some(fs)) => fs.saturating_duration_since(ts).as_millis() as u64,
            _ => 0,
        };
        let finished_at_ms = match (turn_start, self.last_emit) {
            (Some(ts), Some(le)) => Some(le.saturating_duration_since(ts).as_millis() as u64),
            _ => None,
        };
        Some(TurnComponentMetrics {
            component_id: "output".into(),
            kind: "output".into(),
            started_at_ms,
            finished_at_ms,
            latency_ms: Some(self.emit_wall_ms()),
            provider: None,
            cold_start: false,
            error: None,
            extras,
        })
    }
}

// ── Post-audio chain stats ──────────────────────────────────

#[derive(Debug, Default, Clone)]
struct ProcessorStats {
    calls: u64,
    latency_sum_ms: u64,
    latency_max_ms: u64,
    /// Most recent latencies for the p50 (bounded).
    recent_ms: Vec<u64>,
    recent_next: usize,
    timeouts: u64,
    failures: u64,
    /// Σ chunk audio durations the processor was called with.
    audio_ms_sum: u64,
    consecutive_over_budget: u32,
    /// `budget_ratio` at the last warning; warn again only when it doubles.
    warned_ratio: Option<f64>,
    first_call: Option<Instant>,
    last_finish: Option<Instant>,
}

impl ProcessorStats {
    fn p50_ms(&self) -> Option<u64> {
        if self.recent_ms.is_empty() {
            return None;
        }
        let mut v = self.recent_ms.clone();
        v.sort_unstable();
        Some(v[v.len() / 2])
    }

    fn mean_latency_ms(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.latency_sum_ms as f64 / self.calls as f64
        }
    }

    fn mean_chunk_ms(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.audio_ms_sum as f64 / self.calls as f64
        }
    }

    /// Mean latency ÷ mean chunk audio duration (> 1 = cannot keep up).
    fn budget_ratio(&self) -> Option<f64> {
        (self.audio_ms_sum > 0).then(|| round2(self.mean_latency_ms() / self.mean_chunk_ms()))
    }
}

/// Outcome of one `AudioPostProcessor::process` call, as seen by the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    Ok,
    Timeout,
    Failure,
}

/// Per-turn statistics for the inline post-audio chain, one entry per
/// processor (keyed by name, in first-call order).
#[derive(Debug, Default, Clone)]
pub struct ChainStats {
    order: Vec<String>,
    per: HashMap<String, ProcessorStats>,
}

impl ChainStats {
    /// Record one call. `chunk_audio_ms` is the audio duration of the chunk
    /// the processor was given (0 for sentinels). Returns `Some(message)`
    /// when the sustained-slowness warning should be logged.
    pub fn record(
        &mut self,
        processor: &str,
        latency_ms: u64,
        chunk_audio_ms: u64,
        outcome: CallOutcome,
    ) -> Option<String> {
        if !self.per.contains_key(processor) {
            self.order.push(processor.to_string());
        }
        let s = self.per.entry(processor.to_string()).or_default();
        let now = Instant::now();
        let call_start = now
            .checked_sub(std::time::Duration::from_millis(latency_ms))
            .unwrap_or(now);
        s.first_call.get_or_insert(call_start);
        s.last_finish = Some(now);
        s.calls += 1;
        s.latency_sum_ms += latency_ms;
        s.latency_max_ms = s.latency_max_ms.max(latency_ms);
        if s.recent_ms.len() < LATENCY_RING {
            s.recent_ms.push(latency_ms);
        } else {
            s.recent_ms[s.recent_next] = latency_ms;
            s.recent_next = (s.recent_next + 1) % LATENCY_RING;
        }
        s.audio_ms_sum += chunk_audio_ms;
        match outcome {
            CallOutcome::Ok => {}
            CallOutcome::Timeout => s.timeouts += 1,
            CallOutcome::Failure => s.failures += 1,
        }

        // Sustained slowness: N consecutive chunks where the call took
        // longer than the audio it processed.
        if chunk_audio_ms > 0 && latency_ms > chunk_audio_ms {
            s.consecutive_over_budget += 1;
        } else {
            s.consecutive_over_budget = 0;
        }
        if s.consecutive_over_budget < SLOWNESS_WARN_AFTER_CHUNKS {
            return None;
        }
        let ratio = s.budget_ratio()?;
        let should_warn = s.warned_ratio.is_none_or(|w| ratio >= w * 2.0);
        if !should_warn {
            return None;
        }
        s.warned_ratio = Some(ratio);
        Some(format!(
            "audio post-processor \"{processor}\" cannot keep up: p50 {} ms per {} ms chunk \
             (budget_ratio {ratio:.2}) — audio will fall behind playback",
            s.p50_ms().unwrap_or(0),
            s.mean_chunk_ms().round() as u64,
        ))
    }

    pub fn is_empty(&self) -> bool {
        self.per.is_empty()
    }

    /// One `audio_post:<name>` component per processor, in first-call order.
    pub fn components(&self) -> Vec<TurnComponentMetrics> {
        self.components_at(None)
    }

    /// Like [`Self::components`], but stamps `started_at_ms` / `finished_at_ms`
    /// relative to `turn_start` when provided.
    pub fn components_at(&self, turn_start: Option<Instant>) -> Vec<TurnComponentMetrics> {
        self.order
            .iter()
            .filter_map(|name| {
                let s = self.per.get(name)?;
                let mut extras = serde_json::Map::new();
                extras.insert("calls".into(), serde_json::json!(s.calls));
                if let Some(p50) = s.p50_ms() {
                    extras.insert("latency_p50_ms".into(), serde_json::json!(p50));
                }
                extras.insert("latency_max_ms".into(), serde_json::json!(s.latency_max_ms));
                extras.insert("timeouts".into(), serde_json::json!(s.timeouts));
                extras.insert("failures".into(), serde_json::json!(s.failures));
                if let Some(r) = s.budget_ratio() {
                    extras.insert("budget_ratio".into(), serde_json::json!(r));
                }
                let started_at_ms = match (turn_start, s.first_call) {
                    (Some(ts), Some(fc)) => fc.saturating_duration_since(ts).as_millis() as u64,
                    _ => 0,
                };
                let finished_at_ms = match (turn_start, s.last_finish) {
                    (Some(ts), Some(lf)) => {
                        Some(lf.saturating_duration_since(ts).as_millis() as u64)
                    }
                    _ => None,
                };
                Some(TurnComponentMetrics {
                    component_id: format!("audio_post:{name}"),
                    kind: "audio_post".into(),
                    started_at_ms,
                    finished_at_ms,
                    latency_ms: Some(s.latency_sum_ms),
                    provider: Some(name.clone()),
                    cold_start: false,
                    error: None,
                    extras,
                })
            })
            .collect()
    }
}

// ── Descriptors ─────────────────────────────────────────────

impl MetricFieldDescriptor {
    /// A lower-is-better realtime factor (`wall ÷ audio`) judged against
    /// `budget` with explicit absolute severity thresholds.
    fn realtime_factor(self, budget: f64, warn_at: f64, moderate_at: f64, severe_at: f64) -> Self {
        let mut d = self.quality(budget); // Quality = lower-is-better + budget
        if let Some(h) = d.health.as_mut() {
            h.role = Some(super::metrics::MetricRole::Throughput);
            h.on_critical_path = Some(true);
            h.severity_bands = Some(SeverityBands {
                warn: warn_at / budget,
                moderate: moderate_at / budget,
                severe: severe_at / budget,
            });
        }
        d.fmt("0.00")
    }
}

impl ComponentMetricDescriptor {
    /// Descriptor for the `output` component (both engines).
    pub fn output() -> Self {
        use MetricUnit::{Ms, Ratio};
        Self {
            component_id: "output".into(),
            kind: "output".into(),
            label: "Output (streaming phase)".into(),
            fields: vec![
                // > 1.0 = the server emits audio slower than it plays.
                MetricFieldDescriptor::new("emit_rtf", "Emit realtime factor", Ratio)
                    .realtime_factor(0.9, 0.9, 1.0, 1.2)
                    .anchors("first_sent_audio", "last_sent_audio")
                    .owner("engine")
                    .includes("Wall time to emit audio divided by audio duration"),
                MetricFieldDescriptor::new("source_rtf", "Source realtime factor", Ratio)
                    .fmt("0.00")
                    .diagnostic()
                    .anchors("synthesis_start", "synthesis_end")
                    .owner("engine")
                    .includes(
                        "Whole-utterance source production RTF (including inter-sentence gaps)",
                    ),
                // Budget is 10 % of the turn's audio -- relative to another
                // field, so the report engine judges it; here it
                // is display-only.
                MetricFieldDescriptor::new("chain_overhead_ms", "Post-chain overhead", Ms)
                    .diagnostic()
                    .anchors("first_audio_chunk", "last_sent_audio")
                    .owner("engine")
                    .includes("Wall time to emit audio minus source production time"),
                MetricFieldDescriptor::new("audio_output_ms", "Audio output", Ms)
                    .diagnostic()
                    .anchors("first_sent_audio", "last_sent_audio")
                    .owner("engine")
                    .includes("Total audio duration emitted to transport"),
                MetricFieldDescriptor::new("output_wait_ms", "Output stage wait", Ms)
                    .diagnostic()
                    .anchors("first_audio_chunk", "first_sent_audio")
                    .owner("engine")
                    .includes("Time from first raw audio chunk to first packet sent to transport"),
                MetricFieldDescriptor::new("commit_to_first_audio_ms", "Commit to first audio", Ms)
                    .diagnostic()
                    .anchors("turn_commit", "first_sent_audio")
                    .owner("engine")
                    .includes(
                        "Total post-commit wait (llm_wait_ms + tts_wait_ms + output_wait_ms)",
                    ),
            ],
        }
    }

    /// Descriptor for one inline post-processor's `audio_post:<name>`
    /// component.
    pub fn audio_post(name: &str) -> Self {
        use MetricUnit::{Count, Ms, Ratio};
        Self {
            component_id: format!("audio_post:{name}"),
            kind: "audio_post".into(),
            label: format!("Post-processor ({name})"),
            fields: vec![
                // mean latency ÷ mean chunk duration; ≥ 1.0 cannot keep up.
                MetricFieldDescriptor::new("budget_ratio", "Budget ratio", Ratio)
                    .realtime_factor(0.8, 0.8, 0.9, 1.0)
                    .anchors("chunk_in", "chunk_out")
                    .owner("engine")
                    .includes("Mean chunk processing latency divided by chunk duration"),
                // Budget is the mean chunk duration (turn-relative), judged
                // by the report engine; display-only here.
                MetricFieldDescriptor::new("latency_p50_ms", "Call latency (p50)", Ms)
                    .diagnostic()
                    .anchors("chunk_in", "chunk_out")
                    .owner("engine")
                    .includes("Median post-processor call latency"),
                MetricFieldDescriptor::new("latency_max_ms", "Call latency (max)", Ms)
                    .diagnostic()
                    .anchors("chunk_in", "chunk_out")
                    .owner("engine")
                    .includes("Maximum post-processor call latency"),
                MetricFieldDescriptor::new("calls", "Calls", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Number of invocations of the post-processor"),
                MetricFieldDescriptor::new("timeouts", "Timeouts", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Count of calls that exceeded execution budget"),
                MetricFieldDescriptor::new("failures", "Failures", Count)
                    .diagnostic()
                    .anchors("turn_start", "turn_end")
                    .owner("engine")
                    .includes("Count of calls returning an error"),
            ],
        }
    }
}

#[cfg(test)]
#[path = "tests/throughput.rs"]
mod tests;
