//! Speech-to-speech ("realtime") providers.
//!
//! A speech-to-speech model takes the caller's audio and answers in audio,
//! over one WebSocket. The engine owns that socket: connecting, the read and
//! write tasks, keepalives, resampling, reconnection and the session around
//! it. A provider is a synchronous wire-protocol mapper, a
//! [`RealtimeProtocol`], that builds the vendor's JSON envelopes from neutral
//! inputs ([`SessionConfig`], base64 audio) and parses the vendor's frames
//! into [`RealtimeEvent`]s. It does no I/O, so it can be tested frame by
//! frame.
//!
//! - [`event`]: the [`RealtimeEvent`] vocabulary every provider maps onto.
//! - [`protocol`]: the [`RealtimeProtocol`] trait and the neutral session
//!   types.
//! - [`codec`]: base64 PCM16 encoding with resampling, URL query
//!   encoding, and reading a vendor's counts.
//! - [`factory`]: [`RealtimeFactory`], which the binary registers, and the
//!   [`RealtimeConfig`] a provider is built from.

pub mod codec;
pub mod event;
pub mod factory;
pub mod protocol;

pub use event::{MAX_TOOL_ARGUMENT_BYTES, RateLimitInfo, RealtimeEvent};
pub use factory::{RealtimeConfig, RealtimeFactory};
pub use protocol::{
    CloseClass, ConnectInfo, Modalities, RealtimeProtocol, SessionConfig, ToolSpec, TurnDetection,
    default_classify_close,
};
