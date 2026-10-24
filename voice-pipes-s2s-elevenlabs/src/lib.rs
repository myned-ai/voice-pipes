//! `ElevenLabs` Agents (Conversational AI) as a voice-pipes
//! speech-to-speech plugin.
//!
//! A wire-protocol mapper only: the engine's realtime client owns the
//! connection. The binary registers [`factory`] and [`wizard_choices`];
//! `[realtime_provider] provider = "elevenlabs"` selects it.
//!
//! Endpoint:
//! `wss://api.elevenlabs.io/v1/convai/conversation?agent_id=...`.
//! Private agents authenticate with the `xi-api-key` header on the WS
//! upgrade (server-side only — never expose the key to browsers);
//! public agents connect with the bare `agent_id`.
//!
//! Unlike `OpenAI` Realtime / Gemini Live, `ElevenLabs` hosts the whole
//! agent (persona prompt, LLM choice, voice, VAD/turn-taking, tool
//! *definitions*) in the agent's dashboard/API config. The socket is a
//! thin conversation surface:
//!
//! | `RealtimeClient` call       | `ElevenLabs` envelope                                  |
//! |-----------------------------|--------------------------------------------------------|
//! | `connect` + `session_update` | `conversation_initiation_client_data` (+ overrides)   |
//! | `send_audio(pcm)`           | `{"user_audio_chunk": "<b64>"}`                        |
//! | `send_text(text)`           | `{"type":"user_message","text":...}`                   |
//! | `send_function_output(...)` | `{"type":"client_tool_result",...}`                    |
//! | `send_context_message(...)` | `contextual_update` (respond=false) / `user_message`   |
//! | `cancel()`                  | none — barge-in is server-VAD driven                   |
//!
//! Inbound:
//! | `ElevenLabs` event                 | `RealtimeEvent`                          |
//! |------------------------------------|------------------------------------------|
//! | `conversation_initiation_metadata` | `SessionUpdated` (+ records audio rates) |
//! | `audio`                            | `ResponseStarted` (first) + `ResponseAudioDelta` |
//! | `agent_response`                   | `ResponseStarted` (first) + `ResponseTextDelta`  |
//! | `interruption`                     | `ResponseCancelled`                      |
//! | `user_transcript`                  | `InputTranscript { is_final: true }`     |
//! | `client_tool_call`                 | `FunctionCall`                           |
//! | `client_error`                     | `Error`                                  |
//! | `ping`                             | (auto-replied with `pong` — no event)    |
//!
//! Turn boundaries: the wire protocol has no reliable end-of-response
//! event (production omits `agent_response_complete` / a final-audio
//! flag; both are still honoured when present), so `ResponseCompleted`
//! is *inferred* from the strictly turn-based conversation flow: a
//! `user_transcript` (the user spoke next) or a second `agent_response`
//! (the agent's full turn text arrives exactly once per turn — a
//! repeat means a new turn, e.g. after a typed `user_message` that
//! produces no `user_transcript`) closes the open response before the
//! new event is surfaced.
//!
//! Tool calling: tool *definitions* cannot be registered over the
//! socket — they must exist on the agent (dashboard → Tools, type
//! "Client"), with names matching this pipeline's `[[client_tool]]`
//! entries / server tool registry exactly (case-sensitive). Calls then
//! arrive as `client_tool_call` and results return as
//! `client_tool_result`, so the runner's existing dispatch works
//! unchanged. `build_session_update` logs the advertised tool names to
//! make agent-side misconfiguration diagnosable.

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

mod factory;
mod wizard;

pub(crate) use factory::ElevenLabsFactory;
pub use wizard::wizard_choices;

/// The factory the binary registers.
pub fn factory() -> Box<dyn RealtimeFactory> {
    Box::new(ElevenLabsFactory)
}

/// An environment lookup: the process's, or a test's.
pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The process environment, as an [`Env`].
pub(crate) fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};

use anyhow::{Result, anyhow};

use voice_pipes_sdk::s2s::codec::{decode_pcm16_b64, url_query_encode};
use voice_pipes_sdk::s2s::{
    ConnectInfo, Modalities, RealtimeEvent, RealtimeFactory, RealtimeProtocol, SessionConfig,
};
use voice_pipes_sdk::secrets::Secret;
use voice_pipes_sdk::tts::{WordBoundary, WordGrouper};

