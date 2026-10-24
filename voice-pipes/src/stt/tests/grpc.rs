use super::*;

fn make_proto_result(with_metrics: bool, speech_started: bool) -> proto::TranscriptionResult {
    // Access the internal proto module via the path used in grpc.rs.
    // We reconstruct the struct manually since there's no public builder.
    proto::TranscriptionResult {
        text: "test transcript".into(),
        is_final: true,
        confidence: Some(0.90),
        language: Some("en-US".into()),
        segment_index: 1,
        start_time_ms: Some(100),
        end_time_ms: Some(1600),
        words: vec![proto::WordInfo {
            word: "test".into(),
            start_ms: 100,
            end_ms: 500,
            confidence: Some(0.92),
        }],
        is_end_of_turn: Some(false),
        speaker_id: Some(0),
        speech_started,
        inference_metrics: with_metrics.then_some(proto::InferenceMetrics {
            audio_duration_ms: Some(1500),
            inference_count: Some(5),
            rtfx: Some(8.4),
            algorithmic_delay_ms: Some(560),
            effective_latency_ms: Some(627),
            streaming_window_ms: Some(560),
        }),
    }
}

#[test]
fn proto_to_transcription_without_metrics() {
    let proto = make_proto_result(false, false);
    let domain = proto_to_transcription(proto);

    assert_eq!(domain.text, "test transcript");
    assert!(domain.is_final);
    assert_eq!(domain.confidence, Some(0.90));
    assert_eq!(domain.language.as_deref(), Some("en-US"));
    assert_eq!(domain.segment_index, 1);
    assert_eq!(
        domain.start_time,
        Some(std::time::Duration::from_millis(100))
    );
    assert_eq!(
        domain.end_time,
        Some(std::time::Duration::from_millis(1600))
    );
    assert_eq!(domain.words.len(), 1);
    assert_eq!(domain.words[0].word, "test");
    assert_eq!(domain.is_end_of_turn, Some(false));
    assert_eq!(domain.speaker_id, Some(0));
    assert!(!domain.speech_started);
    // Old server that doesn't populate inference_metrics → None.
    assert!(domain.inference_metrics.is_none());
}

#[test]
fn proto_to_transcription_with_metrics() {
    let proto = make_proto_result(true, true);
    let domain = proto_to_transcription(proto);

    assert!(domain.speech_started);
    let m = domain.inference_metrics.expect("metrics should be present");
    assert_eq!(m.audio_duration_ms, Some(1500));
    assert_eq!(m.inference_count, Some(5));
    assert!((m.rtfx.unwrap() - 8.4).abs() < 0.01);
    assert_eq!(m.algorithmic_delay_ms, Some(560));
    assert_eq!(m.effective_latency_ms, Some(627));
    assert_eq!(m.streaming_window_ms, Some(560));
}

#[test]
fn backward_compat_no_metrics_field() {
    // Simulates an old gRPC server that doesn't send inference_metrics.
    // proto3 optional fields default to None when absent.
    let mut proto = make_proto_result(false, false);
    proto.inference_metrics = None; // explicit absence
    let domain = proto_to_transcription(proto);
    assert!(
        domain.inference_metrics.is_none(),
        "absent inference_metrics must not panic and must map to None"
    );
}
