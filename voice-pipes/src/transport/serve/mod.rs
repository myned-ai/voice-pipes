//! Transport server -- WebSocket upgrade handler and session runner.
//!
//! Wires transport sessions into the voice pipeline, providing the
//! full audio round-trip: client audio -> STT -> LLM -> TTS -> client audio.

mod router;
mod runner;
mod session_events;
mod setup;
mod state;
#[cfg(test)]
#[path = "tests/suite/mod.rs"]
mod tests;
mod turn;

/// Allowlist for client-supplied realtime options.
pub mod client_overrides;
mod gateway;

#[cfg(feature = "realtime")]
mod realtime_runner;

pub use router::transport_router;
pub use runner::run_session;
pub use state::TransportState;
