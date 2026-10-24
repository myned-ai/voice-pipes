//! Voice Pipes -- a local, modular voice pipeline.
//!
//! Chains speech-to-text, text processing, and text-to-speech stages
//! into configurable pipelines that run entirely on-device.

pub mod agent;
pub mod api;
pub mod audio;
pub mod audit;
pub mod component;
pub mod config;
/// The inference-session pool, now its own crate so a plugin can share it.
pub use voice_pipes_onnx as onnx;
/// The plugin SDK, for a host that builds providers without depending on it
/// directly.
pub use voice_pipes_sdk as sdk;
pub mod pipeline;
pub mod plugins;
#[cfg(feature = "realtime")]
pub mod realtime;
pub mod stt;
pub mod telemetry;
pub mod text;
pub mod transport;
pub mod tts;
pub mod vad;

#[cfg(any(
    feature = "component-server",
    feature = "stt-grpc",
    feature = "tts-grpc",
    feature = "text-grpc",
    feature = "post-grpc",
))]
pub mod grpc;

pub mod commands;
