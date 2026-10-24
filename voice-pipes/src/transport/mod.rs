//! Transport abstraction layer for bidirectional client communication.
//!
//! Provides a protocol-agnostic interface for exchanging audio, text,
//! and structured data between clients and the voice pipeline.
//! Implementations exist for WebSocket (and in the future, WebTransport).
//!
//! # Architecture
//!
//! ```text
//! Client <--> TransportSession <--> SessionManager <--> Pipeline
//! ```
//!
//! The transport layer handles framing, serialization, connection
//! lifecycle, health monitoring, and reconnection.  The pipeline
//! core only sees typed domain messages via async channels.

pub mod adaptive;
pub mod auth;
pub mod direct;
pub mod error;
pub mod health;
pub mod inject;
pub mod pacer;
pub mod rate;
pub mod serve;
pub mod session;
pub mod session_store;
pub mod wire;
pub mod ws;

pub use error::ErrorCode;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::pipeline::AudioChunk;

// ── Structured message types ────────────────────────────────

/// A generic structured message that flows bidirectionally between client
/// and server. Can represent a text chat message, a tool call/result,
/// a multimedia payload, or any combination.
///
/// The `kind` field determines how the pipeline routes it -- which
/// components it passes through and which it bypasses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineMessage {
    /// Command/type discriminator. Determines routing behavior.
    /// Open string -- new kinds can be added without protocol version bumps.
    /// Examples: "text", "`tool_call`", "`tool_result`", "context", "card",
    /// "image", "`trigger_action`", "suggestions".
    pub kind: String,

    /// Optional text part. For kind="text" this is the chat message body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Optional structured JSON payload. Schema depends on `kind`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,

    /// Optional binary attachment (images, documents, audio clips).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<Vec<u8>>,

    /// Who produced this message.
    #[serde(default)]
    pub role: MessageRole,

    /// The conversation turn this message belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,

    /// Unique message identifier.
    #[serde(default = "generate_message_id")]
    pub message_id: String,
}

fn generate_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl PipelineMessage {
    /// Create a simple text message.
    pub fn text(text: impl Into<String>, role: MessageRole) -> Self {
        Self {
            kind: "text".into(),
            text: Some(text.into()),
            payload: None,
            binary: None,
            role,
            turn_id: None,
            message_id: generate_message_id(),
        }
    }

    /// Tool-transparency: "the agent is calling tool `name` now".
    ///
    /// These are **facts about tool activity, not transcripts** —
    /// clients should render them as activity indicators, never as
    /// spoken content (the spoken filler/result can be barged in on
    /// independently of this message). On the S2S path the pipeline
    /// emits this when forwarding a provider `FunctionCall`; on the
    /// classic path the agent emits it itself via
    /// [`AgentEvent::Message`](crate::agent::AgentEvent) (the pipeline
    /// cannot see agent-internal tools).
    pub fn tool_call(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        let mut payload = serde_json::Map::new();
        payload.insert("name".into(), serde_json::Value::String(name.into()));
        payload.insert("arguments".into(), arguments);
        Self {
            kind: "tool_call".into(),
            text: None,
            payload: Some(serde_json::Value::Object(payload)),
            binary: None,
            role: MessageRole::Assistant,
            turn_id: None,
            message_id: generate_message_id(),
        }
    }

    /// Tool-transparency: "the tool call finished with this result".
    /// Same activity-not-transcript semantics as
    /// [`tool_call`](Self::tool_call).
    pub fn tool_result(name: impl Into<String>, result: serde_json::Value) -> Self {
        let mut payload = serde_json::Map::new();
        payload.insert("name".into(), serde_json::Value::String(name.into()));
        payload.insert("result".into(), result);
        Self {
            kind: "tool_result".into(),
            text: None,
            payload: Some(serde_json::Value::Object(payload)),
            binary: None,
            role: MessageRole::Assistant,
            turn_id: None,
            message_id: generate_message_id(),
        }
    }
}

/// Who produced a message.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    #[default]
    Assistant,
    System,
}

// ── Error classification ────────────────────────────────────

/// Severity level for pipeline errors sent to the client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSeverity {
    /// Non-fatal issue -- pipeline continues, user may not notice.
    Warning,
    /// The current turn failed but the session is recoverable.
    Error,
    /// Unrecoverable -- session should be torn down.
    Fatal,
}

/// Which pipeline component originated an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorComponent {
    Stt,
    Llm,
    Tts,
    Pipeline,
    Transport,
}

// ── Session mode ────────────────────────────────────────────

