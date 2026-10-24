use super::*;

#[test]
fn parse_begin_message() {
    let json = r#"{"type":"Begin","id":"abc-123","expires_at":1759796682}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Begin(b) => {
            assert_eq!(b.id, "abc-123");
            assert_eq!(b.expires_at, 1_759_796_682);
        }
        other => panic!("expected Begin, got {other:?}"),
    }
}

#[test]
fn parse_turn_partial() {
    let json = r#"{
        "type": "Turn",
        "turn_order": 0,
        "turn_is_formatted": true,
        "end_of_turn": false,
        "transcript": "Hi, my name",
        "end_of_turn_confidence": 0.001,
        "words": [
            {"start": 2160, "end": 2560, "text": "Hi,", "confidence": 0.87, "word_is_final": true},
            {"start": 2560, "end": 2560, "text": "my", "confidence": 0.99, "word_is_final": true},
            {"start": 2640, "end": 2720, "text": "name", "confidence": 1.0, "word_is_final": true}
        ],
        "utterance": ""
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Turn(t) => {
            assert_eq!(t.turn_order, 0);
            assert!(!t.end_of_turn);
            assert_eq!(t.transcript, "Hi, my name");
            assert_eq!(t.words.len(), 3);
            assert!(t.utterance.is_empty());
        }
        other => panic!("expected Turn, got {other:?}"),
    }
}

#[test]
fn parse_turn_final_with_utterance() {
    let json = r#"{
        "type": "Turn",
        "turn_order": 0,
        "turn_is_formatted": true,
        "end_of_turn": true,
        "transcript": "Hi, my name is Sonny.",
        "end_of_turn_confidence": 0.43,
        "words": [
            {"start": 2160, "end": 2560, "text": "Hi,", "confidence": 0.87, "word_is_final": true},
            {"start": 2560, "end": 2560, "text": "my", "confidence": 0.99, "word_is_final": true},
            {"start": 2640, "end": 2720, "text": "name", "confidence": 1.0, "word_is_final": true},
            {"start": 2720, "end": 2720, "text": "is", "confidence": 0.73, "word_is_final": true},
            {"start": 3280, "end": 3600, "text": "Sonny.", "confidence": 0.66, "word_is_final": true}
        ],
        "utterance": "Hi, my name is Sonny."
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Turn(t) => {
            assert!(t.end_of_turn);
            assert_eq!(t.transcript, "Hi, my name is Sonny.");
            assert_eq!(t.utterance, "Hi, my name is Sonny.");
            assert_eq!(t.words.len(), 5);
        }
        other => panic!("expected Turn, got {other:?}"),
    }
}

#[test]
fn parse_termination_message() {
    let json =
        r#"{"type":"Termination","audio_duration_seconds":7.0,"session_duration_seconds":7.0}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Termination(t) => {
            assert!((t.audio_duration_seconds - 7.0).abs() < f64::EPSILON);
            assert!((t.session_duration_seconds - 7.0).abs() < f64::EPSILON);
        }
        other => panic!("expected Termination, got {other:?}"),
    }
}

#[test]
fn parse_error_message() {
    // Exact payload observed in production (concurrency limit).
    let json = r#"{"type":"Error","error_code":1008,"error":"Unauthorized Connection: Too many concurrent sessions"}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Error(e) => {
            assert_eq!(e.error_code, Some(1008));
            assert!(e.error.contains("Too many concurrent sessions"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn parse_error_message_without_code() {
    let json = r#"{"type":"Error","error":"something went wrong"}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Error(e) => {
            assert_eq!(e.error_code, None);
            assert_eq!(e.error, "something went wrong");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

/// The client messages are fixed JSON objects naming their type.
#[test]
fn client_messages_are_typed_json() {
    for (message, kind) in [(TERMINATE, "Terminate"), (FORCE_ENDPOINT, "ForceEndpoint")] {
        let v: serde_json::Value = serde_json::from_str(message).unwrap();
        assert_eq!(v, serde_json::json!({ "type": kind }));
    }
}

#[test]
fn parse_turn_with_language_detection() {
    let json = r#"{
        "type": "Turn",
        "turn_order": 0,
        "turn_is_formatted": true,
        "end_of_turn": false,
        "transcript": "Hola",
        "end_of_turn_confidence": 0.01,
        "words": [],
        "utterance": "",
        "language_code": "es",
        "language_confidence": 0.95
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Turn(t) => {
            assert_eq!(t.language_code.as_deref(), Some("es"));
        }
        other => panic!("expected Turn, got {other:?}"),
    }
}

#[test]
fn parse_turn_without_optional_fields() {
    let json = r#"{
        "type": "Turn",
        "turn_order": 1,
        "turn_is_formatted": false,
        "end_of_turn": false,
        "transcript": "test",
        "end_of_turn_confidence": 0.0,
        "words": [],
        "utterance": ""
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Turn(t) => {
            assert!(t.language_code.is_none());
        }
        other => panic!("expected Turn, got {other:?}"),
    }
}
