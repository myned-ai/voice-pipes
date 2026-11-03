//! Google Gemini Live (Bidi) as a voice-pipes speech-to-speech plugin.
//!
//! A wire-protocol mapper only: the engine's realtime client owns the
//! connection. The binary registers [`factory`] and [`wizard_choices`];
//! `[realtime_provider] provider = "gemini-live"` selects it.
//!
//! Endpoint:
//! `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=...`.
//! The API key lives in the URL query (no Authorization header).
//!
//! Payload mapping:
//!
//! | `RealtimeClient` call       | Gemini envelope                                              |
//! |----------------------------|--------------------------------------------------------------|
//! | `connect` + `session_update` | `setup` (model, `responseModalities`, `systemInstruction`, `sessionResumption`) |
//! | `send_audio(pcm)`          | `realtimeInput.audio{audio/pcm;rate=16000, b64}`            |
//! | `send_text(text)`          | `realtimeInput.text` (native-audio-safe; no clientContent)   |
//! | `cancel()`                 | nothing: Gemini Live has no cancel                           |
//!
//! Inbound:
//! | Gemini event                          | RealtimeEvent                       |
//! |---------------------------------------|-------------------------------------|
//! | `serverContent.outputTranscription`   | `ResponseTextDelta`, a word each    |
//! | `serverContent.inputTranscription`    | `InputTranscript` (a partial)       |
//! | `serverContent.modelTurn.parts[].inlineData` (audio/*) | `ResponseAudioDelta` |
//! | `serverContent.turnComplete = true`   | `ResponseCompleted`                 |
//! | `serverContent.interrupted = true`    | `ResponseCancelled`                 |
//! | `toolCall`                            | `FunctionCall`, one per call        |
//! | `toolCallCancellation`                | `FunctionCallsCancelled`            |
//! | `sessionResumptionUpdate` (resumable) | `SessionResumable`                  |
//! | `goAway`                              | `GoingAway`                         |
//! | `setupComplete`                       | `SessionUpdated`                    |
//! | top-level `error`                     | `Error`                             |

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
    CloseClass, ConnectInfo, Modalities, RealtimeEvent, RealtimeFactory, RealtimeProtocol,
    SessionConfig, default_classify_close,
};
use voice_pipes_sdk::secrets::Secret;

mod factory;
mod wizard;

pub(crate) use factory::GeminiLiveFactory;
pub use wizard::wizard_choices;

/// The factory the binary registers.
pub fn factory() -> Box<dyn RealtimeFactory> {
    Box::new(GeminiLiveFactory)
}

/// Convert a JSON-Schema parameters object into Gemini's `Schema` shape.
///
/// The Live API's `Schema` proto accepts only a subset of JSON Schema and
/// rejects envelopes with unknown fields, so this keeps the recognised
/// keys (`type` — uppercased to the proto enum name — `description`,
/// `enum`, `format`, `nullable`, `properties`, `required`, `items`) and
/// drops everything else (`additionalProperties`, `$schema`, ...).
fn json_schema_to_gemini(schema: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = schema.as_object() else {
        // Not an object schema — advertise an empty object so the
        // declaration stays valid.
        return serde_json::json!({ "type": "OBJECT" });
    };
    let mut out = serde_json::Map::new();
    if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
        out.insert(
            "type".into(),
            serde_json::Value::String(t.to_ascii_uppercase()),
        );
    }
    for key in ["description", "enum", "format", "nullable"] {
        if let Some(v) = obj.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
        let converted: serde_json::Map<String, serde_json::Value> = props
            .iter()
            .map(|(k, v)| (k.clone(), json_schema_to_gemini(v)))
            .collect();
        out.insert("properties".into(), serde_json::Value::Object(converted));
    }
    if let Some(required) = obj.get("required") {
        out.insert("required".into(), required.clone());
    }
    if let Some(items) = obj.get("items") {
        out.insert("items".into(), json_schema_to_gemini(items));
    }
    serde_json::Value::Object(out)
}

