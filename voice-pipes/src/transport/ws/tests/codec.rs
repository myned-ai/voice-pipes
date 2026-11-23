use super::*;
use crate::transport::{AudioFormat, NegotiatedConfig, PipelineState, ServerControl, SessionInfo};

#[test]
fn audio_roundtrip() {
    let chunk = AudioChunk {
        samples: vec![100, -200, 300],
        sample_rate: 16_000,
        channels: 1,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    let msg = ServerMessage::Audio(chunk);
    let frame = encode_server_message(&msg, 0).unwrap();

    match frame {
        Message::Binary(bytes) => {
            let decoded = decode_client_message(Message::Binary(bytes)).unwrap();
            match decoded {
                Some(ClientMessage::Audio(c)) => {
                    assert_eq!(c.samples, vec![100, -200, 300]);
                    assert!(!c.is_end_of_utterance);
                }
                other => panic!("expected Audio, got {other:?}"),
            }
        }
        other => panic!("expected Binary, got {other:?}"),
    }
}

/// Off-path annotations ride their own `audio_annotation` text envelope:
/// payload fields verbatim from the
/// [`AudioAnnotation`](crate::pipeline::AudioAnnotation), `extras`
/// forwarded untouched, plus a server timestamp.
#[test]
fn annotation_encodes_as_audio_annotation_envelope() {
    let msg = ServerMessage::Annotation(crate::pipeline::AudioAnnotation {
        utterance_id: 12,
        start_ms: 160,
        duration_ms: 160,
        kind: "blendshapes".into(),
        extras: serde_json::json!({ "fps": 30, "frames": [[0.1, 0.2]], "names": ["a", "b"] }),
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    let Message::Text(text) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["type"], "audio_annotation");
    let p = &v["payload"];
    assert_eq!(p["utterance_id"], 12);
    assert_eq!(p["start_ms"], 160);
    assert_eq!(p["duration_ms"], 160);
    assert_eq!(p["kind"], "blendshapes");
    assert_eq!(p["extras"]["fps"], 30);
    assert_eq!(p["extras"]["frames"][0][1], 0.2);
    assert!(p["server_timestamp_ms"].as_u64().is_some_and(|t| t > 0));
}

#[test]
fn control_cancel_encode_decode() {
    let envelope = serde_json::json!({
        "type": "control",
        "payload": { "action": "cancel" }
    });
    let text = serde_json::to_string(&envelope).unwrap();
    let decoded = decode_client_message(Message::Text(text.into())).unwrap();
    match decoded {
        Some(ClientMessage::Control(ClientControl::Cancel)) => {}
        other => panic!("expected Cancel, got {other:?}"),
    }
}

/// Client-side tool results decode as typed controls with a phase,
/// optional result payload, and optional speak request.
#[test]
fn control_client_tool_result_decodes() {
    let envelope = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "client_tool_result",
            "call_id": "call_1",
            "phase": "completed",
            "result": { "screen": "settings" }
        }
    });
    let text = serde_json::to_string(&envelope).unwrap();
    let decoded = decode_client_message(Message::Text(text.into())).unwrap();
    match decoded {
        Some(ClientMessage::Control(crate::transport::ClientControl::ClientToolResult {
            call_id,
            phase,
            result,
            speak,
        })) => {
            assert_eq!(call_id, "call_1");
            assert_eq!(phase, crate::transport::ClientToolPhase::Completed);
            assert_eq!(result.unwrap()["screen"], "settings");
            assert!(!speak, "speak defaults to false");
        }
        other => panic!("expected ClientToolResult, got {other:?}"),
    }

    // Interim phase without a result, with an explicit speak request.
    let envelope = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "client_tool_result",
            "call_id": "call_2",
            "phase": "failed",
            "speak": true
        }
    });
    let text = serde_json::to_string(&envelope).unwrap();
    let decoded = decode_client_message(Message::Text(text.into())).unwrap();
    match decoded {
        Some(ClientMessage::Control(crate::transport::ClientControl::ClientToolResult {
            phase,
            result,
            speak,
            ..
        })) => {
            assert_eq!(phase, crate::transport::ClientToolPhase::Failed);
            assert!(result.is_none());
            assert!(speak);
        }
        other => panic!("expected ClientToolResult, got {other:?}"),
    }
}

