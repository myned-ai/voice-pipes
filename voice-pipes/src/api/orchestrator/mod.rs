//! Orchestrator HTTP handlers -- manages worker fleet and routing.
//!
//! Optional host extensions (e.g. the `voice-pipes-bench` benchmarking crate)
//! attach their routes through [`router`]'s `extensions` seam rather than being
//! compiled in here.

mod handlers;
pub(crate) mod helpers;
mod pipeline_graph;
mod router;
#[cfg(test)]
#[path = "tests/suite.rs"]
mod tests;

pub use router::{build_cors, router};
