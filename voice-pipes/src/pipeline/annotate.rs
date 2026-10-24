//! Audio Annotators -- off-the-critical-path per-audio metadata.
//!
//! The inline [`AudioPostProcessor`](super::post_audio::AudioPostProcessor)
//! chain is awaited **per chunk before the chunk is forwarded**, which is
//! right for processors that mutate samples and wrong for processors that
//! only *derive metadata* (blendshapes, visemes, phonemes, emotion, VU
//! levels): audio can never leave the worker faster than the processor's
//! per-call latency, and a dead service throttles speech to its timeout.
//!
//! An [`AudioAnnotator`] instead receives a **copy** of the outbound audio
//! through a per-session [`hub::AnnotatorHub`]: the hot path adds one
//! non-blocking queue push, the hub coalesces audio into `batch_ms`
//! segments tagged with `utterance_id` + `start_ms`, calls the annotator
//! with bounded concurrency, and forwards results as the ordered
//! `audio_annotation` wire frame ([`ServerMessage::Annotation`]). Timeouts
//! drop a batch, a circuit breaker skips a failing service for a cooldown,
//! barge-in cancels pending work, and a warm-up call at session start
//! removes the cold-start hole -- audio is never delayed by any of it.
//!
//! Configured through the existing `[[audio_post_processor]]` table with
//! `mode = "annotate"` (see [`crate::config::AudioPostProcessorConfig`]).
//! See the design doc and author guide for audio annotators.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::config::{AnnotatorConfig, AudioPostProcessorConfig, PipelineConfig, PostProcessorMode};

pub mod hub;

pub use hub::{AnnotatorHubs, TurnAnnotatorMetrics};

// ── Data types ──────────────────────────────────────────────

/// One coalesced span of outbound audio for one utterance.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSegment {
    pub utterance_id: u32,
    /// Offset of the first sample from the utterance's first sample, in ms
    /// of audio (not wall time). Matches the client's playback-clock offset
    /// space, so annotations can be placed without any clock arithmetic.
    pub start_ms: u64,
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
    /// True on the last segment of the utterance (may carry zero samples).
    pub end_of_utterance: bool,
}

impl AudioSegment {
    /// Duration of the segment's audio in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        let frames = self.samples.len() as u64 / u64::from(self.channels.max(1));
        if self.sample_rate == 0 {
            return 0;
        }
        frames * 1000 / u64::from(self.sample_rate)
    }
}

/// Metadata about a span of one utterance. `extras` is opaque to vp.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioAnnotation {
    pub utterance_id: u32,
    /// Utterance-offset (ms of audio) of the first described sample.
    pub start_ms: u64,
    pub duration_ms: u64,
    /// Free-form discriminator chosen by the service, e.g. `"blendshapes"`,
    /// `"visemes"`, `"phonemes"`. vp never interprets it.
    pub kind: String,
    /// Opaque JSON, forwarded verbatim to the client.
    pub extras: serde_json::Value,
}

/// What the hub feeds an annotator's command stream.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotatorCommand {
    Segment(AudioSegment),
    /// Barge-in / flush: drop everything queued for `utterance_id` (and any
    /// earlier utterance).
    Cancel {
        utterance_id: u32,
    },
    /// Session start: process a synthetic segment to warm caches; any output
    /// is discarded by the hub (it carries `utterance_id` 0, which real
    /// turns never use).
    Warmup {
        sample_rate: u32,
        channels: u16,
    },
}

/// Utterance id reserved for warm-up traffic; annotations for it are
/// discarded by the hub. Real turns start at 1 in both engines.
pub const WARMUP_UTTERANCE_ID: u32 = 0;

// ── Error ───────────────────────────────────────────────────

#[derive(Debug)]
pub struct AnnotatorError {
    pub annotator: String,
    pub message: String,
}

impl AnnotatorError {
    pub fn new(annotator: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            annotator: annotator.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for AnnotatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "audio annotator \"{}\": {}",
            self.annotator, self.message
        )
    }
}

impl std::error::Error for AnnotatorError {}

// ── Trait ───────────────────────────────────────────────────

/// A metadata producer that runs **off** the audio path.
///
/// Why a stream rather than an async `process(segment)`: the stream is what
/// makes streaming gRPC natural and lets a service decide its own batching
/// and context; the unary fallback is an adapter *inside* the gRPC
/// implementation, so service authors never see two contracts.
#[async_trait]
pub trait AudioAnnotator: Send + Sync {
    /// Human-readable name for logging and metrics (`annotator:<name>`).
    fn name(&self) -> &str;