#[test]
fn ping_returns_none() {
    let result = decode_client_message(Message::Ping(vec![].into())).unwrap();
    assert!(result.is_none());
}

#[test]
fn pong_returns_none() {
    let result = decode_client_message(Message::Pong(vec![].into())).unwrap();
    assert!(result.is_none());
}

/// An over-cap frame must be rejected with `MessageTooLarge`
/// before any decoding work happens.
#[test]
fn oversized_frames_rejected_with_message_too_large() {
    let big = vec![0u8; MAX_CLIENT_MESSAGE_BYTES + 1];
    let err = decode_client_message(Message::Binary(big.into())).unwrap_err();
    match err {
        TransportError::MessageTooLarge { size, max } => {
            assert_eq!(size, MAX_CLIENT_MESSAGE_BYTES + 1);
            assert_eq!(max, MAX_CLIENT_MESSAGE_BYTES);
        }
        other => panic!("expected MessageTooLarge, got {other:?}"),
    }

    let big_text = "x".repeat(MAX_CLIENT_MESSAGE_BYTES + 1);
    let err = decode_client_message(Message::Text(big_text.into())).unwrap_err();
    assert!(matches!(err, TransportError::MessageTooLarge { .. }));
}

#[test]
fn unknown_binary_type_error() {
    let header = BinaryHeader {
        msg_type: 0xFF,
        flags: 0,
        seq_num: 0,
    };
    let frame = header.encode_with_payload(&[]);
    let err = decode_client_message(Message::Binary(frame.into())).unwrap_err();
    assert!(err.to_string().contains("unknown binary message type"));
}

#[test]
fn unknown_json_type_error() {
    let json = serde_json::json!({
        "type": "bogus",
        "payload": {}
    });
    let text = serde_json::to_string(&json).unwrap();
    let err = decode_client_message(Message::Text(text.into())).unwrap_err();
    assert!(err.to_string().contains("unknown message type"));
}

