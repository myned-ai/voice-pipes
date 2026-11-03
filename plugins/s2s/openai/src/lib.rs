//! `OpenAI` Realtime as a voice-pipes speech-to-speech plugin (GA shape).
//!
//! A wire-protocol mapper only: the engine's realtime client owns the
//! connection. The binary registers [`factory`] and [`wizard_choices`];
//! `[realtime_provider] provider = "openai-realtime"` selects it.
//!
//! Endpoint: `wss://api.openai.com/v1/realtime?model=...`. Auth is a
//! `Authorization: Bearer ...` header. The legacy `OpenAI-Beta:
//! realtime=v1` header is intentionally NOT sent -- the Beta API
//! shape was retired (`beta_api_shape_disabled`).
//!
//! Session-update payload uses the GA schema:
//!   * `session.type = "realtime"`
//!   * `output_modalities` (replaces legacy `modalities`)
//!   * `audio.input.format = { type: "audio/pcm", rate: 24000 }`
//!     (replaces legacy `input_audio_format`)
//!   * `audio.output.format` + optional `voice`
//!   * `audio.input.turn_detection`
//!
//! Event names accept both GA (`response.output_audio.delta`,
//! `response.output_audio_transcript.delta`,
//! `response.output_audio.done`) and legacy Beta variants for
//! defensive compatibility with transitional servers / fixtures.

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

use anyhow::{Context, Result};

use voice_pipes_sdk::s2s::codec::{count_u32, decode_pcm16_b64, url_query_encode};
use voice_pipes_sdk::s2s::{
    ConnectInfo, Modalities, RateLimitInfo, RealtimeEvent, RealtimeFactory, RealtimeProtocol,
    SessionConfig, TurnDetection,
};
use voice_pipes_sdk::secrets::Secret;

mod factory;
mod wizard;

pub(crate) use factory::OpenAiRealtimeFactory;
pub use wizard::wizard_choices;

/// The factory the binary registers.
pub fn factory() -> Box<dyn RealtimeFactory> {
    Box::new(OpenAiRealtimeFactory)
}

/// Native sample rate for both input and output PCM on the `OpenAI`
/// Realtime API.
pub(crate) const NATIVE_SAMPLE_RATE: u32 = 24_000;

/// GA Realtime model name. The legacy
/// `gpt-4o-realtime-preview-2024-12-17` preview SKU was retired with
/// the Beta API; the GA endpoint returns `model_not_found` for it.
pub(crate) const DEFAULT_MODEL: &str = "gpt-realtime";

/// The model that transcribes a session's input unless the config names
/// another (`options.input_transcription_model`).
pub(crate) const DEFAULT_TRANSCRIPTION_MODEL: &str = "gpt-realtime-whisper";

/// `OpenAI` Realtime protocol descriptor.
#[derive(Debug, Clone)]
pub(crate) struct OpenAiProtocol {
    model: String,
    /// The key the engine resolved from the environment.
    api_key: Option<Secret>,
    /// The model that transcribes the audio sent to the session, written
    /// to `session.audio.input.transcription.model` when the session asks
    /// for its input transcribed
    /// ([`SessionConfig::transcribe_input`]), so `OpenAI` emits
    /// `conversation.item.input_audio_transcription.*` events. `None`
    /// turns transcription off whatever the session asks.
    input_transcription_model: Option<String>,
    /// ISO-639-1 language code (`"en"`, `"es"`, `"ja"`, …) passed to
    /// the transcription model. Strongly recommended for short
    /// utterances: without it `gpt-4o-mini-transcribe` regularly
    /// mis-detects the language and returns the audio transliterated
    /// into the wrong script (e.g. English "OK can you" → Korean
    /// hangul). `None` lets the model auto-detect, which is fine only
    /// for long, unambiguous audio.
    input_transcription_language: Option<String>,
    /// Optional bias prompt forwarded to the transcription model.
    /// Useful for steering vocabulary (proper nouns, jargon). Empty /
    /// `None` skips the field.
    input_transcription_prompt: Option<String>,
}

