//! `ElevenLabs` TTS provider -- hosted text-to-speech API.
//!
//! Standalone TTS over the REST streaming endpoints (distinct from the
//! `ElevenLabs` *Agents* speech-to-speech provider in the engine, which
//! hosts the whole agent). Two
//! wire paths, chosen per call:
//!
//! - `POST /v1/text-to-speech/{voice_id}/stream` -- raw PCM bytes,
//!   used when no word timing is requested.
//! - `POST /v1/text-to-speech/{voice_id}/stream/with-timestamps` --
//!   newline-delimited JSON chunks carrying base64 audio plus a
//!   char-level `alignment` payload, used to emit [`WordBoundary`]s
//!   for karaoke highlight (the same alignment family the Agents WS
//!   emits; see `realtime::elevenlabs::parse_alignment`). Unlike the
//!   WS payload (chunk-relative ms), the HTTP payload reports
//!   *seconds cumulative from response start*, so a word split across
//!   two chunks can be stitched -- the SDK's `WordGrouper` carries the
//!   partial word over.
//!
//! Auth is the `xi-api-key` header (never a bearer token), read from
//! `api_key` in TOML or the `ELEVENLABS_API_KEY` env var -- the same
//! variable the realtime provider uses, so one key serves both.
//!
//! Output is PCM 16-bit at `sample_rate` (default 24 kHz; the
//! `output_format=pcm_*` query parameter is derived from it).
//!
//! [`WordBoundary`]: voice_pipes_sdk::WordBoundary

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
mod wire;
mod wizard;

pub use factory::factory;
pub use wizard::wizard_choices;

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/elevenlabs.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