#[test]
fn control_state_changed_uses_control_envelope() {
    let msg = ServerMessage::Control(ServerControl::StateChanged {
        state: PipelineState::Speaking,
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    match frame {
        Message::Text(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["type"], "control");
            assert_eq!(v["payload"]["action"], "state_changed");
            assert_eq!(v["payload"]["state"], "speaking");
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn control_session_ready_uses_control_envelope() {
    let info = SessionInfo {
        session_id: "s1".into(),
        session_token: "t1".into(),
        resumed: false,
        client_config: None,
        negotiated: NegotiatedConfig {
            audio_format: AudioFormat::PcmI16Le,
            input_sample_rate: 16000,
            output_sample_rate: 48000,
            channels: 1,
            language: None,
            voice: None,
            stt_provider: None,
            tts_provider: None,
            app_data: None,
            options: std::collections::HashMap::default(),
            mode: crate::transport::SessionMode::default(),
            protocol_version: crate::transport::PROTOCOL_VERSION,
            metric_descriptors: Vec::new(),
            stt_model: None,
            llm_model: None,
            tts_model: None,
            first_utterance_id: 1,
            text_utterance_id_base: 10_000,
            barge_in_enabled: None,
            turn_detection_mode: None,
            ignored_options: Vec::new(),
        },
    };
    let msg = ServerMessage::Control(ServerControl::SessionReady(info));
    let frame = encode_server_message(&msg, 0).unwrap();
    match frame {
        Message::Text(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["type"], "control");
            assert_eq!(v["payload"]["action"], "session_ready");
            assert_eq!(v["payload"]["negotiated"]["output_sample_rate"], 48000);
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn control_turn_metrics_encodes_action_and_fields() {
    use crate::pipeline::{TurnComponentMetrics, TurnMetricsSnapshot};
    let mut tts_extras = serde_json::Map::new();
    tts_extras.insert("ttfb_ms".into(), serde_json::json!(2000));
    let mut llm_extras = serde_json::Map::new();
    llm_extras.insert("ttft_ms".into(), serde_json::json!(150));
    let snapshot = TurnMetricsSnapshot {
        llm_response_text: None,
        utterance_id: 1,
        total_ms: 18500,
        interrupted: false,
        initiator: crate::pipeline::metrics::TurnInitiator::default(),
        components: vec![
            TurnComponentMetrics {
                component_id: "llm".into(),
                kind: "llm".into(),
                extras: llm_extras,
                ..Default::default()
            },
            TurnComponentMetrics {
                component_id: "tts".into(),
                kind: "tts".into(),
                extras: tts_extras,
                ..Default::default()
            },
        ],
    };
    let msg = ServerMessage::Control(ServerControl::TurnMetrics {
        snapshot: snapshot.clone(),
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    match frame {
        Message::Text(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["type"], "control");
            assert_eq!(v["payload"]["action"], "turn_metrics");
            assert_eq!(v["payload"]["snapshot"]["utterance_id"], 1);
            assert_eq!(v["payload"]["snapshot"]["total_ms"], 18500);
            assert_eq!(v["payload"]["snapshot"]["interrupted"], false);
            let comps = v["payload"]["snapshot"]["components"].as_array().unwrap();
            assert_eq!(comps.len(), 2);
            assert_eq!(comps[1]["kind"], "tts");
            assert_eq!(comps[1]["extras"]["ttfb_ms"], 2000);
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn control_error_encodes_all_fields() {
    use crate::transport::{ErrorCode, ErrorComponent, ErrorSeverity};
    let msg = ServerMessage::Control(ServerControl::Error {
        code: ErrorCode::LlmRetriesExhausted,
        message: "Language model failed: connection refused".into(),
        severity: ErrorSeverity::Error,
        turn_id: None,
        component: Some(ErrorComponent::Llm),
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    match frame {
        Message::Text(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["type"], "control");
            assert_eq!(v["payload"]["action"], "error");
            assert_eq!(v["payload"]["code"], 3000);
            assert_eq!(
                v["payload"]["message"],
                "Language model failed: connection refused"
            );
            assert_eq!(v["payload"]["severity"], "error");
            assert_eq!(v["payload"]["component"], "llm");
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn control_session_summary_encodes_action_and_fields() {
    let msg = ServerMessage::Control(ServerControl::SessionSummary {
        turns: 3,
        interrupted_turns: 1,
        avg_ttft_ms: Some(120),
        avg_ttfb_ms: Some(180),
        avg_total_ms: Some(950),
        total_audio_duration_ms: 4_500,
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    match frame {
        Message::Text(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["type"], "control");
            assert_eq!(v["payload"]["action"], "session_summary");
            assert_eq!(v["payload"]["turns"], 3);
            assert_eq!(v["payload"]["interrupted_turns"], 1);
            assert_eq!(v["payload"]["avg_ttft_ms"], 120);
            assert_eq!(v["payload"]["avg_ttfb_ms"], 180);
            assert_eq!(v["payload"]["avg_total_ms"], 950);
            assert_eq!(v["payload"]["total_audio_duration_ms"], 4_500);
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn control_session_summary_omits_none_avgs() {
    let msg = ServerMessage::Control(ServerControl::SessionSummary {
        turns: 0,
        interrupted_turns: 0,
        avg_ttft_ms: None,
        avg_ttfb_ms: None,
        avg_total_ms: None,
        total_audio_duration_ms: 0,
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    match frame {
        Message::Text(json) => {
            assert!(!json.contains("avg_ttft_ms"));
            assert!(!json.contains("avg_ttfb_ms"));
            assert!(!json.contains("avg_total_ms"));
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn control_tts_only_result_round_trip() {
    use crate::transport::{AudioRef, TtsOnlyMetrics};
    let msg = ServerMessage::Control(ServerControl::TtsOnlyResult {
        id: "short-plain-01".into(),
        metrics: TtsOnlyMetrics {
            ttfb_ms: Some(42),
            synthesis_ms: Some(180),
            audio_duration_ms: Some(1500),
            sample_rate: Some(22_050),
            chars: Some(38),
            rtf: Some(0.12),
            provider: Some("piper".into()),
            cold_start: true,
            error: None,
            voice: Some("en_US-amy-low".into()),
            peak_amplitude: Some(3300),
            speech_ratio: Some(0.4),
        },
        audio_ref: AudioRef {
            path: "captures/abc/tts-only/short-plain-01.wav".into(),
            format: "wav".into(),
            sample_rate: 22_050,
            channels: 1,
            duration_ms: 1500,
        },
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    let json = match frame {
        Message::Text(s) => s,
        other => panic!("expected Text, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["type"], "control");
    assert_eq!(v["payload"]["action"], "tts_only_result");
    assert_eq!(v["payload"]["id"], "short-plain-01");
    assert_eq!(v["payload"]["metrics"]["provider"], "piper");
    assert_eq!(v["payload"]["metrics"]["cold_start"], true);
    assert_eq!(v["payload"]["audio_ref"]["format"], "wav");
    assert_eq!(v["payload"]["audio_ref"]["sample_rate"], 22_050);
}

#[test]
fn control_tts_only_error_round_trip() {
    let msg = ServerMessage::Control(ServerControl::TtsOnlyError {
        id: "long-mixed-04".into(),
        error: "tts produced no audio".into(),
    });
    let frame = encode_server_message(&msg, 0).unwrap();
    let json = match frame {
        Message::Text(s) => s,
        other => panic!("expected Text, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["type"], "control");
    assert_eq!(v["payload"]["action"], "tts_only_error");
    assert_eq!(v["payload"]["id"], "long-mixed-04");
    assert_eq!(v["payload"]["error"], "tts produced no audio");
}

#[test]
fn pipeline_message_binary_base64_roundtrip() {
    let json = serde_json::json!({
        "type": "message",
        "payload": {
            "kind": "image",
            "binary": "AQID"
        }
    });
    let decoded = decode_client_message(Message::Text(json.to_string().into()))
        .expect("base64 binary field should decode")
        .expect("message should be Some");
    let ClientMessage::Message(pm) = decoded else {
        panic!("expected ClientMessage::Message");
    };
    assert_eq!(pm.kind, "image");
    assert_eq!(pm.binary.as_deref(), Some(&[1u8, 2, 3][..]));

    let encoded = encode_server_message(&ServerMessage::Message(pm), 0).unwrap();
    let Message::Text(out_text) = encoded else {
        panic!("expected text frame");
    };
    let out_json: serde_json::Value = serde_json::from_str(&out_text).unwrap();
    assert_eq!(out_json["payload"]["binary"], "AQID");
}

#[test]
fn pipeline_message_binary_rejects_invalid_base64() {
    let json = serde_json::json!({
        "type": "message",
        "payload": {
            "kind": "image",
            "binary": "not-valid-base64!!!"
        }
    });
    let err = decode_client_message(Message::Text(json.to_string().into())).unwrap_err();
    assert!(
        matches!(err, TransportError::Codec(_)),
        "expected Codec error, got {err:?}"
    );
}

#[test]
fn control_say_commit_clear_decode() {
    let say_json = serde_json::json!({
        "type": "control",
        "payload": { "action": "say", "text": "Welcome!", "interrupt": true }
    });
    match decode_client_message(Message::Text(say_json.to_string().into())).unwrap() {
        Some(ClientMessage::Control(ClientControl::Say { text, interrupt })) => {
            assert_eq!(text, "Welcome!");
            assert!(interrupt);
        }
        other => panic!("expected ClientControl::Say, got {other:?}"),
    }

    let commit_json = serde_json::json!({
        "type": "control",
        "payload": { "action": "commit_input" }
    });
    assert!(matches!(
        decode_client_message(Message::Text(commit_json.to_string().into())).unwrap(),
        Some(ClientMessage::Control(ClientControl::CommitInput))
    ));

    let clear_json = serde_json::json!({
        "type": "control",
        "payload": { "action": "clear_input" }
    });
    assert!(matches!(
        decode_client_message(Message::Text(clear_json.to_string().into())).unwrap(),
        Some(ClientMessage::Control(ClientControl::ClearInput))
    ));
}

/// Contract test: every `ClientControl` and `ServerControl` wire `action` in
/// Rust must be declared in `voice-pipes-admin/frontend/src/lib/transport/types.ts`
/// and vice versa, preventing silent protocol drift.
#[test]
fn rust_and_typescript_control_actions_match() {
    use crate::transport::{ErrorCode, ErrorSeverity, SessionConfig, SessionMode};
    use std::collections::BTreeSet;

    fn extract_ts_actions(ts: &str, type_name: &str) -> BTreeSet<String> {
        let header = format!("export type {type_name} =");
        let start = ts
            .find(&header)
            .unwrap_or_else(|| panic!("missing `{header}` in types.ts"));
        let rest = &ts[start + header.len()..];
        let end = rest
            .find(";\n")
            .or_else(|| rest.find(";\r\n"))
            .unwrap_or(rest.len());
        let block = &rest[..end];
        let mut actions = BTreeSet::new();
        for part in block.split("action: \"").skip(1) {
            if let Some(q) = part.find('"') {
                actions.insert(part[..q].to_string());
            }
        }
        actions
    }

    fn client_action(ctrl: &ClientControl) -> String {
        // Exhaustive match so adding a new ClientControl variant forces
        // updating this contract test.
        match ctrl {
            ClientControl::Cancel
            | ClientControl::Ping { .. }
            | ClientControl::Configure(_)
            | ClientControl::Resume { .. }
            | ClientControl::PlaybackReport { .. }
            | ClientControl::PlaybackComplete { .. }
            | ClientControl::PlaybackQuality { .. }
            | ClientControl::NetReport { .. }
            | ClientControl::TimingReport { .. }
            | ClientControl::InjectAudio { .. }
            | ClientControl::ClientToolResult { .. }
            | ClientControl::TtsOnlySynthesize { .. }
            | ClientControl::Say { .. }
            | ClientControl::CommitInput
            | ClientControl::ClearInput => {}
        }
        serde_json::to_value(ctrl).unwrap()["action"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn server_action(ctrl: &ServerControl) -> String {
        // Exhaustive match so adding a new ServerControl variant forces
        // updating this contract test.
        match ctrl {
            ServerControl::SessionReady(_)
            | ServerControl::Pong { .. }
            | ServerControl::StateChanged { .. }
            | ServerControl::QualityUpdate { .. }
            | ServerControl::Error { .. }
            | ServerControl::Closing { .. }
            | ServerControl::SessionSummary { .. }
            | ServerControl::TurnMetrics { .. }
            | ServerControl::ClearAudio { .. }
            | ServerControl::TextTruncated { .. }
            | ServerControl::UtteranceDropped { .. }
            | ServerControl::TtsOnlyResult { .. }
            | ServerControl::TtsOnlyError { .. } => {}
        }
        serde_json::to_value(ctrl).unwrap()["action"]
            .as_str()
            .unwrap()
            .to_string()
    }

    let rust_client: BTreeSet<String> = [
        ClientControl::Cancel,
        ClientControl::Ping { timestamp_ms: 0 },
        ClientControl::Configure(SessionConfig::default()),
        ClientControl::Resume {
            session_token: String::new(),
        },
        ClientControl::PlaybackReport {
            utterance_id: 1,
            last_played_offset_ms: 0,
            last_played_text: None,
        },
        ClientControl::PlaybackComplete { utterance_id: 1 },
        ClientControl::PlaybackQuality {
            utterance_id: 1,
            underrun_count: 0,
            total_silence_ms: 0,
            audio_ms: 0,
            heard_ms: 0,
            wall_ms: 0,
            arrival_wall_ms: 0,
            interrupted: false,
            annotation_late_count: 0,
            annotation_late_ms_max: 0,
            gaps: Vec::new(),
        },
        ClientControl::NetReport {
            playback_buffer_ms: 0,
            underruns: 0,
            audio_received_ms: 0,
            wall_ms: 0,
            uplink_backlog_bytes: 0,
        },
        ClientControl::TimingReport {
            utterance_id: 1,
            event: crate::transport::TimingEventKind::SpeechStart,
            client_timestamp_ms: 0,
            detail: None,
        },
        ClientControl::InjectAudio {
            source: String::new(),
            scenario_id: None,
            experiment_id: None,
            speed: 1.0,
        },
        ClientControl::ClientToolResult {
            call_id: String::new(),
            phase: crate::transport::ClientToolPhase::Completed,
            result: None,
            speak: false,
        },
        ClientControl::TtsOnlySynthesize {
            id: String::new(),
            text: String::new(),
            language: None,
            voice: None,
        },
        ClientControl::Say {
            text: String::new(),
            interrupt: false,
        },
        ClientControl::CommitInput,
        ClientControl::ClearInput,
    ]
    .iter()
    .map(client_action)
    .collect();

    let rust_server: BTreeSet<String> = [
        ServerControl::SessionReady(SessionInfo {
            session_id: String::new(),
            session_token: String::new(),
            resumed: false,
            client_config: None,
            negotiated: NegotiatedConfig {
                audio_format: AudioFormat::PcmI16Le,
                input_sample_rate: 16_000,
                output_sample_rate: 16_000,
                channels: 1,
                language: None,
                voice: None,
                stt_provider: None,
                tts_provider: None,
                app_data: None,
                options: std::collections::HashMap::default(),
                mode: SessionMode::Voice,
                protocol_version: 1,
                metric_descriptors: Vec::new(),
                stt_model: None,
                llm_model: None,
                tts_model: None,
                first_utterance_id: 1,
                text_utterance_id_base: 10_000,
                barge_in_enabled: None,
                turn_detection_mode: None,
                ignored_options: Vec::new(),
            },
        }),
        ServerControl::Pong {
            client_timestamp_ms: 0,
            server_timestamp_ms: 0,
        },
        ServerControl::StateChanged {
            state: PipelineState::Listening,
        },
        ServerControl::QualityUpdate {
            quality: crate::transport::ConnectionQuality::Good,
            tier: crate::transport::QualityTier::Full,
        },
        ServerControl::Error {
            code: ErrorCode::InvalidHandshake,
            message: String::new(),
            severity: ErrorSeverity::Error,
            turn_id: None,
            component: None,
        },
        ServerControl::Closing {
            reason: String::new(),
        },
        ServerControl::SessionSummary {
            turns: 0,
            interrupted_turns: 0,
            avg_ttft_ms: None,
            avg_ttfb_ms: None,
            avg_total_ms: None,
            total_audio_duration_ms: 0,
        },
        ServerControl::TurnMetrics {
            snapshot: crate::pipeline::metrics::TurnMetricsSnapshot::default(),
        },
        ServerControl::ClearAudio {
            utterance_id: 1,
            reason: String::new(),
        },
        ServerControl::TextTruncated {
            utterance_id: 1,
            final_text: String::new(),
        },
        ServerControl::UtteranceDropped {
            utterance_id: 1,
            reason: String::new(),
            audio_ms: 0,
        },
        ServerControl::TtsOnlyResult {
            id: String::new(),
            metrics: crate::transport::TtsOnlyMetrics {
                ttfb_ms: Some(0),
                synthesis_ms: Some(0),
                audio_duration_ms: Some(0),
                sample_rate: Some(16_000),
                chars: Some(0),
                rtf: Some(0.0),
                provider: Some(String::new()),
                cold_start: false,
                voice: None,
                error: None,
                peak_amplitude: None,
                speech_ratio: None,
            },
            audio_ref: crate::transport::AudioRef {
                path: String::new(),
                format: "wav".into(),
                sample_rate: 16_000,
                channels: 1,
                duration_ms: 0,
            },
        },
        ServerControl::TtsOnlyError {
            id: String::new(),
            error: String::new(),
        },
    ]
    .iter()
    .map(server_action)
    .collect();

    let ts_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../voice-pipes-admin/frontend/src/lib/transport/types.ts");
    let ts_src = std::fs::read_to_string(&ts_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", ts_path.display()));
    let ts_client = extract_ts_actions(&ts_src, "ClientControl");
    let ts_server = extract_ts_actions(&ts_src, "ServerControl");

    assert_eq!(
        rust_client, ts_client,
        "ClientControl wire actions drifted between Rust and TypeScript types.ts"
    );
    assert_eq!(
        rust_server, ts_server,
        "ServerControl wire actions drifted between Rust and TypeScript types.ts"
    );
}
