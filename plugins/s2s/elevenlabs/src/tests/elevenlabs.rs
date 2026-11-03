//! Parser/builder tests for [`super::ElevenLabsProtocol`].
//!
//! Event shapes pinned against the `ElevenLabs` Agents WebSocket
//! reference (`/v1/convai/conversation`): `audio`/`user_transcript`/
//! `agent_response`/`interruption`/`client_tool_call` server events, the
//! `conversation_initiation_client_data` handshake, and the JSON
//! ping→pong keepalive.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

fn proto() -> ElevenLabsProtocol {
    ElevenLabsProtocol::new("agent_test123")
}

fn b64_pcm(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    BASE64.encode(&bytes)
}

fn audio_event(event_id: i64, samples: &[i16]) -> serde_json::Value {
    serde_json::json!({
        "type": "audio",
        "audio_event": { "audio_base_64": b64_pcm(samples), "event_id": event_id }
    })
}

// ── connect_info ────────────────────────────────────────────

#[test]
fn connect_info_builds_agent_url_and_key_header() {
    let p = proto().with_api_key(Some("xi-test-key".into()));
    let info = p.connect_info().expect("connect_info");
    assert_eq!(
        info.url,
        "wss://api.elevenlabs.io/v1/convai/conversation?agent_id=agent_test123"
    );
    assert_eq!(
        info.headers,
        vec![("xi-api-key".to_string(), "xi-test-key".to_string())]
    );
}

#[test]
fn connect_info_public_agent_omits_key_header() {
    let p = proto().with_api_key(None);
    let info = p.connect_info().expect("connect_info");
    assert!(info.headers.is_empty(), "{:?}", info.headers);
}

#[test]
fn connect_info_requires_agent_id() {
    let p = ElevenLabsProtocol::new("");
    assert!(p.connect_info().is_err());
}

#[test]
fn connect_info_honours_regional_endpoint() {
    let p = proto().with_endpoint("wss://api.eu.residency.elevenlabs.io/");
    let info = p.connect_info().expect("connect_info");
    assert!(
        info.url
            .starts_with("wss://api.eu.residency.elevenlabs.io/v1/convai/conversation"),
        "{}",
        info.url
    );
}

// ── outbound builders ───────────────────────────────────────

#[test]
fn session_update_is_bare_without_overrides() {
    // The runner always fills `instructions` (client-tool guidance is
    // auto-appended), but without `override_prompt = true` nothing may
    // be sent as an override — agents reject overrides that are not
    // enabled in their Security tab.
    let cfg = SessionConfig::new("You are a concise voice assistant.");
    let envs = proto().build_session_update(&cfg);
    assert_eq!(envs.len(), 1);
    assert_eq!(
        envs[0],
        serde_json::json!({ "type": "conversation_initiation_client_data" })
    );
}

#[test]
fn session_update_carries_opted_in_overrides() {
    let mut cfg = SessionConfig::new("Persona text.").with_voice(Some("voice_abc".to_string()));
    cfg.provider_options
        .insert("override_prompt".into(), serde_json::json!(true));
    cfg.provider_options
        .insert("language".into(), serde_json::json!("de"));
    cfg.provider_options
        .insert("first_message".into(), serde_json::json!("Hallo!"));
    cfg.provider_options.insert(
        "dynamic_variables".into(),
        serde_json::json!({ "customer": "Ada" }),
    );
    cfg.provider_options
        .insert("user_id".into(), serde_json::json!("u-1"));

    let p = proto().with_llm_override(Some("gpt-4o".to_string()));
    let envs = p.build_session_update(&cfg);
    assert_eq!(envs.len(), 1);
    let env = &envs[0];
    assert_eq!(env["type"], "conversation_initiation_client_data");
    let ov = &env["conversation_config_override"];
    assert_eq!(ov["agent"]["prompt"]["prompt"], "Persona text.");
    assert_eq!(ov["agent"]["prompt"]["llm"], "gpt-4o");
    assert_eq!(ov["agent"]["language"], "de");
    assert_eq!(ov["agent"]["first_message"], "Hallo!");
    assert_eq!(ov["tts"]["voice_id"], "voice_abc");
    assert_eq!(env["dynamic_variables"]["customer"], "Ada");
    assert_eq!(env["user_id"], "u-1");
}

