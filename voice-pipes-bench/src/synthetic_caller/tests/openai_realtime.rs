//! Adapter-level tests for the `OpenAI` Realtime synthetic-caller.
//!
//! Protocol-level parsing is covered exhaustively by
//! the `voice-pipes-s2s-openai` crate. The tests here only pin the
//! [`super::map_event`] translation that this adapter owns.

use super::*;

#[test]
fn map_event_audio_delta_maps_to_caller_audio_delta() {
    let ev = RealtimeEvent::ResponseAudioDelta {
        samples: vec![1, 2, 3],
        sample_rate: PIPELINE_SR,
        alignment: None,
    };
    match map_event(ev) {
        Some(CallerEvent::AudioDelta(s)) => assert_eq!(s, vec![1, 2, 3]),
        other => panic!("expected AudioDelta, got {other:?}"),
    }
}

#[test]
fn map_event_text_delta_maps_to_caller_transcript_delta() {
    let ev = RealtimeEvent::ResponseTextDelta {
        text: "hi".to_string(),
    };
    match map_event(ev) {
        Some(CallerEvent::TranscriptDelta(s)) => assert_eq!(s, "hi"),
        other => panic!("expected TranscriptDelta, got {other:?}"),
    }
}

#[test]
fn map_event_completed_maps_to_response_done() {
    let ev = RealtimeEvent::ResponseCompleted { response_id: None };
    assert!(matches!(map_event(ev), Some(CallerEvent::ResponseDone)));
}

#[test]
fn map_event_error_maps_to_caller_error() {
    let ev = RealtimeEvent::Error {
        code: None,
        message: "boom".to_string(),
        recoverable: true,
    };
    match map_event(ev) {
        Some(CallerEvent::Error(m)) => assert!(m.contains("boom")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn map_event_closed_surfaces_as_error() {
    let ev = RealtimeEvent::Closed {
        code: None,
        reason: "bye".to_string(),
    };
    match map_event(ev) {
        Some(CallerEvent::Error(m)) => assert!(m.contains("bye")),
        other => panic!("expected Error for Closed, got {other:?}"),
    }
}

#[test]
fn map_event_lifecycle_events_are_dropped() {
    for ev in [
        RealtimeEvent::InputSpeechStarted,
        RealtimeEvent::InputSpeechStopped,
        RealtimeEvent::InputAudioCommitted,
        RealtimeEvent::SessionUpdated,
        RealtimeEvent::ResponseStarted { response_id: None },
        RealtimeEvent::ResponseCancelled { response_id: None },
        RealtimeEvent::InputTranscript {
            text: "x".into(),
            is_final: true,
        },
        RealtimeEvent::FunctionCall {
            call_id: "c".into(),
            name: "f".into(),
            arguments: "{}".into(),
        },
    ] {
        assert!(map_event(ev).is_none());
    }
}

#[test]
fn builder_defaults_to_the_providers_model() {
    let b = OpenAiRealtimeCallerBuilder::default();
    assert!(b.model.is_none());
    assert_eq!(b.api_key_env, "OPENAI_API_KEY");
    assert!(b.voice.is_none());
}
