use super::*;
use std::sync::Arc;

fn dummy_result() -> TranscriptionResult {
    TranscriptionResult::new("hello world", true).with_confidence(Some(0.95))
}

// ── SilenceTimeoutDetector ──

#[tokio::test]
async fn silence_timeout_incomplete_when_short() {
    let det = SilenceTimeoutDetector::new(1000);
    let sig = det
        .evaluate(&dummy_result(), Duration::from_millis(500))
        .await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
}

#[tokio::test]
async fn silence_timeout_forced_when_exceeded() {
    let det = SilenceTimeoutDetector::new(1000);
    let sig = det
        .evaluate(&dummy_result(), Duration::from_millis(1500))
        .await;
    assert_eq!(sig.decision, TurnDecision::ForcedComplete);
}

#[tokio::test]
async fn silence_timeout_exact_boundary() {
    let det = SilenceTimeoutDetector::new(1000);
    let sig = det.evaluate(&dummy_result(), Duration::from_secs(1)).await;
    assert_eq!(sig.decision, TurnDecision::ForcedComplete);
}

#[tokio::test]
async fn silence_timeout_zero_silence() {
    let det = SilenceTimeoutDetector::new(1000);
    let sig = det.evaluate(&dummy_result(), Duration::ZERO).await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
}

// ── ProviderSignalDetector ──

#[tokio::test]
async fn provider_signal_end_of_turn() {
    let det = ProviderSignalDetector::new(2000);
    let mut result = dummy_result();
    result.is_end_of_turn = Some(true);
    let sig = det.evaluate(&result, Duration::ZERO).await;
    assert_eq!(sig.decision, TurnDecision::LikelyComplete);
}

#[tokio::test]
async fn provider_signal_no_signal_short_silence() {
    let det = ProviderSignalDetector::new(2000);
    let sig = det
        .evaluate(&dummy_result(), Duration::from_millis(500))
        .await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
}

#[tokio::test]
async fn provider_signal_no_signal_timeout_fallback() {
    let det = ProviderSignalDetector::new(2000);
    let sig = det.evaluate(&dummy_result(), Duration::from_secs(3)).await;
    assert_eq!(sig.decision, TurnDecision::ForcedComplete);
}

#[tokio::test]
async fn provider_signal_explicit_false() {
    let det = ProviderSignalDetector::new(2000);
    let mut result = dummy_result();
    result.is_end_of_turn = Some(false);
    let sig = det.evaluate(&result, Duration::from_millis(500)).await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
}

// ── LayeredTurnDetector ──

#[tokio::test]
async fn layered_provider_wins_over_silence() {
    // Provider signals complete immediately, silence would not trigger yet
    let mut result = dummy_result();
    result.is_end_of_turn = Some(true);

    let layered = LayeredTurnDetector::new(vec![
        Box::new(ProviderSignalDetector::new(5000)),
        Box::new(SilenceTimeoutDetector::new(1000)),
    ]);

    let sig = layered.evaluate(&result, Duration::from_millis(200)).await;
    assert_eq!(sig.decision, TurnDecision::LikelyComplete);
}

#[tokio::test]
async fn layered_silence_catches_when_provider_doesnt() {
    let layered = LayeredTurnDetector::new(vec![
        Box::new(ProviderSignalDetector::new(5000)),
        Box::new(SilenceTimeoutDetector::new(1000)),
    ]);

    // No provider signal, silence exceeded
    let sig = layered
        .evaluate(&dummy_result(), Duration::from_millis(1500))
        .await;
    assert_eq!(sig.decision, TurnDecision::ForcedComplete);
}

#[tokio::test]
async fn layered_all_incomplete() {
    let layered = LayeredTurnDetector::new(vec![
        Box::new(ProviderSignalDetector::new(5000)),
        Box::new(SilenceTimeoutDetector::new(3000)),
    ]);

    let sig = layered
        .evaluate(&dummy_result(), Duration::from_millis(500))
        .await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
}

#[tokio::test]
async fn layered_empty_returns_incomplete() {
    let layered = LayeredTurnDetector::new(vec![]);
    let sig = layered
        .evaluate(&dummy_result(), Duration::from_secs(999))
        .await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
}

// ── TurnSignal helpers ──

#[test]
fn turn_signal_is_complete() {
    assert!(!TurnSignal::incomplete().is_complete());
    assert!(TurnSignal::likely_complete().is_complete());
    assert!(TurnSignal::forced_complete().is_complete());
}

#[test]
fn rule_based_signals_carry_no_confidence() {
    // A rule has no probability to report, and must not imply one: a caller
    // that speculates on `confidence` should see "no opinion", not zero.
    assert_eq!(TurnSignal::incomplete().confidence, None);
    assert_eq!(TurnSignal::likely_complete().confidence, None);
    assert_eq!(TurnSignal::forced_complete().confidence, None);
}

#[test]
fn builders_attach_prediction_without_changing_decision() {
    let sig = TurnSignal::incomplete()
        .with_confidence(0.42)
        .ending_in(Duration::from_millis(300));
    assert_eq!(sig.decision, TurnDecision::Incomplete);
    assert!(!sig.is_complete());
    assert_eq!(sig.confidence, Some(0.42));
    assert_eq!(sig.expected_end_in, Some(Duration::from_millis(300)));
}

// ── Prediction fields ──

#[tokio::test]
async fn silence_timeout_reports_remaining_lead_time() {
    let det = SilenceTimeoutDetector::new(1000);
    let sig = det
        .evaluate(&dummy_result(), Duration::from_millis(400))
        .await;
    assert_eq!(sig.expected_end_in, Some(Duration::from_millis(600)));
}

