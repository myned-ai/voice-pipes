//! Parser tests for [`super::GeminiProtocol::parse_event`] and the
//! Gemini envelope builders. Mirror the inline tests that lived in
//! `synthetic_caller/gemini_live.rs`.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

fn proto() -> GeminiProtocol {
    GeminiProtocol::new()
}

#[test]
fn parses_text_delta_from_model_turn() {
    let v = serde_json::json!({
        "serverContent": {
            "outputTranscription": {
                "text": "hello"
            }
        }
    });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseTextDelta { text }] if text == "hello"
    ));
}

#[test]
fn parses_audio_delta_and_downsamples() {
    let samples24: Vec<i16> = (0..240).map(|i| (i as i16) * 10).collect();
    let mut bytes = Vec::with_capacity(samples24.len() * 2);
    for s in &samples24 {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let v = serde_json::json!({
        "serverContent": {
            "modelTurn": {
                "parts": [{
                    "inlineData": {
                        "mimeType": "audio/pcm;rate=24000",
                        "data": BASE64.encode(&bytes)
                    }
                }]
            }
        }
    });
    let evs = proto().parse_event(&v);
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        RealtimeEvent::ResponseAudioDelta {
            samples,
            sample_rate,
            ..
        } => {
            // The vendor's rate and samples: the engine resamples the
            // session's audio as one stream.
            assert_eq!(*sample_rate, NATIVE_OUTPUT_SR);
            assert_eq!(*samples, samples24);
        }
        other => panic!("expected ResponseAudioDelta, got {other:?}"),
    }
}

#[test]
fn ignores_non_audio_inline_data() {
    let v = serde_json::json!({
        "serverContent": {
            "modelTurn": {
                "parts": [{
                    "inlineData": { "mimeType": "image/png", "data": "deadbeef" }
                }]
            }
        }
    });
    assert!(proto().parse_event(&v).is_empty());
}

#[test]
fn turn_complete_emits_response_completed() {
    let v = serde_json::json!({
        "serverContent": { "turnComplete": true }
    });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseCompleted { .. }]
    ));
}

#[test]
fn setup_complete_emits_session_updated() {
    let v = serde_json::json!({ "setupComplete": {} });
    let evs = proto().parse_event(&v);
    assert!(matches!(evs.as_slice(), [RealtimeEvent::SessionUpdated]));
}

