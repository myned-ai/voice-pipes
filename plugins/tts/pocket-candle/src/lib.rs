//! Kyutai Pocket-TTS native Rust (Candle) streaming TTS plugin.
//!
//! Runs `FlowLM` + LSD flow-matching + streaming Mimi codec (both decoder and
//! encoder for zero-shot WAV voice cloning) directly in pure Rust using Candle,
//! with zero-copy shared voice KV prefixes (`Arc<VoiceKvPrefix>`), pipelined
//! `FlowLM` <-> Mimi execution, per-step barge-in cancellation, and built-in
//! early silent-start retries.
//!
//! ## Configuration
//!
//! ```toml
//! [tts]
//! provider = "pocket-candle"
//! model_dir = "models/pocket-tts"
//! voice = "alba"
//!
//! # Optional tuning:
//! #   voices_dir = "models/pocket-tts/voices"
//! #   max_concurrent = 4
//! #   lsd_steps = 1
//! #   first_frame_lsd_steps = 1
//! #   temperature = 0.7
//! #   eos_threshold = -4.0
//! #   max_frames = 375
//! #   max_chars = 2000
//! #   max_cached_voices = 32
//! #   silent_start_ms = 1200
//! #   silent_start_peak = 500
//! #   silent_start_retries = 2
//! #   fade_in_ms = 4
//! #   pipeline_mimi = true
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

pub(crate) mod config;
mod factory;
pub(crate) mod model;
mod provider;
mod tokenizer;
pub(crate) mod voices;
mod wizard;

pub use wizard::wizard_choices;

use factory::PocketCandleTtsFactory;
use voice_pipes_sdk::TtsFactory;

/// What a binary registers to make `provider = "pocket-candle"` available.
pub fn factory() -> Box<dyn TtsFactory> {
    Box::new(PocketCandleTtsFactory)
}

/// Public re-exports used only by the optional standalone `pocket-tts-server` binary.
#[cfg(feature = "server")]
#[doc(hidden)]
pub mod server_support {
    pub use crate::config::{PocketCandleConfig, SAMPLE_RATE};
    pub use crate::model::PocketEngine;
    pub use crate::voices::VoiceCache;
}

#[cfg(test)]
#[path = "tests/unit.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