/// Default WS endpoint base. Regional deployments (EU/India/SG
/// residency) override via `[realtime_provider.options] endpoint`.
pub(crate) const DEFAULT_ENDPOINT: &str = "wss://api.elevenlabs.io";
/// Audio rate assumed for both directions until the server's
/// `conversation_initiation_metadata` reports the agent's configured
/// formats (`pcm_16000` is the `ElevenLabs` agent default).
pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 16_000;
/// Default idle window after which a response with no new audio/text is
/// treated as completed (the wire protocol has no end-of-response
/// event — see the module docs). Long enough to bridge inter-sentence
/// TTS generation gaps, short enough that the session doesn't linger
/// in "speaking" after the reply ends. Override via
/// `[realtime_provider.options] completion_idle_ms`.
pub(crate) const COMPLETION_IDLE_MS: u64 = 1_500;

/// `ElevenLabs` Agents protocol descriptor.
///
/// Session-scoped parsing state (negotiated audio formats, response
/// open/closed, last interruption id) uses interior mutability because
/// `parse_event` takes `&self`; a protocol instance is per-session so
/// the atomics are uncontended.
#[derive(Debug)]
pub(crate) struct ElevenLabsProtocol {
    agent_id: String,
    /// The key the engine resolved from the environment.
    api_key: Option<Secret>,
    endpoint: String,
    /// Idle window for inferred response completion (see the module docs above).
    completion_idle_ms: u64,
    /// LLM override sent as `agent.prompt.llm` (from `model` in TOML).
    /// Requires the "LLM" override to be enabled in the agent's
    /// Security tab.
    llm_override: Option<String>,
    /// Uplink rate the agent expects (`user_input_audio_format`).
    input_rate: AtomicU32,
    /// Downlink rate the agent emits (`agent_output_audio_format`).
    output_rate: AtomicU32,
    /// Whether a model response is currently open. `ElevenLabs` has no
    /// explicit response-start event, so the first `audio` /
    /// `agent_response` after idle opens one; see the module docs for
    /// how it is closed (boundary inference).
    response_open: AtomicBool,
    /// Whether the open response has received its `agent_response`
    /// text yet. The full turn text arrives exactly once per turn (in
    /// either order relative to the turn's audio), so a second
    /// `agent_response` while this is set marks the next turn.
    response_has_text: AtomicBool,
    /// `event_id` of the last `interruption` event. Audio events at or
    /// below this id are stale in-flight chunks from the interrupted
    /// response and are dropped (the same filter the official SDKs
    /// apply to prevent "audio ghosts" after a barge-in).
    last_interrupt_id: AtomicI64,
    /// Highest audio `event_id` seen. An audio event with a *lower* id
    /// is a re-delivered frame from a server-side re-stream and is
    /// dropped (strictly-less-than, so providers that stamp one id per
    /// response are unaffected). Live-observed: a response re-streamed
    /// mid-conversation doubled a turn's audio.
    last_audio_id: AtomicI64,
    /// Text of the open response's `agent_response`, for duplicate
    /// suppression: the same text re-delivered while the response is
    /// open and *no new user input has occurred* is a server-side
    /// re-stream, not a new turn. Cleared on any user input
    /// (`user_message`, `user_transcript`), on close, and on
    /// interruption, so a legitimately repeated identical answer to a
    /// repeated question still passes.
    last_agent_text: parking_lot::Mutex<String>,
}

impl Default for ElevenLabsProtocol {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            api_key: None,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            completion_idle_ms: COMPLETION_IDLE_MS,
            llm_override: None,
            input_rate: AtomicU32::new(DEFAULT_SAMPLE_RATE),
            output_rate: AtomicU32::new(DEFAULT_SAMPLE_RATE),
            response_open: AtomicBool::new(false),
            response_has_text: AtomicBool::new(false),
            last_interrupt_id: AtomicI64::new(-1),
            last_audio_id: AtomicI64::new(-1),
            last_agent_text: parking_lot::Mutex::new(String::new()),
        }
    }
}

