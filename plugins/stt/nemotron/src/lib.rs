//! NVIDIA Nemotron-Speech-Streaming-en STT provider -- local ONNX inference.
//!
//! Cache-aware `FastConformer` transducer with native Rust mel preprocessing
//! and split decoder + joint ONNX graphs, productionizing the earlier
//! streaming-decode spike.
//!
//! A voice-pipes recogniser plugin: the binary registers [`factory`] and
//! [`wizard_choices`]; `[stt] provider = "nemotron"` selects it.

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
pub mod mel_native;
mod provider;
pub mod streaming;
pub mod vocab;
mod wizard;

pub use factory::factory;
pub use provider::NemotronStt;
pub use wizard::wizard_choices;
