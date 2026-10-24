//! Realtime session context.
//!
//! [`RealtimeSessionContext`] holds the borrowed per-session state and drives
//! a provider-native realtime turn. Its event handlers are split by direction:
//! - [`client_handlers`]: messages coming from the client (audio, cancel,
//!   ping, timing/playback reports).
//! - [`provider_handlers`]: events coming from the realtime provider (audio /
//!   text deltas, transcripts, response lifecycle, usage, errors).

use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::audit::AuditEvent;
use crate::pipeline::events::{InterruptionTrigger, PipelineEvent};
use crate::pipeline::{AudioChunk, AudioPostProcessor, TurnMetricsSnapshot};
use crate::realtime::event::RateLimitInfo;
use crate::realtime::{
    RealtimeClient, RealtimeProtocol, SessionConfig as RealtimeSessionConfig, ToolRegistry,
};
use crate::transport::{
    ErrorCode, MessageRole, PipelineMessage, PipelineState, ServerControl, ServerMessage,
};

use super::turn::{
    AnchorSource, PendingInterruption, PlaybackTail, RealtimeTurn, RequestAnchor, ResponseTracker,
    SpeechEndStamp, SuppressGate, flush_turn_as_interrupted, try_reconnect,
};
use crate::transport::serve::TransportState;
use crate::transport::serve::gateway::engine_driver::EngineDriver;

mod client_handlers;
mod provider_handlers;

impl EngineDriver for RealtimeSessionContext<'_> {
    async fn cancel_turn(&mut self) -> anyhow::Result<()> {
        self.client.cancel().await
    }

    fn resolve_played_text(&self, utterance_id: u32, offset_ms: u32) -> Option<String> {
        let p = self.pending_interruption.as_ref()?;
        if p.utterance_id != utterance_id {
            return None;
        }
        let turn_played_ms = (offset_ms as u64).min(p.audio_emitted_ms);
        let session_played_ms = p.start_audio_ms.saturating_add(turn_played_ms);
        let view = RealtimeTurn {
            utterance_id: p.utterance_id,
            started_at: Instant::now(),
            ttfb_anchor: None,
            ttfb_ms: None,
            first_text_ms: None,
            speaking_announced: false,
            vp_speech_end: None,
            audio_samples: 0,
            output_sample_rate: None,
            assistant_text: p.full_text.clone(),
            text_delta_idx: 0,
            item_id: None,
            text_timeline: p.text_timeline.clone(),
            output_fragment_idx: 0,
            estimated_text_ms: p.audio_emitted_ms,
            start_audio_ms: p.start_audio_ms,
            user_transcript: None,
            saw_function_call: false,
            opened_by_tool_call: false,
            withheld_transcript: Vec::new(),
            aligned_words: p.aligned_words.clone(),
            output: crate::pipeline::OutputThroughput::default(),
            chain: crate::pipeline::ChainStats::default(),
        };
        let truncated = view.truncate_text_at(session_played_ms);
        if truncated.is_empty() && !p.full_text.is_empty() && turn_played_ms == 0 {
            Some(String::new())
        } else {
            Some(truncated)
        }
    }
}