/// A protobuf `Duration` in its JSON form: seconds with an `s` suffix
/// (`"50s"`, `"0.5s"`).
fn parse_duration(text: &str) -> Option<std::time::Duration> {
    let seconds: f64 = text.strip_suffix('s')?.parse().ok()?;
    std::time::Duration::try_from_secs_f64(seconds).ok()
}

/// Sample rate Gemini Live expects on uplinked audio.
pub(crate) const NATIVE_INPUT_SR: u32 = 16_000;
/// Sample rate Gemini Live emits on downlinked audio.
pub(crate) const NATIVE_OUTPUT_SR: u32 = 24_000;
/// Default model used when the scenario does not pin one.
pub(crate) const DEFAULT_MODEL: &str = "models/gemini-2.5-flash-native-audio-latest";

/// Gemini Live protocol descriptor.
#[derive(Debug, Clone)]
pub(crate) struct GeminiProtocol {
    model: String,
    /// The key the engine resolved from the environment.
    api_key: Option<Secret>,
    /// The turn sent on `configure` to make the model open the
    /// conversation, when the session speaks first
    /// ([`SessionConfig::speaks_first`]). `None` sends none even then.
    opener: Option<String>,
    /// Async function-calling override (`NON_BLOCKING` declarations +
    /// `scheduling` on `FunctionResponse`s). `None` = decide from the
    /// model name (`native-audio`); `Some(_)` = operator override via
    /// `[realtime_provider.options] async_function_calling`.
    async_function_calling: Option<bool>,
}

impl Default for GeminiProtocol {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_string(),
            api_key: None,
            opener: Some("Begin the conversation now.".to_string()),
            async_function_calling: None,
        }
    }
}

impl GeminiProtocol {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let m = model.into();
        self.model = if m.starts_with("models/") {
            m
        } else {
            format!("models/{m}")
        };
        self
    }

    /// The API key the engine resolved; without one, `connect_info` fails.
    #[must_use]
    pub fn with_api_key(mut self, key: Option<Secret>) -> Self {
        self.api_key = key;
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_opener(mut self, opener: Option<String>) -> Self {
        self.opener = opener;
        self
    }

    #[cfg(test)]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Explicitly enable/disable the async function-calling vocabulary,
    /// overriding the model-name heuristic. Wired from
    /// `[realtime_provider.options] async_function_calling = true|false`
    /// so a renamed/new Google model never silently loses (or wrongly
    /// gains) the capability.
    #[must_use]
    pub fn with_async_function_calling(mut self, enabled: Option<bool>) -> Self {
        self.async_function_calling = enabled;
        self
    }

    /// Whether the configured model supports Gemini's async function
    /// calling (`behavior: NON_BLOCKING` declarations + `scheduling` on
    /// the `FunctionResponse`). Per Google's Live API docs this is a
    /// capability of the native-audio models (e.g.
    /// `gemini-2.5-flash-native-audio-*`) and `gemini-3.8-live`;
    /// half-cascade Live models ignore or reject the vocabulary. The config option
    /// `async_function_calling` overrides the name heuristic in either
    /// direction.
    fn supports_async_function_calling(&self) -> bool {
        self.async_function_calling.unwrap_or_else(|| {
            // `gemini-3.8-live` makes NON_BLOCKING the default and honours
            // `scheduling`; its extended-thinking sibling rejects
            // `scheduling` and 3.1 Flash Live is synchronous only.
            self.model.contains("native-audio")
                || self.model.trim_start_matches("models/") == "gemini-3.8-live"
        })
    }

    /// Whether the model takes `thinkingConfig.thinkingBudget`. Only the
    /// 2.x models do; the 3.x Live models use `thinkingLevel` or no
    /// thinking config at all.
    fn uses_thinking_budget(&self) -> bool {
        self.model
            .trim_start_matches("models/")
            .starts_with("gemini-2")
    }
}

fn modalities_to_array(m: Modalities) -> Vec<&'static str> {
    match m {
        Modalities::Text => vec!["TEXT"],
        // Audio, AudioAndText, and any modality newer than this provider.
        // Gemini Live rejects the combined modality with a 1007; assistant
        // text still arrives automatically alongside audio.
        _ => vec!["AUDIO"],
    }
}