/// Determines which pipeline components are active for a session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// Full voice pipeline: STT -> LLM -> TTS (default).
    #[default]
    Voice,
    /// Text-only: no STT/TTS, messages flow client <-> LLM.
    Text,
    /// Voice + typed messages, both STT and text input accepted.
    Hybrid,
    /// Voice input, text-only response (no TTS).
    VoiceInTextOut,
    /// Text input, spoken response (no STT).
    TextInVoiceOut,
}

impl SessionMode {
    /// Whether this mode accepts audio input (requires STT).
    pub fn accepts_audio(&self) -> bool {
        matches!(self, Self::Voice | Self::Hybrid | Self::VoiceInTextOut)
    }

    /// Whether this mode accepts text input.
    pub fn accepts_text(&self) -> bool {
        matches!(self, Self::Text | Self::Hybrid | Self::TextInVoiceOut)
    }

    /// Whether the pipeline should produce voice (TTS) output.
    pub fn produces_audio(&self) -> bool {
        matches!(self, Self::Voice | Self::Hybrid | Self::TextInVoiceOut)
    }

    /// The mode's name as it appears on the wire and in config -- what an
    /// operator would actually type. `Debug` gives `TextInVoiceOut`, which is
    /// the wrong spelling to put in an error message telling someone what to
    /// set.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Text => "text",
            Self::Hybrid => "hybrid",
            Self::VoiceInTextOut => "voice_in_text_out",
            Self::TextInVoiceOut => "text_in_voice_out",
        }
    }
}

// ── Client -> Server messages ────────────────────────────────

/// A message sent from the client to the server.
#[derive(Debug, Clone)]
pub enum ClientMessage {
    /// Raw PCM audio chunk.
    Audio(AudioChunk),
    /// Unified structured message (text, data, tool calls, etc.).
    Message(PipelineMessage),
    /// Control signal from the client.
    Control(ClientControl),
}

