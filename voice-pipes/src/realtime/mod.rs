//! The speech-to-speech ("realtime") client.
//!
//! A generic [`RealtimeClient`] owns the WebSocket to a speech-to-speech
//! model: connecting, the read and write tasks, keepalives, resampling and
//! event fan-out. What differs per vendor is a [`RealtimeProtocol`], a pure
//! wire mapper defined in the SDK (`voice_pipes_sdk::s2s`) and implemented by
//! plugin crates (`OpenAI` Realtime, Gemini Live, `ElevenLabs` Agents), which the
//! binary registers with [`register_realtime_factory`]. The session runner
//! that turns this into a voice pipeline stage lives in
//! `transport::serve::realtime_runner`.
//!
//! * [`event`], [`protocol`], [`codec`] -- the SDK's contract, re-exported.
//! * [`client`] -- the generic [`RealtimeClient<P>`].
//! * [`factory`] -- the provider registry.
//! * [`tools`] -- tool dispatch for speech-to-speech sessions.

pub mod client;
pub mod factory;
mod rate;
pub mod tools;

// The provider contract lives in the SDK; these keep its modules at the
// paths the engine has always used.
pub use voice_pipes_sdk::s2s::{codec, event, protocol};

pub use client::RealtimeClient;
pub use event::RealtimeEvent;
pub use factory::{
    RealtimeConfig, RealtimeFactory, lookup_realtime_factory, realtime_config, realtime_factories,
    register_realtime_factory,
};
pub use protocol::{
    ConnectInfo, Modalities, RealtimeProtocol, SessionConfig, ToolSpec, TurnDetection,
};
pub use tools::{
    ToolError, ToolHandler, ToolOutcome, ToolRegistry, ToolRegistryFactory, build_tool_registry,
    default_tool_registry, register_tool_registry_factory,
};

#[cfg(test)]
pub(crate) mod tests {
    pub mod mock;
    mod runner_round_trip;
}
