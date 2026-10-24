//! Provider-agnostic protocol surface.
//!
//! [`RealtimeProtocol`] is the trait every S2S backend implements. It
//! is purely a *wire-protocol mapper*: it knows how to build the
//! provider's JSON envelopes from neutral inputs ([`SessionConfig`],
//! base64 audio, ...) and how to parse provider events back into the
//! shared [`RealtimeEvent`] vocabulary. All connection/lifecycle/IO
//! lives in the engine's realtime client, so a provider crate is a pure mapper.

use anyhow::Result;

use super::event::RealtimeEvent;

/// Modalities the client expects the model to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Modalities {
    /// Audio only (most common for voice agents).
    Audio,
    /// Text only.
    Text,
    /// Both audio and (assistant) text -- not all providers honour
    /// the combined modality; protocol impls MAY downgrade silently.
    AudioAndText,
}

/// Server-side turn-detection policy applied to the *uplinked* audio.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TurnDetection {
    /// Default server-VAD with optional tunables. `silence_ms`,
    /// `threshold`, and `prefix_padding_ms` are advisory — a provider
    /// that does not support a given knob will fall back to its
    /// default.
    ServerVad {
        /// Silence that ends a turn, in milliseconds.
        silence_ms: Option<u32>,
        /// Speech-detection threshold.
        threshold: Option<f32>,
        /// Audio kept from before the detected start, in milliseconds.
        prefix_padding_ms: Option<u32>,
    },
    /// Disable server-side VAD -- the application drives turn
    /// boundaries explicitly via `create_response` / `cancel`.
    None,
    /// `OpenAI`'s semantic VAD, which uses natural language understanding
    /// to determine turn completion.
    SemanticVad {
        /// How readily a pause ends the turn (`low`, `medium`, `high`,
        /// `auto`).
        eagerness: Option<String>,
    },
}

impl Default for TurnDetection {
    fn default() -> Self {
        TurnDetection::ServerVad {
            silence_ms: None,
            threshold: None,
            prefix_padding_ms: None,
        }
    }
}

/// Function/tool that the model is allowed to invoke. Generic JSON
/// schema is used so the same struct works across providers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ToolSpec {
    /// The name the model calls it by.
    pub name: String,
    /// What it does, for the model.
    pub description: String,
    /// Its parameters, as a JSON Schema object.
    pub parameters_json_schema: serde_json::Value,
}

impl ToolSpec {
    /// A tool the model may call by `name`, described for the model, with
    /// its parameters as a JSON Schema object.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_json_schema: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_json_schema,
        }
    }
}

/// Neutral session configuration applied at `connect` time and
/// mapped to a provider-specific `session.update` / `setup` envelope.
#[derive(Clone)]
#[non_exhaustive]
pub struct SessionConfig {
    /// System / persona instructions handed to the model.
    pub instructions: String,
    /// Optional named voice override (provider-specific catalogue).
    pub voice: Option<String>,
    /// Output modalities the model should produce.
    pub modalities: Modalities,
    /// Server-side turn-detection policy.
    pub turn_detection: TurnDetection,
    /// Tools/functions exposed to the model. Empty = no tools.
    pub tools: Vec<ToolSpec>,
    /// Provider-specific tuning knobs that don't deserve first-class
    /// fields on this neutral struct: the `[realtime_provider.options]`
    /// table, passed through. The keys are the provider's own; each
    /// implementation reads the ones it defines (its factory refuses the
    /// rest at load) and documents them in its crate and provider guide.
    pub provider_options: std::collections::HashMap<String, serde_json::Value>,
    /// The model opens the conversation, with no input from the other side
    /// (a caller placing a call). Otherwise it waits to be spoken to.
    /// Providers whose agent configuration decides this (`ElevenLabs`)
    /// ignore it.
    pub speaks_first: bool,
    /// The provider transcribes the audio it is sent, so the session has
    /// the other side's words as text. Off when nothing reads them.
    pub transcribe_input: bool,
    /// Resume the provider session this token came from
    /// ([`RealtimeEvent::SessionResumable`]) instead of starting afresh:
    /// set on a reconnect, so the conversation's context carries over.
    /// Providers that cannot resume ignore it.
    pub resume_token: Option<crate::secrets::Secret>,
}

impl Default for SessionConfig {
    /// A session that waits to be spoken to and transcribes what it hears:
    /// an agent answering a person.
    fn default() -> Self {
        Self {
            instructions: String::new(),
            voice: None,
            modalities: Modalities::Audio,
            turn_detection: TurnDetection::default(),
            tools: Vec::new(),
            provider_options: std::collections::HashMap::new(),
            speaks_first: false,
            transcribe_input: true,
            resume_token: None,
        }
    }
}

