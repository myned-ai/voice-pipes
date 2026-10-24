//! Post-TTS audio processing chain -- third-party extension point.
//!
//! Provides the [`AudioPostProcessor`] trait and a registry of factories,
//! mirroring the [`TextProcessor`](crate::text::TextProcessor) pattern.
//!
//! Processors run between the TTS task output and the WebSocket transport.
//! Each processor receives a mutable reference to an [`AudioChunk`] and may:
//!   - mutate `chunk.samples` (e.g. apply a filter or resampler)
//!   - attach structured data to `chunk.extras` (e.g. blendshape weights,
//!     phonemes, custom metadata)
//!   - emit additional [`ServerMessage`]s through `event_tx`
//!
//! Failures are non-fatal: a broken third-party processor must never
//! silence the audio stream.  Errors are logged and the chunk continues
//! downstream unchanged.
//!
//! # Pipeline placement
//!
//! The chain runs **before** the transport-side [`AudioPacer`] -- i.e. on
//! every chunk in TTS native cadence, with full caption / EOU / `full_text`
//! metadata intact, *before* the pacer queues the chunk for paced wire
//! release:
//!
//! ```text
//! tts_task -> [AudioPostProcessor chain] -> runner -> AudioPacer -> WebSocket
//! ```
//!
//! Implications for processor authors:
//!
//! * **Real-time budget per chunk.** A processor slower than the chunk's
//!   audio duration will eat into the pacer's burst budget and then
//!   back-pressure the TTS task. Keep per-chunk work well under the chunk
//!   duration (e.g. <= 50 ms for a 100 ms chunk).
//! * **Chunks may be flushed on barge-in.** Work performed here is wasted
//!   when the user interrupts. Don't run billable external calls without
//!   checking for cancellation.
//! * **Order matters.** Processors run in TOML declaration order; later
//!   stages observe earlier stages' edits to `samples` and `extras`.
//! * **Anything written to `chunk.extras` is forwarded verbatim to the
//!   client** as the `extras` key inside the `audio_meta` JSON frame.
//!
//! See the design rationale for the full picture, including why a
//! post-pacer stage is intentionally out of scope for v1.
//!
//! [`AudioPacer`]: crate::transport::pacer

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::config::{AudioPostProcessorConfig, ConfigError, PipelineConfig};
use crate::transport::ServerMessage;

use super::metrics::ComponentMetricDescriptor;
use super::throughput::{CallOutcome, ChainStats};
use super::types::AudioChunk;

/// Generic gRPC audio post-processor -- streams each chunk to a remote
/// service and attaches its opaque JSON response to `chunk.extras`.
#[cfg(feature = "post-grpc")]
mod grpc;

// ── Trait ───────────────────────────────────────────────────

/// A processing stage that runs on every [`AudioChunk`] after TTS and
/// before the chunk is forwarded to the client transport.
#[async_trait]
pub trait AudioPostProcessor: Send + Sync {
    /// Human-readable name for logging and metrics.
    fn name(&self) -> &str;

    /// Process one chunk in place.  See module docs for the contract.
    async fn process(
        &self,
        chunk: &mut AudioChunk,
        event_tx: &mpsc::Sender<ServerMessage>,
    ) -> Result<(), AudioPostProcessorError>;

    /// Descriptor for this processor's per-turn `audio_post:<name>`
    /// component. Advertised on
    /// `session_ready` so the Playground's generic table renders it.
    fn turn_descriptor(&self) -> ComponentMetricDescriptor {
        ComponentMetricDescriptor::audio_post(self.name())
    }
}

// ── Error ───────────────────────────────────────────────────

#[derive(Debug)]
pub struct AudioPostProcessorError {
    pub processor: String,
    pub message: String,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for AudioPostProcessorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "audio post-processor \"{}\": {}",
            self.processor, self.message
        )
    }
}

impl std::error::Error for AudioPostProcessorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

// ── Chain runner ────────────────────────────────────────────

/// Audio duration of a chunk in ms (0 for sentinels / unknown rate).
fn chunk_audio_ms(chunk: &AudioChunk) -> u64 {
    if chunk.sample_rate == 0 || chunk.samples.is_empty() {
        return 0;
    }
    (chunk.samples.len() as u64 / u64::from(chunk.channels.max(1))) * 1000
        / u64::from(chunk.sample_rate)
}

/// Run a chunk through every processor in order.  Failures are logged
/// and skipped; the chunk is never dropped.
///
/// `stats` is the turn's [`ChainStats`]: every call is timed against the
/// chunk's audio duration so a processor that is merely *slower than real
/// time* (never failing, never timing out) still shows up -- as the
/// `audio_post:<name>` turn component and, after
/// [`SLOWNESS_WARN_AFTER_CHUNKS`](super::throughput::SLOWNESS_WARN_AFTER_CHUNKS)
/// consecutive over-budget chunks, as a `warn!` in the worker log.
pub async fn run_chain(
    processors: &[Box<dyn AudioPostProcessor>],
    chunk: &mut AudioChunk,
    event_tx: &mpsc::Sender<ServerMessage>,
    stats: &mut ChainStats,
) {
    let audio_ms = chunk_audio_ms(chunk);
    for proc in processors {
        let started = std::time::Instant::now();
        let result = proc.process(chunk, event_tx).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        let outcome = match &result {
            Ok(()) => CallOutcome::Ok,
            Err(e) if e.message.contains("timed out") => CallOutcome::Timeout,
            Err(_) => CallOutcome::Failure,
        };
        if let Err(e) = result {
            tracing::warn!(
                processor = proc.name(),
                error = %e,
                "audio post-processor failed; chunk continues unmodified",
            );
        }
        if let Some(message) = stats.record(proc.name(), latency_ms, audio_ms, outcome) {
            tracing::warn!(processor = proc.name(), "{message}");
        }
    }
}