#[test]
fn audio_append_uses_user_audio_chunk_key() {
    let env = proto().build_audio_append("QUJD");
    assert_eq!(env, serde_json::json!({ "user_audio_chunk": "QUJD" }));
}

#[test]
fn text_message_and_no_response_kick() {
    let p = proto();
    assert_eq!(
        p.build_text_message("hi").unwrap(),
        serde_json::json!({ "type": "user_message", "text": "hi" })
    );
    assert!(p.build_response_create(Modalities::Audio).is_none());
    assert!(p.build_cancel().is_none());
}

#[test]
fn function_call_output_is_client_tool_result() {
    let env = proto()
        .build_function_call_output("call-7", "navigate", "{\"ok\":true}", true)
        .unwrap();
    assert_eq!(
        env,
        serde_json::json!({
            "type": "client_tool_result",
            "tool_call_id": "call-7",
            "result": "{\"ok\":true}",
            "is_error": false,
        })
    );
}

#[test]
fn context_message_maps_to_contextual_update_or_user_message() {
    let p = proto();
    let silent = p.build_context_message("user opened pricing page", false, Modalities::Audio);
    assert_eq!(
        silent,
        vec![serde_json::json!({
            "type": "contextual_update",
            "text": "user opened pricing page"
        })]
    );
    let spoken = p.build_context_message("tool finished", true, Modalities::Audio);
    assert_eq!(spoken.len(), 1);
    assert_eq!(spoken[0]["type"], "user_message");
    let text = spoken[0]["text"].as_str().unwrap();
    assert!(text.contains("tool finished"), "{text}");
    assert!(text.contains("background context"), "{text}");
}

// ── alignment (audio ↔ transcript timing) ───────────────────

#[test]
fn audio_alignment_groups_chars_into_words() {
    let p = proto();
    let v = serde_json::json!({
        "type": "audio",
        "audio_event": {
            "audio_base_64": b64_pcm(&[1, 2, 3, 4]),
            "event_id": 1,
            "alignment": {
                "chars":               ["H","i"," ","y","o","u","!"],
                "char_start_times_ms": [0, 50, 100, 150, 200, 250, 300],
                "char_durations_ms":   [50, 50, 50, 50, 50, 50, 50],
            }
        }
    });
    let evs = p.parse_event(&v);
    match evs.as_slice() {
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseAudioDelta {
                alignment: Some(words),
                ..
            },
        ] => {
            assert_eq!(words.len(), 2, "{words:?}");
            assert_eq!(words[0].word, "Hi");
            assert_eq!(words[0].start_ms, 0);
            assert_eq!(words[0].duration_ms, 100); // ends at 50+50
            assert_eq!(words[1].word, "you!");
            assert_eq!(words[1].start_ms, 150);
            assert_eq!(words[1].duration_ms, 200); // 350 - 150
        }
        other => panic!("expected aligned audio delta, got {other:?}"),
    }
}

#[test]
fn audio_without_alignment_yields_none() {
    let evs = proto().parse_event(&audio_event(1, &[1, 2, 3, 4]));
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseAudioDelta {
                alignment: None,
                ..
            }
        ]
    ));
}

#[test]
fn malformed_alignment_degrades_without_panic() {
    let p = proto();
    // Ragged arrays: truncated to the shortest — only "Hi" is complete.
    let ragged = serde_json::json!({
        "type": "audio",
        "audio_event": {
            "audio_base_64": b64_pcm(&[1, 2, 3, 4]),
            "event_id": 1,
            "alignment": {
                "chars":               ["H","i"," ","y","o","u"],
                "char_start_times_ms": [0, 50, 100],
                "char_durations_ms":   [50, 50, 50, 50, 50, 50],
            }
        }
    });
    let evs = p.parse_event(&ragged);
    assert!(matches!(
        evs.as_slice(),
        [_, RealtimeEvent::ResponseAudioDelta { alignment: Some(w), .. }] if w.len() == 1 && w[0].word == "Hi"
    ));
    // Junk alignment value: audio still decoded, alignment None.
    let junk = serde_json::json!({
        "type": "audio",
        "audio_event": {
            "audio_base_64": b64_pcm(&[1, 2, 3, 4]),
            "event_id": 2,
            "alignment": "not an object"
        }
    });
    let evs = p.parse_event(&junk);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseAudioDelta {
            alignment: None,
            ..
        }]
    ));
}