impl SessionConfig {
    /// The defaults, with these instructions for the model.
    pub fn new(instructions: impl Into<String>) -> Self {
        Self {
            instructions: instructions.into(),
            ..Self::default()
        }
    }

    with_setters! {
        with_voice => voice: Option<String>,
        with_modalities => modalities: Modalities,
        with_turn_detection => turn_detection: TurnDetection,
        with_tools => tools: Vec<ToolSpec>,
        with_provider_options => provider_options: std::collections::HashMap<String, serde_json::Value>,
        with_speaks_first => speaks_first: bool,
        with_transcribe_input => transcribe_input: bool,
        with_resume_token => resume_token: Option<crate::secrets::Secret>,
    }
}

/// The provider options can carry a credential (an `ElevenLabs` dynamic
/// variable, say), so `Debug` redacts secret-named keys.
impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("instructions", &self.instructions)
            .field("voice", &self.voice)
            .field("modalities", &self.modalities)
            .field("turn_detection", &self.turn_detection)
            .field("tools", &self.tools)
            .field(
                "provider_options",
                &crate::secrets::debug_options(&self.provider_options),
            )
            .field("speaks_first", &self.speaks_first)
            .field("transcribe_input", &self.transcribe_input)
            .field("resume_token", &self.resume_token)
            .finish()
    }
}

/// Connection target produced by a [`RealtimeProtocol`].
///
/// Headers are applied to the WS upgrade request (used by `OpenAI` for
/// `Authorization: Bearer`). Gemini puts the API key in the URL and
/// returns an empty header list.
#[derive(Clone)]
#[non_exhaustive]
pub struct ConnectInfo {
    /// The WebSocket URL.
    pub url: String,
    /// Headers for the upgrade request.
    pub headers: Vec<(String, String)>,
}

impl ConnectInfo {
    /// Connect to `url` with no extra headers.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
        }
    }

    /// With the header `name: value` added to the upgrade request.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// The URL may carry an API key in its query string and the headers
/// carry bearer tokens, so `Debug` shows only the host and the header
/// names.
impl std::fmt::Debug for ConnectInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let host = url_host(&self.url).unwrap_or("<unparsed>");
        f.debug_struct("ConnectInfo")
            .field("host", &host)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// The host of `scheme://[user[:pass]@]host[:port][/path][?query]`, and
/// nothing else of the URL: no credentials, port, path or query.
fn url_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split_once(']')?.0
    } else {
        host_port.split(':').next()?
    };
    (!host.is_empty()).then_some(host)
}

/// Verdict on a server-initiated WebSocket close — see
/// [`RealtimeProtocol::classify_close`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CloseClass {
    /// Transient (network blip, idle timeout, server restart): the
    /// runner may reconnect and resume the session.
    Transient,
    /// Permanent (quota exhausted, auth/entitlement failure, invalid
    /// model): reconnecting reproduces the same close — end the
    /// session and surface the reason to the client instead.
    Fatal,
    /// The provider refused to resume a session from the
    /// [`resume_token`](SessionConfig::resume_token) it was given
    /// (expired, or unknown): reconnect with a fresh session. Only a
    /// connection opened with a token can be refused this way; without
    /// one the close is fatal.
    Unresumable,
}

/// Default close classification shared by all providers.
///
/// Reason keywords cover the vocabularies observed live: `ElevenLabs`
/// closes 1002 with "This request exceeds your quota limit.", Gemini
/// uses 1008/1011 with "Quota exceeded" / "API key not valid",
/// OpenAI-style auth failures say "Incorrect API key provided".
/// Code 1008 (policy violation) is fatal regardless of text — servers
/// use it for auth/entitlement rejections.
pub fn default_classify_close(code: Option<u16>, reason: &str) -> CloseClass {
    if code == Some(1008) {
        return CloseClass::Fatal;
    }
    const FATAL_MARKERS: &[&str] = &[
        "quota",
        "credit",
        "billing",
        "payment",
        "unauthorized",
        "forbidden",
        "authentication",
        "api key",
        "api_key",
        "apikey",
        "permission denied",
        "not entitled",
        "insufficient funds",
        "account suspended",
        "invalid model",
        "model_not_found",
    ];
    let lower = reason.to_lowercase();
    if FATAL_MARKERS.iter().any(|m| lower.contains(m)) {
        CloseClass::Fatal
    } else {
        CloseClass::Transient
    }
}

