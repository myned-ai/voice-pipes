//! Kyutai Pocket-TTS provider -- HTTP sidecar client.
//!
//! Talks to a running `pocket-tts serve` instance (`FastAPI` server bundled
//! with the upstream Python package). The server exposes a single endpoint:
//!
//! ```text
//! POST /tts
//!   body: application/x-www-form-urlencoded
//!     text=<utf-8 text>
//!     voice_url=<built-in name | http(s):// url | hf://path>   (optional)
//!   200 OK: audio/wav (chunked WAV stream, 24 kHz mono i16 PCM)
//! ```
//!
//! The response is a streaming WAV file: a small RIFF/WAVE header followed
//! by raw little-endian PCM samples. We parse the header just enough to
//! locate the start of the `data` chunk, then forward decoded samples as
//! [`AudioFrame`](voice_pipes_sdk::AudioFrame) chunks of ~100 ms.
//!
//! ## Configuration
//!
//! ```toml
//! [tts]
//! provider = "pocket-http"
//! endpoint = "http://127.0.0.1:8000"   # default
//! voice    = "alba"                     # any built-in name, or hf://... / https://...
//!
//! # Optional advanced knobs (passthrough):
//! #   request_timeout_ms = 60000        # default 60s; longest wait for the first or any later audio
//!
//! # Silent-start guard (see below): re-synthesize when the sidecar produces
//! # no sound in the first `silent_start_ms` of a fragment.
//! #   silent_start_ms      = 2000       # default 2000; 0 disables
//! #   silent_start_peak    = 500        # int16 peak below which audio is "silent"
//! #   silent_start_retries = 2          # extra attempts before giving up
//! ```
//!
//! ## Silent-start guard
//!
//! Sampling-based checkpoints occasionally fail to start speaking: the
//! generation runs to its length cap and the whole fragment comes back as
//! near-silent noise (measured on `myned-ai/pocket-tts-croatian`: 2 of 6
//! runs of the same sentence, peak amplitude < 300 vs > 3000 for speech,
//! with the server logging "Maximum generation length reached without
//! EOS"). Healthy output starts speaking within ~1.2 s. So the provider
//! watches the first `silent_start_ms` of every fragment: if the peak never
//! exceeds `silent_start_peak`, it drops that attempt and requests the
//! fragment again (a fresh sample). Audio already forwarded stays forwarded
//! — the listener hears a short pause, then the sentence — so the guard adds
//! no latency to good fragments. The last allowed attempt always streams to
//! completion.
//!
//! See [Pocket-TTS docs](https://kyutai-labs.github.io/pocket-tts/CLI%20Commands/serve/)
//! for how to start the server and the catalog of built-in voices.

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

use factory::PocketHttpTtsFactory;
use voice_pipes_sdk::TtsFactory;

#[cfg(test)]
#[path = "tests/pocket_http.rs"]
mod tests;

/// What a binary registers to make `provider = "pocket-http"` available:
/// `voice_pipes::tts::register_tts_factory(voice_pipes_tts_pocket::factory)`.
pub fn factory() -> Box<dyn TtsFactory> {
    Box::new(PocketHttpTtsFactory)
}

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
