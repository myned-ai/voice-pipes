//! Composite multi-modal reward evaluator for voice agent RL.
//!
//! Evaluates task outcome, entity extraction precision, turn-taking etiquette,
//! and penalizes excess latency (above the 500ms real-time conversational budget)
//! and barge-in overrun.

use serde::{Deserialize, Serialize};

use crate::env::TaskEvaluation;

/// Tuning weights for the multi-modal reward function.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GymRewardWeights {
    /// Weight for verifiable database/environment task success (default 1.0).
    pub task_success_weight: f32,
    /// Weight for slot extraction accuracy from voice input (default 0.5).
    pub slot_accuracy_weight: f32,
    /// Weight for conversational cadence & etiquette (default 0.2).
    pub turn_etiquette_weight: f32,
    /// Real-time latency budget in milliseconds (default 500.0).
    pub latency_target_ms: f64,
    /// Penalty factor per millisecond exceeding the latency budget (default 0.002).
    pub latency_penalty_factor: f32,
    /// Penalty factor per millisecond the agent continues speaking after an interruption (default 0.005).
    pub barge_in_overrun_penalty: f32,
}

impl Default for GymRewardWeights {
    fn default() -> Self {
        Self {
            task_success_weight: 1.0,
            slot_accuracy_weight: 0.5,
            turn_etiquette_weight: 0.2,
            latency_target_ms: 500.0,
            latency_penalty_factor: 0.002,
            barge_in_overrun_penalty: 0.005,
        }
    }
}

/// Itemized breakdown of the composite reward calculated for an episode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GymRewardBreakdown {
    /// Score contributed by task completion (0.0 to 1.0 * weight).
    pub task_reward: f32,
    /// Score contributed by entity extraction accuracy (0.0 to 1.0 * weight).
    pub slot_reward: f32,
    /// Score contributed by natural turn-taking (-0.5 to 0.5 * weight).
    pub turn_reward: f32,
    /// Deduction for slow Time-to-First-Audio (TTFA).
    pub latency_penalty: f32,
    /// Deduction for failing to immediately cut off speech on barge-in.
    pub barge_in_penalty: f32,
    /// Final scalar reward $\mathcal{R}$ for policy optimization.
    pub total_reward: f32,
}

impl GymRewardBreakdown {
    /// Compute the composite reward from episode observations.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "latency and overrun differences fit safely in f32"
    )]
    pub fn compute(
        weights: &GymRewardWeights,
        task_eval: &TaskEvaluation,
        slot_accuracy: f32,
        turn_etiquette: f32,
        observed_latency_ms: Option<f64>,
        barge_in_overrun_ms: Option<u64>,
    ) -> Self {
        let task_reward = task_eval.score * weights.task_success_weight;
        let slot_reward = slot_accuracy.clamp(0.0, 1.0) * weights.slot_accuracy_weight;
        let turn_reward = turn_etiquette.clamp(-1.0, 1.0) * weights.turn_etiquette_weight;

        let latency_penalty = match observed_latency_ms {
            Some(lat) if lat > weights.latency_target_ms => {
                let excess = (lat - weights.latency_target_ms) as f32;
                excess * weights.latency_penalty_factor
            }
            _ => 0.0,
        };

        let barge_in_penalty = match barge_in_overrun_ms {
            Some(overrun) if overrun > 200 => {
                let excess = (overrun.saturating_sub(200)) as f32;
                excess * weights.barge_in_overrun_penalty
            }
            _ => 0.0,
        };

        let total_reward = task_reward + slot_reward + turn_reward - latency_penalty - barge_in_penalty;

        Self {
            task_reward,
            slot_reward,
            turn_reward,
            latency_penalty,
            barge_in_penalty,
            total_reward,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perfect_rollout_reward() {
        let weights = GymRewardWeights::default();
        let eval = TaskEvaluation::success("Task completed");
        let reward = GymRewardBreakdown::compute(
            &weights,
            &eval,
            1.0,  // perfect slot accuracy
            0.5,  // good conversational flow
            Some(380.0), // 380ms TTFA (under 500ms budget)
            Some(120),   // 120ms barge-in reaction (under 200ms budget)
        );

        assert_eq!(reward.task_reward, 1.0);
        assert_eq!(reward.slot_reward, 0.5);
        assert_eq!(reward.turn_reward, 0.1); // 0.5 * 0.2
        assert_eq!(reward.latency_penalty, 0.0);
        assert_eq!(reward.barge_in_penalty, 0.0);
        assert!((reward.total_reward - 1.6).abs() < 1e-4);
    }

    #[test]
    fn test_penalized_slow_rollout_reward() {
        let weights = GymRewardWeights::default();
        let eval = TaskEvaluation::failure("No transfer executed");
        let reward = GymRewardBreakdown::compute(
            &weights,
            &eval,
            0.0,
            0.0,
            Some(1000.0), // 500ms excess latency -> 500 * 0.002 = 1.0 penalty
            Some(400),    // 200ms excess overrun -> 200 * 0.005 = 1.0 penalty
        );

        assert_eq!(reward.task_reward, 0.0);
        assert_eq!(reward.slot_reward, 0.0);
        assert_eq!(reward.latency_penalty, 1.0);
        assert_eq!(reward.barge_in_penalty, 1.0);
        assert!((reward.total_reward - (-2.0)).abs() < 1e-4);
    }
}