/// Maps provider-specific wire protocol onto the shared
/// [`RealtimeEvent`] vocabulary.
///
/// Implementations are wire-protocol descriptors -- the generic
/// the engine's realtime client owns all IO and
/// lifecycle. Mostly stateless; a protocol may keep small parsing
/// state (e.g. `GeminiProtocol` tracks the last raw transcript to
/// compute streaming deltas).
pub trait RealtimeProtocol: Send + Sync + 'static {
    /// Short human-readable name used for tracing targets / metrics.
    fn name(&self) -> &'static str;

    /// PCM16 sample rate the provider expects on uplinked audio
    /// (e.g. `24_000` for `OpenAI`, `16_000` for Gemini Live). The engine
    /// reads it before every uplinked frame, so a protocol that learns its
    /// rate from the vendor after connecting may change the answer.
    fn native_input_sample_rate(&self) -> u32;

    /// PCM16 sample rate the provider emits on downlink audio.
    fn native_output_sample_rate(&self) -> u32;

    /// Build the WS URL and headers. The credential is the one the host
    /// resolved and handed to the factory; a protocol never reads the
    /// environment itself.
    fn connect_info(&self) -> Result<ConnectInfo>;

    /// Build the session-configuration envelope (`session.update` on
    /// `OpenAI`, `setup` on Gemini). May be a multi-envelope sequence
    /// when a provider requires it -- impls return one JSON object
    /// per outbound frame.
    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value>;

    /// Build the audio-append envelope from base64-encoded
    /// native-rate PCM16. The client handles resampling
    /// and encoding before calling this.
    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value;

    /// Build an envelope that triggers the model to produce a
    /// response (sent by the client after `send_text` and as the
    /// initial "kick" on `configure`). `None` if the provider
    /// auto-responds and no explicit kick is needed.
    fn build_response_create(&self, modalities: Modalities) -> Option<serde_json::Value>;

    /// Build a user-text-message envelope. Returns `None` if the
    /// provider does not support text input on a Realtime session.
    fn build_text_message(&self, text: &str) -> Option<serde_json::Value>;

    /// Build a cancel-response envelope. Providers without
    /// first-class cancellation (Gemini Live today) return a no-op
    /// envelope that is safe to send unconditionally.
    fn build_cancel(&self) -> Option<serde_json::Value>;

    /// Classify a server-initiated WebSocket close: should the session
    /// runner attempt to reconnect, or is the condition permanent?
    /// Quota exhaustion, auth failures, and entitlement problems recur
    /// on every reconnect — retrying them is a connection storm (an
    /// `ElevenLabs` quota close was observed re-closing every ~1.5 s,
    /// each attempt opening a fresh billed conversation). The default
    /// matches on close code and universal reason keywords; providers
    /// with richer close vocabularies can override.
    fn classify_close(&self, code: Option<u16>, reason: &str) -> CloseClass {
        default_classify_close(code, reason)
    }

    /// Build a "truncate this stored assistant item to N ms of audio"
    /// envelope. Used after a barge-in so the provider's conversation
    /// history reflects only what the user actually heard (and the
    /// model doesn't reference unspoken words on the next turn).
    /// Returns `None` for providers that don't support per-item
    /// truncation (Gemini Live today).
    fn build_item_truncate(&self, _item_id: &str, _audio_end_ms: u32) -> Option<serde_json::Value> {
        None
    }

    /// Build the envelope that returns a tool-call result to the
    /// model. `None` if tools are not yet wired for this provider.
    ///
    /// `name` is the called function's name — Gemini's
    /// `FunctionResponse` requires it alongside `id` (per the Live API
    /// tools doc); `OpenAI` correlates by `call_id` alone and ignores it.
    ///
    /// `silent` asks the provider not to voice a reaction to this
    /// output (client-side tool acks).
    /// `OpenAI` implements silence by the *caller* withholding the
    /// `response.create` kick, so its builder ignores the flag; Gemini
    /// auto-continues after every `toolResponse`, so its builder maps
    /// the flag onto the native-audio async function-calling
    /// `scheduling: SILENT | WHEN_IDLE` vocabulary instead.
    fn build_function_call_output(
        &self,
        call_id: &str,
        name: &str,
        result: &str,
        silent: bool,
    ) -> Option<serde_json::Value>;

    /// Build the envelope sequence that injects `text` into the
    /// conversation as **context** (system/background role — not a
    /// user message, which would pollute conversation semantics).
    /// Used for the late result of a `Pending` tool call and for
    /// agent-initiated out-of-turn speech. When `respond` is true the
    /// sequence also triggers a model response — each provider
    /// composes its own kick (`OpenAI` appends `response.create`;
    /// Gemini commits the injected turn). Empty when the provider
    /// supports neither.
    fn build_context_message(
        &self,
        _text: &str,
        _respond: bool,
        _modalities: Modalities,
    ) -> Vec<serde_json::Value> {
        Vec::new()
    }

    /// Build an immediate protocol-level reply to an inbound frame,
    /// or `None` when the frame needs no reply. Called by the
    /// client's reader for
    /// every JSON frame *before* [`parse_event`](Self::parse_event);
    /// the reply is written back over the socket without surfacing an
    /// event. Used for application-level keepalives (`ElevenLabs` sends
    /// a JSON `ping` carrying an `event_id` that must be echoed in a
    /// `pong` — distinct from WS-level ping/pong frames, which the WS
    /// library already answers). Like `parse_event`, implementations
    /// MUST be pure (no IO).
    fn build_auto_reply(&self, _v: &serde_json::Value) -> Option<serde_json::Value> {
        None
    }

    /// Parse one server JSON frame into zero or more
    /// [`RealtimeEvent`]s. Implementations MUST be pure (no IO) so
    /// the parser can be unit-tested in isolation.
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent>;

    /// For providers whose wire protocol has NO reliable end-of-response
    /// event (`ElevenLabs` — completion is inferred from the *next* turn's
    /// events): how long a response may go without producing audio/text
    /// before the runner treats it as completed. `None` (default) means
    /// the provider emits explicit completion events and no idle
    /// inference is needed. Without this, the last response of an
    /// exchange never closes: the session sticks in "speaking", the
    /// turn record is only flushed at teardown, and queued agent events
    /// never drain.
    fn inferred_completion_idle_ms(&self) -> Option<u64> {
        None
    }
}