#[test]
fn error_envelope_emits_error() {
    let v = serde_json::json!({ "error": { "message": "bad" } });
    let evs = proto().parse_event(&v);
    match evs.as_slice() {
        [RealtimeEvent::Error { message, .. }] => assert!(message.contains("bad")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn unauthenticated_error_is_fatal() {
    let v = serde_json::json!({
        "error": { "status": "UNAUTHENTICATED", "message": "nope" }
    });
    let evs = proto().parse_event(&v);
    match evs.as_slice() {
        [RealtimeEvent::Error { recoverable, .. }] => assert!(!*recoverable),
        other => panic!("expected fatal Error, got {other:?}"),
    }
}

#[test]
fn parses_tool_call() {
    let v = serde_json::json!({
        "toolCall": {
            "functionCalls": [{
                "id": "c1",
                "name": "lookup",
                "args": { "q": "rust" }
            }]
        }
    });
    let evs = proto().parse_event(&v);
    match evs.as_slice() {
        [
            RealtimeEvent::FunctionCall {
                call_id,
                name,
                arguments,
            },
        ] => {
            assert_eq!(call_id, "c1");
            assert_eq!(name, "lookup");
            assert!(arguments.contains("rust"));
        }
        other => panic!("expected FunctionCall, got {other:?}"),
    }
}

#[test]
fn ignores_unrelated_events() {
    let v = serde_json::json!({ "foo": "bar" });
    assert!(proto().parse_event(&v).is_empty());
}

#[test]
fn session_update_emits_setup_and_opener() {
    // A session that speaks first gets the opener after the setup.
    let cfg = SessionConfig::new("be terse").with_speaks_first(true);
    let envs = proto().build_session_update(&cfg);
    assert_eq!(envs.len(), 2);
    assert!(envs[0]["setup"].is_object());
    assert_eq!(
        envs[0]["setup"]["generationConfig"]["responseModalities"][0],
        "AUDIO"
    );
    assert_eq!(envs[1]["clientContent"]["turnComplete"], true);
}

#[test]
fn session_update_carries_prebuilt_voice() {
    let cfg = SessionConfig::default().with_voice(Some("Orus".into()));
    let envs = proto().build_session_update(&cfg);
    assert_eq!(
        envs[0]["setup"]["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
            ["voiceName"],
        "Orus"
    );
}

#[test]
fn session_update_omits_speech_config_without_voice() {
    let envs = proto().build_session_update(&SessionConfig::default());
    assert!(envs[0]["setup"]["generationConfig"]["speechConfig"].is_null());
}

/// Without a hint Gemini detects the language per utterance and a short one
/// comes back in Russian or Chinese, so the default names English, as the
/// `OpenAI` provider's does.
#[test]
fn session_update_defaults_input_transcription_to_english() {
    let cfg = SessionConfig::default();
    let envs = proto().build_session_update(&cfg);
    assert_eq!(
        envs[0]["setup"]["inputAudioTranscription"]["languageCodes"],
        serde_json::json!(["en-US"])
    );
}

#[test]
fn session_update_sends_no_language_for_auto() {
    let mut cfg = SessionConfig::default();
    cfg.provider_options.insert(
        "input_transcription_language".to_string(),
        serde_json::json!("auto"),
    );
    let envs = proto().build_session_update(&cfg);
    assert_eq!(
        envs[0]["setup"]["inputAudioTranscription"],
        serde_json::json!({})
    );
}

#[test]
fn session_update_forwards_input_transcription_language_string() {
    let mut cfg = SessionConfig::default();
    cfg.provider_options.insert(
        "input_transcription_language".to_string(),
        serde_json::json!("en-US, de-DE"),
    );
    let envs = proto().build_session_update(&cfg);
    assert_eq!(
        envs[0]["setup"]["inputAudioTranscription"]["languageCodes"],
        serde_json::json!(["en-US", "de-DE"])
    );
}

#[test]
fn session_update_forwards_input_transcription_language_array() {
    let mut cfg = SessionConfig::default();
    cfg.provider_options.insert(
        "input_transcription_language".to_string(),
        serde_json::json!(["en-US"]),
    );
    let envs = proto().build_session_update(&cfg);
    assert_eq!(
        envs[0]["setup"]["inputAudioTranscription"]["languageCodes"],
        serde_json::json!(["en-US"])
    );
}

#[test]
fn session_update_can_suppress_opener() {
    let cfg = SessionConfig::default().with_speaks_first(true);
    let envs = proto().with_opener(None).build_session_update(&cfg);
    assert_eq!(envs.len(), 1, "no opener should produce only setup");
}

/// A session that waits to be spoken to sends no opener, and one that asks
/// for no transcript gets no input-transcription block.
#[test]
fn the_session_decides_the_opener_and_input_transcription() {
    let answering = proto().build_session_update(&SessionConfig::default());
    assert_eq!(answering.len(), 1, "{answering:?}");
    assert!(
        answering[0]["setup"]
            .get("inputAudioTranscription")
            .is_some()
    );
    let calling = proto().build_session_update(
        &SessionConfig::default()
            .with_speaks_first(true)
            .with_transcribe_input(false),
    );
    assert_eq!(calling.len(), 2, "{calling:?}");
    assert!(calling[0]["setup"].get("inputAudioTranscription").is_none());
}

#[test]
fn audio_append_uses_realtime_input_envelope() {
    let env = proto().build_audio_append("AAA");
    assert!(
        env["realtimeInput"]["audio"]["mimeType"]
            .as_str()
            .unwrap()
            .starts_with("audio/pcm;rate=16000")
    );
    assert_eq!(env["realtimeInput"]["audio"]["data"], "AAA");
    assert!(env["realtimeInput"]["mediaChunks"].is_null());
}

#[test]
fn text_message_uses_realtime_input_text() {
    let env = proto().build_text_message("hi").unwrap();
    // Typed text must NOT be a `clientContent` turn: native-audio models
    // reject that with a 1007 close once realtime audio has been streamed.
    assert_eq!(env["realtimeInput"]["text"], "hi");
    assert!(env.get("clientContent").is_none());
}

#[test]
fn cancel_is_noop_turn_complete() {
    assert!(proto().build_cancel().is_none());
}

#[test]
fn create_response_is_none_for_gemini() {
    assert!(proto().build_response_create(Modalities::Audio).is_none());
}

#[test]
fn model_prefix_is_auto_added() {
    let p = GeminiProtocol::new().with_model("gemini-2.0-flash-exp");
    assert_eq!(p.model(), "models/gemini-2.0-flash-exp");
}

/// The reply text the mapper emits for `chunks` of `outputTranscription`.
fn replayed_output(chunks: &[&str]) -> String {
    let p = proto();
    chunks
        .iter()
        .flat_map(|chunk| {
            p.parse_event(&serde_json::json!({
                "serverContent": { "outputTranscription": { "text": chunk } }
            }))
        })
        .map(|ev| match ev {
            RealtimeEvent::ResponseTextDelta { text } => text,
            other => panic!("expected ResponseTextDelta, got {other:?}"),
        })
        .collect()
}

/// Output transcription arrives as consecutive pieces, not as the text so
/// far: these are the frames gemini-2.5-flash-native-audio-latest and
/// gemini-3.8-live sent for one reply each.
#[test]
fn output_transcription_pieces_are_appended_as_sent() {
    let live_25 = [
        "I'm sorry",
        " to hear",
        " that.",
        " What",
        " isn't",
        " quite",
        " right?",
        " Can",
        " I help",
        " you fix",
        " it?",
    ];
    assert_eq!(
        replayed_output(&live_25),
        "I'm sorry to hear that. What isn't quite right? Can I help you fix it?"
    );
    let live_38 = [
        "I understand. Could ",
        "you please tell",
        " me what happened, or",
        " what you are trying to transcribe?",
    ];
    assert_eq!(
        replayed_output(&live_38),
        "I understand. Could you please tell me what happened, or what you are trying to transcribe?"
    );
}

/// A piece that starts like the one before it is still a new piece: it
/// must not be cut to the part after their common prefix, or dropped
/// when it repeats.
#[test]
fn output_transcription_pieces_sharing_a_prefix_are_kept_whole() {
    assert_eq!(replayed_output(&[" the", " then", " end"]), " the then end");
    assert_eq!(replayed_output(&["No.", "No.", " No."]), "No.No. No.");
    assert_eq!(
        replayed_output(&["Привет", "Привет мир"]),
        "ПриветПривет мир"
    );
}

/// The pieces are split at spaces, keeping each space with the word
/// before it, so a subscriber sees word-sized deltas.
#[test]
fn output_transcription_pieces_split_into_words() {
    let evs = proto().parse_event(&serde_json::json!({
        "serverContent": { "outputTranscription": { "text": "I understand. Could " } }
    }));
    let words: Vec<_> = evs
        .into_iter()
        .map(|ev| match ev {
            RealtimeEvent::ResponseTextDelta { text } => text,
            other => panic!("expected ResponseTextDelta, got {other:?}"),
        })
        .collect();
    assert_eq!(words, ["I ", "understand. ", "Could "]);
}

/// Declared tools must ride the setup envelope as functionDeclarations
/// with Gemini-shaped (uppercase-typed, subset) parameter schemas —
/// without them the model can never call agent or client-side tools.
#[test]
fn session_update_declares_tools_as_function_declarations() {
    let cfg = SessionConfig::default().with_tools(vec![voice_pipes_sdk::s2s::ToolSpec::new(
        "navigate",
        "Navigate the UI.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "screen": { "type": "string", "description": "target screen" },
                "tabs": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["screen"],
            "additionalProperties": false
        }),
    )]);
    let envs = proto().build_session_update(&cfg);
    let decls = &envs[0]["setup"]["tools"][0]["functionDeclarations"];
    assert_eq!(decls[0]["name"], "navigate");
    assert_eq!(decls[0]["description"], "Navigate the UI.");
    let params = &decls[0]["parameters"];
    assert_eq!(params["type"], "OBJECT", "types map to proto enum names");
    assert_eq!(params["properties"]["screen"]["type"], "STRING");
    assert_eq!(params["properties"]["tabs"]["items"]["type"], "STRING");
    assert_eq!(params["required"][0], "screen");
    assert!(
        params.get("additionalProperties").is_none(),
        "unsupported JSON-Schema keys must be stripped"
    );

    // No tools -> no tools key at all.
    let envs = proto().build_session_update(&SessionConfig::default());
    assert!(envs[0]["setup"].get("tools").is_none());
}

/// Declared tools and googleSearch grounding coexist in the tools array.
#[test]
fn session_update_tools_coexist_with_search_grounding() {
    let mut cfg = SessionConfig::default().with_tools(vec![voice_pipes_sdk::s2s::ToolSpec::new(
        "navigate",
        "Navigate the UI.",
        serde_json::json!({ "type": "object" }),
    )]);
    cfg.provider_options.insert(
        "google_search_grounding".to_string(),
        serde_json::json!(true),
    );
    let envs = proto().build_session_update(&cfg);
    let tools = envs[0]["setup"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert!(tools[0]["functionDeclarations"].is_array());
    assert!(tools[1]["googleSearch"].is_object());
}

/// Native-audio models declare functions `NON_BLOCKING` so the
/// `FunctionResponse` `scheduling` directive works — that's what lets a
/// client-tool ack stay silent (Gemini auto-continues after every
/// `toolResponse`; there is no kick to withhold).
#[test]
fn native_audio_declares_non_blocking_and_schedules_responses() {
    // proto() uses the native-audio default model.
    let cfg = SessionConfig::default().with_tools(vec![voice_pipes_sdk::s2s::ToolSpec::new(
        "navigate",
        "Navigate the UI.",
        serde_json::json!({ "type": "object" }),
    )]);
    let envs = proto().build_session_update(&cfg);
    let decl = &envs[0]["setup"]["tools"][0]["functionDeclarations"][0];
    assert_eq!(decl["behavior"], "NON_BLOCKING");

    // Silent ack (client-side tool) -> record-only continuation.
    let env = proto()
        .build_function_call_output("c1", "navigate", "{\"ok\":true}", true)
        .unwrap();
    assert_eq!(
        env["toolResponse"]["functionResponses"][0]["name"], "navigate",
        "FunctionResponse requires name alongside id (Live API tools doc)"
    );
    // `scheduling` is a FunctionResponse field; nested in `response` it
    // is just tool output and Gemini speaks anyway.
    let fr = &env["toolResponse"]["functionResponses"][0];
    assert_eq!(fr["scheduling"], "SILENT");
    assert_eq!(fr["response"]["output"], "{\"ok\":true}");
    assert!(fr["response"].get("scheduling").is_none());

    // Server-tool output -> spoken when idle (pre-existing behavior).
    let env = proto()
        .build_function_call_output("c2", "navigate", "{}", false)
        .unwrap();
    assert_eq!(
        env["toolResponse"]["functionResponses"][0]["scheduling"],
        "WHEN_IDLE"
    );
}

/// `gemini-3.8-live` defaults to `NON_BLOCKING` and honours `scheduling`;
/// its extended-thinking sibling rejects `scheduling` and 3.1 Flash Live
/// is synchronous only.
#[test]
fn gemini_3_8_live_gets_async_function_calling() {
    let cfg = SessionConfig::default().with_tools(vec![voice_pipes_sdk::s2s::ToolSpec::new(
        "navigate",
        "Navigate the UI.",
        serde_json::json!({ "type": "object" }),
    )]);
    let behavior = |model: &str| {
        proto().with_model(model).build_session_update(&cfg)[0]["setup"]["tools"][0]
            ["functionDeclarations"][0]
            .get("behavior")
            .cloned()
    };
    assert_eq!(
        behavior("gemini-3.8-live"),
        Some(serde_json::json!("NON_BLOCKING"))
    );
    assert_eq!(
        behavior("models/gemini-3.8-live"),
        Some(serde_json::json!("NON_BLOCKING"))
    );
    assert_eq!(behavior("gemini-3.8-live-extended-thinking"), None);
    assert_eq!(behavior("gemini-3.1-flash-live-preview"), None);
}

/// `thinkingBudget` is a 2.x field; the 3.x Live models take
/// `thinkingLevel` (or, for `gemini-3.8-live`, nothing).
#[test]
fn thinking_budget_only_reaches_2x_models() {
    let mut cfg = SessionConfig::default();
    cfg.provider_options
        .insert("thinking_budget".to_string(), serde_json::json!(0));
    let thinking = |model: &str, cfg: &SessionConfig| {
        proto().with_model(model).build_session_update(cfg)[0]["setup"]["generationConfig"]
            .get("thinkingConfig")
            .cloned()
    };
    assert_eq!(
        thinking("gemini-2.5-flash-native-audio-latest", &cfg),
        Some(serde_json::json!({ "thinkingBudget": 0 }))
    );
    assert_eq!(thinking("gemini-3.8-live", &cfg), None);

    cfg.provider_options
        .insert("thinking_level".to_string(), serde_json::json!("minimal"));
    assert_eq!(
        thinking("gemini-3.1-flash-live-preview", &cfg),
        Some(serde_json::json!({ "thinkingLevel": "minimal" }))
    );
}

/// The Live API reports output tokens as `responseTokenCount`.
#[test]
fn usage_reads_live_api_response_token_count() {
    let v = serde_json::json!({
        "serverContent": { "turnComplete": true },
        "usageMetadata": {
            "promptTokenCount": 760,
            "responseTokenCount": 10,
            "totalTokenCount": 770
        }
    });
    let events = proto().parse_event(&v);
    assert!(events.iter().any(|e| matches!(
        e,
        RealtimeEvent::ResponseUsage {
            input_tokens: 760,
            output_tokens: 10,
            total_tokens: 770,
            ..
        }
    )));
}

/// Half-cascade Live models don't support async function calling: no
/// `NON_BLOCKING` behavior, no scheduling key (best-effort silence only).
#[test]
fn non_native_audio_models_omit_async_function_vocabulary() {
    let p = proto().with_model("gemini-2.0-flash-live-001");
    let cfg = SessionConfig::default().with_tools(vec![voice_pipes_sdk::s2s::ToolSpec::new(
        "navigate",
        "Navigate the UI.",
        serde_json::json!({ "type": "object" }),
    )]);
    let envs = p.build_session_update(&cfg);
    let decl = &envs[0]["setup"]["tools"][0]["functionDeclarations"][0];
    assert!(decl.get("behavior").is_none());

    let env = p
        .build_function_call_output("c1", "navigate", "{}", true)
        .unwrap();
    assert!(
        env["toolResponse"]["functionResponses"][0]
            .get("scheduling")
            .is_none()
    );
}

/// The `async_function_calling` option overrides the model-name
/// heuristic in both directions, so a renamed Google model can be
/// force-enabled (or a misbehaving one force-disabled) from config
/// without a code change.
#[test]
fn async_function_calling_config_override() {
    let tool_cfg = SessionConfig::default().with_tools(vec![voice_pipes_sdk::s2s::ToolSpec::new(
        "navigate",
        "Navigate the UI.",
        serde_json::json!({ "type": "object" }),
    )]);

    // Force-enable on a model the heuristic would reject.
    let p = proto()
        .with_model("gemini-4.0-super-live")
        .with_async_function_calling(Some(true));
    let envs = p.build_session_update(&tool_cfg);
    assert_eq!(
        envs[0]["setup"]["tools"][0]["functionDeclarations"][0]["behavior"],
        "NON_BLOCKING"
    );
    let env = p
        .build_function_call_output("c1", "navigate", "{}", true)
        .unwrap();
    assert_eq!(
        env["toolResponse"]["functionResponses"][0]["scheduling"],
        "SILENT"
    );

    // Force-disable on a native-audio model.
    let p = proto().with_async_function_calling(Some(false));
    let envs = p.build_session_update(&tool_cfg);
    assert!(
        envs[0]["setup"]["tools"][0]["functionDeclarations"][0]
            .get("behavior")
            .is_none()
    );
    let env = p
        .build_function_call_output("c1", "navigate", "{}", true)
        .unwrap();
    assert!(
        env["toolResponse"]["functionResponses"][0]
            .get("scheduling")
            .is_none()
    );
}

/// Explicitly-configured VAD knobs ride the setup as
/// `automaticActivityDetection`; absent config keeps Google's server
/// defaults (no `realtimeInputConfig` at all — the runner's neutral
/// `TurnDetection` defaults must NOT leak into Gemini sessions).
#[test]
fn session_update_forwards_explicit_vad_tuning_only() {
    // No knobs -> no realtimeInputConfig.
    let envs = proto().build_session_update(&SessionConfig::default());
    assert!(
        envs[0]["setup"].get("realtimeInputConfig").is_none(),
        "unconfigured sessions keep Google's VAD defaults"
    );

    // Explicit knobs -> forwarded, with sensitivity enums mapped.
    let mut cfg = SessionConfig::default();
    cfg.provider_options
        .insert("start_sensitivity".into(), serde_json::json!("high"));
    cfg.provider_options
        .insert("end_sensitivity".into(), serde_json::json!("low"));
    cfg.provider_options
        .insert("vad_prefix_padding_ms".into(), serde_json::json!(300));
    cfg.provider_options
        .insert("vad_silence_ms".into(), serde_json::json!(700));
    let envs = proto().build_session_update(&cfg);
    let aad = &envs[0]["setup"]["realtimeInputConfig"]["automaticActivityDetection"];
    assert_eq!(aad["startOfSpeechSensitivity"], "START_SENSITIVITY_HIGH");
    assert_eq!(aad["endOfSpeechSensitivity"], "END_SENSITIVITY_LOW");
    assert_eq!(aad["prefixPaddingMs"], 300);
    assert_eq!(aad["silenceDurationMs"], 700);

    // Unrecognised sensitivity values are ignored, not forwarded.
    let mut cfg = SessionConfig::default();
    cfg.provider_options
        .insert("start_sensitivity".into(), serde_json::json!("maximum"));
    let envs = proto().build_session_update(&cfg);
    assert!(envs[0]["setup"].get("realtimeInputConfig").is_none());
}

// ── Malformed input ─────────────────────────────────────────

fn is_error(e: &RealtimeEvent, want: &str) -> bool {
    matches!(e, RealtimeEvent::Error { code: Some(c), recoverable: true, .. } if c == want)
}

/// Frames that are not event objects, or carry nothing this parser knows,
/// yield nothing rather than a panic.
#[test]
fn non_object_and_unknown_frames_yield_nothing() {
    for v in [
        serde_json::json!(null),
        serde_json::json!(42),
        serde_json::json!("serverContent"),
        serde_json::json!([{ "setupComplete": {} }]),
        serde_json::json!({ "brandNewMessage": { "x": 1 } }),
    ] {
        assert!(proto().parse_event(&v).is_empty(), "{v}");
    }
}

/// A field of the wrong type is skipped, never trusted and never a panic.
#[test]
fn wrongly_typed_fields_are_skipped() {
    for v in [
        serde_json::json!({ "serverContent": 3 }),
        serde_json::json!({ "serverContent": { "modelTurn": { "parts": "audio" } } }),
        serde_json::json!({ "serverContent": { "modelTurn": { "parts": [ { "inlineData": { "mimeType": 1, "data": 2 } }, 7 ] } } }),
        serde_json::json!({ "serverContent": { "outputTranscription": { "text": 5 } } }),
        serde_json::json!({ "serverContent": { "inputTranscription": [] } }),
        serde_json::json!({ "toolCall": { "functionCalls": "navigate" } }),
        serde_json::json!({ "toolCall": { "functionCalls": [ 1, { "id": 2, "name": 3 } ] } }),
        serde_json::json!({ "usageMetadata": "many tokens" }),
        serde_json::json!({ "error": 3 }),
    ] {
        let evs = proto().parse_event(&v);
        assert!(
            !evs.iter().any(|e| matches!(
                e,
                RealtimeEvent::ResponseAudioDelta { .. } | RealtimeEvent::FunctionCall { .. }
            )),
            "{v} -> {evs:?}"
        );
    }
}

/// Audio that is not base64, or far beyond any real chunk, is a
/// recoverable error, and no audio is emitted for it.
#[test]
fn bad_and_oversized_audio_are_recoverable_errors() {
    let huge = "A".repeat(voice_pipes_sdk::s2s::codec::MAX_AUDIO_B64_BYTES + 1);
    for data in ["not base64 at all!", huge.as_str()] {
        let v = serde_json::json!({
            "serverContent": { "modelTurn": { "parts": [
                { "inlineData": { "mimeType": "audio/pcm;rate=24000", "data": data } }
            ] } }
        });
        let evs = proto().parse_event(&v);
        assert!(evs.iter().any(|e| is_error(e, "audio_decode")), "{evs:?}");
        assert!(
            !evs.iter()
                .any(|e| matches!(e, RealtimeEvent::ResponseAudioDelta { .. })),
            "{evs:?}"
        );
    }
}

/// Tool arguments beyond the cap never become a `FunctionCall`.
#[test]
fn oversized_tool_arguments_are_refused() {
    let big = "x".repeat(voice_pipes_sdk::s2s::event::MAX_TOOL_ARGUMENT_BYTES + 1);
    let v = serde_json::json!({
        "toolCall": { "functionCalls": [ { "id": "c1", "name": "lookup", "args": { "blob": big } } ] }
    });
    let evs = proto().parse_event(&v);
    assert!(
        matches!(evs.as_slice(), [e] if is_error(e, "tool_arguments_too_large")),
        "{evs:?}"
    );
}

/// A function call with no id cannot have its result returned: it is
/// reported, not dispatched.
#[test]
fn a_function_call_without_an_id_is_reported() {
    let evs = proto().parse_event(&serde_json::json!({
        "toolCall": { "functionCalls": [{ "name": "lookup", "args": { "q": 1 } }] }
    }));
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::Error {
                recoverable: true,
                ..
            }]
        ),
        "{evs:?}"
    );
}