impl GeminiProtocol {
    fn connect_target(&self) -> Result<ConnectInfo> {
        let api_key = self.api_key.as_ref().context(
            "missing API key -- set the variable [realtime_provider] api_key_env names \
             (GEMINI_API_KEY by default)",
        )?;
        let url = format!(
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
            url_query_encode(api_key.expose())
        );
        Ok(ConnectInfo::new(url))
    }
}

impl RealtimeProtocol for GeminiProtocol {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn native_input_sample_rate(&self) -> u32 {
        NATIVE_INPUT_SR
    }

    fn native_output_sample_rate(&self) -> u32 {
        NATIVE_OUTPUT_SR
    }

    /// A resumption token the server no longer holds closes the new
    /// connection at setup, in words of its own on each model: 1007
    /// "Invalid session handle" (2.5 native audio) and 1008 "Requested
    /// entity was not found." (3.x). The default reads 1008 as fatal and
    /// 1007 as worth the same retry.
    fn classify_close(&self, code: Option<u16>, reason: &str) -> CloseClass {
        // Three wordings for one fact, a resumption handle the server no
        // longer holds: the handle is invalid, the entity behind it is not
        // found, or (after the server itself dropped the connection with an
        // internal error) the session is "not found or expired".
        let lower = reason.to_lowercase();
        if (code == Some(1007) && lower.contains("invalid session handle"))
            || (code == Some(1008)
                && (lower.contains("requested entity was not found")
                    || lower.contains("session not found")))
        {
            return CloseClass::Unresumable;
        }
        default_classify_close(code, reason)
    }

