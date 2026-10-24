//! Command entry-points, split by domain.
//!
//! * [`batch`] -- file-based TTS, STT, and STT->LLM->TTS pipeline
//! * [`live`]  -- real-time mic sessions (full loop and listen-only)
//! * [`serve`] -- worker node (HTTP debug API + WebSocket transport)
//! * [`orchestrate`] -- orchestrator node (fleet management)
//! * [`text`]  -- text-only processor chain
//! * [`inspect`] -- list compiled-in providers + their parameter schemas
//! * [`healthz`] -- probe a health endpoint (the images' `HEALTHCHECK`)
//! * `models` -- fetch and list local model files (with the `wizard` feature)

use std::sync::atomic::AtomicU64;

pub mod batch;
pub mod check;
#[cfg(feature = "wizard")]
pub mod config_wizard;
pub mod healthz;
pub mod inspect;
#[cfg(feature = "realtime")]
#[cfg(feature = "audio-device")]
pub mod interactive;
#[cfg(feature = "audio-device")]
pub mod live;
#[cfg(feature = "wizard")]
pub mod models;
pub mod orchestrate;
pub mod serve;
pub mod text;
pub mod validate_config;

// ── Shared constants ────────────────────────────────────────

pub(crate) static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
pub(crate) const AUDIT_FLUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
#[cfg(feature = "audio-device")]
pub(crate) const HARDWARE_FLUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
#[cfg(feature = "audio-device")]
pub(crate) const TRACE_CHANNEL_CAP: usize = 128;
#[cfg(feature = "audio-device")]
pub(crate) const AUDIT_SINK_CAP: usize = 256;

// ── Re-exports (keep `voice_pipes::commands::run_*` paths working) ──

pub use batch::{run_pipe, run_stt, run_tts};
pub use check::run_check;
#[cfg(feature = "wizard")]
pub use config_wizard::run_config_wizard;
pub use healthz::{DEFAULT_HEALTHZ_URL, run_healthz};
pub use inspect::{InspectFormat, InspectKind, run_inspect};
#[cfg(feature = "realtime")]
#[cfg(feature = "audio-device")]
pub use interactive::run_interactive;
#[cfg(feature = "audio-device")]
pub use live::{run_listen, run_live};
#[cfg(feature = "wizard")]
pub use models::{run_models_fetch, run_models_list};
pub use orchestrate::run_orchestrate;
pub use serve::run_serve;
pub use text::run_text;
pub use validate_config::{ValidateConfigFormat, run_validate_config};
