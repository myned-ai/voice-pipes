//! Voice Activity Detection (VAD) -- segmenters and barge-in engines.
//!
//! VAD is a first-class pipeline stage with its own component kind
//! ([`crate::component::ComponentKind::Vad`]). It has two independent trait
//! surfaces, one per phase of the conversation:
//!
//! | Trait | Mode | Role |
//! |-------|------|------|
//! | [`segmenter::VadBackend`] | synchronous | Classifies microphone PCM chunks as speech/silence, driving utterance segmentation *before* STT. |
//! | [`engine::VadEngine`] | async | Emits [`engine::VadEvent`]s consumed by the orchestrator for barge-in and interruption *while* the assistant is speaking. |
//!
//! # Component registration
//!
//! Both trait objects are wrapped as [`crate::component::Component`]s so they
//! participate in the shared component registry and are visible at the
//! `/debug/components` endpoint alongside all other pipeline stages.
//!
//! # Module layout
//!
//! - [`engine`]    -- [`engine::VadEngine`] trait and [`engine::VadEvent`] type.
//! - [`segmenter`] -- [`segmenter::VadBackend`] trait and built-in implementations.
//! - [`error`]     -- [`VadError`] unified error type for both surfaces.

pub mod engine;
pub mod error;
pub mod segmenter;

pub use error::VadError;
pub use segmenter::register_vad_factory;
