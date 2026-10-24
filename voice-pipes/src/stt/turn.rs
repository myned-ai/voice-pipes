//! Turn detection -- decides when a user has finished speaking.

use async_trait::async_trait;
use std::time::Duration;

use super::TranscriptionResult;

// The contract (signal, detector, config and factory) is the SDK's; the
// rule-based detectors every pipeline needs live here.
pub use voice_pipes_sdk::turn::{
    TurnConfig, TurnDecision, TurnDetector, TurnDetectorFactory, TurnSignal,
};

// ── SilenceTimeoutDetector ──────────────────────────────────

/// Baseline detector: commits the turn when silence exceeds a timeout.
/// This replicates the pre-existing behaviour where `is_final` +
/// silence gap triggers the utterance boundary.
#[derive(Debug)]
pub struct SilenceTimeoutDetector {
    pub timeout: Duration,
}

impl SilenceTimeoutDetector {
    /// Create a detector that fires after `timeout_ms` of silence.
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

impl Default for SilenceTimeoutDetector {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[async_trait]
impl TurnDetector for SilenceTimeoutDetector {
    async fn evaluate(
        &self,
        _result: &TranscriptionResult,
        silence_duration: Duration,
    ) -> TurnSignal {
        if silence_duration >= self.timeout {
            TurnSignal::forced_complete()
        } else {
            // The timeout is a deadline, not a prediction, but it is a real
            // upper bound on how long this turn can still run -- worth
            // reporting so a caller has some lead time rather than none.
            TurnSignal::incomplete().ending_in(self.timeout.saturating_sub(silence_duration))
        }
    }

    fn name(&self) -> &'static str {
        "silence-timeout"
    }
}

// ── ProviderSignalDetector ──────────────────────────────────

/// Uses the `is_end_of_turn` flag from STT providers that support
/// semantic endpointing (e.g. `AssemblyAI`, Deepgram).  Falls back to
/// silence timeout when the provider doesn't signal.
#[derive(Debug)]
pub struct ProviderSignalDetector {
    pub fallback_timeout: Duration,
}

impl ProviderSignalDetector {
    /// Create a detector with a fallback silence timeout in milliseconds.
    pub fn new(fallback_timeout_ms: u64) -> Self {
        Self {
            fallback_timeout: Duration::from_millis(fallback_timeout_ms),
        }
    }
}

impl Default for ProviderSignalDetector {
    fn default() -> Self {
        Self::new(2000)
    }
}

#[async_trait]
impl TurnDetector for ProviderSignalDetector {
    async fn evaluate(
        &self,
        result: &TranscriptionResult,
        silence_duration: Duration,
    ) -> TurnSignal {
        if result.is_end_of_turn == Some(true) {
            TurnSignal::likely_complete()
        } else if silence_duration >= self.fallback_timeout {
            TurnSignal::forced_complete()
        } else {
            TurnSignal::incomplete()
                .ending_in(self.fallback_timeout.saturating_sub(silence_duration))
        }
    }

    fn name(&self) -> &'static str {
        "provider-signal"
    }
}

// ── LayeredTurnDetector ─────────────────────────────────────

/// Composes multiple detectors in priority order.  The first detector
/// that returns a complete signal wins.
pub struct LayeredTurnDetector {
    detectors: Vec<Box<dyn TurnDetector>>,
}

impl std::fmt::Debug for LayeredTurnDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayeredTurnDetector")
            .field("detector_count", &self.detectors.len())
            .finish_non_exhaustive()
    }
}

impl LayeredTurnDetector {
    /// Compose multiple detectors -- first definitive signal wins.
    pub fn new(detectors: Vec<Box<dyn TurnDetector>>) -> Self {
        Self { detectors }
    }
}

