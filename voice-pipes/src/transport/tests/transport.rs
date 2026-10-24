use super::*;

/// The `net_report` frame (S3) decodes from its documented wire shape,
/// and every field defaults so a client can send only what it knows.
#[test]
fn net_report_frame_decodes_and_fields_default() {
    let wire = r#"{
        "action": "net_report",
        "playback_buffer_ms": 180,
        "underruns": 2,
        "audio_received_ms": 1000,
        "wall_ms": 1400,
        "uplink_backlog_bytes": 65536
    }"#;
    match serde_json::from_str::<ClientControl>(wire).expect("deserialize") {
        ClientControl::NetReport {
            playback_buffer_ms,
            underruns,
            audio_received_ms,
            wall_ms,
            uplink_backlog_bytes,
        } => {
            assert_eq!(playback_buffer_ms, 180);
            assert_eq!(underruns, 2);
            // wall > received means audio arrived slower than it plays.
            assert_eq!(wall_ms, 1400);
            assert_eq!(audio_received_ms, 1000);
            assert_eq!(uplink_backlog_bytes, 65536);
        }
        other => panic!("expected NetReport, got {other:?}"),
    }

    // A minimal client sends the action alone.
    match serde_json::from_str::<ClientControl>(r#"{"action":"net_report"}"#)
        .expect("bare net_report must decode")
    {
        ClientControl::NetReport {
            underruns, wall_ms, ..
        } => {
            assert_eq!(underruns, 0);
            assert_eq!(wall_ms, 0);
        }
        other => panic!("expected NetReport, got {other:?}"),
    }
}

/// The `playback_quality` frame deserializes from the documented
/// `snake_case` wire shape and round-trips.
#[test]
fn playback_quality_frame_roundtrip() {
    let wire = r#"{
        "action": "playback_quality",
        "utterance_id": 42,
        "underrun_count": 33, "total_silence_ms": 9040,
        "audio_ms": 16120, "heard_ms": 16120, "wall_ms": 25160,
        "gaps": [ { "at_audio_ms": 480, "silence_ms": 288 } ]
    }"#;
    let frame: ClientControl = serde_json::from_str(wire).expect("deserialize");
    let ClientControl::PlaybackQuality {
        utterance_id,
        underrun_count,
        total_silence_ms,
        audio_ms,
        heard_ms,
        wall_ms,
        ref gaps,
        ..
    } = frame
    else {
        panic!("wrong variant: {frame:?}");
    };
    assert_eq!(
        (
            utterance_id,
            underrun_count,
            total_silence_ms,
            audio_ms,
            heard_ms,
            wall_ms
        ),
        (42, 33, 9040, 16120, 16120, 25160)
    );
    assert_eq!(
        gaps,
        &[PlaybackGap {
            at_audio_ms: 480,
            silence_ms: 288
        }]
    );

    let json = serde_json::to_value(&frame).unwrap();
    assert_eq!(json["action"], "playback_quality");
    assert_eq!(json["gaps"][0]["at_audio_ms"], 480);
}