/// Client-initiated control signals.
/// One playback break (jitter-buffer underrun) at `at_audio_ms` in the
/// ideal (gapless) audio timeline, lasting `silence_ms`. The coordinate
/// system indexes straight into the server-side TTS fragment concat, so
/// the as-heard reconstruction is a pure splice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackGap {
    /// Position in the ideal audio, milliseconds.
    pub at_audio_ms: u32,
    /// Silence duration, milliseconds.
    pub silence_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ClientControl {
    /// Cancel current pipeline operation (barge-in).
    Cancel,
    /// Application-level ping for liveness.
    Ping {
        /// Client-side timestamp in epoch milliseconds.
        timestamp_ms: u64,
    },
    /// Audio/session configuration during handshake.
    Configure(SessionConfig),
    /// Resume a previous session after reconnection.
    Resume {
        /// Opaque token issued by the server on the previous connection.
        session_token: String,
    },
    /// Inject a pre-recorded audio file into the pipeline for benchmarking.
    InjectAudio {
        /// Filesystem path or URL to the WAV file.
        source: String,
        /// Scenario identifier for grouping benchmark results.
        #[serde(skip_serializing_if = "Option::is_none")]
        scenario_id: Option<String>,
        /// Experiment identifier for linking related benchmark runs.
        #[serde(skip_serializing_if = "Option::is_none")]
        experiment_id: Option<String>,
        /// Playback speed multiplier: 1.0 = real-time, 0.0 = as-fast-as-possible.
        #[serde(default = "default_inject_speed")]
        speed: f32,
    },
    /// Client reports what audio was actually played before an interruption.
    PlaybackReport {
        /// Which utterance this report relates to.
        utterance_id: u32,
        /// Milliseconds of audio the client actually played.
        last_played_offset_ms: u32,
        /// The text that was spoken up to the interruption point (best effort).
        #[serde(skip_serializing_if = "Option::is_none")]
        last_played_text: Option<String>,
    },
    /// Client reports that it fully finished playing an utterance.
    PlaybackComplete {
        /// Which utterance finished playing.
        utterance_id: u32,
    },
    /// Client-computed per-turn playback quality: underrun gap list in
    /// ideal-audio coordinates plus aggregates. Sent once per turn at
    /// playback-complete. Drives the `NNN_playback.json` capture artifact
    /// and `SessionDetail.playback_health`.
    PlaybackQuality {
        /// Which utterance this report relates to.
        utterance_id: u32,
        /// Number of playback breaks (== `gaps.len()`).
        underrun_count: u32,
        /// Total inserted silence in milliseconds (== Σ `gaps[].silence_ms`).
        total_silence_ms: u32,
        /// Ideal audio delivered to the client (Σ chunk durations), ms.
        audio_ms: u32,
        /// Audio actually heard (enqueue-bounded), ms.
        heard_ms: u32,
        /// Client wall-clock span of the turn's playback, ms.
        wall_ms: u32,
        /// Each underrun, positioned in the IDEAL (gapless) timeline.
        gaps: Vec<PlaybackGap>,
        /// Wall time from the first to the last audio frame *received* by
        /// the client, ms. `arrival_wall_ms / audio_ms` > 1 means audio
        /// arrived slower than it plays (server-side starvation), ≤ 1 with
        /// underruns means bursty delivery vs. the prebuffer (jitter).
        #[serde(default)]
        arrival_wall_ms: u32,
        /// True when the frame was sent from the flush path (barge-in,
        /// cancel) instead of natural playback completion.
        #[serde(default)]
        interrupted: bool,
        /// `audio_annotation` frames that arrived after the audio they
        /// describe had already started playing (annotators only).
        #[serde(default)]
        annotation_late_count: u32,
        /// Max lateness of those annotations, ms.
        #[serde(default)]
        annotation_late_ms_max: u32,
    },
    /// Periodic link report from the client, used by the adaptive-quality
    /// controller as a mid-turn signal source.
    ///
    /// `PlaybackQuality` is accurate but arrives once per turn, which is too
    /// late to react *during* a long one. This is the same kind of evidence
    /// sampled every couple of seconds instead. Cheap enough to ignore: about
    /// 150 bytes per report.
    ///
    /// Every field is optional-by-default so a client can send whatever it
    /// happens to know.
    NetReport {
        /// Audio queued and not yet played, ms. Falling towards zero means
        /// the client is about to underrun.
        #[serde(default)]
        playback_buffer_ms: u32,
        /// Underruns since the previous report.
        #[serde(default)]
        underruns: u32,
        /// Audio received since the previous report, ms.
        #[serde(default)]
        audio_received_ms: u32,
        /// Wall time the above arrived over, ms. With `audio_received_ms`
        /// this gives an in-flight `arrival_rtf` rather than a per-turn one.
        #[serde(default)]
        wall_ms: u32,
        /// S5 -- the client's own send backlog (`ws.bufferedAmount`), the
        /// only view either side gets of uplink congestion.
        #[serde(default)]
        uplink_backlog_bytes: u64,
    },
    /// Client-side timing event for end-to-end latency measurement.
    TimingReport {
        /// Which utterance this report relates to.
        utterance_id: u32,
        /// Typed timing event. Unknown values are accepted via
        /// [`TimingEventKind::Other`] for forward compatibility.
        event: TimingEventKind,
        /// Client-side epoch timestamp in milliseconds.
        client_timestamp_ms: u64,
        /// Optional payload with additional timing data.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<serde_json::Value>,
    },
    /// Client reports the outcome of a client-fulfilled tool call
    /// (kind `trigger_action` command issued by the server). Correlated by `call_id`.
    /// `Started`/`Progress` are optional telemetry; `Completed`/`Failed`
    /// resolve the call and inject the (untrusted, size-capped) `result`
    /// into the model's conversation context.
    ClientToolResult {
        /// The `call_id` from the `trigger_action` payload.
        call_id: String,
        /// Lifecycle phase of the client-side execution.
        phase: ClientToolPhase,
        /// Structured outcome reported by the client. Becomes model
        /// context — treated as data, never as instructions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        /// Client requests that the outcome be spoken even when the
        /// tool's `speak_on_result` is off (the "client decides"
        /// affordance). Ignored on interim phases.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        speak: bool,
    },
    /// Drive the variant's TTS provider directly with `text`, bypassing
    /// STT and LLM. Used by the benchmark harness for TTS-only A/B runs.
    /// The server replies with [`ServerControl::TtsOnlyResult`] on success
    /// or [`ServerControl::TtsOnlyError`] on failure.
    TtsOnlySynthesize {
        /// Stable identifier echoed back in the result, used by the runner
        /// to correlate request/response across the wire.
        id: String,
        /// Text to synthesise.
        text: String,
        /// Optional BCP-47 language tag for providers that accept hints.
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// Optional voice override for providers that accept hints.
        #[serde(skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
    },
}

/// Lifecycle phase of a client-side tool execution reported via
/// [`ClientControl::ClientToolResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientToolPhase {
    /// The client began executing the command (telemetry only).
    Started,
    /// Interim state for multi-step client workflows (telemetry only).
    Progress,
    /// The command finished; `result` carries the outcome.
    Completed,
    /// The command failed; `result` carries the error description.
    Failed,
}

fn default_inject_speed() -> f32 {
    1.0
}

fn default_error_severity() -> ErrorSeverity {
    ErrorSeverity::Error
}

// ── Server -> Client messages ────────────────────────────────

