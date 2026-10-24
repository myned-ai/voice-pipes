//! Voice Pipes benchmarking & automated voice-testing — a host extension.
//!
//! This crate provides controlled A/B comparison of pipeline configurations
//! against identical audio inputs with reproducible, quantitative metrics:
//! the experiment/suite data model ([`types`]), storage ([`store`]), the
//! runner ([`runner`]), scoring ([`scoring`]), AI-driven synthetic callers
//! ([`synthetic_caller`]), and the orchestrator HTTP surface ([`router`]).
//!
//! It is an optional **host extension**: it depends on the whole `voice-pipes`
//! engine and attaches to the orchestrator through that crate's neutral
//! `extensions` router seam. The core engine has no dependency on this crate,
//! so it can be dropped from a build without touching core.

#![forbid(unsafe_code)]
#![allow(
    clippy::wildcard_imports,
    reason = "the runner is split into files that share one import list through `use super::*` by design"
)]

pub mod runner;
pub mod scoring;
pub mod store;
pub mod synthetic_caller;
pub mod turn_detection;
pub mod types;

mod http;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use voice_pipes::api::types::OrchestratorState;

/// Build the benchmark HTTP routes as one mergeable router.
///
/// Owns a [`store::BenchmarkStore`] rooted at `benchmarks_dir` (attached as a
/// request extension) and returns a router to merge into the orchestrator via
/// its `extensions` seam:
///
/// ```ignore
/// voice_pipes::commands::run_orchestrate(
///     cfg_path,
///     |cfg| vec![voice_pipes_bench::router(&cfg.benchmarks_dir)],
/// ).await
/// ```
pub fn router(benchmarks_dir: &Path) -> Router<Arc<OrchestratorState>> {
    http::routes(store::BenchmarkStore::new(benchmarks_dir))
}