#[tokio::test]
async fn provider_signal_reports_remaining_lead_time() {
    let det = ProviderSignalDetector::new(2000);
    let sig = det
        .evaluate(&dummy_result(), Duration::from_millis(500))
        .await;
    assert_eq!(sig.expected_end_in, Some(Duration::from_millis(1500)));
}

#[tokio::test]
async fn layered_keeps_soonest_predicted_end_across_layers() {
    // Provider falls back at 5 s, silence at 3 s; with 500 ms elapsed the
    // soonest either layer expects the turn to end is 2.5 s away.
    let layered = LayeredTurnDetector::new(vec![
        Box::new(ProviderSignalDetector::new(5000)),
        Box::new(SilenceTimeoutDetector::new(3000)),
    ]);

    let sig = layered
        .evaluate(&dummy_result(), Duration::from_millis(500))
        .await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
    assert_eq!(sig.expected_end_in, Some(Duration::from_millis(2500)));
}

/// Stands in for a model-based detector: reports a probability without ever
/// claiming the turn is over, so the layered detector's propagation can be
/// tested without a model export.
struct ConfidenceStub(f32);

#[async_trait]
impl TurnDetector for ConfidenceStub {
    async fn evaluate(
        &self,
        _result: &TranscriptionResult,
        _silence_duration: Duration,
    ) -> TurnSignal {
        TurnSignal::incomplete().with_confidence(self.0)
    }

    fn name(&self) -> &'static str {
        "confidence-stub"
    }
}

/// Records which channel each audio feed arrived on.
#[derive(Default)]
struct ChannelSpy {
    user: parking_lot::Mutex<Vec<i16>>,
    agent: parking_lot::Mutex<Vec<i16>>,
}

/// A detector that records into a shared [`ChannelSpy`].
struct SpyLayer(Arc<ChannelSpy>);

#[async_trait]
impl TurnDetector for SpyLayer {
    async fn evaluate(
        &self,
        _result: &TranscriptionResult,
        _silence_duration: Duration,
    ) -> TurnSignal {
        TurnSignal::incomplete()
    }

    fn feed_audio(&self, samples: &[i16], _sample_rate: u32) {
        self.0.user.lock().extend_from_slice(samples);
    }

    fn feed_agent_audio(&self, samples: &[i16], _sample_rate: u32) {
        self.0.agent.lock().extend_from_slice(samples);
    }

    fn name(&self) -> &'static str {
        "channel-spy"
    }
}

#[test]
fn agent_audio_defaults_to_a_no_op() {
    // The rule-based detectors must ignore the agent channel rather than
    // fail to compile or panic when a runner tees it to them.
    let det = SilenceTimeoutDetector::new(1000);
    det.feed_agent_audio(&[1, 2, 3], 16_000);
}

#[tokio::test]
async fn layered_forwards_each_channel_to_every_layer() {
    let a = Arc::new(ChannelSpy::default());
    let b = Arc::new(ChannelSpy::default());
    let layered = LayeredTurnDetector::new(vec![
        Box::new(SpyLayer(Arc::clone(&a))),
        Box::new(SpyLayer(Arc::clone(&b))),
    ]);

    layered.feed_audio(&[1, 2], 16_000);
    layered.feed_agent_audio(&[7, 8, 9], 16_000);

    for spy in [&a, &b] {
        assert_eq!(&*spy.user.lock(), &[1, 2]);
        assert_eq!(&*spy.agent.lock(), &[7, 8, 9]);
    }
}

#[tokio::test]
async fn layered_keeps_highest_confidence_across_layers() {
    let layered = LayeredTurnDetector::new(vec![
        Box::new(ConfidenceStub(0.2)),
        Box::new(ConfidenceStub(0.7)),
        Box::new(ConfidenceStub(0.4)),
    ]);

    let sig = layered.evaluate(&dummy_result(), Duration::ZERO).await;
    assert_eq!(sig.decision, TurnDecision::Incomplete);
    assert_eq!(sig.confidence, Some(0.7));
}

#[tokio::test]
async fn layered_confidence_survives_a_layer_with_no_opinion() {
    // A rule-based layer reporting `None` must not erase a model layer's
    // probability -- absence of an opinion is not an opinion of zero.
    let layered = LayeredTurnDetector::new(vec![
        Box::new(SilenceTimeoutDetector::new(5000)),
        Box::new(ConfidenceStub(0.6)),
    ]);

    let sig = layered
        .evaluate(&dummy_result(), Duration::from_millis(100))
        .await;
    assert_eq!(sig.confidence, Some(0.6));
}

#[tokio::test]
async fn layered_empty_has_no_prediction() {
    let layered = LayeredTurnDetector::new(vec![]);
    let sig = layered
        .evaluate(&dummy_result(), Duration::from_secs(999))
        .await;
    assert_eq!(sig.confidence, None);
    assert_eq!(sig.expected_end_in, None);
}

// ── Default constructors ──

#[test]
fn default_silence_timeout() {
    let det = SilenceTimeoutDetector::default();
    assert_eq!(det.timeout, Duration::from_secs(1));
}

#[test]
fn default_provider_signal() {
    let det = ProviderSignalDetector::default();
    assert_eq!(det.fallback_timeout, Duration::from_secs(2));
}

// ── Factory registry tests ──────────────────────────────

#[test]
fn turn_factory_names_are_unique() {
    let factories = turn_detector_factories();
    let names: Vec<&str> = factories.iter().map(|f| f.name()).collect();
    let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate turn factory names: {names:?}"
    );
}

#[test]
fn turn_factory_includes_expected() {
    let factories = turn_detector_factories();
    let names: Vec<&str> = factories.iter().map(|f| f.name()).collect();
    for expected in &["silence", "provider", "layered"] {
        assert!(names.contains(expected), "missing factory: {expected}");
    }
}