/// A message sent from the server to the client.
#[expect(
    clippy::large_enum_variant,
    reason = "the large variant is the dominant case on the audio hot path; boxing adds indirection"
)]
#[derive(Debug, Clone)]
pub enum ServerMessage {
    /// Audio chunk to play.
    Audio(AudioChunk),
    /// Unified structured message (text, data, tool calls, etc.).
    Message(PipelineMessage),
    /// Server-initiated control signal.
    Control(ServerControl),
    /// Off-path per-audio metadata from an annotate-mode post-processor.
    /// Wire: the `audio_annotation` text
    /// frame. Ordered on the same socket as audio; may precede or follow
    /// the audio it describes; passes the pacer immediately.
    Annotation(crate::pipeline::AudioAnnotation),
}

/// Server-initiated control signals.
#[expect(
    clippy::large_enum_variant,
    reason = "the large variant is the dominant case on the audio hot path; boxing adds indirection"
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ServerControl {
    /// Session is established -- here is your session info.
    SessionReady(SessionInfo),
    /// Pong response to a client ping.
    Pong {
        /// Echo of the client's timestamp.
        client_timestamp_ms: u64,
        /// Server-side timestamp in epoch milliseconds.
        server_timestamp_ms: u64,
    },
    /// Pipeline processing state changed.
    StateChanged {
        /// The new pipeline state.
        state: PipelineState,
    },
    /// Connection quality changed. Sent on every **tier** transition, not on
    /// every measurement -- a control frame per RTT sample would compete with
    /// the audio the tier exists to protect.
    QualityUpdate {
        /// Current assessed quality level (the ping/pong bucket).
        quality: ConnectionQuality,
        /// The fidelity tier the session is now running at. Clients should
        /// degrade in step: a smaller avatar update rate, a deeper jitter
        /// buffer, a "poor connection" hint in the UI.
        #[serde(default)]
        tier: QualityTier,
    },
    /// An error occurred.
    Error {
        /// Error code for programmatic handling.
        code: ErrorCode,
        /// Human-readable message.
        message: String,
        /// Severity level.
        #[serde(default = "default_error_severity")]
        severity: ErrorSeverity,
        /// Which conversation turn this error relates to.
        #[serde(skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        /// Which pipeline component originated the error.
        #[serde(skip_serializing_if = "Option::is_none")]
        component: Option<ErrorComponent>,
    },
    /// Session is ending.
    Closing {
        /// Explanation for the closure.
        reason: String,
    },
    /// Aggregate metrics for the session, sent immediately before
    /// [`ServerControl::Closing`]. Provides a structured rollup of the
    /// session's turns for analytics and lightweight clients that do
    /// not buffer per-turn `TurnMetrics` frames.
    SessionSummary {
        /// Total number of completed turns (interrupted or not).
        turns: u32,
        /// Subset of `turns` that were interrupted (server-side barge-in,
        /// client cancel, or text-input interruption).
        interrupted_turns: u32,
        /// Mean time-to-first-token across turns that produced one (ms).
        #[serde(skip_serializing_if = "Option::is_none")]
        avg_ttft_ms: Option<u64>,
        /// Mean time-to-first-byte across turns that produced one (ms).
        #[serde(skip_serializing_if = "Option::is_none")]
        avg_ttfb_ms: Option<u64>,
        /// Mean end-to-end turn duration across all completed turns (ms).
        #[serde(skip_serializing_if = "Option::is_none")]
        avg_total_ms: Option<u64>,
        /// Sum of synthesised audio duration across all turns (ms).
        total_audio_duration_ms: u64,
    },
    /// Per-turn latency metrics sent after each utterance completes.
    ///
    /// The wire shape is `{ "action": "turn_metrics", "snapshot": { ... } }`
    /// where `snapshot` is a [`crate::pipeline::TurnMetricsSnapshot`] --
    /// utterance id, total time, interrupted flag, and an ordered list of
    /// per-component slices. Per-stage timings (TTFT, TTFB, STT latency,
    /// LLM generation, TTS synthesis sub-metrics) live inside the
    /// `components[].extras` map; consumers use the snapshot helpers
    /// (`ttft_ms()`, `ttfb_ms()`, `component_by_kind("tts")`, ...) to read
    /// well-known stages without hardcoding key strings.
    TurnMetrics {
        snapshot: crate::pipeline::TurnMetricsSnapshot,
    },
    /// Instructs the client to immediately flush its audio playback buffer.
    /// Sent on barge-in to stop stale audio from playing.
    ClearAudio {
        /// Which utterance's audio should be flushed.
        utterance_id: u32,
        /// Why the audio is being cleared (e.g. "`barge_in`", "cancel").
        reason: String,
    },
    /// Informs the client that history was truncated due to a barge-in.
    TextTruncated {
        /// Which utterance was interrupted out.
        utterance_id: u32,
        /// The exact string the user actually heard (to sync client transcript).
        final_text: String,
    },
    /// The user spoke and no turn came of it: the recogniser returned no
    /// words (`empty_transcript`), too few (`too_short`), or never answered
    /// (`stt_timeout`). Without this the client sees speech end and then
    /// nothing, which after a barge-in looks like an agent that was cut off
    /// and then refused to answer.
    UtteranceDropped {
        /// The id the speech would have become; the next committed
        /// utterance takes it.
        utterance_id: u32,
        /// Why no turn was made.
        reason: String,
        /// How much speech the recogniser was given.
        audio_ms: u64,
    },
    /// Successful response to a [`ClientControl::TtsOnlySynthesize`] request:
    /// per-synthesis metrics plus a pointer to the produced WAV on disk
    /// for downstream WER scoring.
    TtsOnlyResult {
        /// Echo of the request id.
        id: String,
        /// Per-synthesis metrics (timing, RTF, provider, cold-start flag).
        metrics: TtsOnlyMetrics,
        /// On-disk handle for the produced audio.
        audio_ref: AudioRef,
    },
    /// Failure response to a [`ClientControl::TtsOnlySynthesize`] request.
    TtsOnlyError {
        /// Echo of the request id.
        id: String,
        /// Provider-supplied error description.
        error: String,
    },
}