// ── Factory ─────────────────────────────────────────────────

/// Registration metadata + construction for one audio post-processor.
pub trait AudioPostProcessorFactory: Send + Sync {
    /// Name as it appears in TOML `type = "..."`.
    fn name(&self) -> &str;

    /// Local processors don't need an API key.
    fn is_local(&self) -> bool {
        true
    }

    /// Provider-specific config validation.
    fn validate(&self, cfg: &AudioPostProcessorConfig) -> Vec<ConfigError>;

    /// Construct the processor.
    fn build(
        &self,
        cfg: &AudioPostProcessorConfig,
    ) -> Result<Box<dyn AudioPostProcessor>, anyhow::Error>;

    /// Construct the off-path variant for `mode = "annotate"`
    /// (see [`super::annotate`]). Factories that only know the inline
    /// contract keep the default, which is a config error at startup.
    fn build_annotator(
        &self,
        cfg: &AudioPostProcessorConfig,
    ) -> Result<Arc<dyn super::annotate::AudioAnnotator>, anyhow::Error> {
        Err(anyhow::anyhow!(
            "audio post-processor type \"{}\" does not support mode = \"annotate\"",
            cfg.processor_type
        ))
    }
}

/// Look up a registered audio post-processor factory by name.
pub fn lookup_audio_post_processor_factory(
    name: &str,
) -> Option<Box<dyn AudioPostProcessorFactory>> {
    audio_post_processor_factories()
        .into_iter()
        .find(|f| f.name() == name)
}

/// Collect all compiled-in audio post-processor factories.
pub fn audio_post_processor_factories() -> Vec<Box<dyn AudioPostProcessorFactory>> {
    // `mut` is only exercised by the feature-gated push below.
    #[cfg_attr(
        not(feature = "post-grpc"),
        allow(
            unused_mut,
            reason = "mut is only exercised by the feature-gated push below"
        )
    )]
    let mut factories: Vec<Box<dyn AudioPostProcessorFactory>> = vec![
        Box::new(NoopFactory),
        Box::new(super::annotate::LevelFactory),
    ];
    #[cfg(feature = "post-grpc")]
    factories.push(Box::new(grpc::GrpcPostProcessorFactory));
    factories
}

/// Build the **inline** chain of [`AudioPostProcessor`]s from the pipeline
/// configuration. Entries with `mode = "annotate"` are skipped here; they
/// are built by [`super::annotate::build_audio_annotators`].
pub fn build_audio_post_processors(
    cfg: &PipelineConfig,
) -> Result<Vec<Box<dyn AudioPostProcessor>>, anyhow::Error> {
    let factories = audio_post_processor_factories();
    let mut processors: Vec<Box<dyn AudioPostProcessor>> = Vec::new();

    for proc_cfg in &cfg.audio_post_processor {
        if proc_cfg.mode == crate::config::PostProcessorMode::Annotate {
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
        let processor = factory.build(proc_cfg)?;
        processors.push(processor);
    }

    Ok(processors)
}

/// Convenience: an empty processor chain wrapped in `Arc` for callers that
/// don't have any configured processors (tests, text-only mode, etc.).
pub fn empty_chain() -> Arc<Vec<Box<dyn AudioPostProcessor>>> {
    Arc::new(Vec::new())
}

// ── Built-in: Noop ──────────────────────────────────────────

/// A passthrough processor that performs no work.  Used as a default and
/// to exercise the chain in tests.
#[derive(Debug)]
pub struct NoopPostProcessor;

#[async_trait]
impl AudioPostProcessor for NoopPostProcessor {
    fn name(&self) -> &'static str {
        "noop"
    }

    async fn process(
        &self,
        _chunk: &mut AudioChunk,
        _event_tx: &mpsc::Sender<ServerMessage>,
    ) -> Result<(), AudioPostProcessorError> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct NoopFactory;

impl AudioPostProcessorFactory for NoopFactory {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn validate(&self, _cfg: &AudioPostProcessorConfig) -> Vec<ConfigError> {
        vec![]
    }

    fn build(
        &self,
        _cfg: &AudioPostProcessorConfig,
    ) -> Result<Box<dyn AudioPostProcessor>, anyhow::Error> {
        Ok(Box::new(NoopPostProcessor))
    }

    fn build_annotator(
        &self,
        _cfg: &AudioPostProcessorConfig,
    ) -> Result<Arc<dyn super::annotate::AudioAnnotator>, anyhow::Error> {
        Ok(Arc::new(super::annotate::NoopAnnotator))
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/post_audio.rs"]
mod tests;
