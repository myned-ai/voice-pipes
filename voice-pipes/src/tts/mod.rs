//! Text-to-speech providers.
//!
//! Re-exports the SDK's [`TtsProvider`] / [`TtsFactory`] traits, which
//! built-in voices and plugin crates implement alike, and holds the provider
//! registry.
//!
//! # Plugin registration
//!
//! External provider crates depend only on `voice-pipes-sdk` and call
//! [`register_tts_factory`] before `voice_pipes::run()`:
//!
//! ```ignore
//! my_gemini_clone::register();   // calls register_tts_factory internally
//! voice_pipes::run().await;
//! ```

pub mod espeak;

#[cfg(feature = "tts-grpc")]
pub mod grpc;

// ── SDK re-exports ──────────────────────────────────────────────────────────
// Data types and the TtsSession trait are defined in voice-pipes-sdk so that
// plugin crates can use them without depending on the binary.
// All existing `use crate::tts::*` imports continue to compile unchanged.

pub use voice_pipes_sdk::tts::{
    SynthesisHints, SynthesisRequest, TtsError, TtsSession, TtsSessionConfig, WordBoundary,
};

// ── Provider and factory traits ─────────────────────────────────────────────
//
// Built-in voices and plugin crates implement the same SDK traits, so a
// registered factory joins the built-ins as it is.

pub use voice_pipes_sdk::{BuildContext, TtsFactory, TtsProvider};

/// Look up a registered TTS factory by name or alias (including external
/// plugins).
pub fn lookup_tts_factory(name: &str) -> Option<std::sync::Arc<dyn TtsFactory>> {
    TTS_FACTORIES.lookup(name)
}

// ── Factory registry ──────────────────────────────────────────────────────

pub(crate) static TTS_FACTORIES: crate::plugins::Registry<dyn TtsFactory> =
    crate::plugins::Registry::new("TTS", builtin_tts_factories);

/// Register an external TTS factory before calling `voice_pipes::run()`.
///
/// # Example
/// ```ignore
/// voice_pipes::tts::register_tts_factory(my_tts_plugin::factory)?;
/// ```
///
/// Thread-safe.
///
/// # Errors
/// When the factory's name or one of its aliases already names a built-in
/// or an earlier plugin: one provider would shadow the other.
pub fn register_tts_factory(
    ctor: fn() -> Box<dyn TtsFactory>,
) -> Result<(), crate::plugins::RegistrationError> {
    TTS_FACTORIES.register(ctor)
}

/// The TTS factories compiled into this build.
fn builtin_tts_factories() -> Vec<Box<dyn TtsFactory>> {
    vec![
        #[cfg(feature = "tts-grpc")]
        Box::new(grpc::GrpcTtsFactory),
    ]
}

/// Collect all TTS provider factories: compiled-in built-ins plus any
/// externally registered plugin factories.
pub fn tts_factories() -> Vec<std::sync::Arc<dyn TtsFactory>> {
    TTS_FACTORIES.all()
}

#[cfg(test)]
#[path = "tests/tts.rs"]
mod tests;
