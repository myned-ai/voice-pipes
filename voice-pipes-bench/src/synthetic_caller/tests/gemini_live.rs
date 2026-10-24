//! Adapter-level tests for the Gemini Live synthetic-caller.
//!
//! Protocol-level parsing is covered exhaustively by the
//! `voice-pipes-s2s-gemini` crate. These tests pin the adapter's own
//! [`super::map_event`] translation and the [`GeminiLiveCallerBuilder`]
//! defaults.

use super::*;

#[test]
fn map_event_audio_delta_maps_to_caller_audio_delta() {
    let ev = RealtimeEvent::ResponseAudioDelta {
        samples: vec![10, 20, 30],
        sample_rate: PIPELINE_SR,
        alignment: None,
    };
    match map_event(ev) {
        Some(CallerEvent::AudioDelta(s)) => assert_eq!(s, vec![10, 20, 30]),
        other => panic!("expected AudioDelta, got {other:?}"),
    }
}

#[test]
fn map_event_text_delta_maps_to_caller_transcript_delta() {
    let ev = RealtimeEvent::ResponseTextDelta {
        text: "hello".to_string(),
    };
    match map_event(ev) {
        Some(CallerEvent::TranscriptDelta(s)) => assert_eq!(s, "hello"),
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
        recoverable: false,
    };
    match map_event(ev) {
        Some(CallerEvent::Error(m)) => assert!(m.contains("boom")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn map_event_lifecycle_events_are_dropped() {
    for ev in [
        RealtimeEvent::SessionUpdated,
        RealtimeEvent::InputSpeechStarted,
        RealtimeEvent::ResponseCancelled { response_id: None },
    ] {
        assert!(map_event(ev).is_none());
    }
}

#[test]
fn builder_defaults_to_the_providers_model_and_env() {
    let b = GeminiLiveCallerBuilder::default();
    assert!(b.model.is_none());
    assert_eq!(b.api_key_env, "GEMINI_API_KEY");
}