/// Per-synthesis metrics emitted alongside a [`ServerControl::TtsOnlyResult`].
///
/// Wire shape is flat: `{ ttfb_ms, synthesis_ms, audio_duration_ms, ...,
/// voice }`. This is the TTS-only benchmark surface and lives independently
/// of the per-turn metrics registry (which targets full pipelines).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TtsOnlyMetrics {
    /// Time (ms) from request to first audio sample.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    /// Total synthesis span (ms) -- request to last audio chunk (pre-pacer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_ms: Option<u64>,
    /// Wall-clock duration (ms) of the produced audio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_duration_ms: Option<u64>,
    /// Native sample rate of the provider for this synthesis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Number of input characters fed to TTS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<u32>,
    /// Realtime factor: `synthesis_ms / audio_duration_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtf: Option<f32>,
    /// Human-readable provider name (`"piper"`, `"pocket-http"`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// True iff this synthesis was the first one against the provider handle
    /// (i.e. potentially included model-load / lazy-fetch overhead).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cold_start: bool,
    /// Populated when synthesis produced no audio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Voice id used (when the provider exposes one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Peak absolute sample amplitude (int16 scale) of the produced audio.
    /// Near zero means the provider returned silence — a sampling TTS that
    /// ran to its generation cap without ever speaking looks like a
    /// *successful* synthesis by every other metric here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_amplitude: Option<u16>,
    /// Fraction (0–1) of 100 ms windows carrying speech-level energy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_ratio: Option<f32>,
}

/// On-disk pointer to a synthesised WAV file. Returned alongside a
/// [`ServerControl::TtsOnlyResult`] so the runner can read the file for
/// round-trip WER scoring without streaming bytes back in-band.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioRef {
    /// Filesystem path to the WAV file on the worker host.
    pub path: String,
    /// Always `"wav"`; the field leaves room for other encodings.
    pub format: String,
    /// PCM sample rate of the file.
    pub sample_rate: u32,
    /// PCM channel count of the file.
    pub channels: u16,
    /// Wall-clock duration of the audio in milliseconds.
    pub duration_ms: u64,
}

// ── Shared types ────────────────────────────────────────────

