//! Ingress audio filters for cleaning microphone PCM before VAD and STT.
//!
//! An audio filter operates in-place on incoming 16-bit mono PCM chunks.
//! Filters can perform noise suppression (e.g. `DeepFilterNet`), acoustic echo
//! cancellation, or voice isolation.
//!
//! # Trait contract
//!
//! - **In-place mutation**: A filter mutates `samples: &mut [i16]` directly.
//! - **Cadence**: Called once per incoming microphone chunk (typically 10-20 ms).
//! - **Non-fatal error**: If a filter reports an error, the pipeline logs the
//!   warning and allows the unmodified chunk to proceed downstream.
//! - **Metrics**: Telemetry duration is recorded for each chunk.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::component::Component;
use crate::config::{ConfigError, FilterStageConfig};

/// Telemetry metrics returned from processing an audio chunk through a filter.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FilterMetrics {
    /// Computation duration inside the filter inference loop.
    pub compute_duration: Duration,
    /// Estimated signal-to-noise ratio improvement in dB, if computed.
    pub snr_improvement_db: Option<f32>,
    /// Whether the chunk was classified as suppressed noise.
    pub is_suppressed: bool,
}

impl FilterMetrics {
    /// Create new filter metrics with compute duration.
    pub fn new(compute_duration: Duration) -> Self {
        Self {
            compute_duration,
            snr_improvement_db: None,
            is_suppressed: false,
        }
    }

    /// Set estimated SNR improvement in dB.
    #[must_use]
    pub fn with_snr_improvement(mut self, db: f32) -> Self {
        self.snr_improvement_db = Some(db);
        self
    }

    /// Set suppression flag.
    #[must_use]
    pub fn with_suppressed(mut self, suppressed: bool) -> Self {
        self.is_suppressed = suppressed;
        self
    }
}

/// Errors reported by an [`AudioFilter`].
#[derive(Debug)]
#[non_exhaustive]
pub enum FilterError {
    /// The model failed to load.
    ModelLoadFailed(String),
    /// Inference or filtering computation failed.
    InferenceFailed(String),
    /// Sample rate received is not supported by the filter.
    UnsupportedSampleRate {
        /// Expected sample rate in Hz.
        expected: u32,
        /// Actual sample rate in Hz.
        actual: u32,
    },
    /// Invalid configuration options.
    ConfigError(String),
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::ModelLoadFailed(msg) => write!(f, "filter model load failed: {msg}"),
            FilterError::InferenceFailed(msg) => write!(f, "filter inference failed: {msg}"),
            FilterError::UnsupportedSampleRate { expected, actual } => {
                write!(f, "filter expects {expected} Hz, received {actual} Hz")
            }
            FilterError::ConfigError(msg) => write!(f, "filter config error: {msg}"),
        }
    }
}

impl std::error::Error for FilterError {}

/// An audio filter processing microphone PCM chunks in place.
pub trait AudioFilter: Send {
    /// Process a chunk of mono i16 PCM samples at `sample_rate` in place.
    fn process_chunk(
        &mut self,
        samples: &mut [i16],
        sample_rate: u32,
    ) -> Result<FilterMetrics, FilterError>;

    /// Reset internal filter/recurrent states.
    fn reset(&mut self);

    /// Provider identifier for logging and metrics.
    fn name(&self) -> &'static str;

    /// Health component reporting.
    fn as_component(&self) -> Option<Arc<dyn Component>> {
        None
    }
}

/// Validates configuration and constructs an [`AudioFilter`].
pub trait AudioFilterFactory: Send + Sync {
    /// Provider name, e.g. "deepfilter".
    fn name(&self) -> &'static str;

    /// Additional aliases for this filter provider.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Check if name matches.
    fn matches(&self, n: &str) -> bool {
        self.name() == n || self.aliases().contains(&n)
    }

    /// Report configuration problems.
    fn validate(&self, cfg: &FilterStageConfig) -> Vec<ConfigError>;

    /// Construct an [`AudioFilter`]. Called once per session.
    fn build(
        &self,
        cfg: &FilterStageConfig,
        ctx: &crate::build::BuildContext<'_>,
    ) -> Result<Box<dyn AudioFilter>, crate::BuildError>;
}

#[cfg(test)]
#[path = "tests/filter.rs"]
mod tests;