#[test]
fn correction_event_emits_nothing() {
    // Observability-only (logged); must not surface events or count as
    // an unhandled type.
    let evs = proto().parse_event(&serde_json::json!({
        "type": "agent_response_correction",
        "agent_response_correction_event": {
            "original_agent_response": "one two three",
            "corrected_agent_response": "one two",
            "event_id": 5,
        }
    }));
    assert!(evs.is_empty(), "{evs:?}");
}

#[test]
fn declares_inferred_completion_idle_window() {
    // No end-of-response wire event → the runner must be told to close
    // responses on audio/text idle (otherwise the session sticks in
    // "speaking" after every final reply).
    assert_eq!(
        proto().inferred_completion_idle_ms(),
        Some(COMPLETION_IDLE_MS)
    );
    assert_eq!(
        proto()
            .with_completion_idle_ms(800)
            .inferred_completion_idle_ms(),
        Some(800)
    );
}

// ── keepalive ───────────────────────────────────────────────

#[test]
fn ping_gets_pong_auto_reply_and_no_event() {
    let p = proto();
    let ping =
        serde_json::json!({ "type": "ping", "ping_event": { "event_id": 42, "ping_ms": 15 } });
    assert_eq!(
        p.build_auto_reply(&ping).unwrap(),
        serde_json::json!({ "type": "pong", "event_id": 42 })
    );
    assert!(p.parse_event(&ping).is_empty());
    // Non-ping frames never trigger a reply.
    assert!(
        p.build_auto_reply(&serde_json::json!({ "type": "audio" }))
            .is_none()
    );
}

// ── inbound parsing ─────────────────────────────────────────

#[test]
fn initiation_metadata_updates_rates_and_acks_session() {
    let p = proto();
    let v = serde_json::json!({
        "type": "conversation_initiation_metadata",
        "conversation_initiation_metadata_event": {
            "conversation_id": "conv_1",
            "agent_output_audio_format": "pcm_24000",
            "user_input_audio_format": "pcm_16000",
        }
    });
    let evs = p.parse_event(&v);
    assert_eq!(evs, vec![RealtimeEvent::SessionUpdated]);
    assert_eq!(p.native_input_sample_rate(), 16_000);
    assert_eq!(p.native_output_sample_rate(), 24_000);

    // Downlink audio is now decoded at the announced 24 kHz and handed on
    // at that rate: the engine resamples the session's audio as one stream.
    let samples24: Vec<i16> = (0..240).map(|i| (i as i16) * 10).collect();
    let evs = p.parse_event(&audio_event(1, &samples24));
    match evs.as_slice() {
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseAudioDelta {
                samples,
                sample_rate,
                ..
            },
        ] => {
            assert_eq!(*sample_rate, 24_000);
            assert_eq!(*samples, samples24);
        }
        other => panic!("expected started + audio delta, got {other:?}"),
    }
}