/// Audio/session configuration sent by the client during handshake.
///
/// The client sends this in a `Configure` control message right after
/// connecting.  The server inspects it, merges with pipeline caps, and
/// responds with [`NegotiatedConfig`] inside [`SessionInfo`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Audio sample encoding format.
    #[serde(default)]
    pub audio_format: AudioFormat,
    /// Input audio sample rate in Hz (client -> server).
    /// Legacy field -- also accepted as a fallback for `input_sample_rate`.
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    /// Preferred output (playback) sample rate in Hz (server -> client).
    /// When `None`, the server picks the best rate (usually TTS native).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sample_rate: Option<u32>,
    /// Number of audio channels.
    #[serde(default = "default_channels")]
    pub channels: u16,
    /// BCP-47 language tag (e.g. `"en-US"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Voice or speaker identifier for TTS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Provider-specific key-value options.
    #[serde(default)]
    pub options: HashMap<String, serde_json::Value>,
    /// Arbitrary application data from the client (forwarded opaquely).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_data: Option<serde_json::Value>,
    /// User-defined tags for categorisation and filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Session mode -- determines which pipeline components are active.
    #[serde(default)]
    pub mode: SessionMode,
    /// Wire protocol version the client implements.
    ///
    /// When omitted, the server assumes the current [`PROTOCOL_VERSION`].
    /// Mismatched versions are surfaced via [`NegotiatedConfig::protocol_version`]
    /// and a server-side warning so deployments can detect drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    /// The client will capture at whatever `input_sample_rate` the server
    /// returns in `session_ready`, rather than at the rate it proposed in
    /// `sample_rate`.
    ///
    /// **This gate exists for safety, not politeness.** The input rate is
    /// bound once at the handshake and never renegotiated, so a server that
    /// lowered the rate for a client that kept sending 48 kHz would read that
    /// audio at the wrong rate — fast, distorted, and useless to STT — with no
    /// mechanism to notice or correct it. Absent or `false` (every client
    /// written before this field existed), the server honours `sample_rate`
    /// exactly as before.
    ///
    /// Only meaningful when the client can resample; the browser SDK can
    /// (`AudioCaptureService` converts whatever the `AudioContext` gives it).
    #[serde(default)]
    pub accepts_negotiated_input_rate: bool,
}

/// Resolved input sample rate from a [`SessionConfig`].
impl SessionConfig {
    /// The effective input sample rate (prefers `sample_rate`).
    pub fn input_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Default sample rate: 48 kHz (browser-native, avoids lossy browser resampling).
pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 48_000;

/// Default channel count: mono.
const DEFAULT_CHANNELS: u16 = 1;

fn default_sample_rate() -> u32 {
    DEFAULT_SAMPLE_RATE
}

fn default_channels() -> u16 {
    DEFAULT_CHANNELS
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            audio_format: AudioFormat::default(),
            sample_rate: DEFAULT_SAMPLE_RATE,
            output_sample_rate: None,
            channels: DEFAULT_CHANNELS,
            language: None,
            voice: None,
            options: HashMap::new(),
            app_data: None,
            tags: Vec::new(),
            mode: SessionMode::default(),
            protocol_version: None,
            // Conservative: a client that never said it can adapt is assumed
            // not to, so the server honours its proposed rate verbatim.
            accepts_negotiated_input_rate: false,
        }
    }
}

/// Wire protocol version implemented by this build.
///
/// Bumped when an incompatible client/server change is shipped. Clients may
/// declare a preferred version via [`SessionConfig::protocol_version`]; the
/// server echoes its negotiated version in [`NegotiatedConfig::protocol_version`].
pub const PROTOCOL_VERSION: u32 = 1;

/// Typed client-reported timing event.
///
/// Known variants serialize/deserialize as `snake_case` strings. Any unknown
/// string value is preserved in [`TimingEventKind::Other`] so older or newer
/// clients can keep interoperating without a server upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingEventKind {
    /// Client microphone captured the first speech frame.
    SpeechStart,
    /// Client VAD detected end of speech.
    SpeechEnd,
    /// Client received the first audio byte from the server.
    AudioReceive,
    /// Client started playing TTS audio.
    PlaybackStart,
    /// Client played the first PCM sample.
    FirstBytePlayed,
    /// Client finished playing TTS audio.
    PlaybackEnd,
    /// Client audio buffer underrun (glitch).
    Underrun,
    /// Forward-compatible fallback for unknown event names.
    #[serde(untagged)]
    Other(String),
}

impl TimingEventKind {
    /// Stable lowercase name used in logs, audit records, and analytics.
    pub fn as_str(&self) -> &str {
        match self {
            Self::SpeechStart => "speech_start",
            Self::SpeechEnd => "speech_end",
            Self::AudioReceive => "audio_receive",
            Self::PlaybackStart => "playback_start",
            Self::FirstBytePlayed => "first_byte_played",
            Self::PlaybackEnd => "playback_end",
            Self::Underrun => "underrun",
            Self::Other(s) => s.as_str(),
        }
    }
}

impl std::fmt::Display for TimingEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Audio sample encoding format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    /// Raw PCM signed 16-bit little-endian.
    #[default]
    #[serde(alias = "pcm_i16")]
    PcmI16Le,
    /// Raw PCM float 32-bit little-endian.
    #[serde(alias = "pcm_f32")]
    PcmF32Le,
    /// Opus-encoded audio payloads (experimental; transport-level fallback
    /// may apply when the active transport path cannot decode Opus yet).
    Opus,
}

/// Connection quality assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionQuality {
    /// RTT < 100 ms, no measurable loss.
    Excellent,
    /// RTT < 300 ms, loss < 1%.
    Good,
    /// RTT < 1000 ms, loss < 5%.
    Poor,
    /// No response to pings.
    Lost,
}

