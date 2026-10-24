//! `OpenAI` TTS provider -- hosted text-to-speech API.
//!
//! Uses the `/v1/audio/speech` endpoint to synthesize speech progressively.
//! The model returns raw PCM 16-bit 24 kHz audio. Any server speaking the
//! same API works too: the setup wizard offers the `VibeVoice` GPU sidecar as
//! a second entry, and a WAV header in the response is stripped.

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

pub use wizard::wizard_choices;

use voice_pipes_sdk::TtsFactory;

use crate::factory::OpenAiTtsFactory;

#[cfg(test)]
#[path = "tests/openai.rs"]
mod tests;

/// What a binary registers to make `provider = "openai"` available:
/// `voice_pipes::tts::register_tts_factory(voice_pipes_tts_openai::factory)`.
pub fn factory() -> Box<dyn TtsFactory> {
    Box::new(OpenAiTtsFactory)
}

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
