//! Errors specific to the VAD subsystem.
//!
//! VAD is its own component kind and owns its own error taxonomy; backends
//! and engines return [`VadError`] rather than errors borrowed from other
//! pipeline stages. The type lives in the SDK so a plugin backend reports
//! the same errors a built-in one does.

pub use voice_pipes_sdk::vad::VadError;
