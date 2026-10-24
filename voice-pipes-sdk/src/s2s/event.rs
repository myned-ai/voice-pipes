//! Provider-agnostic event vocabulary for the Realtime client.
//!
//! Every [`RealtimeProtocol`](super::protocol::RealtimeProtocol)
//! implementation maps its wire-level event stream onto these typed
//! variants. Variants cover the union of behaviours across the
//! providers, so consumers need no provider-specific arms.

/// Largest tool-call argument string accepted from a provider. Tool
/// arguments are a JSON object of a few parameters; anything larger is
/// refused before it reaches the agent layer, the logs, or the client.
/// The engine enforces it on every event a provider produces;
/// [`RealtimeEvent::function_call`] applies it where the call is parsed.
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;

/// A typed event, as the engine's realtime client surfaces it.
///
/// Non-exhaustive: a new variant is an additive SDK change, so a consumer
/// outside this crate keeps a catch-all arm and ignores what it does not
/// know.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RealtimeEvent {
    /// Server-side VAD detected the start of speech in the audio we
    /// uplinked. Provider lifecycle event -- useful for diagnostics
    /// when "the caller seems mute" stalls happen.
    InputSpeechStarted,

    /// Server-side VAD detected end-of-speech in the audio we
    /// uplinked.
    InputSpeechStopped,

    /// Server committed the input audio buffer as a turn (will now
    /// produce a response).
    InputAudioCommitted,

    /// Provider produced a transcript for the audio we uplinked.
    /// `is_final` distinguishes a partial from a closed transcript.
    InputTranscript {
        /// The next piece of the transcript, appended to the pieces
        /// before it (a partial), or the whole transcript (a final).
        text: String,
        /// Whether this closes the transcript.
        is_final: bool,
    },

    /// Model began producing a response.
    ResponseStarted {
        /// The provider's response id, when it has one.
        response_id: Option<String>,
    },

    /// Provider attached an output item (e.g. an assistant `message`)
    /// to the in-flight response. The `item_id` is the handle needed
    /// to mutate that item later -- specifically to send `OpenAI`'s
    /// `conversation.item.truncate` envelope when the client barges
    /// in, so the model's stored conversation history reflects only
    /// the audio the user actually heard. Providers without an
    /// addressable item concept (Gemini Live today) do not emit this
    /// variant.
    ResponseOutputItemAdded {
        /// The provider's id for the item.
        item_id: String,
    },

    /// Streaming audio output from the model, already decoded to
    /// little-endian PCM16 at the requested pipeline sample rate.
    ResponseAudioDelta {
        /// Mono PCM16 samples.
        samples: Vec<i16>,
        /// Their sample rate.
        sample_rate: u32,
        /// Word-level speech timing for THIS chunk's audio, when the
        /// provider supplies it (`ElevenLabs` `alignment`; `OpenAI`/Gemini
        /// emit `None`). Offsets are relative to the chunk's audio
        /// start, in wall-milliseconds of the original audio — they
        /// are unaffected by resampling. Consumers use it for exact
        /// transcript↔audio mapping (barge-in truncation, karaoke);
        /// `None` falls back to their own estimates.
        alignment: Option<Vec<crate::tts::WordBoundary>>,
    },

    /// Streaming text output from the model -- either the assistant
    /// transcript paired with audio, or pure text in text-only mode.
    ResponseTextDelta {
        /// The text added by this delta.
        text: String,
    },

    /// Model finished a response (audio + text terminals collapsed
    /// onto a single boundary event).
    ResponseCompleted {
        /// The provider's response id, when it has one.
        response_id: Option<String>,
    },

    /// In-flight response was cancelled (typically in reply to our
    /// own `cancel()` call).
    ResponseCancelled {
        /// The provider's response id, when it has one.
        response_id: Option<String>,
    },

    /// Model invoked a tool / function. The adapter above is
    /// responsible for executing it and feeding the result back via
    /// the client's `send_function_output`.
    FunctionCall {
        /// The provider's id for this call, echoed with the result.
        call_id: String,
        /// The tool's name.
        name: String,
        /// The arguments, a JSON object as text.
        arguments: String,
    },

    /// The model withdrew calls it made and has not had answered, as
    /// when the user speaks over them (Gemini Live's
    /// `toolCallCancellation`). Their results are no longer wanted: the
    /// adapter should not send them, or start a response for them.
    FunctionCallsCancelled {
        /// The ids of the withdrawn calls.
        call_ids: Vec<String>,
    },

    /// The provider will close the connection soon, in about
    /// `time_left` when it says (Gemini Live's `goAway`, sent before a
    /// connection reaches its lifetime). A session that can resume
    /// ([`SessionResumable`](Self::SessionResumable)) moves to a new
    /// connection at a quiet moment before then, keeping its context.
    GoingAway {
        /// How long the provider says the connection has left.
        time_left: Option<std::time::Duration>,
    },

    /// The provider can resume this session on a new connection, context
    /// and all, from `token` (Gemini Live's `sessionResumptionUpdate`).
    /// Each supersedes the one before; a reconnect passes the latest in
    /// [`SessionConfig::resume_token`](super::protocol::SessionConfig::resume_token).
    SessionResumable {
        /// What the new connection presents to resume the session.
        token: crate::secrets::Secret,
    },

    /// Provider session was acknowledged (e.g. `session.created`,
    /// `setupComplete`). Useful for one-shot "wait until configured"
    /// gates.
    SessionUpdated,

    /// Provider emitted an error event. `recoverable` is a hint --
    /// transient errors (rate-limit, network blip) MAY be retried by
    /// the adapter; fatal errors (auth, `model_not_found`) MUST cause
    /// the session to be closed.
    Error {
        /// The provider's error code, when it gives one.
        code: Option<String>,
        /// What went wrong.
        message: String,
        /// Whether the session can carry on.
        recoverable: bool,
    },

    /// Underlying WebSocket closed (terminal). No further events.
    /// `code` is the WebSocket close code when the server sent a close
    /// frame (`None` for a bare stream end) — consumers feed it to
    /// [`RealtimeProtocol::classify_close`](super::protocol::RealtimeProtocol::classify_close)
    /// to decide between reconnecting and giving up.
    Closed {
        /// The WebSocket close code, when the server sent one.
        code: Option<u16>,
        /// The close reason, or why the stream ended.
        reason: String,
    },

    /// Per-response token-usage reported by the provider (`OpenAI`
    /// Realtime emits this as `response.done.response.usage`).
    /// All token counts are best-effort — providers may omit any
    /// subfield, so consumers MUST treat zeros as "unknown".
    ///
    /// `input_text_tokens` + `input_audio_tokens` typically sum to
    /// `input_tokens`; `input_cached_tokens` is a discount on
    /// `input_tokens` already counted (prompt cache hits, costed at
    /// the cached-token rate).
    ResponseUsage {
        /// The provider's response id, when it has one.
        response_id: Option<String>,
        /// All tokens.
        total_tokens: u32,
        /// Input tokens.
        input_tokens: u32,
        /// Output tokens.
        output_tokens: u32,
        /// Input tokens that were text.
        input_text_tokens: u32,
        /// Input tokens that were audio.
        input_audio_tokens: u32,
        /// Input tokens served from the prompt cache.
        input_cached_tokens: u32,
        /// Output tokens that were text.
        output_text_tokens: u32,
        /// Output tokens that were audio.
        output_audio_tokens: u32,
    },

    /// Rate-limit hints emitted by the provider (`OpenAI` Realtime
    /// `rate_limits.updated`). One entry per limit category
    /// (`requests`, `tokens`, ...). Surfaced so callers can warn
    /// before they trip the cap.
    RateLimits {
        /// One entry per limit category.
        items: Vec<RateLimitInfo>,
    },
}

