//! Turn detection: deciding when the user has finished speaking.
//!
//! A [`TurnDetector`] is asked after every recognition result whether the
//! user's turn is over, and may also be fed the microphone and the agent's
//! own audio. The engine has the rule-based detectors every pipeline needs
//! (silence timeout, the recogniser's end-of-turn signal, both layered);
//! model-based ones are plugins, registered with
//! `voice_pipes::stt::turn::register_turn_factory` and selected by
//! `[turn_detection] mode`.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::Duration;

use async_trait::async_trait;

use crate::build::BuildContext;
use crate::config::ConfigError;
use crate::schema::ParamDescriptor;
use crate::stt::TranscriptionResult;

// ── Signal ──────────────────────────────────────────────────

/// What a detector concluded about the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnDecision {
    /// User is still speaking or thinking -- do not commit yet.
    Incomplete,
    /// High confidence the turn is done (provider signal or model).
    LikelyComplete,
    /// Silence timeout exceeded -- forced commit.
    ForcedComplete,
}

impl TurnDecision {
    /// Returns `true` for any complete variant.
    pub fn is_complete(self) -> bool {
        matches!(self, Self::LikelyComplete | Self::ForcedComplete)
    }
}

/// The outcome of evaluating whether the user's turn is complete.
///
/// The decision is the part the orchestrator acts on: commit the utterance or
/// keep listening. The other two fields are what a model-based detector knows
/// but a rule cannot, and they exist so downstream stages can start work
/// *before* the turn ends rather than after:
///
/// * `confidence` lets a caller act on a rising probability — arm speculative
///   work at a low threshold while still committing only at a high one.
/// * `expected_end_in` is the forecasting half: how much lead time is left.
///
/// Both are `None` for detectors that only apply a rule, so a caller must
/// treat their absence as "no opinion", never as zero.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct TurnSignal {
    /// Commit or keep listening.
    pub decision: TurnDecision,
    /// P(the turn is complete), in `0.0..=1.0`.
    pub confidence: Option<f32>,
    /// How long until the turn is expected to end, for detectors that predict
    /// rather than react. `Some(Duration::ZERO)` means "ending now".
    pub expected_end_in: Option<Duration>,
}

impl TurnSignal {
    /// A signal carrying a decision and nothing else -- the rule-based case.
    pub const fn new(decision: TurnDecision) -> Self {
        Self {
            decision,
            confidence: None,
            expected_end_in: None,
        }
    }

    /// The user is still going.
    pub const fn incomplete() -> Self {
        Self::new(TurnDecision::Incomplete)
    }

    /// A provider or model says the turn is done.
    pub const fn likely_complete() -> Self {
        Self::new(TurnDecision::LikelyComplete)
    }

    /// Silence ran out; commit regardless of what the model thinks.
    pub const fn forced_complete() -> Self {
        Self::new(TurnDecision::ForcedComplete)
    }

    /// Attach the probability behind the decision.
    #[must_use]
    pub fn with_confidence(mut self, probability: f32) -> Self {
        self.confidence = Some(probability);
        self
    }

    /// Attach the predicted lead time before the turn ends.
    #[must_use]
    pub fn ending_in(mut self, lead: Duration) -> Self {
        self.expected_end_in = Some(lead);
        self
    }

    /// Returns `true` for any complete decision.
    pub fn is_complete(self) -> bool {
        self.decision.is_complete()
    }
}

impl From<TurnDecision> for TurnSignal {
    fn from(decision: TurnDecision) -> Self {
        Self::new(decision)
    }
}

// ── Trait ────────────────────────────────────────────────────

/// Evaluates STT results + silence duration to decide when a user's
/// conversational turn is finished.
#[async_trait]
pub trait TurnDetector: Send + Sync {
    /// Called after each STT result (partial or final).
    ///
    /// * `result` -- the latest transcription result
    /// * `silence_duration` -- time since last speech was detected
    async fn evaluate(
        &self,
        result: &TranscriptionResult,
        silence_duration: Duration,
    ) -> TurnSignal;

    /// Optional: feed raw mic audio so model-based detectors can
    /// maintain a rolling window for semantic endpointing.
    ///
    /// Default no-op for detectors that don't need audio (silence,
    /// provider-signal). `samples` are i16 PCM at `sample_rate` Hz.
    fn feed_audio(&self, _samples: &[i16], _sample_rate: u32) {}

    /// Optional: feed the audio the agent is playing back.
    ///
    /// Turn-taking is a property of the conversation, not of the user alone:
    /// models that predict *when* a turn will end read both sides, because
    /// how the agent is speaking changes when the user will. A detector that
    /// only ever sees the user's channel cannot tell a pause that invites a
    /// reply from one that does not.
    ///
    /// The engine feeds it what the client will actually hear, after the
    /// output processing. Default no-op; `samples` are i16 PCM at
    /// `sample_rate` Hz.
    fn feed_agent_audio(&self, _samples: &[i16], _sample_rate: u32) {}

    /// The user's turn ended: committed, or dropped as noise. A detector
    /// that keeps the user's audio for one turn (Smart Turn's window) starts
    /// the next turn without it; one that models the whole conversation
    /// keeps its history. Default no-op.
    fn turn_ended(&self) {}

    /// Human-readable name for logging / metrics.
    fn name(&self) -> &str;
}

// ── Config and factory ───────────────────────────────────────