/// The frame gemini-2.5-flash-native-audio-latest sent when speech
/// overlapped a pending call, right before its `interrupted` turn.
#[test]
fn tool_call_cancellation_withdraws_the_calls() {
    let evs = proto().parse_event(&serde_json::json!({
        "toolCallCancellation": { "ids": ["function-call-5647403545955885841"] }
    }));
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::FunctionCallsCancelled { call_ids }]
                if call_ids == &["function-call-5647403545955885841"]
        ),
        "{evs:?}"
    );
    let empty = proto().parse_event(&serde_json::json!({ "toolCallCancellation": { "ids": [] } }));
    assert!(empty.is_empty(), "{empty:?}");
}

/// Resumption is always on, so the server issues tokens; a reconnect
/// presents the latest, and a resumed session does not open again.
#[test]
fn session_update_turns_resumption_on_and_resumes_from_a_token() {
    let fresh = proto().build_session_update(&SessionConfig::default().with_speaks_first(true));
    assert_eq!(
        fresh[0]["setup"]["sessionResumption"],
        serde_json::json!({})
    );
    assert_eq!(fresh.len(), 2, "a fresh session is opened: {fresh:?}");

    let resumed = proto().build_session_update(
        &SessionConfig::default()
            .with_speaks_first(true)
            .with_resume_token(Some(Secret::new("CihmMGl0YTRn"))),
    );
    assert_eq!(
        resumed[0]["setup"]["sessionResumption"]["handle"],
        "CihmMGl0YTRn"
    );
    assert_eq!(resumed.len(), 1, "no second opener: {resumed:?}");
}

