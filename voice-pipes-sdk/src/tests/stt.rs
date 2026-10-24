use super::*;

#[test]
fn transient_classification() {
    assert!(SttError::InferenceFailed("x".into()).is_transient());
    assert!(SttError::Timeout(Duration::from_secs(1)).is_transient());
    assert!(SttError::RateLimited { retry_after: None }.is_transient());
    assert!(SttError::NetworkError("x".into()).is_transient());
    assert!(
        SttError::ApiError {
            status: 503,
            message: "x".into()
        }
        .is_transient()
    );
    assert!(!SttError::ModelLoadFailed("x".into()).is_transient());
    assert!(!SttError::ConfigError("x".into()).is_transient());
    assert!(!SttError::InvalidAudio("x".into()).is_transient());
    assert!(!SttError::AuthError("x".into()).is_transient());
    assert!(!SttError::ChannelClosed.is_transient());
}

/// A request the service refused (a 4xx other than a timeout) fails the
/// same way again: retrying it is a loop, not a recovery. A timeout and a
/// server error may pass.
#[test]
fn a_refused_request_is_not_transient() {
    let api = |status| SttError::ApiError {
        status,
        message: "x".into(),
    };
    for status in [400, 404, 409, 422] {
        assert!(!api(status).is_transient(), "{status}");
    }
    for status in [408, 500, 503] {
        assert!(api(status).is_transient(), "{status}");
    }
}

#[test]
fn recognition_request_defaults() {
    let r = RecognitionRequest::default();
    assert_eq!(r.sample_rate, 16_000);
    assert_eq!(r.channels, 1);
    assert_eq!(r.encoding, AudioEncoding::PcmI16);
    assert!(r.punctuation);
    assert!(!r.interim_results);
}

/// A result built from `new` and the setters carries what was set and the
/// defaults for the rest, as a literal with `..Default::default()` did.
#[test]
fn a_result_is_built_from_new_and_setters() {
    let r = TranscriptionResult::new("hello", true)
        .with_confidence(Some(0.9))
        .with_segment_index(3)
        .with_inference_metrics(Some(InferenceMetrics::new().with_rtfx(Some(12.5))));
    assert_eq!(r.text, "hello");
    assert!(r.is_final);
    assert_eq!(r.confidence, Some(0.9));
    assert_eq!(r.segment_index, 3);
    assert_eq!(r.inference_metrics.and_then(|m| m.rtfx), Some(12.5));
    assert!(r.words.is_empty() && r.language.is_none() && !r.speech_started);
}

/// Measured metrics carry the duration, the passes and audio time over
/// wall time; no wall time is no `rtfx`, and a nonsense duration is 0 ms.
#[test]
fn measured_metrics_carry_duration_passes_and_rtfx() {
    let m = InferenceMetrics::measured(1.5, Duration::from_millis(300), 4);
    assert_eq!(m.audio_duration_ms, Some(1_500));
    assert_eq!(m.inference_count, Some(4));
    assert_eq!(m.rtfx, Some(5.0));
    assert_eq!(m.algorithmic_delay_ms, None);
    assert_eq!(m.effective_latency_ms, None);
    assert_eq!(m.streaming_window_ms, None);

    let idle = InferenceMetrics::measured(1.0, Duration::ZERO, 1);
    assert_eq!(idle.rtfx, None);
    assert_eq!(
        InferenceMetrics::measured(-1.0, Duration::ZERO, 0).audio_duration_ms,
        Some(0)
    );
    assert_eq!(
        InferenceMetrics::measured(f64::NAN, Duration::ZERO, 0).audio_duration_ms,
        Some(0)
    );
}

/// A delay floor sets the algorithmic delay and, with a positive `rtfx`,
/// the effective latency `delay × (1 + 1/RTFx)`.
#[test]
fn a_delay_floor_implies_the_effective_latency() {
    let m = InferenceMetrics::measured(4.0, Duration::from_secs(1), 10).with_delay_floor_ms(560);
    assert_eq!(m.algorithmic_delay_ms, Some(560));
    assert_eq!(m.effective_latency_ms, Some(700));

    let unmeasured = InferenceMetrics::measured(4.0, Duration::ZERO, 1).with_delay_floor_ms(560);
    assert_eq!(unmeasured.algorithmic_delay_ms, Some(560));
    assert_eq!(unmeasured.effective_latency_ms, None);
}

/// A fragment from `new` is mid-response with nothing else set.
#[test]
fn a_fragment_is_built_from_new_and_setters() {
    let f = crate::text::TextFragment::new("Hi.");
    assert_eq!(f.text, "Hi.");
    assert!(!f.is_end_of_response && f.token_count.is_none() && f.first_token_ms.is_none());
    let last = crate::text::TextFragment::new("")
        .with_is_end_of_response(true)
        .with_token_count(Some(7));
    assert!(last.is_end_of_response);
    assert_eq!(last.token_count, Some(7));
}

/// A stream started to replace one the engine gave up on may wait for the
/// slot the replaced stream's last run still holds; by default a request
/// leaves the wait to the provider.
#[test]
fn a_request_can_ask_to_wait_for_a_slot() {
    assert!(RecognitionRequest::default().slot_wait.is_none());
    let r = RecognitionRequest::default().with_slot_wait(Some(Duration::from_secs(60)));
    assert_eq!(r.slot_wait, Some(Duration::from_secs(60)));
}