/// How much fidelity the session should currently spend on the wire.
///
/// The single actionable output of the adaptive controller
/// ([`crate::transport::adaptive`]). Consumers read this and ignore the raw
/// signals unless they want to be clever.
///
/// Ordered worst-last, so `<` means "better": `Full < Reduced < Minimal <
/// Critical`. The controller relies on that ordering to decide whether an
/// observation is an improvement or a degradation, so do not reorder the
/// variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityTier {
    /// Everything as configured. The default, and the only tier a session
    /// ever sees while adaptive quality is disabled.
    #[default]
    Full,
    /// The link is struggling: shed the cheap fidelity first.
    Reduced,
    /// The link cannot carry a full-fidelity session.
    Minimal,
    /// Barely usable: protect intelligible speech and nothing else.
    Critical,
}

impl QualityTier {
    /// The next tier up, or `None` at [`QualityTier::Full`].
    ///
    /// Upgrades climb one step at a time — a link that just recovered
    /// has not yet proven it can carry `Full`, and oscillating between rates
    /// is worse for the listener than sitting one tier too low.
    pub fn better(self) -> Option<Self> {
        match self {
            Self::Full => None,
            Self::Reduced => Some(Self::Full),
            Self::Minimal => Some(Self::Reduced),
            Self::Critical => Some(Self::Minimal),
        }
    }

    /// Name as it appears in config and on the wire.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
            Self::Minimal => "minimal",
            Self::Critical => "critical",
        }
    }
}

/// A point-in-time view of the link, fused from every available signal.
///
/// Published on a per-session `watch` channel by the adaptive controller.
/// Everything except `tier` is diagnostics: `tier` is the knob consumers act
/// on. Fields are `Option` because the signals arrive at different rates and
/// some never arrive at all (a client that never sends `net_report`, a
/// transport with no send-latency instrumentation).
///
/// Signal sources are numbered S1, S2, S3, ... in the order the adaptive
/// controller was extended to consume them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NetworkConditions {
    /// S1 — ping/pong RTT bucket, unchanged semantics.
    pub quality: ConnectionQuality,
    /// S1 — smoothed round-trip time.
    pub rtt_ms: Option<u64>,
    /// S2/S3 — client arrival wall time over ideal audio duration. Above 1.0
    /// means the downlink could not sustain real time.
    pub arrival_rtf: Option<f32>,
    /// S2/S3 — playback underruns reported since the last observation.
    pub underruns_recent: u32,
    /// S4 — how long the server's socket writes are taking (a proxy for
    /// kernel send-buffer pressure).
    pub send_latency_ms: Option<u64>,
    /// S5 — client-side `ws.bufferedAmount` watermark, the uplink's backlog.
    pub uplink_backlog_bytes: Option<u64>,
    /// The derived, actionable knob.
    pub tier: QualityTier,
}

impl Default for NetworkConditions {
    fn default() -> Self {
        Self {
            quality: ConnectionQuality::Good,
            rtt_ms: None,
            arrival_rtf: None,
            underruns_recent: 0,
            send_latency_ms: None,
            uplink_backlog_bytes: None,
            tier: QualityTier::Full,
        }
    }
}

/// Session information returned to the client after connection.
///
/// Sent as a `session_ready` control message once handshake is complete.
/// The `negotiated` field tells the client what rates and providers the
/// server will actually use -- the client MUST honour these for correct
/// audio pipeline behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Unique session identifier.
    pub session_id: String,
    /// Token for reconnection.
    pub session_token: String,
    /// Whether this is a resumed session.
    pub resumed: bool,
    /// Echo of the client's original request (for debugging / logging).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_config: Option<SessionConfig>,
    /// The server's negotiated configuration -- authoritative.
    pub negotiated: NegotiatedConfig,
}

