//! Acoustic adversary & speech perturbation generator for voice agent stress-testing.
//!
//! Generates natural phone dialogue patterns including hesitation fillers
//! ("um", "uh"), mid-utterance self-corrections, and interruption timing specs.

use serde::{Deserialize, Serialize};

/// Acoustic and disfluency configuration for a simulated caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdversaryConfig {
    /// Probability of inserting a hesitation filler or thinking pause (0.0 to 1.0).
    pub hesitation_rate: f32,
    /// Probability of the caller self-correcting mid-utterance (0.0 to 1.0).
    pub self_correction_rate: f32,
    /// Whether barge-in interruption should be attempted mid-response.
    pub attempt_barge_in: bool,
    /// Delay in milliseconds after agent starts speaking before firing the interruption.
    pub barge_in_delay_ms: u64,
}

impl Default for AdversaryConfig {
    fn default() -> Self {
        Self {
            hesitation_rate: 0.25,
            self_correction_rate: 0.20,
            attempt_barge_in: false,
            barge_in_delay_ms: 450,
        }
    }
}

/// Helper that transforms clean scenario scripts into realistic, disfluent voice utterances.
#[derive(Debug, Clone)]
pub struct AcousticAdversary {
    config: AdversaryConfig,
}

impl Default for AcousticAdversary {
    fn default() -> Self {
        Self::new(AdversaryConfig::default())
    }
}

impl AcousticAdversary {
    /// Create a new adversary with the given configuration.
    pub fn new(config: AdversaryConfig) -> Self {
        Self { config }
    }

    /// Access the underlying configuration.
    pub fn config(&self) -> &AdversaryConfig {
        &self.config
    }

    /// Apply disfluencies to a prompt: inserts hesitations or pauses.
    pub fn perturb_prompt(&self, text: &str, seed: u64) -> String {
        // Deterministic pseudo-random choice without external random crates
        let hash = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let roll = ((hash >> 32) as u32 % 100) as f32 / 100.0;

        if roll < self.config.hesitation_rate {
            let filler = if hash.is_multiple_of(2) { "um, " } else { "uh, " };
            format!("{filler}{text}")
        } else {
            text.to_string()
        }
    }

    /// Construct a self-correction utterance where the caller changes their mind mid-speech.
    pub fn make_self_correction(
        &self,
        prefix: &str,
        initial_value: &str,
        corrected_value: &str,
    ) -> String {
        format!("{prefix} {initial_value}... wait, actually make that {corrected_value}, not {initial_value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perturb_prompt_determinism() {
        let adv = AcousticAdversary::new(AdversaryConfig {
            hesitation_rate: 0.8,
            self_correction_rate: 0.2,
            attempt_barge_in: false,
            barge_in_delay_ms: 400,
        });

        let text = "transfer 100 dollars to checking";
        let out1 = adv.perturb_prompt(text, 42);
        let out2 = adv.perturb_prompt(text, 42);
        assert_eq!(out1, out2);
    }

    #[test]
    fn test_make_self_correction() {
        let adv = AcousticAdversary::default();
        let s = adv.make_self_correction("send", "$100", "$250");
        assert!(s.contains("send $100... wait, actually make that $250, not $100"));
    }
}
