//! Voice Activity Projection -- anticipatory, two-channel turn detection, as
//! a voice-pipes turn detector plugin. The binary registers [`factory`];
//! `[turn_detection] mode = "vap"` selects it. It is experimental and
//! offers no setup-wizard entry.
//!
//! Every other detector answers "has the turn ended", after the fact and
//! from the user's channel alone. VAP answers a different question: over the
//! next two seconds, who will be holding the floor? It reads both sides at
//! once, because that is where the answer lives -- how the agent is speaking
//! changes when the user will stop, and a pause that invites a reply is not
//! distinguishable from one that does not if you only ever hear one person.
//!
//! Because it predicts rather than reacts, it can be asked *while the user is
//! still speaking*, and the answer carries lead time the pipeline can spend
//! before the turn is over. That is the whole point: `TurnSignal::confidence`
//! and `expected_end_in` are populated on every evaluation, not only once
//! silence has already been paid for.
//!
//! Model: <https://github.com/ErikEkstedt/VAP> (MIT), exported by
//! `scripts/export-vap-onnx.py`. The graph takes `waveform` `[1, 2, N]` at
//! 16 kHz -- channel 0 the user, channel 1 the agent -- and returns `p_now`
//! and `p_future`, each `[1, frames]`, being P(channel 0 holds the floor)
//! over roughly the next 0-0.6 s and 0.6-1.2 s. The aggregation from the
//! model's 256 raw classes happens inside the graph.
//!
//! ## The window is the model's, not the config's
//!
//! `N` is fixed at export time: a dynamic-length export bakes the attention
//! mask at trace length and then fails on every other input. So the window is
//! read out of the loaded model and the ring buffers are sized to match it,
//! rather than being configurable and silently disagreeing with the file.
//! Changing the window means exporting a different file. The cost is roughly
//! linear in it -- about 21 ms per run at 2 s, 346 ms at 20 s -- which is why
//! the trained 20 s context is not what you want to run continuously.
//!
//! ## Alignment
//!
//! The two channels have to line up in time. While the user is speaking (or
//! when upstream VAD is `"passthrough"`), the user's channel drives the clock:
//! appending to it extends the agent's channel with the same number of zeros,
//! and arriving agent audio is written over the tail of that silence. Once the
//! pipeline calls [`voice_pipes_sdk::turn::TurnDetector::turn_ended`] and local
//! VAD (`"silero"` / `"energy"`) or echo-gating suppresses silent microphone
//! frames while the agent speaks, arriving agent audio advances both channels
//! together (filling the user channel with zeros) until user audio resumes.
//! Agent audio is teed when it is sent rather than when it is played, so the
//! two channels are aligned to transport jitter, not to the sample.

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
mod detector;
mod factory;

pub use detector::VapTurnDetector;
pub use factory::factory;

#[cfg(test)]
#[path = "tests/turn_vap.rs"]
mod tests;
