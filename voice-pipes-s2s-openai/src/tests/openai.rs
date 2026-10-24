//! Parser tests for [`super::OpenAiProtocol::parse_event`].
//!
//! Mirror the inline `translate_event` tests that used to live in
//! `synthetic_caller/openai_realtime.rs`. Pin both the GA and legacy
//! Beta event names so a regression in either path breaks here.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

fn proto() -> OpenAiProtocol {
    OpenAiProtocol::new()
}

#[test]
fn parses_legacy_response_audio_delta() {
    let samples24: Vec<i16> = (0..240).map(|i| (i as i16) * 10).collect();
    let mut bytes = Vec::with_capacity(samples24.len() * 2);
    for s in &samples24 {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let b64 = BASE64.encode(&bytes);
    let v = serde_json::json!({ "type": "response.audio.delta", "delta": b64 });
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
            assert_eq!(*sample_rate, NATIVE_SAMPLE_RATE);
            assert_eq!(*samples, samples24);
        }
        other => panic!("expected ResponseAudioDelta, got {other:?}"),
    }
}

#[test]
fn parses_ga_output_audio_delta() {
    let pcm: Vec<i16> = (0..48).map(|i| i as i16 * 100).collect();
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for s in &pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let v = serde_json::json!({
        "type": "response.output_audio.delta",
        "delta": BASE64.encode(&bytes),
    });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseAudioDelta { samples, .. }] if !samples.is_empty()
    ));
}

#[test]
fn parses_legacy_transcript_delta() {
    let v = serde_json::json!({
        "type": "response.audio_transcript.delta",
        "delta": "hello "
    });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseTextDelta { text }] if text == "hello "
    ));
}

#[test]
fn parses_ga_output_audio_transcript_delta() {
    let v = serde_json::json!({
        "type": "response.output_audio_transcript.delta",
        "delta": "hello"
    });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseTextDelta { text }] if text == "hello"
    ));
}

#[test]
fn parses_text_only_output_text_delta() {
    let v = serde_json::json!({
        "type": "response.output_text.delta",
        "delta": "hi"
    });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseTextDelta { text }] if text == "hi"
    ));
}

#[test]
fn parses_response_done_as_completed() {
    let v = serde_json::json!({ "type": "response.done" });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseCompleted { .. }]
    ));
}

#[test]
fn parses_ga_output_audio_done_is_ignored() {
    let v = serde_json::json!({ "type": "response.output_audio.done" });
    let evs = proto().parse_event(&v);
    assert!(evs.is_empty());
}

#[test]
fn parses_legacy_response_audio_done_is_ignored() {
    let v = serde_json::json!({ "type": "response.audio.done" });
    let evs = proto().parse_event(&v);
    assert!(evs.is_empty());
}