/// The `[turn_detection]` settings a detector is built from.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TurnConfig {
    /// The selected mode: the detector's name.
    pub mode: String,
    /// The silence timeout every mode uses, as its rule or its fallback.
    pub silence_timeout_ms: u64,
    /// Every other key of the section, for the selected detector to read
    /// (`smart_threshold = 0.5` arrives as `options["smart_threshold"]`).
    pub options: HashMap<String, serde_json::Value>,
}

impl TurnConfig {
    /// A reader over this section's options (`turn_detection.<key>` in
    /// errors), with the keys the engine itself reads already accounted for.
    pub fn options(&self) -> crate::options::ProviderOptions<'_> {
        crate::options::ProviderOptions::new("turn_detection", &self.options).with_engine_keys(&[])
    }

    /// Settings for `mode` with the given silence timeout and no options.
    pub fn new(mode: impl Into<String>, silence_timeout_ms: u64) -> Self {
        Self {
            mode: mode.into(),
            silence_timeout_ms,
            options: HashMap::new(),
        }
    }

    with_setters! {
        with_options => options: HashMap<String, serde_json::Value>,
    }

    /// `options[key]` as a string, when it is one.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, the one typed reader every provider config shares"
    )]
    pub fn option_str(&self, key: &str) -> Option<&str> {
        self.options.get(key).and_then(serde_json::Value::as_str)
    }

    /// `options[key]` as a number, when it is one.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, the one typed reader every provider config shares"
    )]
    pub fn option_f64(&self, key: &str) -> Option<f64> {
        self.options.get(key).and_then(serde_json::Value::as_f64)
    }

    /// `options[key]` as an unsigned integer, when it is one.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, the one typed reader every provider config shares"
    )]
    pub fn option_u64(&self, key: &str) -> Option<u64> {
        self.options.get(key).and_then(serde_json::Value::as_u64)
    }

    /// `options[key]` as a whole number in `range`, or `default` when unset.
    ///
    /// # Errors
    /// Against `turn_detection.<key>`, telling apart a value that is not a
    /// whole number (a fraction, a string, a negative) from one outside
    /// `range`.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, the one typed reader every provider config shares"
    )]
    pub fn whole_option(
        &self,
        key: &str,
        range: RangeInclusive<u64>,
        default: u64,
    ) -> Result<u64, ConfigError> {
        let Some(v) = self.options.get(key) else {
            return Ok(default);
        };
        let (lo, hi) = (range.start(), range.end());
        match v.as_u64() {
            Some(n) if range.contains(&n) => Ok(n),
            Some(n) => Err(option_error(
                key,
                format!("must be in [{lo}, {hi}], got {n}"),
            )),
            None => Err(option_error(
                key,
                format!("must be a whole number in [{lo}, {hi}], got {v}"),
            )),
        }
    }

    /// `options[key]` as a number in `range`, or `default` when unset.
    ///
    /// # Errors
    /// Against `turn_detection.<key>`, telling apart a value that is not a
    /// number from one outside `range`.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, the one typed reader every provider config shares"
    )]
    pub fn number_option(
        &self,
        key: &str,
        range: RangeInclusive<f64>,
        default: f64,
    ) -> Result<f64, ConfigError> {
        let Some(v) = self.options.get(key) else {
            return Ok(default);
        };
        let (lo, hi) = (range.start(), range.end());
        match v.as_f64() {
            Some(n) if range.contains(&n) => Ok(n),
            Some(n) => Err(option_error(
                key,
                format!("must be in [{lo}, {hi}], got {n}"),
            )),
            None => Err(option_error(
                key,
                format!("must be a number in [{lo}, {hi}], got {v}"),
            )),
        }
    }

    /// An error for every option not in `known`: a misspelt or misplaced
    /// setting is refused rather than silently ignored.
    pub fn unknown_options(&self, known: &[&str]) -> Vec<ConfigError> {
        let mut keys: Vec<&String> = self
            .options
            .keys()
            .filter(|k| !known.contains(&k.as_str()))
            .collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                ConfigError::new(
                    format!("turn_detection.{k}"),
                    format!("not a setting of turn detection mode \"{}\"", self.mode),
                )
                .with_kind(crate::ConfigErrorKind::UnknownKey)
            })
            .collect()
    }
}

fn option_error(key: &str, message: String) -> ConfigError {
    ConfigError::new(format!("turn_detection.{key}"), message)
}

/// Registration metadata and construction for one turn detector.
pub trait TurnDetectorFactory: Send + Sync {
    /// Name as it appears in TOML `turn_detection.mode = "..."`.
    fn name(&self) -> &str;

    /// Additional names that resolve to this factory (aliases).
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Whether `n` is this factory's name or one of its aliases.
    fn matches(&self, n: &str) -> bool {
        self.name() == n || self.aliases().contains(&n)
    }

    /// Check the settings. Empty = valid. Refuse options the detector does
    /// not read ([`TurnConfig::unknown_options`]).
    fn validate(&self, cfg: &TurnConfig) -> Vec<ConfigError>;

    /// Construct the detector.
    ///
    /// # Errors
    /// When the settings cannot produce a working detector (a model that
    /// does not load, say).
    fn build(
        &self,
        cfg: &TurnConfig,
        ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, crate::BuildError>;

    /// The detector's own settings, as the admin console shows them beside
    /// the mode and the silence timeout. Default: none.
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        Vec::new()
    }
}

#[cfg(test)]
#[path = "tests/turn.rs"]
mod tests;