    /// Open a per-session annotation stream. `commands` yields batched audio
    /// in order (plus cancel / warm-up); the implementation sends annotations
    /// on `out` whenever it has them -- any time after the segment, any
    /// granularity. Returning ends the stream (the hub re-opens it after the
    /// breaker cooldown while the session is alive). Must not block on `out`
    /// beyond its bounded capacity; the hub drains it continuously.
    async fn run(
        &self,
        session_id: String,
        commands: mpsc::Receiver<AnnotatorCommand>,
        out: mpsc::Sender<AudioAnnotation>,
    ) -> Result<(), AnnotatorError>;
}

/// One configured annotate-mode entry: the annotator plus its hub tuning.
#[derive(Clone)]
pub struct AnnotatorSpec {
    pub annotator: Arc<dyn AudioAnnotator>,
    pub config: AnnotatorConfig,
}

impl fmt::Debug for AnnotatorSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnnotatorSpec")
            .field("annotator", &self.annotator.name())
            .field("config", &self.config)
            .finish()
    }
}

/// Build every `[[audio_post_processor]]` entry with `mode = "annotate"`.
/// Inline entries are skipped here and built by
/// [`super::post_audio::build_audio_post_processors`].
pub fn build_audio_annotators(cfg: &PipelineConfig) -> Result<Vec<AnnotatorSpec>, anyhow::Error> {
    let factories = super::post_audio::audio_post_processor_factories();
    let mut specs = Vec::new();
    for proc_cfg in &cfg.audio_post_processor {
        if proc_cfg.mode != PostProcessorMode::Annotate {
            continue;
        }
        let factory = factories
            .iter()
            .find(|f| f.name() == proc_cfg.processor_type)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown or disabled audio post-processor type: \"{}\"",
                    proc_cfg.processor_type,
                )
            })?;
        let annotator = factory.build_annotator(proc_cfg)?;
        specs.push(AnnotatorSpec {
            annotator,
            config: proc_cfg.annotator_config(),
        });
    }
    Ok(specs)
}

// ── Per-turn metrics extras ─────────────────────────────────

/// Typed `extras` for the `annotator:<name>` turn component
/// (`kind = "annotator"`). Keys match
/// [`crate::pipeline::metrics::ComponentMetricDescriptor::annotator`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AnnotatorExtras {
    /// Segments handed to the annotator this turn.
    pub segments: u64,
    /// Annotations forwarded to the client this turn.
    pub annotations: u64,
    /// Median segment-sent → annotation-received latency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_max_ms: Option<u64>,
    /// Max over annotations of `audio_emitted_ms_at_receipt − (start_ms +
    /// duration_ms)`: how far behind the audio the metadata runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lag_max_ms: Option<u64>,
    /// Chunks the tee had to drop because the annotator queue was full.
    pub dropped_chunks: u64,
    /// Segments that received no annotation within `timeout_ms`.
    pub timeouts: u64,
    /// Segments skipped while the circuit breaker was open.
    pub skipped_segments: u64,
    /// Serialized annotation `extras` bytes sent to the client this turn.
    /// Excludes the fixed `audio_annotation` envelope, which is
    /// `annotations × envelope_size`.
    pub annotation_bytes: u64,
    /// `annotation_bytes` as kbit/s over the turn's audio -- the figure to
    /// compare against the audio downlink itself (PCM i16 at 24 kHz is
    /// 384 kbit/s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation_kbps: Option<f64>,
    /// Breaker state at turn end.
    pub breaker_open: bool,
}

// ── Built-in: noop ──────────────────────────────────────────

/// Consumes segments and emits nothing. Exercises the hub without a service.
#[derive(Debug, Default)]
pub struct NoopAnnotator;

#[async_trait]
impl AudioAnnotator for NoopAnnotator {
    fn name(&self) -> &'static str {
        "noop"
    }

    async fn run(
        &self,
        _session_id: String,
        mut commands: mpsc::Receiver<AnnotatorCommand>,
        _out: mpsc::Sender<AudioAnnotation>,
    ) -> Result<(), AnnotatorError> {
        while commands.recv().await.is_some() {}
        Ok(())
    }
}

// ── Built-in: level ─────────────────────────────────────────

/// Frames per second of the built-in level annotator's output.
pub const LEVEL_FPS: u32 = 25;