/// The announced rate sizes resampler buffers on both directions, so a
/// rate outside the known set is refused like an unknown codec instead
/// of being stored: `pcm_0` and `pcm_1` would otherwise ask the
/// resampler for gigabytes per audio frame.
#[test]
fn absurd_sample_rates_are_refused_and_not_stored() {
    for tag in ["pcm_0", "pcm_1", "pcm_4000000000", "pcm_12345"] {
        let p = proto();
        let before = p.native_output_sample_rate();
        let v = serde_json::json!({
            "type": "conversation_initiation_metadata",
            "conversation_initiation_metadata_event": {
                "agent_output_audio_format": tag,
                "user_input_audio_format": "pcm_16000",
            }
        });
        let evs = p.parse_event(&v);
        assert!(
            evs.iter().any(|e| matches!(
                e,
                RealtimeEvent::Error { code: Some(c), recoverable: false, .. }
                    if c == "unsupported_audio_format"
            )),
            "{tag}: {evs:?}"
        );
        assert_eq!(
            p.native_output_sample_rate(),
            before,
            "{tag} must not be stored"
        );
    }
    // Every supported rate is still accepted.
    for rate in voice_pipes_sdk::s2s::codec::SUPPORTED_SAMPLE_RATES {
        assert_eq!(
            ElevenLabsProtocol::parse_format_rate(&format!("pcm_{rate}")),
            Some(*rate)
        );
    }
}

/// An audio field far beyond any real delta is refused before it is
/// decoded, as a recoverable error, and the session goes on.
#[test]
fn oversized_audio_field_is_a_recoverable_error() {
    let p = proto();
    let huge = "A".repeat(voice_pipes_sdk::s2s::codec::MAX_AUDIO_B64_BYTES + 1);
    let v = serde_json::json!({
        "type": "audio",
        "audio_event": { "audio_base_64": huge, "event_id": 1 }
    });
    let evs = p.parse_event(&v);
    assert!(
        evs.iter().any(|e| matches!(
            e,
            RealtimeEvent::Error { code: Some(c), recoverable: true, .. } if c == "audio_decode"
        )),
        "{evs:?}"
    );
    assert!(
        !evs.iter()
            .any(|e| matches!(e, RealtimeEvent::ResponseAudioDelta { .. })),
        "{evs:?}"
    );
}

/// Tool arguments beyond the cap never become a `FunctionCall`.
#[test]
fn oversized_tool_arguments_are_refused() {
    let p = proto();
    let big = "x".repeat(voice_pipes_sdk::s2s::event::MAX_TOOL_ARGUMENT_BYTES + 1);
    let v = serde_json::json!({
        "type": "client_tool_call",
        "client_tool_call": {
            "tool_call_id": "c1",
            "tool_name": "lookup",
            "parameters": { "blob": big }
        }
    });
    let evs = p.parse_event(&v);
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::Error { code: Some(c), recoverable: true, .. }]
                if c == "tool_arguments_too_large"
        ),
        "{evs:?}"
    );
}

#[test]
fn ulaw_format_surfaces_fatal_error() {
    let p = proto();
    let v = serde_json::json!({
        "type": "conversation_initiation_metadata",
        "conversation_initiation_metadata_event": {
            "agent_output_audio_format": "ulaw_8000",
            "user_input_audio_format": "pcm_16000",
        }
    });
    let evs = p.parse_event(&v);
    assert!(
        matches!(
            evs.as_slice(),
            [
                RealtimeEvent::Error {
                    recoverable: false,
                    ..
                },
                RealtimeEvent::SessionUpdated
            ]
        ),
        "{evs:?}"
    );
}

#[test]
fn first_audio_opens_response_once() {
    let p = proto();
    let evs = p.parse_event(&audio_event(1, &[1, 2, 3, 4]));
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseAudioDelta { .. }
        ]
    ));
    // Second chunk: no duplicate ResponseStarted.
    let evs = p.parse_event(&audio_event(2, &[5, 6, 7, 8]));
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseAudioDelta { .. }]
    ));
}

#[test]
fn agent_response_complete_closes_response_once() {
    let p = proto();
    let _ = p.parse_event(&audio_event(1, &[1, 2, 3, 4]));
    let done = serde_json::json!({
        "type": "agent_response_complete",
        "agent_response_complete_event": { "event_id": 2 }
    });
    let evs = p.parse_event(&done);
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseCompleted { .. }]
    ));
    // Idempotent: a second completion (or one with no open response)
    // emits nothing.
    assert!(p.parse_event(&done).is_empty());
}

