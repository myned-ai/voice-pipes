//! Voice Pipes Gym: native in-memory RL task environments and state verifiers.
//!
//! Provides verifiable task state machines, conversational slot extractors,
//! and determinism checks for voice agents executing tool calls without
//! external network or database dependencies.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

pub mod adversary;
pub mod banking;
pub mod clinic;
pub mod env;
pub mod orders;
pub mod reward;

pub use adversary::{AcousticAdversary, AdversaryConfig};
pub use banking::{BankingGoal, BankingTaskEnv};
pub use clinic::{AppointmentBooking, ClinicBookingTaskEnv, ClinicGoal};
pub use env::{TaskEnvironment, TaskEvaluation};
pub use orders::{OrderGoal, OrderRecord, OrderSupportTaskEnv};
pub use reward::{GymRewardBreakdown, GymRewardWeights};
pub use voice_pipes_sdk::agent::{ClientToolCall, ClientToolSpec};