#[async_trait]
impl TurnDetector for LayeredTurnDetector {
    async fn evaluate(
        &self,
        result: &TranscriptionResult,
        silence_duration: Duration,
    ) -> TurnSignal {
        // No layer said "complete", but their incomplete signals still carry
        // the sharpest view anyone has of how this turn is going: keep the
        // highest probability and the soonest predicted end, so a caller that
        // speculates is not punished for composing detectors.
        let mut pending = TurnSignal::incomplete();
        for det in &self.detectors {
            let signal = det.evaluate(result, silence_duration).await;
            if signal.is_complete() {
                return signal;
            }
            pending.confidence = match (pending.confidence, signal.confidence) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            pending.expected_end_in = match (pending.expected_end_in, signal.expected_end_in) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        pending
    }

    fn feed_audio(&self, samples: &[i16], sample_rate: u32) {
        for det in &self.detectors {
            det.feed_audio(samples, sample_rate);
        }
    }

    fn feed_agent_audio(&self, samples: &[i16], sample_rate: u32) {
        for det in &self.detectors {
            det.feed_agent_audio(samples, sample_rate);
        }
    }

    fn turn_ended(&self) {
        for det in &self.detectors {
            det.turn_ended();
        }
    }

    fn name(&self) -> &'static str {
        "layered"
    }
}

// ── Factories ────────────────────────────────────────────────

/// Factory for the silence-timeout turn detector.
#[derive(Debug)]
pub struct SilenceTimeoutFactory;

impl TurnDetectorFactory for SilenceTimeoutFactory {
    fn name(&self) -> &'static str {
        "silence"
    }

    fn validate(&self, cfg: &TurnConfig) -> Vec<crate::config::ConfigError> {
        cfg.unknown_options(&[])
    }

    fn build(
        &self,
        cfg: &TurnConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        Ok(Box::new(SilenceTimeoutDetector::new(
            cfg.silence_timeout_ms,
        )))
    }
}

/// Factory for the provider-signal turn detector.
#[derive(Debug)]
pub struct ProviderSignalFactory;

impl TurnDetectorFactory for ProviderSignalFactory {
    fn name(&self) -> &'static str {
        "provider"
    }

    fn validate(&self, cfg: &TurnConfig) -> Vec<crate::config::ConfigError> {
        cfg.unknown_options(&[])
    }

    fn build(
        &self,
        cfg: &TurnConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        Ok(Box::new(ProviderSignalDetector::new(
            cfg.silence_timeout_ms,
        )))
    }
}

/// Factory for the layered (provider -> silence fallback) turn detector.
#[derive(Debug)]
pub struct LayeredTurnFactory;

impl TurnDetectorFactory for LayeredTurnFactory {
    fn name(&self) -> &'static str {
        "layered"
    }

    fn validate(&self, cfg: &TurnConfig) -> Vec<crate::config::ConfigError> {
        cfg.unknown_options(&[])
    }

    fn build(
        &self,
        cfg: &TurnConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let detectors: Vec<Box<dyn TurnDetector>> = vec![
            Box::new(ProviderSignalDetector::new(cfg.silence_timeout_ms)),
            Box::new(SilenceTimeoutDetector::new(cfg.silence_timeout_ms)),
        ];
        Ok(Box::new(LayeredTurnDetector::new(detectors)))
    }
}

type Ctor = fn() -> Box<dyn TurnDetectorFactory>;

pub(crate) static TURN_FACTORIES: crate::plugins::Registry<dyn TurnDetectorFactory> =
    crate::plugins::Registry::new("turn detection", builtin_turn_factories);

/// Register a turn detector from outside the engine, so `[turn_detection]
/// mode` can select it by name. Call once at startup, before any session
/// starts. Thread-safe.
///
/// # Errors
/// When the factory's name or one of its aliases already names a built-in
/// or an earlier plugin: one detector would shadow the other.
pub fn register_turn_factory(ctor: Ctor) -> Result<(), crate::plugins::RegistrationError> {
    TURN_FACTORIES.register(ctor)
}

/// The rule-based detectors every build has.
fn builtin_turn_factories() -> Vec<Box<dyn TurnDetectorFactory>> {
    vec![
        Box::new(SilenceTimeoutFactory),
        Box::new(ProviderSignalFactory),
        Box::new(LayeredTurnFactory),
    ]
}

/// Every turn detector factory: built in, then registered.
pub fn turn_detector_factories() -> Vec<std::sync::Arc<dyn TurnDetectorFactory>> {
    TURN_FACTORIES.all()
}

/// The turn detector factory for `mode`, by name or alias.
pub fn lookup_turn_factory(mode: &str) -> Option<std::sync::Arc<dyn TurnDetectorFactory>> {
    TURN_FACTORIES.lookup(mode)
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/turn.rs"]
mod tests;