#[test]
fn final_audio_flag_closes_response() {
    let p = proto();
    let v = serde_json::json!({
        "type": "audio",
        "audio_event": {
            "audio_base_64": b64_pcm(&[1, 2, 3, 4]),
            "event_id": 1,
            "is_final": true,
        }
    });
    let evs = p.parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseAudioDelta { .. },
            RealtimeEvent::ResponseCompleted { .. }
        ]
    ));
}

#[test]
fn agent_response_text_arrives_as_single_delta() {
    let p = proto();
    let v = serde_json::json!({
        "type": "agent_response",
        "agent_response_event": { "agent_response": "Hello there.", "event_id": 3 }
    });
    let evs = p.parse_event(&v);
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseTextDelta { text }
        ] if text == "Hello there."
    ));
}

#[test]
fn interruption_cancels_and_filters_stale_audio() {
    let p = proto();
    let _ = p.parse_event(&audio_event(5, &[1, 2, 3, 4]));
    let evs = p.parse_event(&serde_json::json!({
        "type": "interruption",
        "interruption_event": { "event_id": 7 }
    }));
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseCancelled { .. }]
    ));
    // In-flight chunks of the interrupted response (event_id <= 7) are
    // dropped; the next response's audio passes and reopens a turn.
    assert!(p.parse_event(&audio_event(6, &[1, 2, 3, 4])).is_empty());
    assert!(p.parse_event(&audio_event(7, &[1, 2, 3, 4])).is_empty());
    let evs = p.parse_event(&audio_event(8, &[1, 2, 3, 4]));
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseAudioDelta { .. }
        ]
    ));
}

#[test]
fn user_transcript_is_final() {
    let evs = proto().parse_event(&serde_json::json!({
        "type": "user_transcript",
        "user_transcription_event": { "user_transcript": "what's the weather", "event_id": 9 }
    }));
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::InputTranscript { text, is_final: true }] if text == "what's the weather"
    ));
}

// ── inferred turn boundaries (production sends no completion event) ─

fn agent_response(text: &str, event_id: i64) -> serde_json::Value {
    serde_json::json!({
        "type": "agent_response",
        "agent_response_event": { "agent_response": text, "event_id": event_id }
    })
}

#[test]
fn user_transcript_closes_open_agent_turn() {
    // Voice flow: agent turn N is open (text + audio streamed), the
    // user speaks → the transcript is the only end-of-turn signal.
    let p = proto();
    let _ = p.parse_event(&agent_response("Hi there.", 1));
    let _ = p.parse_event(&audio_event(2, &[1, 2, 3, 4]));
    let evs = p.parse_event(&serde_json::json!({
        "type": "user_transcript",
        "user_transcription_event": { "user_transcript": "hello", "event_id": 3 }
    }));
    assert!(
        matches!(
            evs.as_slice(),
            [
                RealtimeEvent::ResponseCompleted { .. },
                RealtimeEvent::InputTranscript { is_final: true, .. }
            ]
        ),
        "{evs:?}"
    );
    // The next agent turn opens fresh.
    let evs = p.parse_event(&agent_response("How can I help?", 4));
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseTextDelta { .. }
        ]
    ));
}

#[test]
fn second_agent_response_starts_new_turn() {
    // Typed `user_message` turns produce no `user_transcript`, so the
    // next turn's `agent_response` is the only boundary signal.
    let p = proto();
    let evs = p.parse_event(&agent_response("Hello", 1));
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseTextDelta { text }
        ] if text == "Hello"
    ));
    let _ = p.parse_event(&audio_event(2, &[1, 2, 3, 4]));
    let evs = p.parse_event(&agent_response("Hello there. How can I help you today?", 3));
    assert!(
        matches!(
            evs.as_slice(),
            [
                RealtimeEvent::ResponseCompleted { .. },
                RealtimeEvent::ResponseStarted { .. },
                RealtimeEvent::ResponseTextDelta { text }
            ] if text == "Hello there. How can I help you today?"
        ),
        "{evs:?}"
    );
}