impl ElevenLabsProtocol {
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            ..Self::default()
        }
    }

    /// The API key the engine resolved; without one, `connect_info` fails.
    #[must_use]
    pub fn with_api_key(mut self, key: Option<Secret>) -> Self {
        self.api_key = key;
        self
    }

    /// Override the WS endpoint base (regional residency deployments,
    /// e.g. `wss://api.eu.residency.elevenlabs.io`). Any trailing `/`
    /// is trimmed.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        let e = endpoint.into();
        self.endpoint = e.trim_end_matches('/').to_string();
        self
    }

    /// Idle window for inferred response completion. Raise it if the
    /// agent's speech gets split into two turns mid-reply (very long
    /// inter-sentence generation gaps); lower it for snappier
    /// end-of-turn detection.
    #[must_use]
    pub fn with_completion_idle_ms(mut self, ms: u64) -> Self {
        self.completion_idle_ms = ms;
        self
    }

    /// LLM the agent should run instead of its configured one
    /// (`agent.prompt.llm` override). Wired from `model` in
    /// `[realtime_provider]`.
    #[must_use]
    pub fn with_llm_override(mut self, llm: Option<String>) -> Self {
        self.llm_override = llm;
        self
    }

    /// Parse an `ElevenLabs` audio-format tag (`pcm_16000`, `pcm_24000`,
    /// ...) into a sample rate. `ulaw_8000` (telephony) is not decoded
    /// by this pipeline. The rate comes from the server and sizes
    /// resampler buffers on both directions, so only known rates are
    /// accepted.
    fn parse_format_rate(tag: &str) -> Option<u32> {
        let rate: u32 = tag.strip_prefix("pcm_")?.parse().ok()?;
        voice_pipes_sdk::s2s::codec::SUPPORTED_SAMPLE_RATES
            .contains(&rate)
            .then_some(rate)
    }

    /// Group the char-level `alignment` payload of an `audio` event
    /// into word-level timings.
    ///
    /// Wire facts (probed live 2026-07-14):
    ///   - `char_start_times_ms` are **chunk-relative** (restart at 0
    ///     on every audio event), so values pass through unchanged;
    ///   - every audio chunk of a response carried `alignment` (no
    ///     `normalized_alignment` sibling observed);
    ///   - the chars joined across a response's chunks equal the
    ///     `agent_response` text exactly (plus a trailing space);
    ///   - a chunk's audio may extend past its last char (trailing
    ///     padding) — word times, not chunk duration, are
    ///     authoritative.
    ///
    /// Whitespace delimits words; punctuation stays attached. Ragged /
    /// malformed arrays truncate to the shortest length — a bad frame
    /// degrades to fewer (or no) words, never a panic. A word split
    /// across two chunks surfaces as two partial words with correct
    /// times (cosmetic; downstream matching is whitespace-insensitive).
    fn parse_alignment(v: &serde_json::Value) -> Option<Vec<WordBoundary>> {
        let chars = v.get("chars")?.as_array()?;
        let starts = v.get("char_start_times_ms")?.as_array()?;
        let durations = v.get("char_durations_ms")?.as_array()?;
        // Times restart with every chunk, so each chunk groups on its own.
        let mut grouper = WordGrouper::new();
        let mut words: Vec<WordBoundary> = Vec::new();
        for ((ch, start), dur) in chars.iter().zip(starts).zip(durations) {
            // A non-string entry reads as "", which ends the open word.
            let ch = ch.as_str().unwrap_or("");
            let start = whole_ms(start.as_f64().unwrap_or(0.0));
            let dur = whole_ms(dur.as_f64().unwrap_or(0.0));
            // Vendor floats: a huge value casts to u64::MAX, so the sum
            // saturates rather than overflowing.
            grouper.push(ch, start, start.saturating_add(dur), &mut words);
        }
        grouper.flush(&mut words);
        if words.is_empty() { None } else { Some(words) }
    }

    /// Open a response if none is in flight; returns the
    /// `ResponseStarted` event to emit when this call opened it.
    fn open_response(&self) -> Option<RealtimeEvent> {
        let was_open = self.response_open.swap(true, Ordering::Relaxed);
        (!was_open).then(|| {
            self.response_has_text.store(false, Ordering::Relaxed);
            RealtimeEvent::ResponseStarted { response_id: None }
        })
    }

    /// Close the open response; returns the `ResponseCompleted` event
    /// to emit when this call closed one.
    fn close_response(&self) -> Option<RealtimeEvent> {
        let was_open = self.response_open.swap(false, Ordering::Relaxed);
        was_open.then(|| {
            self.response_has_text.store(false, Ordering::Relaxed);
            self.last_agent_text.lock().clear();
            RealtimeEvent::ResponseCompleted { response_id: None }
        })
    }
}

