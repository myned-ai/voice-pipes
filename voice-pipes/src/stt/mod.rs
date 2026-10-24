//! Speech-to-text providers and supporting infrastructure.
//!
//! Re-exports the SDK's [`SttProvider`] and [`SttFactory`] traits, which
//! built-in recognisers and plugin crates implement alike, and holds the
//! provider registry, VAD backends, turn detection, and concrete STT
//! implementations.
//!
//! # Plugin registration
//!
//! External provider crates depend only on `voice-pipes-sdk` and call
//! [`register_stt_factory`] before `voice_pipes::run()`:
//!
//! ```ignore
//! my_nemo_ctc::register();   // calls register_stt_factory internally
//! voice_pipes::run().await;
//! ```

#[cfg(feature = "stt-grpc")]
pub mod grpc;
pub mod provider;
pub mod turn;

// ── SDK re-exports ─────────────────────────────────────────────
// The canonical definitions now live in voice-pipes-sdk. We re-export them at
// the same paths so all existing `use crate::stt::*` imports continue to work.

pub use voice_pipes_sdk::{
    AudioChunk, AudioEncoding, InferenceMetrics, RecognitionRequest, SttCapability, SttError,
    TranscriptionResult, WordInfo,
};

// ── Provider and factory traits ──────────────────────────────
//
// Built-in recognisers and plugin crates implement the same SDK traits, so a
// registered factory joins the built-ins as it is.

pub use voice_pipes_sdk::stt::WarmUpScope;
pub use voice_pipes_sdk::{BuildContext, SttFactory, SttProvider};

// ── External factory registry ─────────────────────────────────

pub(crate) static STT_FACTORIES: crate::plugins::Registry<dyn SttFactory> =
    crate::plugins::Registry::new("STT", builtin_stt_factories);

/// Register an external STT factory before calling `voice_pipes::run()`.
///
/// # Example
/// ```ignore
/// voice_pipes::stt::register_stt_factory(my_stt_plugin::factory)?;
/// ```
///
/// Thread-safe.
///
/// # Errors
/// When the factory's name or one of its aliases already names a built-in
/// or an earlier plugin: one provider would shadow the other.
pub fn register_stt_factory(
    ctor: fn() -> Box<dyn SttFactory>,
) -> Result<(), crate::plugins::RegistrationError> {
    STT_FACTORIES.register(ctor)
}

/// Look up a registered STT factory by name (or alias).
pub fn lookup_stt_factory(name: &str) -> Option<std::sync::Arc<dyn SttFactory>> {
    STT_FACTORIES.lookup(name)
}

/// The STT factories compiled into this build.
fn builtin_stt_factories() -> Vec<Box<dyn SttFactory>> {
    vec![
        #[cfg(feature = "stt-grpc")]
        Box::new(grpc::GrpcSttFactory),
    ]
}

/// Collect all STT provider factories: compiled-in built-ins plus any
/// externally registered plugin factories.
pub fn stt_factories() -> Vec<std::sync::Arc<dyn SttFactory>> {
    STT_FACTORIES.all()
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/stt.rs"]
mod tests;
