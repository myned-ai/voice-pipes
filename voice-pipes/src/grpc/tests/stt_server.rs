use super::*;
use crate::stt::{InferenceMetrics, WordInfo};
use std::time::Duration;

fn base_result() -> TranscriptionResult {
    TranscriptionResult::new("hello world", true)
        .with_confidence(Some(0.95))
        .with_language(Some("en-US".into()))
        .with_start_time(Some(Duration::from_millis(0)))
        .with_end_time(Some(Duration::from_millis(1500)))
        .with_words(vec![
            WordInfo::new(
                "hello",
                Duration::from_millis(100),
                Duration::from_millis(400),
            )
            .with_confidence(Some(0.98)),
        ])
        .with_is_end_of_turn(Some(true))
        .with_speech_started(true)
}

#[test]
fn transcription_to_proto_without_metrics() {
    let r = base_result();
    let p = transcription_to_proto(&r);

    assert_eq!(p.text, "hello world");
    assert!(p.is_final);
    assert_eq!(p.confidence, Some(0.95));
    assert_eq!(p.language.as_deref(), Some("en-US"));
    assert_eq!(p.start_time_ms, Some(0));
    assert_eq!(p.end_time_ms, Some(1500));
    assert_eq!(p.words.len(), 1);
    assert_eq!(p.words[0].word, "hello");
    assert_eq!(p.words[0].start_ms, 100);
    assert_eq!(p.words[0].end_ms, 400);
    assert_eq!(p.words[0].confidence, Some(0.98));
    assert_eq!(p.is_end_of_turn, Some(true));
    assert!(p.speech_started);
    assert!(p.inference_metrics.is_none());
}

#[test]
fn transcription_to_proto_with_metrics() {
    let mut r = base_result();
    r.inference_metrics = Some(
        InferenceMetrics::new()
            .with_audio_duration_ms(Some(1500))
            .with_inference_count(Some(7))
            .with_rtfx(Some(12.3))
            .with_algorithmic_delay_ms(Some(560))
            .with_effective_latency_ms(Some(605))
            .with_streaming_window_ms(Some(560)),
    );

    let p = transcription_to_proto(&r);
    let m = p.inference_metrics.expect("metrics should be set");

    assert_eq!(m.audio_duration_ms, Some(1500));
    assert_eq!(m.inference_count, Some(7));
    assert!((m.rtfx.unwrap() - 12.3).abs() < 0.01);
    assert_eq!(m.algorithmic_delay_ms, Some(560));
    assert_eq!(m.effective_latency_ms, Some(605));
    assert_eq!(m.streaming_window_ms, Some(560));
}

#[test]
fn transcription_to_proto_partial_metrics() {
    // Providers that only report RTFx (e.g. batch-mode) leave the
    // streaming-specific fields as None.
    let mut r = base_result();
    r.inference_metrics = Some(
        InferenceMetrics::new()
            .with_audio_duration_ms(Some(2000))
            .with_inference_count(Some(1))
            .with_rtfx(Some(4.0)),
    );

    let p = transcription_to_proto(&r);
    let m = p.inference_metrics.expect("metrics should be set");

    assert_eq!(m.audio_duration_ms, Some(2000));
    assert_eq!(m.inference_count, Some(1));
    assert!(m.algorithmic_delay_ms.is_none());
    assert!(m.effective_latency_ms.is_none());
    assert!(m.streaming_window_ms.is_none());
}

/// A request that leaves `sample_rate` out (proto3 sends 0) is refused at
/// the boundary, before a recogniser is asked to resample from 0 Hz.
#[test]
fn a_request_without_a_sample_rate_is_refused() {
    let req = |rate| proto::TranscribeRequest {
        samples_i16: vec![0; 320],
        sample_rate: rate,
        channels: 1,
    };
    for rate in [0, 1, 999_999] {
        assert_eq!(proto_to_audio_frame(&req(rate)).err(), Some(rate));
    }
    let frame = proto_to_audio_frame(&req(16_000)).unwrap();
    assert_eq!((frame.sample_rate, frame.samples.len()), (16_000, 160));
}