/// Server-authoritative configuration resulting from negotiation.
///
/// The client should configure its capture device to `input_sample_rate`
/// and its playback device to `output_sample_rate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegotiatedConfig {
    /// Audio encoding that will be used on the wire.
    pub audio_format: AudioFormat,
    /// Sample rate the server expects to *receive* from the client (Hz).
    pub input_sample_rate: u32,
    /// Sample rate the server will *send* to the client (Hz).
    pub output_sample_rate: u32,
    /// Number of audio channels.
    pub channels: u16,
    /// Resolved language tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Resolved voice / speaker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// STT provider name (informational).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_provider: Option<String>,
    /// TTS provider name (informational).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_provider: Option<String>,
    /// Arbitrary server-side application data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_data: Option<serde_json::Value>,
    /// Provider-specific key-value options echoed or augmented by the server.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub options: HashMap<String, serde_json::Value>,
    /// The resolved session mode.
    #[serde(default)]
    pub mode: SessionMode,
    /// Wire protocol version the server implements (authoritative).
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    /// Per-component metric descriptors (label, unit, fields) assembled at
    /// session build time.
    /// Frontend renders one generic `<TurnComponentMetricsTable>` driven
    /// by these so a new component never requires a UI code change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metric_descriptors: Vec<crate::pipeline::ComponentMetricDescriptor>,
    /// `Configure.options` keys the client sent that the server
    /// refused because they are not in `[realtime_provider] client_overrides`.
    /// Empty (and omitted) when everything was accepted or the session is
    /// not backed by a realtime provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_options: Vec<String>,
    /// Resolved identity of the model actually running for each component —
    /// even when the provider is a separate process (an OpenAI-compatible
    /// TTS/LLM sidecar, etc.). Resolved once at session setup and persisted to
    /// `metadata.json`, so the session report shows the real model + version
    /// instead of just the provider name. See [`crate::component::ModelIdentity`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt_model: Option<crate::component::ModelIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_model: Option<crate::component::ModelIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_model: Option<crate::component::ModelIdentity>,
    /// The id the server will give this session's first spoken utterance.
    ///
    /// A client that files its own timing reports (`speech_start`,
    /// `speech_end`) has to name the turn they belong to *before* the server
    /// has announced it, so it predicts the id, and the prediction needs the
    /// base. Stated here so a client reads it rather than assumes it.
    #[serde(default = "default_first_utterance_id")]
    pub first_utterance_id: u32,
    /// Ids above this belong to typed-text turns, which are numbered from a
    /// range of their own so they never collide with spoken ones.
    ///
    /// A client predicting the next *spoken* id must leave these out of its
    /// count: one typed message would otherwise move its prediction past
    /// ten thousand, and every spoken turn after it would be mislabelled.
    #[serde(default = "default_text_utterance_id_base")]
    pub text_utterance_id_base: u32,
}

/// The id of a session's first spoken utterance, in every session kind.
///
/// One value on purpose. If cascade and speech-to-speech sessions counted
/// from different bases, a client written against one would file the first
/// turn's speech reports under the second turn on the other -- which puts a
/// whole turn's duration into the second turn's measured response time. Starting at 1 also keeps a real turn distinct from the
/// zero that an unset `utterance_id` defaults to. (Typed-text turns are
/// numbered from a separate high range and are unaffected.)
pub const FIRST_UTTERANCE_ID: u32 = 1;

fn default_first_utterance_id() -> u32 {
    FIRST_UTTERANCE_ID
}

/// Typed-text turns are numbered upward from here (the first is one past it),
/// in a range far above any spoken session so the two cannot collide.
pub const TEXT_UTTERANCE_ID_BASE: u32 = 10_000;

fn default_text_utterance_id_base() -> u32 {
    TEXT_UTTERANCE_ID_BASE
}

fn default_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

/// Pipeline processing state, mirroring [`crate::pipeline::TurnState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineState {
    /// Waiting for user speech.
    Listening,
    /// Running STT / LLM processing.
    Processing,
    /// Playing TTS audio.
    Speaking,
    /// Output buffer is empty, waiting for client playback report.
    Draining,
}

impl From<crate::pipeline::TurnState> for PipelineState {
    fn from(ts: crate::pipeline::TurnState) -> Self {
        match ts {
            crate::pipeline::TurnState::Listening => Self::Listening,
            crate::pipeline::TurnState::Processing => Self::Processing,
            crate::pipeline::TurnState::Speaking => Self::Speaking,
            crate::pipeline::TurnState::Draining => Self::Draining,
        }
    }
}

/// Metadata about a transport session, available throughout its lifetime.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    /// Unique session identifier.
    pub session_id: String,
    /// Which transport protocol is in use.
    pub transport_type: TransportType,
    /// Remote address of the client, if available.
    pub remote_addr: Option<SocketAddr>,
    /// When this session was first established.
    pub connected_at: Instant,
    /// Current connection quality.
    pub quality: ConnectionQuality,
    /// Smoothed round-trip time in milliseconds.
    pub rtt_ms: Option<u64>,
    /// Identity from authentication, if any.
    pub auth_identity: Option<String>,
}

/// Transport protocol discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportType {
    /// WebSocket (RFC 6455).
    WebSocket,
    /// WebTransport over QUIC (future).
    WebTransport,
    /// In-process channels (CLI, tests).
    Direct,
}

#[cfg(test)]
#[path = "tests/transport.rs"]
mod tests;