/// Blanket impl so callers that resolve a protocol dynamically (e.g.
/// through a [`RealtimeFactory`](super::RealtimeFactory)) can still
/// parameterise the engine's generic client over
/// `Box<dyn RealtimeProtocol>` without an extra wrapper newtype.
impl RealtimeProtocol for Box<dyn RealtimeProtocol> {
    fn name(&self) -> &'static str {
        (**self).name()
    }
    fn native_input_sample_rate(&self) -> u32 {
        (**self).native_input_sample_rate()
    }
    fn native_output_sample_rate(&self) -> u32 {
        (**self).native_output_sample_rate()
    }
    fn connect_info(&self) -> Result<ConnectInfo> {
        (**self).connect_info()
    }
    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value> {
        (**self).build_session_update(cfg)
    }
    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value {
        (**self).build_audio_append(b64_pcm)
    }
    fn build_response_create(&self, modalities: Modalities) -> Option<serde_json::Value> {
        (**self).build_response_create(modalities)
    }
    fn build_text_message(&self, text: &str) -> Option<serde_json::Value> {
        (**self).build_text_message(text)
    }
    fn build_cancel(&self) -> Option<serde_json::Value> {
        (**self).build_cancel()
    }
    fn classify_close(&self, code: Option<u16>, reason: &str) -> CloseClass {
        (**self).classify_close(code, reason)
    }
    fn build_item_truncate(&self, item_id: &str, audio_end_ms: u32) -> Option<serde_json::Value> {
        (**self).build_item_truncate(item_id, audio_end_ms)
    }
    fn build_function_call_output(
        &self,
        call_id: &str,
        name: &str,
        result: &str,
        silent: bool,
    ) -> Option<serde_json::Value> {
        (**self).build_function_call_output(call_id, name, result, silent)
    }
    fn build_context_message(
        &self,
        text: &str,
        respond: bool,
        modalities: Modalities,
    ) -> Vec<serde_json::Value> {
        (**self).build_context_message(text, respond, modalities)
    }
    fn build_auto_reply(&self, v: &serde_json::Value) -> Option<serde_json::Value> {
        (**self).build_auto_reply(v)
    }
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent> {
        (**self).parse_event(v)
    }
    fn inferred_completion_idle_ms(&self) -> Option<u64> {
        (**self).inferred_completion_idle_ms()
    }
}

#[cfg(test)]
#[path = "../tests/s2s_protocol.rs"]
mod tests;