impl ElevenLabsProtocol {
    fn connect_target(&self) -> Result<ConnectInfo> {
        if self.agent_id.trim().is_empty() {
            return Err(anyhow!(
                "elevenlabs requires an agent id -- set [realtime_provider.options] agent_id"
            ));
        }
        let url = format!(
            "{}/v1/convai/conversation?agent_id={}",
            self.endpoint,
            url_query_encode(&self.agent_id)
        );
        // The key is optional: public agents accept anonymous
        // connections. Private agents reject the upgrade without it.
        let info = ConnectInfo::new(url);
        Ok(match &self.api_key {
            Some(key) => info.with_header("xi-api-key", key.expose()),
            None => info,
        })
    }
}

impl RealtimeProtocol for ElevenLabsProtocol {
    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    fn native_input_sample_rate(&self) -> u32 {
        // The engine reads this before every uplinked frame, so the rate
        // `conversation_initiation_metadata` names applies to all audio
        // after the handshake.
        self.input_rate.load(Ordering::Relaxed)
    }

    fn native_output_sample_rate(&self) -> u32 {
        self.output_rate.load(Ordering::Relaxed)
    }

    fn connect_info(&self) -> Result<ConnectInfo> {
        self.connect_target()
    }

    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value> {
        self.response_open.store(false, Ordering::Relaxed);
        self.response_has_text.store(false, Ordering::Relaxed);
        self.last_interrupt_id.store(-1, Ordering::Relaxed);
        self.last_audio_id.store(-1, Ordering::Relaxed);
        self.last_agent_text.lock().clear();
        // The agent's persona/voice/tools live in the ElevenLabs agent
        // config; `conversation_config_override` fields are rejected
        // unless explicitly enabled in the agent's Security tab. Only
        // deliberately-set values are sent so a default pipeline works
        // against an agent with overrides disabled:
        //   - `voice` (TOML `voice`)            → tts.voice_id
        //   - `model` (TOML `model`)            → agent.prompt.llm
        //   - options `override_prompt = true`  → agent.prompt.prompt from
        //     `instructions` (opt-in because the runner auto-appends
        //     client-tool guidance to otherwise-empty instructions)
        //   - options `language`                → agent.language
        //   - options `first_message`           → agent.first_message
        //   - options `dynamic_variables` (table) / `user_id` ride the
        //     top-level initiation payload.
        let opts = &cfg.provider_options;
        let mut agent = serde_json::Map::new();
        let mut prompt = serde_json::Map::new();
        if opts
            .get("override_prompt")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            && !cfg.instructions.is_empty()
        {
            prompt.insert("prompt".into(), serde_json::json!(cfg.instructions));
        }
        if let Some(llm) = &self.llm_override {
            prompt.insert("llm".into(), serde_json::json!(llm));
        }
        if !prompt.is_empty() {
            agent.insert("prompt".into(), serde_json::Value::Object(prompt));
        }
        if let Some(lang) = opts.get("language").and_then(|v| v.as_str()) {
            agent.insert("language".into(), serde_json::json!(lang));
        }
        if let Some(msg) = opts.get("first_message").and_then(|v| v.as_str()) {
            agent.insert("first_message".into(), serde_json::json!(msg));
        }

        let mut overrides = serde_json::Map::new();
        if !agent.is_empty() {
            overrides.insert("agent".into(), serde_json::Value::Object(agent));
        }
        if let Some(voice) = &cfg.voice {
            overrides.insert("tts".into(), serde_json::json!({ "voice_id": voice }));
        }
        if cfg.modalities == Modalities::Text {
            overrides.insert(
                "conversation".into(),
                serde_json::json!({ "text_only": true }),
            );
        }

        let mut init = serde_json::Map::new();
        init.insert(
            "type".into(),
            serde_json::json!("conversation_initiation_client_data"),
        );
        if !overrides.is_empty() {
            init.insert(
                "conversation_config_override".into(),
                serde_json::Value::Object(overrides),
            );
        }
        if let Some(vars) = opts.get("dynamic_variables").filter(|v| v.is_object()) {
            init.insert("dynamic_variables".into(), vars.clone());
        }
        if let Some(user_id) = opts.get("user_id").and_then(|v| v.as_str()) {
            init.insert("user_id".into(), serde_json::json!(user_id));
        }

        // Tool definitions cannot ride the socket — they must exist on
        // the agent with exactly these (case-sensitive) names. Logged
        // so a name mismatch is diagnosable from the worker log alone.
        if !cfg.tools.is_empty() {
            let names: Vec<&str> = cfg.tools.iter().map(|t| t.name.as_str()).collect();
            tracing::info!(
                agent_id = %self.agent_id,
                tools = ?names,
                "elevenlabs: tools must be defined as Client tools on the agent \
                 (dashboard → Agent → Tools) with these exact names"
            );
        }

        vec![serde_json::Value::Object(init)]
    }

    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value {
        serde_json::json!({ "user_audio_chunk": b64_pcm })
    }

