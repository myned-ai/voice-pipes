//! What a factory receives besides the provider's own config.
//!
//! Every stage's factory builds through one method, `build(cfg, ctx)`. The
//! context carries what the framework supplies; it is `#[non_exhaustive]`, so
//! a new input is a new field with a default, not a new method on every
//! factory trait.

use tokio::sync::mpsc;

use crate::audit::TraceEvent;
use crate::config::{ConfigError, TextChunkingConfig};
use crate::params::AtomicParamStore;

/// What the framework hands a factory when it builds a provider.
///
/// A provider reads what it uses and ignores the rest:
///
/// ```
/// use voice_pipes_sdk::BuildContext;
///
/// let ctx = BuildContext::new();
/// assert!(ctx.audit_tx.is_none());
/// assert!(ctx.params.is_none());
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct BuildContext<'a> {
    /// Where the provider sends trace events, when the session records them.
    pub audit_tx: Option<mpsc::Sender<TraceEvent>>,
    /// This component's live parameters, when the framework has a store for
    /// them: a provider adopts the cells of the parameters it declared
    /// mutable, so an edit applies on its next call.
    pub params: Option<&'a AtomicParamStore>,
    /// Sentence-splitting tuning, for providers that split streamed text.
    pub text_chunking: TextChunkingConfig,
}

impl<'a> BuildContext<'a> {
    /// A context with nothing supplied: no audit channel, no live
    /// parameters, the default text chunking.
    pub fn new() -> Self {
        Self::default()
    }

    /// Send trace events to `audit_tx`.
    #[must_use]
    pub fn with_audit(mut self, audit_tx: Option<mpsc::Sender<TraceEvent>>) -> Self {
        self.audit_tx = audit_tx;
        self
    }

    /// Adopt live parameters from `params`.
    #[must_use]
    pub fn with_params(mut self, params: Option<&'a AtomicParamStore>) -> Self {
        self.params = params;
        self
    }

    /// Split streamed text with `text_chunking`.
    #[must_use]
    pub fn with_text_chunking(mut self, text_chunking: TextChunkingConfig) -> Self {
        self.text_chunking = text_chunking;
        self
    }
}

/// Why a factory could not build its component.
///
/// A refused config keeps its problems, one [`ConfigError`] each, so a host
/// can show them as it shows `validate`'s; anything else keeps its cause.
/// `?` on an `anyhow::Error` or a `Vec<ConfigError>` converts.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// The config is refused.
    #[error("invalid configuration:\n{}", lines(.0))]
    Config(Vec<ConfigError>),
    /// Building failed for another reason: a model that would not load, a
    /// service that could not be reached.
    #[error(transparent)]
    Failed(Box<dyn std::error::Error + Send + Sync>),
}

fn lines(errors: &[ConfigError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

impl BuildError {
    /// A failure described by `message`.
    pub fn failed(message: impl std::fmt::Display) -> Self {
        Self::Failed(message.to_string().into())
    }

    /// The config's problems, when that is why the build failed.
    pub fn config_errors(&self) -> &[ConfigError] {
        match self {
            Self::Config(errors) => errors,
            Self::Failed(_) => &[],
        }
    }
}

impl From<anyhow::Error> for BuildError {
    fn from(e: anyhow::Error) -> Self {
        Self::Failed(e.into())
    }
}

/// A stage's own error, from a constructor a factory calls, is a failure.
macro_rules! failed_from {
    ($($error:ty),*) => {
        $(
            impl From<$error> for BuildError {
                fn from(e: $error) -> Self {
                    Self::Failed(Box::new(e))
                }
            }
        )*
    };
}

failed_from!(
    crate::stt::SttError,
    crate::tts::TtsError,
    crate::text::TextProcessorError,
    crate::vad::VadError,
    std::io::Error
);

impl From<Vec<ConfigError>> for BuildError {
    fn from(errors: Vec<ConfigError>) -> Self {
        Self::Config(errors)
    }
}

#[cfg(test)]
#[path = "tests/build.rs"]
mod tests;
