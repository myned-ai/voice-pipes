//! Core pipeline tasks -- STT, LLM, TTS, and playback orchestration.
//!
//! Provides the async task functions that wire audio capture through
//! speech recognition, text processing, synthesis, and playback.

pub mod annotate;
pub mod events;
pub mod filter;
pub mod extras;
mod interrupt;
mod llm;
pub mod metrics;
mod orchestrate;
#[cfg(feature = "audio-device")]
mod playback;
pub mod post_audio;
pub mod speculative;
mod stt;
pub mod throughput;
mod tts;
mod types;
mod vad;

pub use metrics::{
    ComponentMetricDescriptor, MetricFieldDescriptor, MetricUnit, TurnComponentMetrics,
    TurnInitiator, TurnMetricsSnapshot, TurnRecorder, default_descriptors, realtime_descriptors,
};
pub use speculative::{SpeculativeCommitResult, SpeculativeSessionManager};
pub use types::{
    AudioChunk, AudioLedger, AudioLedgerEntry, BargeInSettings, CaptionMeta, PipelineTurn,
    SharedTurnState, TurnState, Utterance, UtteranceSource, WordTiming, get_turn_state,
    new_turn_state, set_turn_state,
};

pub use annotate::{
    AnnotatorCommand, AnnotatorError, AnnotatorHubs, AnnotatorSpec, AudioAnnotation,
    AudioAnnotator, AudioSegment, build_audio_annotators,
};
pub use interrupt::mic_audio_splitter;
pub use llm::llm_task;
pub use orchestrate::process_utterance;
#[cfg(feature = "audio-device")]
pub use playback::playback_task;
pub use post_audio::{
    AudioPostProcessor, AudioPostProcessorError, AudioPostProcessorFactory,
    audio_post_processor_factories, build_audio_post_processors, empty_chain,
};
pub use stt::stt_task;
pub use throughput::{CallOutcome, ChainStats, OutputThroughput};
pub use tts::tts_task;
pub use filter::{FilterChain, build_filter_chain};
pub use vad::{vad_filter_task, vad_filter_task_with_filter};

#[cfg(test)]
#[path = "tests/suite/mod.rs"]
mod tests;
