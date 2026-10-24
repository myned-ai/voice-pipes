//! gRPC server wrappers -- expose local providers as gRPC services.
//!
//! Each wrapper takes a `Box<dyn XxxProvider>` and implements the
//! corresponding tonic service trait, converting between proto and
//! domain types. Used by the `component-server` binary to host any
//! combination of STT, TTS, and text-processing services.
//!
//! [`params`] hosts both the server and the client-side
//! [`RemoteParamStore`](params::RemoteParamStore) and is therefore
//! available whenever *any* gRPC feature is enabled.

pub mod auth;
pub mod client;
pub mod params;

#[cfg(feature = "component-server")]
mod status;
#[cfg(feature = "component-server")]
pub mod stt_server;
#[cfg(feature = "component-server")]
pub mod text_server;
#[cfg(feature = "component-server")]
pub mod tts_server;