/// The frames both models sent: 3.8 issues a token straight after setup,
/// 2.5 first sends an empty update, and neither is resumable mid-turn.
#[test]
fn session_resumption_updates_carry_the_latest_token() {
    let evs = proto().parse_event(&serde_json::json!({
        "sessionResumptionUpdate": {
            "newHandle": "6974487e-3ba5-4186-9624-530e34a18649",
            "resumable": true
        }
    }));
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::SessionResumable { token }]
                if token.expose() == "6974487e-3ba5-4186-9624-530e34a18649"
        ),
        "{evs:?}"
    );
    for frame in [
        serde_json::json!({ "sessionResumptionUpdate": {} }),
        serde_json::json!({ "sessionResumptionUpdate": { "resumable": false } }),
        serde_json::json!({ "sessionResumptionUpdate": { "newHandle": "x", "resumable": false } }),
    ] {
        let evs = proto().parse_event(&frame);
        assert!(evs.is_empty(), "{frame}: {evs:?}");
    }
}

/// gemini-2.5-flash-native-audio-latest sent this 540 s into an idle
/// connection.
#[test]
fn go_away_carries_the_time_left() {
    let evs = proto().parse_event(&serde_json::json!({ "goAway": { "timeLeft": "50s" } }));
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::GoingAway { time_left: Some(t) }] if *t == std::time::Duration::from_secs(50)
        ),
        "{evs:?}"
    );
    let evs = proto().parse_event(&serde_json::json!({ "goAway": { "timeLeft": "0.5s" } }));
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::GoingAway { time_left: Some(t) }] if *t == std::time::Duration::from_millis(500)
        ),
        "{evs:?}"
    );
    let evs = proto().parse_event(&serde_json::json!({ "goAway": {} }));
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::GoingAway { time_left: None }]
        ),
        "{evs:?}"
    );
}

