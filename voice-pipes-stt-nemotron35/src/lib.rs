//! NVIDIA Nemotron 3.5 ASR as a voice-pipes plugin.
//!
//! Built on `voice-pipes-sdk` alone, so that adding a recogniser is shown to
//! need no change to the engine. The engine's plugin wrapper adapts what is
//! implemented here into the pipeline, health registry, metrics and probes.

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

pub mod engine;
pub mod features;
pub mod model;
mod provider;

pub use provider::{factory, wizard_choices};