    fn connect_info(&self) -> Result<ConnectInfo> {
        self.connect_target()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one linear mapping from SessionConfig + options onto the setup envelope"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "writes keys into JSON objects this function builds; a write into an object cannot fail"
    )]
    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value> {
        // Optional Gemini-specific knobs surfaced via
        // `SessionConfig.provider_options` (populated from
        // `[realtime_provider.options]` + per-session client
        // overrides):
        //   - `thinking_budget`: i64 (0 = disabled,
        //     -1 = dynamic, 1..32768 = fixed token cap). Gemini 2.x only:
        //     the 3.x Live models replaced it with `thinkingLevel`, and
        //     `gemini-3.8-live` takes no thinking config at all.
        //   - `thinking_level`: string ("minimal" | "low" | "medium" |
        //     "high"), sent as `thinkingLevel` for the 3.x models that
        //     accept it (`gemini-3.1-flash-live-preview`,
        //     `gemini-3.8-live-extended-thinking`).
        //   - `context_window_compression`: bool (lets Live API
        //     trim long sessions instead of dropping turns)
        //   - `google_search_grounding`: bool (adds the built-in
        //     `googleSearchRetrieval` tool spec)
        //   - `input_transcription_language`: BCP-47 code(s) as a
        //     string ("en-US", comma-separated for several) or array;
        //     "auto" sends none. Language hint for the input-ASR side
        //     channel. Defaults to "en-US" like the OpenAI provider's "en":
        //     left to auto-detect, Gemini decides the language per
        //     utterance and a short one comes back in Russian or Chinese
        //     (per Google's Live API best-practices doc, and seen in the
        //     Playground).
        let opts = &cfg.provider_options;
        let mut generation_config = serde_json::json!({
            "responseModalities": modalities_to_array(cfg.modalities),
        });
        if let Some(level) = opts.get("thinking_level").and_then(|v| v.as_str()) {
            generation_config["thinkingConfig"] = serde_json::json!({ "thinkingLevel": level });
        } else if let Some(budget) = opts
            .get("thinking_budget")
            .and_then(serde_json::Value::as_i64)
        {
            if self.uses_thinking_budget() {
                generation_config["thinkingConfig"] =
                    serde_json::json!({ "thinkingBudget": budget });
            } else {
                tracing::warn!(
                    model = %self.model,
                    budget,
                    "thinking_budget applies to Gemini 2.x only; not sent (use thinking_level)"
                );
            }
        }
        // Named prebuilt voice (Puck, Charon, Kore, Fenrir, Aoede, Leda, Orus,
        // Zephyr, ...). Absent -> Gemini's per-model default voice.
        if let Some(v) = &cfg.voice {
            generation_config["speechConfig"] = serde_json::json!({
                "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": v } }
            });
        }
        let lang_codes: Vec<String> = match opts.get("input_transcription_language") {
            Some(serde_json::Value::String(s)) => s
                .split(',')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_string)
                .collect(),
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect(),
            _ => vec!["en-US".to_string()],
        };
        let lang_codes: Vec<String> = lang_codes
            .into_iter()
            .filter(|c| !c.eq_ignore_ascii_case("auto"))
            .collect();
        // Only a session that asks for its input transcribed gets the
        // transcription block; without it Gemini sends no input
        // transcripts.
        let input_transcription = if !cfg.transcribe_input {
            None
        } else if lang_codes.is_empty() {
            Some(serde_json::json!({}))
        } else {
            Some(serde_json::json!({ "languageCodes": lang_codes }))
        };
        // Voice-activity tuning (realtimeInputConfig.automaticActivityDetection).
        // Emitted ONLY for explicitly-configured keys so sessions without
        // them keep Google's server defaults — the runner always fills its
        // neutral TurnDetection struct with OpenAI-oriented defaults, so
        // reading the raw option keys (not cfg.turn_detection) is what
        // distinguishes "operator chose this" from "runner default".
        // Recognised keys: `start_sensitivity` / `end_sensitivity`
        // ("high" | "low"), `vad_prefix_padding_ms`, `vad_silence_ms`.
        // Raising start sensitivity / prefix padding is the lever for
        // clipped sentence onsets in the input transcription.
        let mut aad = serde_json::Map::new();
        let sensitivity = |key: &str, prefix: &str| -> Option<serde_json::Value> {
            opts.get(key).and_then(|v| v.as_str()).and_then(|s| {
                match s.to_ascii_lowercase().as_str() {
                    "high" => Some(serde_json::Value::String(format!("{prefix}_HIGH"))),
                    "low" => Some(serde_json::Value::String(format!("{prefix}_LOW"))),
                    other => {
                        tracing::warn!(
                            key,
                            value = other,
                            "unrecognised Gemini VAD sensitivity (expected \"high\" or \"low\") -- ignored"
                        );
                        None
                    }
                }
            })
        };
        if let Some(v) = sensitivity("start_sensitivity", "START_SENSITIVITY") {
            aad.insert("startOfSpeechSensitivity".into(), v);
        }
        if let Some(v) = sensitivity("end_sensitivity", "END_SENSITIVITY") {
            aad.insert("endOfSpeechSensitivity".into(), v);
        }
        // Both knobs may come from the client (they are in the default
        // override allowlist); clamp so an absurd value cannot make the
        // provider reject the setup and end the session.
        const MAX_VAD_MS: u64 = 5_000;
        if let Some(ms) = opts
            .get("vad_prefix_padding_ms")
            .and_then(serde_json::Value::as_u64)
        {
            aad.insert(
                "prefixPaddingMs".into(),
                serde_json::json!(ms.min(MAX_VAD_MS)),
            );
        }
        if let Some(ms) = opts
            .get("vad_silence_ms")
            .and_then(serde_json::Value::as_u64)
        {
            aad.insert(
                "silenceDurationMs".into(),
                serde_json::json!(ms.min(MAX_VAD_MS)),
            );
        }

        let mut setup_body = serde_json::json!({
            "model": self.model,
            "generationConfig": generation_config,
            "systemInstruction": {
                "parts": [{ "text": cfg.instructions }]
            },
            "outputAudioTranscription": {}
        });
        if let Some(t) = input_transcription {
            setup_body["inputAudioTranscription"] = t;
        }
        if !aad.is_empty() {
            setup_body["realtimeInputConfig"] = serde_json::json!({
                "automaticActivityDetection": serde_json::Value::Object(aad),
            });
        }
        if opts
            .get("context_window_compression")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            setup_body["contextWindowCompression"] = serde_json::json!({ "slidingWindow": {} });
        }
        // Declared tools (agent tool backend + client-side tools) ride the
        // setup as functionDeclarations; the optional built-in
        // googleSearch tool is appended alongside them.
        let mut tools: Vec<serde_json::Value> = Vec::new();
        if !cfg.tools.is_empty() {
            let declarations: Vec<serde_json::Value> = cfg
                .tools
                .iter()
                .map(|t| {
                    let mut decl = serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": json_schema_to_gemini(&t.parameters_json_schema),
                    });
                    // Native-audio models support async function calling:
                    // NON_BLOCKING declarations keep the conversation
                    // flowing during a call, and — crucially — let the
                    // function response carry a `scheduling` directive
                    // (`SILENT` for client-tool acks; Gemini otherwise
                    // auto-voices a reaction to every toolResponse).
                    if self.supports_async_function_calling() {
                        decl["behavior"] = serde_json::json!("NON_BLOCKING");
                    }
                    decl
                })
                .collect();
            tools.push(serde_json::json!({ "functionDeclarations": declarations }));
        }
        if opts
            .get("google_search_grounding")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            tools.push(serde_json::json!({ "googleSearch": {} }));
        }
        if !tools.is_empty() {
            setup_body["tools"] = serde_json::Value::Array(tools);
        }
        // Resumption on: the server issues tokens that let a new
        // connection carry this session's context on, which a connection
        // needs when it reaches its lifetime (`goAway`) or drops.
        setup_body["sessionResumption"] = match &cfg.resume_token {
            Some(token) => serde_json::json!({ "handle": token.expose() }),
            None => serde_json::json!({}),
        };
        let setup = serde_json::json!({ "setup": setup_body });
        let mut envs = vec![setup];
        // A resumed session has already begun: the model has spoken
        // first, and must not open the conversation again.
        if let Some(opener) = self
            .opener
            .as_ref()
            .filter(|_| cfg.speaks_first && cfg.resume_token.is_none())
        {
            envs.push(serde_json::json!({
                "clientContent": {
                    "turns": [{
                        "role": "user",
                        "parts": [{ "text": opener }]
                    }],
                    "turnComplete": true
                }
            }));
        }
        envs
    }

    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value {
        // `realtimeInput.audio`, not the older `mediaChunks` array: the 3.x
        // Live models accept a `mediaChunks` frame without error and then
        // never hear it (no transcript, no reply), while the 2.5 models take
        // either.
        serde_json::json!({
            "realtimeInput": {
                "audio": {
                    "mimeType": format!("audio/pcm;rate={NATIVE_INPUT_SR}"),
                    "data": b64_pcm,
                }
            }
        })
    }

    fn build_response_create(&self, _modalities: Modalities) -> Option<serde_json::Value> {
        // Gemini auto-responds when the client sends a turnComplete
        // text/audio chunk -- no explicit `response.create` analog.
        None
    }

    fn build_text_message(&self, text: &str) -> Option<serde_json::Value> {
        // Typed user text rides the realtime activity stream
        // (`realtimeInput.text`) rather than `clientContent.turns`: the
        // native-audio models close the socket with 1007 ("CONTENT_TYPE_AUDIO
        // is not supported for this model configuration") when a
        // `clientContent` turn arrives after realtime audio has been
        // streamed, which silently drops the message and forces a
        // context-losing reconnect. `realtimeInput.text` is treated as user
        // activity, so the model responds without an explicit turnComplete.
        Some(serde_json::json!({
            "realtimeInput": { "text": text }
        }))
    }

    fn build_cancel(&self) -> Option<serde_json::Value> {
        // Gemini Live has no first-class cancel; sending a
        // turnComplete with empty parts was the closest equivalent but it
        // causes issues when quickly followed by another turnComplete (like text).
        // Returning None avoids confusing the server.
        None
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "writes keys into JSON objects this function builds; a write into an object cannot fail"
    )]
    fn build_function_call_output(
        &self,
        call_id: &str,
        name: &str,
        result: &str,
        silent: bool,
    ) -> Option<serde_json::Value> {
        // Gemini auto-continues after every toolResponse — there is no
        // kick to withhold. On native-audio models the async
        // function-calling `scheduling` key (inside the response map)
        // controls that continuation: SILENT = record only, WHEN_IDLE =
        // respond once idle (the pre-existing spoken behavior). Older
        // Live models ignore/reject the vocabulary, so it is omitted
        // there and silence is best-effort via the ack's instruction.
        // Both `id` and `name` are required on a FunctionResponse per
        // the Live API tools doc. `scheduling` is a field of the
        // FunctionResponse itself: nested inside `response` it is just
        // part of the tool output, and both 2.5 and 3.8 then speak after
        // a "SILENT" ack.
        let mut function_response = serde_json::json!({
            "id": call_id,
            "name": name,
            "response": { "output": result },
        });
        if self.supports_async_function_calling() {
            function_response["scheduling"] =
                serde_json::json!(if silent { "SILENT" } else { "WHEN_IDLE" });
        }
        Some(serde_json::json!({
            "toolResponse": {
                "functionResponses": [function_response]
            }
        }))
    }

    fn build_context_message(
        &self,
        text: &str,
        respond: bool,
        _modalities: Modalities,
    ) -> Vec<serde_json::Value> {
        // Gemini Live has no system-role mid-session item; the closest
        // shape is a user-role clientContent tagged as context.
        // `turnComplete` doubles as the kick: `true` commits the turn
        // and the model responds (Gemini auto-continues — that's why
        // `build_response_create` is `None`); `false` appends context
        // without triggering a response.
        vec![serde_json::json!({
            "clientContent": {
                "turns": [{
                    "role": "user",
                    "parts": [{ "text": format!("[background context — not spoken by the user] {text}") }]
                }],
                "turnComplete": respond
            }
        })]
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one handling block per wire event type; splitting the dispatch would obscure the protocol surface"
    )]
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent> {
        let mut out = Vec::new();

        if let Some(err) = v.get("error") {
            let code = err
                .get("status")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("(no message)")
                .to_string();
            let recoverable = !matches!(
                code.as_deref(),
                Some("UNAUTHENTICATED" | "PERMISSION_DENIED" | "NOT_FOUND")
            );
            out.push(RealtimeEvent::Error {
                code,
                message: format!("gemini error: {message}"),
                recoverable,
            });
            return out;
        }

        if v.get("setupComplete").is_some() {
            out.push(RealtimeEvent::SessionUpdated);
            return out;
        }

        if let Some(tc) = v.get("toolCall")
            && let Some(calls) = tc.get("functionCalls").and_then(|c| c.as_array())
        {
            for c in calls {
                let call_id = c
                    .get("id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = c
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = c
                    .get("args")
                    .map_or_else(|| "{}".to_string(), std::string::ToString::to_string);
                out.push(RealtimeEvent::function_call(call_id, name, arguments));
            }
            return out;
        }

        if let Some(update) = v.get("sessionResumptionUpdate") {
            // Updates without a handle, or not resumable at this point
            // (mid-generation, a tool call pending), change nothing: the
            // last token issued still resumes.
            if let Some(handle) = update.get("newHandle").and_then(|h| h.as_str())
                && !handle.is_empty()
                && update
                    .get("resumable")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            {
                out.push(RealtimeEvent::SessionResumable {
                    token: Secret::new(handle),
                });
            }
            return out;
        }

        if let Some(go_away) = v.get("goAway") {
            out.push(RealtimeEvent::GoingAway {
                time_left: go_away
                    .get("timeLeft")
                    .and_then(|t| t.as_str())
                    .and_then(parse_duration),
            });
            return out;
        }

        // The model withdrew calls it has not had answered: the user spoke
        // over them. It comes right before the `interrupted` turn.
        if let Some(ids) = v
            .get("toolCallCancellation")
            .and_then(|c| c.get("ids"))
            .and_then(|ids| ids.as_array())
        {
            let call_ids: Vec<String> = ids
                .iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect();
            if !call_ids.is_empty() {
                out.push(RealtimeEvent::FunctionCallsCancelled { call_ids });
            }
            return out;
        }

        let Some(sc) = v.get("serverContent") else {
            return out;
        };

        if let Some(model_turn) = sc.get("modelTurn")
            && let Some(parts) = model_turn.get("parts").and_then(|p| p.as_array())
        {
            for part in parts {
                // We ignore `part.text` because for Gemini 2.0 Flash/Pro it contains the
                // thought process rather than the spoken transcript. The spoken transcript
                // arrives in `outputTranscription`.

                if let Some(inline) = part.get("inlineData") {
                    let mime = inline
                        .get("mimeType")
                        .and_then(|m| m.as_str())
                        .unwrap_or("");
                    if !mime.starts_with("audio/") {
                        continue;
                    }
                    let Some(b64) = inline.get("data").and_then(|d| d.as_str()) else {
                        continue;
                    };
                    match decode_pcm16_b64(b64, NATIVE_OUTPUT_SR) {
                        Ok(samples) => out.push(RealtimeEvent::ResponseAudioDelta {
                            samples,
                            sample_rate: NATIVE_OUTPUT_SR,
                            alignment: None,
                        }),
                        Err(e) => out.push(RealtimeEvent::Error {
                            code: Some("audio_decode".to_string()),
                            message: format!("gemini inlineData base64: {e}"),
                            recoverable: true,
                        }),
                    }
                }
            }
        }

        // Each transcription is the next piece of the text, not the text
        // so far: a piece is appended as sent, even when it starts like
        // the one before it.
        if let Some(output_transcription) = sc.get("outputTranscription")
            && let Some(text) = output_transcription.get("text").and_then(|t| t.as_str())
            && !text.is_empty()
        {
            out.extend(
                text.split_inclusive(' ')
                    .map(|word| RealtimeEvent::ResponseTextDelta {
                        text: word.to_string(),
                    }),
            );
        }

        if let Some(input_transcription) = sc.get("inputTranscription")
            && let Some(text) = input_transcription.get("text").and_then(|t| t.as_str())
            && !text.is_empty()
        {
            out.push(RealtimeEvent::InputTranscript {
                text: text.to_string(),
                is_final: false,
            });
        }

        if let Some(usage) = v.get("usageMetadata").or_else(|| sc.get("usageMetadata")) {
            let input_tokens = count_u32(usage.get("promptTokenCount"));
            // The Live API names it `responseTokenCount`; the unary
            // API's `candidatesTokenCount` is kept as a fallback.
            let output_tokens = count_u32(
                usage
                    .get("responseTokenCount")
                    .or_else(|| usage.get("candidatesTokenCount")),
            );
            let total_tokens = count_u32(usage.get("totalTokenCount"));
            let input_cached_tokens = count_u32(usage.get("cachedContentTokenCount"));

            // Only emit if there's actually some token count (to avoid empty usage spam)
            if total_tokens > 0 || input_tokens > 0 || output_tokens > 0 {
                out.push(RealtimeEvent::ResponseUsage {
                    response_id: None,
                    total_tokens,
                    input_tokens,
                    output_tokens,
                    input_text_tokens: 0,
                    input_audio_tokens: 0,
                    input_cached_tokens,
                    output_text_tokens: 0,
                    output_audio_tokens: 0,
                });
            }
        }

        if sc
            .get("turnComplete")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            out.push(RealtimeEvent::ResponseCompleted { response_id: None });
        }

        if sc
            .get("interrupted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            out.push(RealtimeEvent::ResponseCancelled { response_id: None });
        }

        out
    }
}

#[cfg(test)]
#[path = "tests/gemini.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/factory.rs"]
mod factory_tests;