/// The closes each model sent, by probe, in the form the engine's client
/// reports them: a token the server does not hold is refused in words of
/// its own, and the rest keep the default's reading.
#[test]
fn closes_are_classified_by_what_gemini_sends() {
    let p = proto();
    let close = |code: u16, reason: &str| {
        p.classify_close(
            Some(code),
            &format!("server close (code={code}, reason={reason})"),
        )
    };
    assert_eq!(
        close(1008, "Requested entity was not found."),
        CloseClass::Unresumable
    );
    assert_eq!(
        close(1007, "Invalid session handle"),
        CloseClass::Unresumable
    );
    // Sent on a resume right after the server dropped the connection with
    // a 1011 internal error (seen live 2026-10-10): the handle died with it.
    assert_eq!(
        close(1008, "BidiGenerateContent session not found or expired"),
        CloseClass::Unresumable
    );
    assert_eq!(
        close(
            1008,
            "models/gemini-9.9-nonexistent is not found for API version v1beta, or is not              supported for bidiGenerateContent. Call ModelS"
        ),
        CloseClass::Fatal
    );
    assert_eq!(
        close(1007, "API key not valid. Please pass a valid API key."),
        CloseClass::Fatal
    );
    assert_eq!(close(1011, "Internal error"), CloseClass::Transient);
}

#[test]
fn metric_conformance() {
    let res = voice_pipes_sdk::testing::assert_s2s_metrics_conformance(
        24_000,
        NATIVE_OUTPUT_SR,
        std::time::Duration::from_millis(200),
    );
    assert!(res.is_ok(), "{res:?}");
}
