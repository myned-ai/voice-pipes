//! Voice activity detection backends as plugins.
//!
//! A VAD backend classifies each chunk of microphone audio as speech or
//! silence; the pipeline turns that stream of classifications into
//! utterances before recognition. A plugin implements [`VadBackend`] and a
//! [`VadFactory`], and is selected by `mode = "<name>"` under `[vad]` like a
//! built-in one. Utterance timing (minimum speech and silence durations,
//! pre-roll) stays with the pipeline, so a backend only answers "is this
//! chunk speech".
//!
//! # Example
//! ```
//! use voice_pipes_sdk::vad::{VadBackend, VadError, VadFactory, VadResult};
//! use voice_pipes_sdk::{ConfigError, VadConfig};
//!
//! struct MyVad;
//!
//! impl VadBackend for MyVad {
//!     fn process_chunk(&mut self, samples: &[i16], _rate: u32) -> Result<VadResult, VadError> {
//!         let loud = samples.iter().any(|s| s.unsigned_abs() > 1000);
//!         Ok(VadResult { speech_prob: if loud { 1.0 } else { 0.0 }, is_speech: loud })
//!     }
//!     fn reset(&mut self) {}
//!     fn name(&self) -> &str { "my-vad" }
//! }
//!
//! struct MyVadFactory;
//!
//! impl VadFactory for MyVadFactory {
//!     fn name(&self) -> &str { "my-vad" }
//!     fn validate(&self, _cfg: &VadConfig) -> Vec<ConfigError> { vec![] }
//!     fn build(
//!         &self,
//!         _cfg: &VadConfig,
//!         _ctx: &voice_pipes_sdk::BuildContext<'_>,
//!     ) -> Result<Box<dyn VadBackend>, voice_pipes_sdk::BuildError> {
//!         Ok(Box::new(MyVad))
//!     }
//! }
//! ```

use std::fmt;
use std::sync::Arc;

use crate::component::Component;
use crate::config::{ConfigError, VadConfig};

/// Classification of one chunk.
#[derive(Debug, Clone, Copy)]
pub struct VadResult {
    /// Speech probability in [0.0, 1.0].
    pub speech_prob: f32,
    /// Whether this chunk is classified as speech.
    pub is_speech: bool,
}

/// Errors a VAD backend reports.
#[derive(Debug)]
#[non_exhaustive]
pub enum VadError {
    /// Model file failed to load.
    ModelLoadFailed(String),
    /// Inference failed at runtime.
    InferenceFailed(String),
    /// Configuration was invalid (missing model path, bad threshold, ...).
    ConfigError(String),
}

impl fmt::Display for VadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VadError::ModelLoadFailed(msg) => write!(f, "vad model load failed: {msg}"),
            VadError::InferenceFailed(msg) => write!(f, "vad inference failed: {msg}"),
            VadError::ConfigError(msg) => write!(f, "vad config error: {msg}"),
        }
    }
}

impl std::error::Error for VadError {}

/// A voice activity detector, called once per chunk of microphone audio.
pub trait VadBackend: Send {
    /// Classify a chunk of mono i16 PCM at the given sample rate.
    fn process_chunk(&mut self, samples: &[i16], sample_rate: u32) -> Result<VadResult, VadError>;

    /// Reset internal state (between utterances or after an error).
    fn reset(&mut self);

    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// Expose this backend as a [`Component`] for health reporting.
    /// Default: not reported.
    fn as_component(&self) -> Option<Arc<dyn Component>> {
        None
    }
}

/// Validates configuration and builds a [`VadBackend`].
pub trait VadFactory: Send + Sync {
    /// The name `[vad] mode = "..."` selects this backend by.
    fn name(&self) -> &str;

    /// Additional names that resolve to this factory (aliases).
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Whether `n` is this factory's name or one of its aliases.
    fn matches(&self, n: &str) -> bool {
        self.name() == n || self.aliases().contains(&n)
    }

    /// Report configuration problems without building the backend.
    fn validate(&self, cfg: &VadConfig) -> Vec<ConfigError>;

    /// Whether this backend can run with `cfg` as it stands, which the
    /// engine's `auto` mode asks of the binary's preferred backend before
    /// falling back to energy. By default, when [`validate`](Self::validate)
    /// reports nothing; a backend that loads a file also checks it is there.
    fn can_run(&self, cfg: &VadConfig) -> bool {
        self.validate(cfg).is_empty()
    }

    /// Build one backend. Called once per session.
    fn build(
        &self,
        cfg: &VadConfig,
        ctx: &crate::build::BuildContext<'_>,
    ) -> Result<Box<dyn VadBackend>, crate::BuildError>;
}