/// Generic loudness annotator: per segment, `LEVEL_FPS` frames of
/// `[rms, peak]` in `0..=1`. Domain-agnostic ("VU levels") and cheap, so it
/// doubles as the reference `AudioAnnotator` and lets the Playground exercise
/// the full annotate path (batching, wire frame, metrics) without any
/// external service. Kind: `"audio_level"`.
#[derive(Debug, Default)]
pub struct LevelAnnotator;

impl LevelAnnotator {
    /// Compute the `[rms, peak]` frames for one segment.
    pub fn frames(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<[f32; 2]> {
        let channels = usize::from(channels.max(1));
        if sample_rate == 0 || samples.is_empty() {
            return Vec::new();
        }
        let frame_len = (sample_rate as usize / LEVEL_FPS as usize).max(1) * channels;
        samples
            .chunks(frame_len)
            .map(|frame| {
                let mut sum_sq = 0.0f64;
                let mut peak = 0.0f32;
                for s in frame {
                    let v = f32::from(*s) / 32768.0;
                    sum_sq += f64::from(v * v);
                    peak = peak.max(v.abs());
                }
                let rms = (sum_sq / frame.len() as f64).sqrt() as f32;
                [rms, peak]
            })
            .collect()
    }

    /// Build the annotation for one segment (`None` for empty segments).
    pub fn annotate(seg: &AudioSegment) -> Option<AudioAnnotation> {
        let frames = Self::frames(&seg.samples, seg.sample_rate, seg.channels);
        if frames.is_empty() {
            return None;
        }
        Some(AudioAnnotation {
            utterance_id: seg.utterance_id,
            start_ms: seg.start_ms,
            duration_ms: seg.duration_ms(),
            kind: "audio_level".to_string(),
            extras: serde_json::json!({
                "fps": LEVEL_FPS,
                "names": ["rms", "peak"],
                "frames": frames,
            }),
        })
    }
}

#[async_trait]
impl AudioAnnotator for LevelAnnotator {
    fn name(&self) -> &'static str {
        "level"
    }

    async fn run(
        &self,
        _session_id: String,
        mut commands: mpsc::Receiver<AnnotatorCommand>,
        out: mpsc::Sender<AudioAnnotation>,
    ) -> Result<(), AnnotatorError> {
        while let Some(cmd) = commands.recv().await {
            if let AnnotatorCommand::Segment(seg) = cmd
                && let Some(ann) = Self::annotate(&seg)
                && out.send(ann).await.is_err()
            {
                break;
            }
        }
        Ok(())
    }
}

/// Inline counterpart of [`LevelAnnotator`]: attaches
/// `extras.audio_level = { rms, peak }` per chunk.
#[derive(Debug, Default)]
pub struct LevelPostProcessor;

#[async_trait]
impl super::post_audio::AudioPostProcessor for LevelPostProcessor {
    fn name(&self) -> &'static str {
        "level"
    }

    async fn process(
        &self,
        chunk: &mut super::types::AudioChunk,
        _event_tx: &mpsc::Sender<crate::transport::ServerMessage>,
    ) -> Result<(), super::post_audio::AudioPostProcessorError> {
        if chunk.samples.is_empty() {
            return Ok(());
        }
        // One frame spanning the whole chunk.
        let frames = LevelAnnotator::frames(&chunk.samples, chunk.samples.len() as u32, 1);
        if let Some([rms, peak]) = frames.first() {
            let mut obj = chunk
                .extras
                .take()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            obj.insert(
                "audio_level".to_string(),
                serde_json::json!({ "rms": rms, "peak": peak }),
            );
            chunk.extras = Some(serde_json::Value::Object(obj));
        }
        Ok(())
    }
}

/// Factory for the built-in `level` processor (inline + annotate).
#[derive(Debug)]
pub struct LevelFactory;

impl super::post_audio::AudioPostProcessorFactory for LevelFactory {
    fn name(&self) -> &'static str {
        "level"
    }

    fn validate(&self, _cfg: &AudioPostProcessorConfig) -> Vec<crate::config::ConfigError> {
        vec![]
    }

    fn build(
        &self,
        _cfg: &AudioPostProcessorConfig,
    ) -> Result<Box<dyn super::post_audio::AudioPostProcessor>, anyhow::Error> {
        Ok(Box::new(LevelPostProcessor))
    }

    fn build_annotator(
        &self,
        _cfg: &AudioPostProcessorConfig,
    ) -> Result<Arc<dyn AudioAnnotator>, anyhow::Error> {
        Ok(Arc::new(LevelAnnotator))
    }
}

#[cfg(test)]
#[path = "tests/annotate.rs"]
mod tests;
