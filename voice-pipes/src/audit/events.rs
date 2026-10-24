//! Audit trace event — re-exported from `voice-pipes-sdk`.
//!
//! The canonical definition lives in the SDK so plugin crates can emit
//! [`TraceEvent`]s without depending on the full voice-pipes binary.

pub use voice_pipes_sdk::TraceEvent;
