//! Gemini TTS provider -- Google Gemini text-to-speech via REST API.
//!
//! Uses the `generateContent` endpoint with `response_modalities: ["AUDIO"]`
//! to synthesize speech. The model returns raw PCM 16-bit 24 kHz audio
//! encoded as base64.
//!
//! Note: The Gemini TTS API does not support server-side streaming.
//! `synthesize_stream` fetches the full audio then chunks it locally
//! into ~100 ms frames for progressive delivery through the pipeline.

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

mod config;
mod factory;
mod provider;
mod wizard;

pub use factory::factory;
pub use wizard::wizard_choices;

#[cfg(test)]
#[path = "tests/gemini.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