impl Default for OpenAiProtocol {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            api_key: None,
            input_transcription_model: Some(DEFAULT_TRANSCRIPTION_MODEL.to_string()),
            input_transcription_language: None,
            input_transcription_prompt: None,
        }
    }
}

impl OpenAiProtocol {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// The API key the engine resolved; without one, `connect_info` fails.
    #[must_use]
    pub fn with_api_key(mut self, key: Option<Secret>) -> Self {
        self.api_key = key;
        self
    }

    /// The model that transcribes the session's input, or `None` for no
    /// transcription. See [`OpenAiProtocol::input_transcription_model`].
    #[must_use]
    pub fn with_input_transcription_model(mut self, model: Option<String>) -> Self {
        self.input_transcription_model = model;
        self
    }

    /// Set an explicit ISO-639-1 language hint for the transcription
    /// model. Recommended for short / interactive utterances where the
    /// auto-detector frequently picks the wrong language.
    #[must_use]
    pub fn with_input_transcription_language(mut self, language: Option<String>) -> Self {
        self.input_transcription_language = language;
        self
    }

    /// Optional vocabulary-biasing prompt for the transcription model.
    #[must_use]
    pub fn with_input_transcription_prompt(mut self, prompt: Option<String>) -> Self {
        self.input_transcription_prompt = prompt;
        self
    }
}

fn modalities_to_array(m: Modalities) -> Vec<&'static str> {
    match m {
        Modalities::Text => vec!["text"],
        // Audio, AudioAndText, and any modality newer than this provider.
        // OpenAI GA `output_modalities` takes a single modality, not both.
        // Requesting ["audio"] still delivers streaming assistant transcripts
        // over the WebSocket channel automatically.
        _ => vec!["audio"],
    }
}

impl OpenAiProtocol {
    fn connect_target(&self) -> Result<ConnectInfo> {
        let api_key = self.api_key.as_ref().context(
            "missing API key -- set the variable [realtime_provider] api_key_env names \
             (OPENAI_API_KEY by default)",
        )?;
        let url = format!(
            "wss://api.openai.com/v1/realtime?model={}",
            url_query_encode(&self.model)
        );
        Ok(ConnectInfo::new(url)
            .with_header("Authorization", format!("Bearer {}", api_key.expose())))
    }
}

impl RealtimeProtocol for OpenAiProtocol {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn native_input_sample_rate(&self) -> u32 {
        NATIVE_SAMPLE_RATE
    }

    fn native_output_sample_rate(&self) -> u32 {
        NATIVE_SAMPLE_RATE
    }

