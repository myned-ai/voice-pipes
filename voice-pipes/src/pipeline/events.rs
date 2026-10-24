//! Pipeline events for agentic system integration.
//!
//! `PipelineEvent` provides a structured, observable stream of everything
//! happening in the pipeline -- from utterance lifecycle to interruptions
//! and conversation history modifications.  External systems (audit,
//! analytics, agentic memory) subscribe to these events via a
//! `tokio::sync::broadcast` channel without coupling to the pipeline internals.

use serde::Serialize;

use super::metrics::TurnMetricsSnapshot;

/// What triggered an interruption.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionTrigger {
    /// User spoke over the assistant (server-detected barge-in).
    ServerVad,
    /// Client explicitly sent a Cancel control message.
    ClientCancel,
    /// User spoke over the assistant during LLM processing (early barge-in).
    EarlyBarge,
}

/// Describes a modification to conversation history caused by an event
/// (e.g., truncation after barge-in).
#[derive(Debug, Clone, Serialize)]
pub struct HistoryModification {
    /// The utterance whose assistant response was modified.
    pub utterance_id: u32,
    /// The full text the LLM produced.
    pub original_text: String,
    /// The text retained in history (what the user actually heard).
    pub retained_text: String,
    /// How the text was truncated.
    pub method: TruncationMethod,
    /// Milliseconds of audio the user actually heard (from `PlaybackReport`).
    /// Present only when method == `PlaybackReport`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played_ms: Option<u32>,
}

/// How the assistant response was truncated.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationMethod {
    /// Truncated at a sentence boundary (best-effort, no client report).
    SentenceBoundary,
    /// Truncated using the client's `PlaybackReport` (sample-accurate).
    PlaybackReport,
}

/// A structured event emitted by the pipeline for external consumers.
///
/// Events are designed to be serializable (for audit logs / webhooks)
/// and cheaply cloneable (broadcast channel).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PipelineEvent {
    /// A new utterance arrived from STT or text input.
    UtteranceReceived {
        utterance_id: u32,
        text: String,
        source: String,
    },
    /// STT finished producing the final transcript for this utterance.
    ///
    /// `elapsed_ms` is the duration measured by the STT pipeline from the
    /// final speech frame (end-of-speech / endpointing decision) to the
    /// final transcript being available.  `None` for text-source utterances
    /// that bypass STT.
    SttCompleted {
        utterance_id: u32,
        elapsed_ms: Option<u64>,
    },
    /// LLM processing started for an utterance.
    LlmStarted { utterance_id: u32 },
    /// First LLM token arrived.
    LlmFirstToken { utterance_id: u32, elapsed_ms: u64 },
    /// LLM finished generating.
    LlmCompleted {
        utterance_id: u32,
        text_len: usize,
        elapsed_ms: u64,
        is_partial: bool,
    },
    /// TTS started synthesizing audio for an utterance.
    TtsStarted { utterance_id: u32 },
    /// First TTS audio chunk is ready.
    TtsFirstAudio { utterance_id: u32, elapsed_ms: u64 },
    /// TTS finished synthesizing all audio for an utterance.
    TtsCompleted { utterance_id: u32 },
    /// A turn completed without interruption.
    TurnCompleted { utterance_id: u32, total_ms: u64 },
    /// The user interrupted the assistant.
    Interrupted {
        utterance_id: u32,
        trigger: InterruptionTrigger,
        /// Milliseconds of audio the user heard (if known from `PlaybackReport`).
        played_ms: Option<u32>,
    },
    /// Resolved barge-in record - emitted once per interrupted turn after
    /// history truncation is complete. Supersedes any prior Interrupted
    BargeInResolved {
        utterance_id: u32,
        trigger: InterruptionTrigger,
        played_ms: Option<u32>,
        retained_text: String,
        barge_in_rtt_ms: Option<u64>,
    },
    /// Conversation history was modified (e.g., truncated after interruption).
    HistoryModified { modification: HistoryModification },
    /// Client reported a timing event.
    ClientTiming {
        utterance_id: u32,
        timing_event: String,
        client_timestamp_ms: u64,
    },
    /// Caption metadata was emitted for an audio segment.
    CaptionEmitted {
        utterance_id: u32,
        fragment_idx: u32,
        text: String,
    },
    /// Speech reached the recogniser and no turn came of it.
    ///
    /// Every other event describes a turn that happened. This one records the
    /// turn that did not: the caller spoke, the utterance was thrown away, and
    /// without this the session shows only a long silence with no cause.
    ///
    /// `reason` is `empty_transcript` (the recogniser returned no words) or
    /// `too_short` (fewer characters than the configured minimum).
    /// `had_interim_text` separates the two cases that look alike: a
    /// recogniser that produced words and then took them back, which is a
    /// fault, from one that heard nothing in a sound the VAD mistook for
    /// speech, which is the system working.
    UtteranceDropped {
        utterance_id: u32,
        reason: String,
        text: String,
        had_interim_text: bool,
        audio_ms: u64,
    },
    /// A pipeline error occurred.
    PipelineError {
        utterance_id: Option<u32>,
        component: String,
        message: String,
    },
    /// Per-turn latency metrics, surfaced after the turn completes.
    ///
    /// This is the event-stream counterpart of the wire-side
    /// [`crate::transport::ServerControl::TurnMetrics`]: external consumers
    /// (audit sinks, webhooks, aggregators) get the full per-component
    /// latency breakdown here without having to subscribe to the
    /// per-client outgoing message stream.
    TurnMetricsReady {
        utterance_id: u32,
        metrics: TurnMetricsSnapshot,
    },
}
