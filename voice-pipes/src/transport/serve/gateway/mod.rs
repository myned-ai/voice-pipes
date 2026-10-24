//! Gateway layer for transport session orchestration.
//!
//! This module is the home for behavior shared between the classic and
//! realtime session runners. It does not introduce a parallel event model;
//! it operates on the existing transport/pipeline types and centralizes the
//! parts that are common to both runners.
//!
//! Contents:
//! - [`state_machine`]: the single, validated source of truth for
//!   [`crate::pipeline::TurnState`] transitions.
//! - [`shared_handlers`]: control-message handling common to the classic and
//!   realtime runners (ping, client-timing emission).
//! - [`engine_driver`]: the `EngineDriver` seam capturing the operations that
//!   legitimately differ per engine (cancel, played-text resolution).
//! - [`golden`] (test-only): the golden protocol-trace regression net.
//!   The classic driver test that exercises it lives in
//!   `transport::serve::tests` so it can reuse the mock-provider harness.

pub(crate) mod engine_driver;
pub(crate) mod shared_handlers;
pub(crate) mod state_machine;

#[cfg(test)]
pub(crate) mod golden;