    fn connect_info(&self) -> Result<ConnectInfo> {
        self.connect_target()
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "writes keys into JSON objects this function builds; a write into an object cannot fail"
    )]
    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value> {
        let mut audio_output = serde_json::json!({
            "format": { "type": "audio/pcm", "rate": NATIVE_SAMPLE_RATE },
        });
        if let Some(v) = &cfg.voice {
            audio_output["voice"] = serde_json::Value::String(v.clone());
        }
        let turn_detection = match &cfg.turn_detection {
            TurnDetection::ServerVad {
                silence_ms,
                threshold,
                prefix_padding_ms,
            } => {
                let mut td = serde_json::json!({ "type": "server_vad" });
                if let Some(ms) = silence_ms {
                    td["silence_duration_ms"] = serde_json::json!(ms);
                }
                if let Some(t) = threshold {
                    td["threshold"] = serde_json::json!(t);
                }
                if let Some(ms) = prefix_padding_ms {
                    td["prefix_padding_ms"] = serde_json::json!(ms);
                }
                td
            }
            TurnDetection::SemanticVad { eagerness } => {
                let mut td = serde_json::json!({ "type": "semantic_vad" });
                if let Some(e) = eagerness {
                    td["eagerness"] = serde_json::json!(e);
                }
                td
            }
            // The Realtime API turns detection off with null; it has no
            // "none" type.
            TurnDetection::None => serde_json::Value::Null,
            // A policy newer than this provider: the server's own VAD.
            _ => serde_json::json!({ "type": "server_vad" }),
        };
        let mods = modalities_to_array(cfg.modalities);
        let mut audio_input = serde_json::json!({
            "format": { "type": "audio/pcm", "rate": NATIVE_SAMPLE_RATE },
            "turn_detection": turn_detection,
        });
        if let Some(model) = self
            .input_transcription_model
            .as_ref()
            .filter(|_| cfg.transcribe_input)
        {
            let mut t = serde_json::json!({ "model": model });
            if let Some(lang) = &self.input_transcription_language {
                t["language"] = serde_json::Value::String(lang.clone());
            }
            if let Some(prompt) = &self.input_transcription_prompt
                && !prompt.is_empty()
            {
                t["prompt"] = serde_json::Value::String(prompt.clone());
            }
            audio_input["transcription"] = t;
        }
        let mut session = serde_json::json!({
            "type": "realtime",
            "output_modalities": mods,
            "instructions": cfg.instructions,
            "audio": {
                "input": audio_input,
                "output": audio_output,
            },
        });
        // Declared tools (agent tool backend + client-side tools). GA
        // realtime sessions take flat function entries on
        // `session.tools`, with the JSON Schema passed through verbatim.
        if !cfg.tools.is_empty() {
            let tools: Vec<serde_json::Value> = cfg
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters_json_schema,
                    })
                })
                .collect();
            session["tools"] = serde_json::Value::Array(tools);
        }
        let update = serde_json::json!({
            "type": "session.update",
            "session": session,
        });
        // A session that waits to be spoken to must not be kicked: the
        // model would open in an arbitrary language before the user has
        // said anything. One that speaks first gets its opening turn now.
        if !cfg.speaks_first {
            return vec![update];
        }
        let kick = serde_json::json!({
            "type": "response.create",
            "response": { "output_modalities": mods }
        });
        vec![update, kick]
    }

    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "input_audio_buffer.append",
            "audio": b64_pcm,
        })
    }

    fn build_response_create(&self, modalities: Modalities) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "response.create",
            "response": { "output_modalities": modalities_to_array(modalities) }
        }))
    }

    fn build_text_message(&self, text: &str) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            }
        }))
    }

    fn build_cancel(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "response.cancel"
        }))
    }

    fn build_item_truncate(&self, item_id: &str, audio_end_ms: u32) -> Option<serde_json::Value> {
        // OpenAI Realtime GA: `conversation.item.truncate` prunes the
        // server-side audio + transcript stored for `item_id` down to
        // the playback offset the client actually heard. Without this,
        // the next user turn sees the full (un-spoken) assistant reply
        // in conversation history and the model often repeats / refers
        // to words the user never heard.
        Some(serde_json::json!({
            "type": "conversation.item.truncate",
            "item_id": item_id,
            "content_index": 0,
            "audio_end_ms": audio_end_ms,
        }))
    }

    fn build_function_call_output(
        &self,
        call_id: &str,
        _name: &str,
        result: &str,
        _silent: bool,
    ) -> Option<serde_json::Value> {
        // `_name` is unused: OpenAI correlates outputs by `call_id`
        // alone. `_silent` is a no-op here: OpenAI only speaks about a
        // tool output when the caller kicks `response.create`, so
        // silence is implemented by the runner withholding the kick.
        Some(serde_json::json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": result,
            }
        }))
    }

    fn build_context_message(
        &self,
        text: &str,
        respond: bool,
        modalities: Modalities,
    ) -> Vec<serde_json::Value> {
        // System-role item: background context the model treats as
        // instruction, not as something the user said. Late `Pending`
        // tool results must land here — OpenAI accepts only one
        // `function_call_output` per call_id.
        let item = serde_json::json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "system",
                "content": [{ "type": "input_text", "text": text }]
            }
        });
        let mut out = vec![item];
        if respond && let Some(kick) = self.build_response_create(modalities) {
            out.push(kick);
        }
        out
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one match arm per wire event type; splitting the dispatch would obscure the protocol surface"
    )]
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent> {
        let Some(kind) = v.get("type").and_then(|x| x.as_str()) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(1);
        match kind {
            // GA: response.output_audio.delta -- legacy: response.audio.delta
            "response.output_audio.delta" | "response.audio.delta" => {
                if let Some(b64) = v.get("delta").and_then(|x| x.as_str()) {
                    match decode_pcm16_b64(b64, NATIVE_SAMPLE_RATE) {
                        Ok(samples) => out.push(RealtimeEvent::ResponseAudioDelta {
                            samples,
                            sample_rate: NATIVE_SAMPLE_RATE,
                            alignment: None,
                        }),
                        Err(e) => out.push(RealtimeEvent::Error {
                            code: Some("audio_decode".to_string()),
                            message: format!("openai audio.delta base64: {e}"),
                            recoverable: true,
                        }),
                    }
                }
            }
            // GA: response.output_audio_transcript.delta / response.output_text.delta
            // Legacy: response.audio_transcript.delta
            "response.output_audio_transcript.delta"
            | "response.output_text.delta"
            | "response.audio_transcript.delta" => {
                if let Some(d) = v.get("delta").and_then(|x| x.as_str()) {
                    out.push(RealtimeEvent::ResponseTextDelta {
                        text: d.to_string(),
                    });
                }
            }
            // Turn boundary -- only response.done is the complete response boundary.
            "response.done" => {
                let id = v
                    .get("response")
                    .and_then(|r| r.get("id"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                // Surface the per-response token usage before the
                // turn-boundary event so downstream consumers can
                // attach it to the still-open turn.
                if let Some(usage) = v.get("response").and_then(|r| r.get("usage")) {
                    let get_u32 = |val: &serde_json::Value, key: &str| count_u32(val.get(key));
                    let total_tokens = get_u32(usage, "total_tokens");
                    let input_tokens = get_u32(usage, "input_tokens");
                    let output_tokens = get_u32(usage, "output_tokens");
                    let input_details = usage.get("input_token_details");
                    let output_details = usage.get("output_token_details");
                    let input_text_tokens = input_details.map_or(0, |d| get_u32(d, "text_tokens"));
                    let input_audio_tokens =
                        input_details.map_or(0, |d| get_u32(d, "audio_tokens"));
                    let input_cached_tokens =
                        input_details.map_or(0, |d| get_u32(d, "cached_tokens"));
                    let output_text_tokens =
                        output_details.map_or(0, |d| get_u32(d, "text_tokens"));
                    let output_audio_tokens =
                        output_details.map_or(0, |d| get_u32(d, "audio_tokens"));
                    out.push(RealtimeEvent::ResponseUsage {
                        response_id: id.clone(),
                        total_tokens,
                        input_tokens,
                        output_tokens,
                        input_text_tokens,
                        input_audio_tokens,
                        input_cached_tokens,
                        output_text_tokens,
                        output_audio_tokens,
                    });
                }
                out.push(RealtimeEvent::ResponseCompleted { response_id: id });
            }
            // Sub-segment audio completions are deliberately ignored (they
            // would create duplicate / empty phantom turns during live S2S
            // sessions), exactly like unknown event kinds in the wildcard.
            #[expect(
                clippy::match_same_arms,
                reason = "explicitly documenting that these known events are intentionally dropped"
            )]
            "response.output_audio.done" | "response.audio.done" => {}
            "rate_limits.updated" => {
                if let Some(arr) = v.get("rate_limits").and_then(|x| x.as_array()) {
                    let items: Vec<RateLimitInfo> = arr
                        .iter()
                        .filter_map(|entry| {
                            let name = entry.get("name").and_then(|s| s.as_str())?.to_string();
                            let limit = count_u32(entry.get("limit"));
                            let remaining = count_u32(entry.get("remaining"));
                            let reset_seconds = entry
                                .get("reset_seconds")
                                .and_then(serde_json::Value::as_f64)
                                .map_or(0.0, seconds_f32);
                            Some(RateLimitInfo::new(name, limit, remaining, reset_seconds))
                        })
                        .collect();
                    if !items.is_empty() {
                        out.push(RealtimeEvent::RateLimits { items });
                    }
                }
            }
            "response.created" => {
                let id = v
                    .get("response")
                    .and_then(|r| r.get("id"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                out.push(RealtimeEvent::ResponseStarted { response_id: id });
            }
            "response.cancelled" => {
                let id = v
                    .get("response")
                    .and_then(|r| r.get("id"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                out.push(RealtimeEvent::ResponseCancelled { response_id: id });
            }
            "response.output_item.added" => {
                // Surface the assistant message's `item_id` so the
                // runner can later send `conversation.item.truncate`
                // when the user barges in. Only a message carries audio
                // that can be truncated; a function-call item is skipped.
                let item = v.get("item");
                let is_message =
                    item.and_then(|i| i.get("type")).and_then(|t| t.as_str()) == Some("message");
                if is_message
                    && let Some(item_id) = item.and_then(|i| i.get("id")).and_then(|s| s.as_str())
                {
                    out.push(RealtimeEvent::ResponseOutputItemAdded {
                        item_id: item_id.to_string(),
                    });
                }
            }
            "input_audio_buffer.speech_started" => {
                out.push(RealtimeEvent::InputSpeechStarted);
            }
            "input_audio_buffer.speech_stopped" => {
                out.push(RealtimeEvent::InputSpeechStopped);
            }
            "input_audio_buffer.committed" => {
                out.push(RealtimeEvent::InputAudioCommitted);
            }
            "conversation.item.input_audio_transcription.completed" => {
                if let Some(t) = v.get("transcript").and_then(|s| s.as_str()) {
                    out.push(RealtimeEvent::InputTranscript {
                        text: t.to_string(),
                        is_final: true,
                    });
                }
            }
            "conversation.item.input_audio_transcription.delta" => {
                if let Some(t) = v.get("delta").and_then(|s| s.as_str()) {
                    out.push(RealtimeEvent::InputTranscript {
                        text: t.to_string(),
                        is_final: false,
                    });
                }
            }
            "session.created" | "session.updated" => {
                out.push(RealtimeEvent::SessionUpdated);
            }
            "response.function_call_arguments.done" => {
                let call_id = v
                    .get("call_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = v
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = v
                    .get("arguments")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                out.push(RealtimeEvent::function_call(call_id, name, arguments));
            }
            "error" => {
                let code = v
                    .get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                let message = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("(no message)")
                    .to_string();
                // OpenAI's "fatal" errors are model_not_found,
                // beta_api_shape_disabled, auth -- everything else is
                // best-effort recoverable.
                let recoverable = !matches!(
                    code.as_deref(),
                    Some(
                        "model_not_found"
                            | "beta_api_shape_disabled"
                            | "invalid_api_key"
                            | "missing_api_key"
                    )
                );
                out.push(RealtimeEvent::Error {
                    code,
                    message: format!("openai error: {message}"),
                    recoverable,
                });
            }
            _ => {
                // Unknown / ignored lifecycle events: trace-only.
            }
        }
        out
    }
}

/// A vendor's seconds as the `f32` the SDK carries: a float's precision,
/// not a range, is lost, and a value past `f32`'s range becomes infinity.
#[expect(
    clippy::cast_possible_truncation,
    reason = "an f64 to f32 cast rounds and saturates to infinity; it cannot panic"
)]
fn seconds_f32(seconds: f64) -> f32 {
    seconds as f32
}

#[cfg(test)]
#[path = "tests/openai.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/factory.rs"]
mod factory_tests;