#[test]
fn redelivered_agent_response_is_dropped() {
    // Live-observed: a server-side re-stream re-delivered a turn's
    // text (and audio) with no user input in between — it must not
    // duplicate the transcript or fabricate a new turn.
    let p = proto();
    let _ = p.parse_event(&agent_response(
        "I am sorry, I cannot display a settings screen.",
        1,
    ));
    let _ = p.parse_event(&audio_event(2, &[1, 2, 3, 4]));
    let evs = p.parse_event(&agent_response(
        "I am sorry, I cannot display a settings screen.",
        3,
    ));
    assert!(evs.is_empty(), "{evs:?}");
}

#[test]
fn identical_answer_after_new_user_input_is_a_new_turn() {
    // The duplicate guard must not eat a legitimately repeated answer:
    // sending new user input (typed user_message) clears the memory.
    let p = proto();
    let _ = p.parse_event(&agent_response("It is 3 o'clock.", 1));
    let _ = p.build_text_message("what time is it?");
    let evs = p.parse_event(&agent_response("It is 3 o'clock.", 2));
    assert!(
        matches!(
            evs.as_slice(),
            [
                RealtimeEvent::ResponseCompleted { .. },
                RealtimeEvent::ResponseStarted { .. },
                RealtimeEvent::ResponseTextDelta { .. }
            ]
        ),
        "{evs:?}"
    );
}

#[test]
fn chat_response_parts_are_ignored() {
    // agent_response is the single authoritative transcript source;
    // surfacing the streamed parts too duplicates the turn text.
    let evs = proto().parse_event(&serde_json::json!({
        "type": "agent_chat_response_part",
        "text_response_part": { "text": "Hel", "type": "delta", "event_id": 1 }
    }));
    assert!(evs.is_empty(), "{evs:?}");
}

#[test]
fn replayed_audio_frames_are_dropped() {
    let p = proto();
    assert!(!p.parse_event(&audio_event(5, &[1, 2, 3, 4])).is_empty());
    assert!(!p.parse_event(&audio_event(6, &[1, 2, 3, 4])).is_empty());
    // Same id again (providers may stamp one id per response) passes...
    assert!(!p.parse_event(&audio_event(6, &[1, 2, 3, 4])).is_empty());
    // ...but an id going backwards is a re-stream of played audio.
    assert!(p.parse_event(&audio_event(4, &[1, 2, 3, 4])).is_empty());
}

#[test]
fn audio_and_agent_response_share_one_turn_either_order() {
    // Audio may start streaming before the turn's `agent_response`
    // text arrives — that must NOT split the turn.
    let p = proto();
    let evs = p.parse_event(&audio_event(1, &[1, 2, 3, 4]));
    assert!(matches!(
        evs.as_slice(),
        [
            RealtimeEvent::ResponseStarted { .. },
            RealtimeEvent::ResponseAudioDelta { .. }
        ]
    ));
    let evs = p.parse_event(&agent_response("Hi.", 2));
    assert!(
        matches!(evs.as_slice(), [RealtimeEvent::ResponseTextDelta { .. }]),
        "text of the already-open turn must not close/reopen it: {evs:?}"
    );
    let evs = p.parse_event(&audio_event(3, &[5, 6, 7, 8]));
    assert!(matches!(
        evs.as_slice(),
        [RealtimeEvent::ResponseAudioDelta { .. }]
    ));
}

#[test]
fn client_tool_call_maps_to_function_call() {
    let evs = proto().parse_event(&serde_json::json!({
        "type": "client_tool_call",
        "client_tool_call": {
            "tool_name": "navigate",
            "tool_call_id": "call-42",
            "parameters": { "url": "/pricing" },
            "event_id": 11,
            "expects_response": true,
        }
    }));
    match evs.as_slice() {
        [
            RealtimeEvent::FunctionCall {
                call_id,
                name,
                arguments,
            },
        ] => {
            assert_eq!(call_id, "call-42");
            assert_eq!(name, "navigate");
            let args: serde_json::Value = serde_json::from_str(arguments).unwrap();
            assert_eq!(args["url"], "/pricing");
        }
        other => panic!("expected FunctionCall, got {other:?}"),
    }
}