impl RealtimeEvent {
    /// A tool call from the provider, or a recoverable error when it cannot
    /// be run. Every provider builds its calls here and the engine applies
    /// it again to every call it receives, so all of them follow one rule:
    /// * no call id, or no name: an error. Without an id the result cannot
    ///   be returned to the model, and without a name there is nothing to
    ///   call.
    /// * no arguments (empty or blank): `{}`, a call that takes none.
    /// * arguments over [`MAX_TOOL_ARGUMENT_BYTES`]: an error, so an
    ///   oversized string never reaches the agent layer, the logs or the
    ///   client whole.
    pub fn function_call(call_id: String, name: String, arguments: String) -> Self {
        if call_id.trim().is_empty() || name.trim().is_empty() {
            let missing = if name.trim().is_empty() {
                "name"
            } else {
                "call id"
            };
            return Self::Error {
                code: Some("tool_call_incomplete".to_string()),
                message: format!("tool call without a {missing} not run"),
                recoverable: true,
            };
        }
        let arguments = if arguments.trim().is_empty() {
            "{}".to_string()
        } else {
            arguments
        };
        if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
            return Self::Error {
                code: Some("tool_arguments_too_large".to_string()),
                message: format!(
                    "tool call '{name}' carried {} bytes of arguments (limit {})",
                    arguments.len(),
                    MAX_TOOL_ARGUMENT_BYTES
                ),
                recoverable: true,
            };
        }
        Self::FunctionCall {
            call_id,
            name,
            arguments,
        }
    }
}

/// One rate-limit entry. Mirrors the `OpenAI` Realtime
/// `rate_limits.updated.rate_limits[*]` shape.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RateLimitInfo {
    /// The limit's category (`requests`, `tokens`, ...).
    pub name: String,
    /// The cap.
    pub limit: u32,
    /// What is left of it.
    pub remaining: u32,
    /// Seconds until it resets.
    pub reset_seconds: f32,
}

impl RateLimitInfo {
    /// The limit `name`, capped at `limit`, with `remaining` left until it
    /// resets in `reset_seconds`.
    pub fn new(name: impl Into<String>, limit: u32, remaining: u32, reset_seconds: f32) -> Self {
        Self {
            name: name.into(),
            limit,
            remaining,
            reset_seconds,
        }
    }
}