pub struct RealtimeSessionContext<'a> {
    pub session_id: &'a str,
    pub provider_label: &'a str,
    pub provider_model: &'a Option<String>,
    pub client: &'a mut RealtimeClient<Box<dyn RealtimeProtocol>>,
    pub out_tx: &'a mpsc::Sender<ServerMessage>,
    /// Post-audio chain (shared with the cascade path). Every outbound audio
    /// chunk -- provider deltas and the end-of-utterance sentinel -- is run
    /// through this before it reaches `out_tx`, so `audio_meta.extras` behaves
    /// identically in realtime and cascade engines.
    pub post_processors: &'a [Box<dyn AudioPostProcessor>],
    /// Off-path audio annotators: teed in
    /// `emit_audio_chunk`, cancelled on barge-in, metrics at turn end.
    pub annotators: &'a crate::pipeline::AnnotatorHubs,
    /// Continuous two-sided recorder for this session, when configured.
    /// Teed the uplink in `handle_client_audio` and the downlink in
    /// `emit_audio_chunk`, so a realtime session yields the same artefact
    /// a cascade one does.
    pub session_recorder: Option<&'a crate::audit::SessionRecorder>,
    /// Where link signals go (see `transport::adaptive`).
    pub observations: crate::transport::adaptive::ObservationSink,
    /// Outbound-audio gate armed with every `ClearAudio` (see
    /// [`SuppressGate`]).
    pub suppress_audio: &'a SuppressGate,
    pub streaming_audit: &'a Option<crate::audit::StreamingAuditSink>,
    pub pe_tx: &'a tokio::sync::broadcast::Sender<PipelineEvent>,
    pub client_sample_rate: u32,
    pub output_sample_rate: u32,
    pub audio_chunks_uplinked: &'a mut u64,
    pub audio_chunks_downlinked: &'a mut u64,
    pub next_utterance_id: &'a mut u32,
    pub current_turn: &'a mut Option<RealtimeTurn>,
    pub input_audio_ms_acc: &'a mut u64,
    pub pending_interruption: &'a mut Option<PendingInterruption>,
    /// A completed turn whose audio the client may still be playing —
    /// keeps the audible tail interruptible after `ResponseCompleted`
    /// closed `current_turn` (S2S providers generate audio much faster
    /// than realtime). See [`PlaybackTail`].
    pub playback_tail: &'a mut Option<PlaybackTail>,
    pub pending_input_audio: &'a mut Vec<i16>,
    pub pending_input_rate: &'a mut u32,
    pub pending_user_transcript: &'a mut Option<String>,
    pub pending_user_transcript_flushed: &'a mut bool,
    pub ignoring_stray_deltas: &'a mut bool,
    pub session_audio_emitted_ms: &'a mut u64,
    /// Anchor captured when the user finished speaking (or submitted
    /// text) but the assistant turn has not yet been created. Drained
    /// into the next turn's `ttfb_anchor` so TTFB is measured from
    /// end-of-user-speech rather than first-audio-byte, and its wall
    /// clock lands in the turn snapshot as `user_speech_end_epoch_ms`.
    /// See [`RealtimeTurn::ttfb_base`] and [`RequestAnchor`].
    pub request_anchor: &'a mut Option<RequestAnchor>,
    /// vp's own uplink end-of-speech observer; `None` when disabled.
    pub endpoint_observer: &'a mut Option<super::endpoint_observer::EndpointObserver>,
    /// The observer's latest stamp not yet attached to a turn.
    pub vp_speech_end: &'a mut Option<SpeechEndStamp>,
    pub persist_turn: &'a (dyn Fn(TurnMetricsSnapshot, Option<String>) + Send + Sync),
    /// Client playback health for the most recent turn, shared with the
    /// `persist_turn` closure.
    pub pending_playback_health:
        &'a std::sync::Arc<parking_lot::Mutex<Option<crate::api::types::PlaybackHealth>>>,
    /// Per-session client-tool allowlist.
    /// Checked on every provider `FunctionCall` BEFORE the server
    /// `ToolRegistry` — a matching name is routed to the client, never
    /// executed by the pipeline.
    pub client_tools: &'a crate::agent::client_tools::ClientToolIndex,
    /// In-flight client tool calls awaiting a `ClientToolResult`
    /// (bounded: hard cap + TTL; session-local).
    pub client_calls: &'a mut crate::agent::client_tools::ClientToolCalls,
    /// The user transcript most recently flushed to the client, kept
    /// until the next `InputSpeechStarted`. Guards against providers
    /// re-delivering the final transcription after the turn already
    /// consumed it (`OpenAI`'s `input_audio_transcription.completed`
    /// routinely lags a fast tool-call turn), which would otherwise
    /// surface the same user bubble twice under a fresh utterance id.
    pub last_flushed_user_transcript: &'a mut Option<String>,
    /// Set on every provider `FunctionCall`; cleared when the model
    /// audibly speaks (first audio delta of any turn). While set, a
    /// zero-audio turn's transcript is *phantom* — Gemini transcribes
    /// unspoken "planned speech" around tool calls, and its known
    /// duplicate-response quirk emits it in follow-up turns that carry
    /// no function call themselves — and must not reach the client.
    /// Session-scoped because the call can arrive before any turn
    /// exists and the duplicates arrive in later turns.
    pub phantom_transcript_guard: &'a mut bool,
}