    fn build_response_create(&self, _modalities: Modalities) -> Option<serde_json::Value> {
        // The agent auto-responds after server-side turn detection;
        // there is no explicit "respond now" envelope.
        None
    }

    fn build_text_message(&self, text: &str) -> Option<serde_json::Value> {
        // New user input: whatever the agent said before is no longer a
        // re-delivery candidate — an identical follow-up answer is a
        // legitimate new turn.
        self.last_agent_text.lock().clear();
        Some(serde_json::json!({ "type": "user_message", "text": text }))
    }

    fn build_cancel(&self) -> Option<serde_json::Value> {
        // No client-driven cancel: barge-in is detected by the server's
        // VAD on the continuously-uplinked mic audio, which then emits
        // an `interruption` event.
        None
    }

    fn build_function_call_output(
        &self,
        call_id: &str,
        name: &str,
        result: &str,
        silent: bool,
    ) -> Option<serde_json::Value> {
        // Whether the agent voices a reaction is governed by the
        // agent-side tool config ("wait for response" + LLM prompt);
        // there is no per-result silence flag, so `silent` relies on
        // the standing system-prompt convention (like OpenAI).
        let _ = (name, silent);
        Some(serde_json::json!({
            "type": "client_tool_result",
            "tool_call_id": call_id,
            "result": result,
            "is_error": false,
        }))
    }

    fn build_context_message(
        &self,
        text: &str,
        respond: bool,
        _modalities: Modalities,
    ) -> Vec<serde_json::Value> {
        if respond {
            // No "inject context and respond" primitive; the closest is
            // a user message tagged as background so the model doesn't
            // attribute it to the caller's voice.
            vec![serde_json::json!({
                "type": "user_message",
                "text": format!("[background context — not spoken by the user] {text}"),
            })]
        } else {
            // First-class non-interrupting context injection.
            vec![serde_json::json!({ "type": "contextual_update", "text": text })]
        }
    }

    fn inferred_completion_idle_ms(&self) -> Option<u64> {
        Some(self.completion_idle_ms)
    }