#[test]
fn client_error_is_recoverable() {
    let evs = proto().parse_event(&serde_json::json!({
        "type": "client_error",
        "error_event": { "code": 1008, "error_name": "override_not_allowed", "message": "nope" }
    }));
    assert!(
        matches!(
            evs.as_slice(),
            [RealtimeEvent::Error { code: Some(c), recoverable: true, .. }]
                if c == "override_not_allowed"
        ),
        "{evs:?}"
    );
}

#[test]
fn telemetry_events_are_ignored() {
    let p = proto();
    for v in [
        serde_json::json!({ "type": "vad_score", "vad_score_event": { "vad_score": 0.9 } }),
        serde_json::json!({
            "type": "agent_response_correction",
            "agent_response_correction_event": {
                "original_agent_response": "long text",
                "corrected_agent_response": "long",
                "event_id": 12,
            }
        }),
        serde_json::json!({ "type": "mcp_connection_status", "mcp_connection_status": {} }),
        serde_json::json!({ "some": "junk with no type" }),
    ] {
        assert!(p.parse_event(&v).is_empty(), "{v}");
    }
}

/// Alignment times at the edge of what a float holds cast to `u64::MAX`;
/// a word's end is their sum, which saturates instead of overflowing.
#[test]
fn huge_alignment_times_saturate_without_panic() {
    let v = serde_json::json!({
        "type": "audio",
        "audio_event": {
            "audio_base_64": b64_pcm(&[1, 2, 3, 4]),
            "event_id": 1,
            "alignment": {
                "chars": ["H", "i"],
                "char_start_times_ms": [1e300, 1e300],
                "char_durations_ms": [1e300, 1e300],
            }
        }
    });
    let evs = proto().parse_event(&v);
    assert!(
        evs.iter().any(|e| matches!(
            e,
            RealtimeEvent::ResponseAudioDelta { alignment: Some(w), .. } if w[0].word == "Hi"
        )),
        "{evs:?}"
    );
}

/// Frames that are not event objects, name an event this parser does not
/// know, or carry fields of the wrong type yield no audio and no tool
/// call, and never a panic.
#[test]
fn non_object_unknown_and_wrongly_typed_frames_are_skipped() {
    for v in [
        serde_json::json!(null),
        serde_json::json!(42),
        serde_json::json!("audio"),
        serde_json::json!([{ "type": "ping" }]),
        serde_json::json!({ "type": 7 }),
        serde_json::json!({ "type": "brand_new_event", "x": 1 }),
        serde_json::json!({ "type": "audio", "audio_event": "loud" }),
        serde_json::json!({ "type": "audio", "audio_event": { "audio_base_64": 42 } }),
        serde_json::json!({ "type": "client_tool_call", "client_tool_call": { "tool_call_id": 1, "tool_name": [] } }),
        serde_json::json!({ "type": "agent_response", "agent_response_event": 3 }),
        serde_json::json!({ "type": "ping", "ping_event": "now" }),
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

/// A client tool call with no id cannot have its result returned: it is
/// reported, not dispatched.
#[test]
fn a_client_tool_call_without_an_id_is_reported() {
    let evs = proto().parse_event(&serde_json::json!({
        "type": "client_tool_call",
        "client_tool_call": { "tool_name": "lookup", "parameters": { "q": 1 } }
    }));
    assert!(
        evs.iter().any(|e| matches!(
            e,
            RealtimeEvent::Error {
                recoverable: true,
                ..
            }
        )),
        "{evs:?}"
    );
    assert!(
        !evs.iter()
            .any(|e| matches!(e, RealtimeEvent::FunctionCall { .. }))
    );
}

#[test]
fn metric_conformance() {
    let res = voice_pipes_sdk::testing::assert_s2s_metrics_conformance(
        16_000,
        DEFAULT_SAMPLE_RATE,
        std::time::Duration::from_millis(200),
    );
    assert!(res.is_ok(), "{res:?}");
}
