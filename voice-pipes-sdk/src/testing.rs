//! Test helpers for code built on the SDK.
//!
//! [`chunking_invariant`] and [`build_refuses_what_validate_refuses`] are
//! always available. The streaming conformance checks every STT provider
//! must pass need this crate's `testing` feature (enable it in
//! `[dev-dependencies]`).

mod chunking;
#[cfg(feature = "testing")]
mod conformance;
mod factory;
#[cfg(feature = "testing")]
mod metrics;

pub use chunking::chunking_invariant;
#[cfg(feature = "testing")]
pub use conformance::{back_to_back_utterances_stay_apart, two_utterances_on_one_stream};
pub use factory::build_refuses_what_validate_refuses;
#[cfg(feature = "testing")]
pub use metrics::{
    assert_llm_metrics_conformance, assert_s2s_metrics_conformance,
    assert_stt_metrics_conformance, assert_tts_metrics_conformance,
    assert_vad_metrics_conformance, MetricConformanceError,
};