#[test]
fn parses_error_envelope_with_message() {
    let v = serde_json::json!({
        "type": "error",
        "error": { "message": "boom" }
    });
    let evs = proto().parse_event(&v);
    match evs.as_slice() {
        [
            RealtimeEvent::Error {
                message,
                recoverable,
                ..
            },
        ] => {
            assert!(message.contains("boom"));
            assert!(*recoverable);
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn parses_error_envelope_without_message() {
    // Regression: server "error" event without a `error.message`
    // field must still surface an Error (not be silently dropped).
    let v = serde_json::json!({ "type": "error", "error": {} });
    let evs = proto().parse_event(&v);
    match evs.as_slice() {
        [RealtimeEvent::Error { message, .. }] => {
            assert!(message.contains("(no message)"), "got: {message}");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn marks_fatal_error_codes_as_non_recoverable() {
    let v = serde_json::json!({
        "type": "error",
        "error": { "code": "model_not_found", "message": "nope" }
    });
    let evs = proto().parse_event(&v);
    match evs.as_slice() {
        [
            RealtimeEvent::Error {
                recoverable, code, ..
            },
        ] => {
            assert!(!*recoverable);
            assert_eq!(code.as_deref(), Some("model_not_found"));
        }
        other => panic!("expected fatal Error, got {other:?}"),
    }
}

#[test]
fn ignores_unknown_events() {
    let v = serde_json::json!({ "type": "completely.unknown" });
    assert!(proto().parse_event(&v).is_empty());
}

#[test]
fn ignores_events_without_type() {
    let v = serde_json::json!({ "foo": "bar" });
    assert!(proto().parse_event(&v).is_empty());
}

#[test]
fn surfaces_input_audio_lifecycle_events() {
    for (k, expect) in [
        (
            "input_audio_buffer.speech_started",
            RealtimeEvent::InputSpeechStarted,
        ),
        (
            "input_audio_buffer.speech_stopped",
            RealtimeEvent::InputSpeechStopped,
        ),
        (
            "input_audio_buffer.committed",
            RealtimeEvent::InputAudioCommitted,
        ),
    ] {
        let v = serde_json::json!({ "type": k });
        let evs = proto().parse_event(&v);
        assert_eq!(evs.len(), 1, "{k} -> {evs:?}");
        assert_eq!(evs[0], expect);
    }
}

#[test]
fn surfaces_input_audio_transcription_completed() {
    let v = serde_json::json!({
        "type": "conversation.item.input_audio_transcription.completed",
        "transcript": "hi there"
    });
    let evs = proto().parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::InputTranscript { text, is_final: true }] if text == "hi there"
    ));
}

#[test]
fn session_update_payload_is_ga_shape() {
    let cfg = SessionConfig::new("be helpful")
        .with_voice(Some("alloy".to_string()))
        .with_speaks_first(true);
    let envs = proto().build_session_update(&cfg);
    assert_eq!(envs.len(), 2, "expected session.update + opener kick");
    let update = &envs[0];
    assert_eq!(update["type"], "session.update");
    assert_eq!(update["session"]["type"], "realtime");
    assert_eq!(update["session"]["output_modalities"][0], "audio");
    assert_eq!(
        update["session"]["audio"]["input"]["format"]["rate"],
        24_000
    );
    assert_eq!(update["session"]["audio"]["output"]["voice"], "alloy");
    let kick = &envs[1];
    assert_eq!(kick["type"], "response.create");
    assert_eq!(kick["response"]["output_modalities"][0], "audio");
}

#[test]
fn text_message_uses_ga_conversation_item_shape() {
    let env = proto().build_text_message("hello").unwrap();
    assert_eq!(env["type"], "conversation.item.create");
    assert_eq!(env["item"]["role"], "user");
    assert_eq!(env["item"]["content"][0]["text"], "hello");
}

#[test]
fn cancel_envelope_is_response_cancel() {
    let env = proto().build_cancel().unwrap();
    assert_eq!(env["type"], "response.cancel");
}

#[test]
fn connect_info_includes_bearer_auth() {
    let p = OpenAiProtocol::new().with_api_key(Some("sk-test-1234".into()));
    let info = p.connect_info().unwrap();
    assert!(
        info.url
            .starts_with("wss://api.openai.com/v1/realtime?model=")
    );
    assert!(
        info.headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test-1234")
    );
}

/// Declared tools must ride `session.tools` as flat GA function entries
/// (JSON Schema passed through verbatim) — without them the model can
/// never call agent or client-side tools.
#[test]
fn session_update_declares_tools() {
    let cfg = SessionConfig::default().with_tools(vec![voice_pipes_sdk::s2s::ToolSpec::new(
        "navigate",
        "Navigate the UI.",
        serde_json::json!({
            "type": "object",
            "properties": { "screen": { "type": "string" } },
            "required": ["screen"]
        }),
    )]);
    let envs = proto().build_session_update(&cfg);
    let tools = envs[0]["session"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "navigate");
    assert_eq!(tools[0]["description"], "Navigate the UI.");
    assert_eq!(
        tools[0]["parameters"]["properties"]["screen"]["type"], "string",
        "JSON Schema passes through verbatim"
    );

    // No tools -> no tools key at all.
    let envs = proto().build_session_update(&SessionConfig::default());
    assert!(envs[0]["session"].get("tools").is_none());
}

// ── Malformed input ─────────────────────────────────────────

fn is_error(e: &RealtimeEvent, want: &str) -> bool {
    matches!(e, RealtimeEvent::Error { code: Some(c), recoverable: true, .. } if c == want)
}

/// Frames that are not event objects, or name an event this parser does
/// not know, yield nothing rather than a panic.
#[test]
fn non_object_and_unknown_frames_yield_nothing() {
    for v in [
        serde_json::json!(null),
        serde_json::json!(42),
        serde_json::json!("response.output_audio.delta"),
        serde_json::json!([{ "type": "session.created" }]),
        serde_json::json!({ "type": 7 }),
        serde_json::json!({ "type": "response.brand_new_event", "delta": "x" }),
    ] {
        assert!(proto().parse_event(&v).is_empty(), "{v}");
    }
}

/// A field of the wrong type is skipped, never trusted and never a panic.
#[test]
fn wrongly_typed_fields_are_skipped() {
    for v in [
        serde_json::json!({ "type": "response.output_audio.delta", "delta": 42 }),
        serde_json::json!({ "type": "response.output_audio_transcript.delta", "delta": ["a"] }),
        serde_json::json!({ "type": "response.function_call_arguments.done", "call_id": 5, "name": {}, "arguments": 1 }),
        serde_json::json!({ "type": "rate_limits.updated", "rate_limits": "all of them" }),
        serde_json::json!({ "type": "rate_limits.updated", "rate_limits": [{ "limit": "many" }, 3] }),
        serde_json::json!({ "type": "response.done", "response": { "usage": "lots" } }),
        serde_json::json!({ "type": "error", "error": 3 }),
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

/// Audio that is not base64, or far beyond any real delta, is a
/// recoverable error, and no audio is emitted for it.
#[test]
fn bad_and_oversized_audio_are_recoverable_errors() {
    let huge = "A".repeat(voice_pipes_sdk::s2s::codec::MAX_AUDIO_B64_BYTES + 1);
    for delta in ["not base64 at all!", huge.as_str()] {
        let v = serde_json::json!({ "type": "response.output_audio.delta", "delta": delta });
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
        "type": "response.function_call_arguments.done",
        "call_id": "c1",
        "name": "lookup",
        "arguments": big,
    });
    let evs = proto().parse_event(&v);
    assert!(
        matches!(evs.as_slice(), [e] if is_error(e, "tool_arguments_too_large")),
        "{evs:?}"
    );
}

/// Only an assistant message's audio can be truncated when the user barges
/// in; a function-call item added to the response is not surfaced for it.
#[test]
fn only_message_items_are_offered_for_truncation() {
    let added = |item: serde_json::Value| {
        proto().parse_event(&serde_json::json!({
            "type": "response.output_item.added",
            "item": item,
        }))
    };
    assert_eq!(
        added(serde_json::json!({ "id": "item_1", "type": "message", "role": "assistant" })),
        vec![RealtimeEvent::ResponseOutputItemAdded {
            item_id: "item_1".into()
        }]
    );
    assert!(
        added(serde_json::json!({ "id": "item_2", "type": "function_call", "name": "lookup", "call_id": "c1" }))
            .is_empty()
    );
}

/// Semantic VAD is sent as semantic VAD, with the eagerness asked for; the
/// Realtime API supports it, and replacing it with fixed server-VAD values
/// discarded the operator's choice.
#[test]
fn semantic_vad_is_sent_as_asked() {
    let cfg = SessionConfig::new("be helpful").with_turn_detection(TurnDetection::SemanticVad {
        eagerness: Some("low".into()),
    });
    let envelopes = proto().build_session_update(&cfg);
    let td = &envelopes[0]["session"]["audio"]["input"]["turn_detection"];
    assert_eq!(td["type"], "semantic_vad", "{td}");
    assert_eq!(td["eagerness"], "low", "{td}");
}

/// No turn detection is `turn_detection: null`, which is how the Realtime
/// API turns it off; it has no `"none"` type, and the session would keep
/// its own VAD or be refused.
#[test]
fn no_turn_detection_is_sent_as_null() {
    let cfg = SessionConfig::new("be helpful").with_turn_detection(TurnDetection::None);
    let envelopes = proto().build_session_update(&cfg);
    let input = &envelopes[0]["session"]["audio"]["input"];
    assert!(
        input
            .get("turn_detection")
            .is_some_and(serde_json::Value::is_null),
        "{input}"
    );
}

/// A call with no id is reported, not dropped; one with no arguments takes
/// none.
#[test]
fn an_incomplete_function_call_is_reported_and_empty_arguments_are_none() {
    let done = |v: serde_json::Value| proto().parse_event(&v);
    assert!(matches!(
        done(serde_json::json!({ "type": "response.function_call_arguments.done", "name": "lookup", "arguments": "{}" })).as_slice(),
        [RealtimeEvent::Error { recoverable: true, .. }]
    ));
    assert!(matches!(
        done(serde_json::json!({ "type": "response.function_call_arguments.done", "call_id": "c1", "name": "lookup" })).as_slice(),
        [RealtimeEvent::FunctionCall { arguments, .. }] if arguments == "{}"
    ));
}

#[test]
fn metric_conformance() {
    let res = voice_pipes_sdk::testing::assert_s2s_metrics_conformance(
        24_000,
        NATIVE_SAMPLE_RATE,
        std::time::Duration::from_millis(200),
    );
    assert!(res.is_ok(), "{res:?}");
}