    fn build_auto_reply(&self, v: &serde_json::Value) -> Option<serde_json::Value> {
        // Application-level keepalive: the server pings with an
        // event_id and expects a matching pong (the WS-level ping/pong
        // frames are separate and already handled by tungstenite).
        if v.get("type").and_then(|t| t.as_str()) != Some("ping") {
            return None;
        }
        let event_id = v.get("ping_event").and_then(|p| p.get("event_id"))?;
        Some(serde_json::json!({ "type": "pong", "event_id": event_id }))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one match arm per wire event type; splitting the dispatch \
                  across helpers would obscure the protocol surface"
    )]
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent> {
        let mut out = Vec::new();
        let Some(ty) = v.get("type").and_then(|t| t.as_str()) else {
            return out;
        };
        match ty {
            "conversation_initiation_metadata" => {
                let meta = v
                    .get("conversation_initiation_metadata_event")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                for (key, slot) in [
                    ("user_input_audio_format", &self.input_rate),
                    ("agent_output_audio_format", &self.output_rate),
                ] {
                    let Some(tag) = meta.get(key).and_then(|f| f.as_str()) else {
                        continue;
                    };
                    match Self::parse_format_rate(tag) {
                        Some(rate) => slot.store(rate, Ordering::Relaxed),
                        None => out.push(RealtimeEvent::Error {
                            code: Some("unsupported_audio_format".to_string()),
                            message: format!(
                                "elevenlabs agent uses {key}={tag}; this pipeline only \
                                 supports pcm_* formats — change the agent's audio \
                                 format in its Voice/Advanced settings"
                            ),
                            recoverable: false,
                        }),
                    }
                }
                out.push(RealtimeEvent::SessionUpdated);
            }
            "audio" => {
                let Some(ev) = v.get("audio_event") else {
                    return out;
                };
                // Drop stale in-flight chunks of an interrupted
                // response (same event_id filter as the official SDKs).
                let event_id = ev
                    .get("event_id")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(i64::MAX);
                if event_id <= self.last_interrupt_id.load(Ordering::Relaxed) {
                    return out;
                }
                // Drop re-delivered frames: an id *below* the highest
                // seen means a server-side re-stream of audio already
                // played (strictly-less-than so equal-id chunk trains
                // are unaffected).
                if event_id != i64::MAX {
                    if event_id < self.last_audio_id.load(Ordering::Relaxed) {
                        tracing::debug!(event_id, "elevenlabs: dropping re-delivered audio frame");
                        return out;
                    }
                    self.last_audio_id.store(event_id, Ordering::Relaxed);
                }
                if let Some(b64) = ev.get("audio_base_64").and_then(|d| d.as_str()) {
                    out.extend(self.open_response());
                    let rate = self.output_rate.load(Ordering::Relaxed);
                    match decode_pcm16_b64(b64, rate) {
                        Ok(samples) => out.push(RealtimeEvent::ResponseAudioDelta {
                            samples,
                            sample_rate: rate,
                            alignment: ev.get("alignment").and_then(Self::parse_alignment),
                        }),
                        Err(e) => out.push(RealtimeEvent::Error {
                            code: Some("audio_decode".to_string()),
                            message: format!("elevenlabs audio_base_64: {e}"),
                            recoverable: true,
                        }),
                    }
                }
                if ev
                    .get("is_final")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    out.extend(self.close_response());
                }
            }
            "agent_response" => {
                if let Some(text) = v
                    .get("agent_response_event")
                    .and_then(|e| e.get("agent_response"))
                    .and_then(|t| t.as_str())
                {
                    // Re-delivery guard: the same text again while the
                    // response is open and no user input intervened is
                    // a server-side re-stream (live-observed doubling a
                    // turn's transcript+audio), not a new turn.
                    let mut last = self.last_agent_text.lock();
                    if self.response_open.load(Ordering::Relaxed)
                        && !text.is_empty()
                        && *last == text
                    {
                        tracing::debug!(
                            "elevenlabs: dropping re-delivered duplicate agent_response"
                        );
                        return out;
                    }
                    *last = text.to_string();
                    // Release before close/open below re-take the lock.
                    drop(last);
                    // The full turn text arrives exactly once per turn:
                    // a repeat while the response is open means the
                    // previous turn ended without any wire signal (the
                    // only closer for typed `user_message` turns, which
                    // produce no `user_transcript`).
                    if self.response_has_text.load(Ordering::Relaxed) {
                        out.extend(self.close_response());
                        // close_response cleared the memory; restore it
                        // for the turn this text is opening.
                        *self.last_agent_text.lock() = text.to_string();
                    }
                    out.extend(self.open_response());
                    self.response_has_text.store(true, Ordering::Relaxed);
                    // The agent transcript arrives as one block, not a
                    // token stream — surface it as a single delta.
                    out.push(RealtimeEvent::ResponseTextDelta {
                        text: text.to_string(),
                    });
                }
            }
            "agent_response_complete" => {
                out.extend(self.close_response());
            }
            "interruption" => {
                if let Some(id) = v
                    .get("interruption_event")
                    .and_then(|e| e.get("event_id"))
                    .and_then(serde_json::Value::as_i64)
                {
                    self.last_interrupt_id.store(id, Ordering::Relaxed);
                }
                self.response_open.store(false, Ordering::Relaxed);
                self.response_has_text.store(false, Ordering::Relaxed);
                self.last_agent_text.lock().clear();
                out.push(RealtimeEvent::ResponseCancelled { response_id: None });
            }
            "user_transcript" => {
                if let Some(text) = v
                    .get("user_transcription_event")
                    .and_then(|e| e.get("user_transcript"))
                    .and_then(|t| t.as_str())
                {
                    // The user spoke next, so any open agent turn is
                    // over — close it before surfacing the transcript
                    // (barge-ins arrive as `interruption` instead).
                    // New user input also invalidates the duplicate-
                    // suppression memory.
                    out.extend(self.close_response());
                    self.last_agent_text.lock().clear();
                    out.push(RealtimeEvent::InputTranscript {
                        text: text.to_string(),
                        is_final: true,
                    });
                }
            }
            "client_tool_call" => {
                if let Some(call) = v.get("client_tool_call") {
                    let call_id = call
                        .get("tool_call_id")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = call
                        .get("tool_name")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = call
                        .get("parameters")
                        .map_or_else(|| "{}".to_string(), std::string::ToString::to_string);
                    out.push(RealtimeEvent::function_call(call_id, name, arguments));
                }
            }
            "client_error" => {
                let ev = v
                    .get("error_event")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let code = ev
                    .get("error_name")
                    .and_then(|c| c.as_str())
                    .map(std::string::ToString::to_string)
                    .or_else(|| ev.get("code").map(std::string::ToString::to_string));
                let message = ev
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("(no message)")
                    .to_string();
                // Protocol/validation complaints (e.g. an override the
                // agent's security settings reject) don't tear the
                // socket down; genuinely fatal conditions surface as a
                // server close frame → `Closed`.
                out.push(RealtimeEvent::Error {
                    code,
                    message: format!("elevenlabs error: {message}"),
                    recoverable: true,
                });
            }
            "agent_response_correction" => {
                // ElevenLabs' own view of what was spoken before an
                // interruption. Our persisted truncation comes from the
                // client's PlaybackReport (ground truth of what was
                // *played*); this is surfaced for cross-checking only.
                if let Some(ev) = v.get("agent_response_correction_event") {
                    let original = ev
                        .get("original_agent_response")
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    let corrected = ev
                        .get("corrected_agent_response")
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    tracing::info!(
                        original_chars = original.chars().count(),
                        corrected_chars = corrected.chars().count(),
                        "elevenlabs: server-side truncation of interrupted response \
                         (cross-check against our PlaybackReport truncation)"
                    );
                }
            }
            "guardrail_triggered" => {
                let name = v
                    .get("guardrail_triggered_event")
                    .and_then(|e| e.get("guardrail_name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("(unnamed)");
                tracing::warn!(guardrail = %name, "elevenlabs guardrail triggered");
            }
            // `ping` is answered via `build_auto_reply`; the rest are
            // dashboard/telemetry events with no pipeline mapping:
            // vad_score, tentative-response previews,
            // agent_chat_response_part (streamed text parts — the full
            // `agent_response` is the single authoritative transcript
            // source; surfacing both duplicates the turn text), agent
            // tool telemetry, MCP status, ...
            "ping"
            | "vad_score"
            | "agent_chat_response_part"
            | "internal_vad_score"
            | "internal_tentative_agent_response"
            | "tentative_agent_response"
            | "internal_turn_probability"
            | "agent_response_metadata"
            | "agent_chat_response_part_metadata"
            | "agent_tool_request"
            | "agent_tool_response"
            | "agent_tool_response_full_payload"
            | "mcp_tool_call"
            | "mcp_connection_status" => {}
            other => {
                tracing::debug!(event_type = %other, "elevenlabs: unhandled event type");
            }
        }
        out
    }
}

/// A vendor's milliseconds as whole milliseconds: negative and NaN become
/// 0, and a value past `u64` saturates.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a float to int cast saturates and maps NaN to 0; it cannot panic"
)]
fn whole_ms(ms: f64) -> u64 {
    ms.max(0.0) as u64
}

#[cfg(test)]
#[path = "tests/elevenlabs.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/factory.rs"]
mod factory_tests;
