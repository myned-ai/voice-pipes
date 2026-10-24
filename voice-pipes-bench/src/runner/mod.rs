//! Benchmark runner: orchestrates experiment execution across workers and
//! scenario types.
//!
//! The top-level entry point is [`run_experiment`]. Per-scenario flows live in
//! focused submodules; the shared session-drain and scoring helpers are in
//! [`collect`] and [`summary`]:
//! - [`conversation`], [`barge_in`], [`synthetic_caller`], [`tts_only`]: one
//!   scenario flow each, dispatched from `run_scenario`.
//! - [`collect`]: drain a WebSocket session into a [`RunOutcome`].
//! - [`summary`]: scenario scoring and variant-summary aggregation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use super::scoring::{TtsScore, TtsScorer};
use super::store::BenchmarkStore;
use super::types::*;
use voice_pipes::api::types::{WorkerRecord, WorkerStatus};

mod barge_in;
mod collect;
mod conversation;
mod gym;
mod summary;
mod synthetic_caller;
mod tts_only;

use self::barge_in::*;
use self::collect::*;
use self::conversation::*;
use self::gym::*;
use self::summary::*;
use self::synthetic_caller::*;
use self::tts_only::*;

// ── Configuration ───────────────────────────────────────────

/// Tuning knobs for the benchmark runner.
#[derive(Clone)]
pub struct RunnerConfig {
    /// Playback speed for audio injection (0.0 = AFAP, 1.0 = realtime).
    pub inject_speed: f32,
    /// Per-scenario timeout (includes injection + full pipeline pass).
    pub scenario_timeout: Duration,
    /// Settle time after `injection_complete` -- waits for pipeline tail
    /// (LLM + TTS) to finish producing messages.
    pub settle_timeout: Duration,
    /// If set, only run variants whose IDs are in this list.
    /// Other variants are skipped (their existing results are preserved).
    pub variant_filter: Option<Vec<String>>,
    /// If set, only run scenarios whose IDs are in this list.
    /// Other scenarios are skipped entirely (no result row is emitted
    /// for them in this run).
    pub scenario_filter: Option<Vec<String>>,
    /// If set, pin every variant to this specific worker (by `worker_id`).
    /// Overrides tag-based selection. The worker must be Active and have
    /// spare capacity; tag constraints from `variant.tags` are bypassed.
    pub target_worker_id: Option<String>,
    /// Optional reference scorer applied to every TTS-only rep. When
    /// `None`, `ScenarioScoring` is left unset for TTS-only scenarios.
    pub tts_scorer: Option<Arc<dyn TtsScorer>>,
}

impl std::fmt::Debug for RunnerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerConfig")
            .field("inject_speed", &self.inject_speed)
            .field("scenario_timeout", &self.scenario_timeout)
            .field("settle_timeout", &self.settle_timeout)
            .field("variant_filter", &self.variant_filter)
            .field("scenario_filter", &self.scenario_filter)
            .field("target_worker_id", &self.target_worker_id)
            .field("tts_scorer", &self.tts_scorer.as_ref().map(|_| "<scorer>"))
            .finish()
    }
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            inject_speed: 0.0,
            scenario_timeout: Duration::from_secs(120),
            settle_timeout: Duration::from_secs(15),
            variant_filter: None,
            scenario_filter: None,
            target_worker_id: None,
            tts_scorer: None,
        }
    }
}

/// Response returned to the caller after a run completes or fails.
#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub experiment_id: String,
    pub status: ExperimentStatus,
    pub scenarios_run: u32,
    pub scenarios_passed: u32,
    pub scenarios_failed: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ── Public entry point ──────────────────────────────────────

/// Read the variant's TOML config, compute SHA-256, freeze a snapshot to the
/// experiment folder, and parse key fields into a [`PipelineConfigSummary`].
async fn freeze_variant_config(
    store: &BenchmarkStore,
    experiment_id: &str,
    variant: &mut Variant,
) -> Option<PipelineConfigSummary> {
    let config_path = &variant.config_path;
    // Containment: `config_path` comes from the (untrusted) create
    // request. Require it to resolve inside the current working directory so a
    // variant can't exfiltrate arbitrary files (e.g. an SSH key) by pointing
    // the "config" at them.
    let base = match std::env::current_dir().and_then(|d| d.canonicalize()) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "could not resolve working directory for config containment");
            return None;
        }
    };
    let canonical = match tokio::fs::canonicalize(config_path).await {
        Ok(c) => c,
        Err(e) => {
            warn!(variant_id = %variant.id, path = %config_path.display(), error = %e, "could not resolve variant config");
            return None;
        }
    };
    if !canonical.starts_with(&base) {
        warn!(variant_id = %variant.id, path = %config_path.display(), "variant config_path escapes the working directory; refusing to read");
        return None;
    }
    let toml_content = match tokio::fs::read_to_string(&canonical).await {
        Ok(c) => c,
        Err(e) => {
            warn!(variant_id = %variant.id, path = %config_path.display(), error = %e, "could not read variant config");
            return None;
        }
    };

    // Compute SHA-256 and update the variant.
    let hash = format!("{:x}", Sha256::digest(toml_content.as_bytes()));
    variant.config_hash = hash;

    // Freeze the snapshot.
    if let Err(e) = store
        .save_variant_config(experiment_id, &variant.id, &toml_content)
        .await
    {
        warn!(variant_id = %variant.id, error = %e, "failed to freeze variant config");
    }

    Some(parse_config_summary(&toml_content))
}

/// Extract key pipeline settings from a TOML config string.
fn parse_config_summary(toml_content: &str) -> PipelineConfigSummary {
    let table: toml::Table = toml_content.parse().unwrap_or_default();

    let pipeline_name = table
        .get("pipeline")
        .and_then(|v| v.get("name"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let vad_mode = table
        .get("vad")
        .and_then(|v| v.get("mode"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let stt_provider = table
        .get("stt")
        .and_then(|v| v.get("provider"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let stt_model = table
        .get("stt")
        .and_then(|v| v.get("model_dir"))
        .and_then(toml::Value::as_str)
        .map(|s| s.rsplit('/').next().unwrap_or(s).to_string());

    // text_processor is an array of tables -- take the first one.
    let tp = table
        .get("text_processor")
        .and_then(toml::Value::as_array)
        .and_then(|a| a.first());

    let llm_model = tp
        .and_then(|v| v.get("model"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let llm_base_url = tp
        .and_then(|v| v.get("base_url"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let llm_temperature = tp
        .and_then(|v| v.get("temperature"))
        .and_then(toml::Value::as_float);

    let llm_max_tokens = tp
        .and_then(|v| v.get("max_tokens"))
        .and_then(toml::Value::as_integer)
        .map(|v| v as u32);

    let llm_system_prompt = tp
        .and_then(|v| v.get("system_prompt"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let tts_provider = table
        .get("tts")
        .and_then(|v| v.get("provider"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let tts_model = table
        .get("tts")
        .and_then(|v| v.get("model"))
        .and_then(toml::Value::as_str)
        .map(|s| s.rsplit('/').next().unwrap_or(s).to_string());

    let turn_detection_mode = table
        .get("turn_detection")
        .and_then(|v| v.get("mode"))
        .and_then(toml::Value::as_str)
        .map(String::from);

    let silence_timeout_ms = table
        .get("turn_detection")
        .and_then(|v| v.get("silence_timeout_ms"))
        .and_then(toml::Value::as_integer)
        .map(|v| v as u64);

    let barge_in_enabled = table
        .get("barge_in")
        .and_then(|v| v.get("enabled"))
        .and_then(toml::Value::as_bool);

    PipelineConfigSummary {
        pipeline_name,
        vad_mode,
        stt_provider,
        stt_model,
        llm_model,
        llm_base_url,
        llm_temperature,
        llm_max_tokens,
        llm_system_prompt,
        tts_provider,
        tts_model,
        turn_detection_mode,
        silence_timeout_ms,
        barge_in_enabled,
    }
}

/// Execute an experiment end-to-end.
///
/// 1. Load experiment + suite from store.
/// 2. Transition status -> Running.
/// 3. For each variant x scenario x repetition -> WS connect, inject, collect.
/// 4. Score against expected outcomes.
/// 5. Save results + transition status -> Complete / Failed.
pub async fn run_experiment(
    store: &BenchmarkStore,
    workers: &Arc<DashMap<String, WorkerRecord>>,
    experiment_id: &str,
    config: &RunnerConfig,
) -> anyhow::Result<RunReport> {
    // Ensure a rustls crypto provider is installed before any TLS work
    // (synthetic-caller pulls tokio-tungstenite with rustls-tls-native-roots,
    // which panics on first use if no default CryptoProvider is selected).
    #[cfg(feature = "synthetic-caller")]
    {
        static RUSTLS_INIT: std::sync::Once = std::sync::Once::new();
        RUSTLS_INIT.call_once(|| {
            voice_pipes_net::install_crypto_provider();
        });
    };

    // ── Load & validate ─────────────────────────────────────
    let mut experiment = store
        .load_experiment(experiment_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("experiment {experiment_id} not found"))?;

    let is_partial_rerun = experiment.status == ExperimentStatus::Complete
        || experiment.status == ExperimentStatus::Failed;

    if !is_partial_rerun && experiment.status != ExperimentStatus::Draft {
        anyhow::bail!(
            "experiment {} is {:?}, expected Draft (or Complete/Failed for partial rerun)",
            experiment_id,
            experiment.status
        );
    }

    // Load existing results for partial reruns -- we'll merge new results in.
    let existing_results = if is_partial_rerun {
        store
            .load_results(experiment_id)
            .await?
            .map(|r| r.variant_results)
    } else {
        None
    };

    let suite = store
        .load_suite(&experiment.test_suite_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("test suite {} not found", experiment.test_suite_id))?;

    // ── Transition -> Running ────────────────────────────────
    experiment.status = ExperimentStatus::Running;
    experiment.started_at = Some(Utc::now());
    store.save_experiment(&experiment).await?;
    info!(experiment_id, "benchmark runner started");

    // ── Freeze variant configs for reproducibility ──────────
    let mut config_summaries = Vec::new();
    for variant in &mut experiment.variants {
        let summary = freeze_variant_config(store, experiment_id, variant).await;
        config_summaries.push(summary);
    }
    // Re-save experiment with updated config_hash values.
    store.save_experiment(&experiment).await?;

    // ── Run each variant ────────────────────────────────────
    let mut all_variant_results = Vec::new();
    let mut any_error = false;

    for (idx, variant) in experiment.variants.iter().enumerate() {
        // If a variant filter is set, skip variants not in the list and
        // preserve their existing results from a previous run.
        if let Some(ref filter) = config.variant_filter
            && !filter.contains(&variant.id)
        {
            if let Some(ref existing) = existing_results
                && let Some(prev) = existing.iter().find(|vr| vr.variant_id == variant.id)
            {
                info!(variant_id = %variant.id, "skipping variant (not in filter), keeping previous results");
                all_variant_results.push(prev.clone());
                continue;
            }
            info!(variant_id = %variant.id, "skipping variant (not in filter, no previous results)");
            continue;
        }

        info!(
            experiment_id,
            variant_id = %variant.id,
            variant_name = %variant.name,
            "running variant"
        );

        let Some(ws_url) =
            resolve_worker_ws_with_pin(workers, &variant.tags, config.target_worker_id.as_deref())
        else {
            let reason = if let Some(pin) = config.target_worker_id.as_deref() {
                format!(
                    "pinned worker '{pin}' is not Active or has no spare capacity -- \
                     check that this worker is running and registered"
                )
            } else {
                format!(
                    "no available worker matching variant tags {:?} -- \
                     check that a worker with these tags is running and registered",
                    variant.tags,
                )
            };
            error!(
                variant_id = %variant.id,
                tags = ?variant.tags,
                target_worker = ?config.target_worker_id,
                "no available worker for variant"
            );
            any_error = true;
            all_variant_results.push(empty_variant_result(variant, &suite, &reason));
            continue;
        };

        let mut scenario_results = Vec::new();

        for scenario in &suite.scenarios {
            if let Some(ref filter) = config.scenario_filter
                && !filter.contains(&scenario.id)
            {
                // Scenario-granular merge: a scenario-filtered rerun used
                // to rebuild this variant's results from only the
                // scenarios that ran, silently dropping its previous
                // rows for every skipped scenario. Keep the prior result
                // when one exists so partial reruns refine results
                // instead of erasing them.
                if let Some(prev) = existing_results.as_ref().and_then(|existing| {
                    existing
                        .iter()
                        .find(|vr| vr.variant_id == variant.id)
                        .and_then(|vr| vr.scenarios.iter().find(|s| s.scenario_id == scenario.id))
                }) {
                    info!(
                        scenario_id = %scenario.id,
                        "skipping scenario (not in filter), keeping previous result"
                    );
                    scenario_results.push(prev.clone());
                } else {
                    info!(
                        scenario_id = %scenario.id,
                        "skipping scenario (not in filter)"
                    );
                }
                continue;
            }
            let result = run_scenario(
                &ws_url,
                &experiment.id,
                variant,
                scenario,
                experiment.repetitions,
                config,
            )
            .await;
            scenario_results.push(result);
        }

        let summary = compute_summary(&scenario_results);
        all_variant_results.push(VariantResults {
            variant_id: variant.id.clone(),
            variant_name: variant.name.clone(),
            scenarios: scenario_results,
            summary,
            config_summary: config_summaries[idx].clone(),
            error_reason: None,
        });
    }

    // ── Persist results ─────────────────────────────────────
    let results = ExperimentResults {
        experiment_id: experiment_id.to_string(),
        completed_at: Utc::now(),
        variant_results: all_variant_results.clone(),
    };
    store.save_results(&results).await?;

    // ── Transition -> Complete / Failed ──────────────────────
    let total_run: u32 = all_variant_results
        .iter()
        .map(|v| v.summary.scenarios_run)
        .sum();
    let total_pass: u32 = all_variant_results
        .iter()
        .map(|v| v.summary.scenarios_passed)
        .sum();
    let total_fail: u32 = all_variant_results
        .iter()
        .map(|v| v.summary.scenarios_failed)
        .sum();

    experiment.status = if any_error {
        ExperimentStatus::Failed
    } else {
        ExperimentStatus::Complete
    };
    experiment.completed_at = Some(Utc::now());
    store.save_experiment(&experiment).await?;

    info!(
        experiment_id,
        status = ?experiment.status,
        total_run,
        total_pass,
        total_fail,
        "benchmark runner finished"
    );

    Ok(RunReport {
        experiment_id: experiment_id.to_string(),
        status: experiment.status,
        scenarios_run: total_run,
        scenarios_passed: total_pass,
        scenarios_failed: total_fail,
        error: None,
    })
}

// ── Worker resolution ───────────────────────────────────────

/// Build a connectable `ws://addr/ws` URL from a worker record.
///
/// `transport_address` is the worker's *bind* address, so a wildcard host
/// (`0.0.0.0`) is unroutable as-is. In that case reuse the host from the
/// worker's advertised debug-API `address` — reachable from the orchestrator
/// by construction (that's where heartbeats go) — with the transport port,
/// so containerized fleets resolve the compose service name. Fall back to
/// `127.0.0.1` for host-native stacks whose debug bind is also wildcard.
fn worker_ws_url(w: &WorkerRecord) -> String {
    let raw_addr = w.transport_address.as_deref().unwrap_or(&w.address);
    let (host, port) = raw_addr.rsplit_once(':').unwrap_or((raw_addr, ""));
    let host = if host == "0.0.0.0" || host.is_empty() {
        w.address
            .rsplit_once(':')
            .map(|(h, _)| h)
            .filter(|h| !h.is_empty() && *h != "0.0.0.0")
            .unwrap_or("127.0.0.1")
    } else {
        host
    };
    format!("ws://{host}:{port}/ws")
}

/// If `pinned` is set, return that worker's WS URL when it is Active and has
/// spare capacity (tag constraints are bypassed). Otherwise fall back to
/// tag-based least-loaded selection.
fn resolve_worker_ws_with_pin(
    workers: &DashMap<String, WorkerRecord>,
    required_tags: &[String],
    pinned: Option<&str>,
) -> Option<String> {
    if let Some(id) = pinned {
        let entry = workers.get(id)?;
        let w = entry.value();
        if w.status != WorkerStatus::Active {
            return None;
        }
        if w.capacity.active_sessions >= w.capacity.max_concurrent {
            return None;
        }
        return Some(worker_ws_url(w));
    }
    resolve_worker_ws(workers, required_tags)
}

/// Find the least-loaded active worker whose tags are a superset of `required`.
fn resolve_worker_ws(
    workers: &DashMap<String, WorkerRecord>,
    required_tags: &[String],
) -> Option<String> {
    let mut best: Option<(String, u32)> = None;

    for entry in workers {
        let w = entry.value();
        if w.status != WorkerStatus::Active {
            continue;
        }
        if w.capacity.active_sessions >= w.capacity.max_concurrent {
            continue;
        }
        if !required_tags.iter().all(|t| w.tags.contains(t)) {
            continue;
        }

        let load = w.capacity.active_sessions;
        if best.as_ref().is_none_or(|(_, prev)| load < *prev) {
            best = Some((worker_ws_url(w), load));
        }
    }

    best.map(|(url, _)| url)
}

// ── Scenario runner ─────────────────────────────────────────

/// Run one scenario (all repetitions) against a single worker.
async fn run_scenario(
    ws_url: &str,
    experiment_id: &str,
    variant: &Variant,
    scenario: &TestScenario,
    repetitions: u32,
    config: &RunnerConfig,
) -> ScenarioResult {
    // Dispatch by input variant. AudioFile and TextPrompt fall through
    // to the single-turn injection path (`run_single_with_retry`
    // is called per repetition below); every other variant is routed to
    // its dedicated runner.
    match &scenario.input {
        ScenarioInput::TtsOnlyCorpus { entries } => {
            // TTS-only corpora bypass the audio-injection pipeline
            // entirely: each entry drives one rep against the variant's
            // TTS provider directly, sharing a single WebSocket so
            // cold-start tracking is meaningful.
            return run_tts_only_scenario(ws_url, variant, scenario, entries, config).await;
        }
        ScenarioInput::Conversation { turns } => {
            return run_conversation_scenario(ws_url, experiment_id, scenario, turns, config).await;
        }
        ScenarioInput::BargeIn {
            initial,
            interrupt,
            follow_up,
        } => {
            return run_barge_in_scenario(
                ws_url,
                experiment_id,
                scenario,
                initial,
                interrupt,
                follow_up.as_ref(),
                config,
            )
            .await;
        }
        ScenarioInput::SyntheticCaller { caller } => {
            return run_synthetic_caller_scenario(ws_url, experiment_id, scenario, caller, config)
                .await;
        }
        ScenarioInput::GymTask { task } => {
            return run_gym_scenario(ws_url, experiment_id, scenario, task, config).await;
        }
        ScenarioInput::AudioFile { .. } | ScenarioInput::TextPrompt { .. } => {
            // Fall through to the single-turn loop below.
        }
    }

    let mut latencies = Vec::new();
    let mut ttfts = Vec::new();
    let mut ttfbs = Vec::new();
    let mut stt_latencies = Vec::new();
    let mut llm_generations = Vec::new();
    let mut llm_token_counts = Vec::new();
    let mut tts_ttfbs = Vec::new();
    let mut tts_synth_all: Vec<f64> = Vec::new();
    let mut tts_synth_cold: Vec<f64> = Vec::new();
    let mut tts_synth_warm: Vec<f64> = Vec::new();
    let mut tts_audio_durs: Vec<f64> = Vec::new();
    let mut tts_rtfs: Vec<f64> = Vec::new();
    let mut tts_chars_per_sec: Vec<f64> = Vec::new();
    let mut tts_chars_total: Vec<u32> = Vec::new();
    let mut tts_sample_rate_last: Option<u32> = None;
    let mut tts_provider_last: Option<String> = None;
    let mut tts_failures: u32 = 0;
    let mut tts_attempts: u32 = 0;
    let mut tts_last_error: Option<String> = None;
    let mut last_stt = None;
    let mut last_response = None;
    let mut last_audio_dur = None;
    let mut session_ids = Vec::new();
    let mut failed_reps: u32 = 0;
    let mut last_error: Option<String> = None;

    for rep in 0..repetitions {
        debug!(
            scenario = %scenario.name,
            variant = %variant.name,
            repetition = rep + 1,
            "running repetition"
        );

        match run_single_with_retry(ws_url, experiment_id, &scenario.id, scenario, config).await {
            Ok(outcome) => {
                session_ids.extend(outcome.session_id);
                if let Some(ms) = outcome.e2e_latency_ms {
                    latencies.push(ms);
                }
                if let Some(ms) = outcome.ttft_ms {
                    ttfts.push(ms);
                }
                if let Some(ms) = outcome.ttfb_ms {
                    ttfbs.push(ms);
                }
                if let Some(ms) = outcome.stt_latency_ms {
                    stt_latencies.push(ms);
                }
                if let Some(ms) = outcome.llm_generation_ms {
                    llm_generations.push(ms);
                }
                if let Some(t) = outcome.llm_tokens {
                    llm_token_counts.push(t);
                }
                if let Some(ms) = outcome.tts_ttfb_ms {
                    tts_ttfbs.push(ms);
                }
                if let Some(ms) = outcome.tts_synthesis_ms {
                    tts_synth_all.push(ms);
                    if outcome.tts_cold_start {
                        tts_synth_cold.push(ms);
                    } else {
                        tts_synth_warm.push(ms);
                    }
                }
                if let Some(secs) = outcome.tts_audio_duration_s {
                    tts_audio_durs.push(secs);
                    if let Some(chars) = outcome.tts_chars
                        && secs > 0.0
                    {
                        tts_chars_per_sec.push(chars as f64 / secs);
                    }
                }
                if let Some(rtf) = outcome.tts_rtf {
                    tts_rtfs.push(rtf);
                }
                if let Some(chars) = outcome.tts_chars {
                    tts_chars_total.push(chars);
                }
                if outcome.tts_sample_rate.is_some() {
                    tts_sample_rate_last = outcome.tts_sample_rate;
                }
                if outcome.tts_provider.is_some() {
                    tts_provider_last = outcome.tts_provider.clone();
                }
                tts_attempts += 1;
                if let Some(ref e) = outcome.tts_error {
                    tts_failures += 1;
                    tts_last_error = Some(e.clone());
                }
                if outcome.stt_text.is_some() {
                    last_stt = outcome.stt_text;
                }
                if outcome.response_text.is_some() {
                    last_response = outcome.response_text;
                }
                if outcome.audio_duration_s.is_some() {
                    last_audio_dur = outcome.audio_duration_s;
                }
            }
            Err(e) => {
                warn!(
                    scenario = %scenario.name,
                    repetition = rep + 1,
                    error = %e,
                    "scenario repetition failed"
                );
                failed_reps += 1;
                last_error = Some(format!("{e:#}"));
            }
        }
    }

    let avg_llm_tokens = if llm_token_counts.is_empty() {
        None
    } else {
        Some(
            (llm_token_counts.iter().map(|t| *t as u64).sum::<u64>()
                / llm_token_counts.len() as u64) as u32,
        )
    };

    let metrics = ScenarioMetrics {
        e2e_latency_ms: avg(&latencies),
        ttft_ms: avg(&ttfts),
        ttfb_ms: avg(&ttfbs),
        stt_text: last_stt.clone(),
        response_text: last_response.clone(),
        audio_duration_s: last_audio_dur,
        stt_latency_ms: avg(&stt_latencies),
        llm_generation_ms: avg(&llm_generations),
        llm_tokens: avg_llm_tokens,
        tts_ttfb_ms: avg(&tts_ttfbs),
        tts_synthesis_ms: avg(&tts_synth_all),
        tts_audio_duration_s: avg(&tts_audio_durs),
        tts_sample_rate: tts_sample_rate_last,
        tts_chars: if tts_chars_total.is_empty() {
            None
        } else {
            // Mean character count (rounded) across reps that produced TTS.
            let sum: u64 = tts_chars_total.iter().map(|c| u64::from(*c)).sum();
            Some((sum / tts_chars_total.len() as u64) as u32)
        },
        tts_rtf: avg(&tts_rtfs),
        tts_chars_per_second: avg(&tts_chars_per_sec),
        tts_provider: tts_provider_last,
        // Scenario-level cold flag: true iff *any* rep was the cold one.
        tts_cold_start: !tts_synth_cold.is_empty(),
        tts_error: (tts_failures > 0).then(|| {
            format!(
                "{tts_failures}/{tts_attempts} reps produced no audio: {}",
                tts_last_error.as_deref().unwrap_or("unknown"),
            )
        }),
        // Per-entry speech stats exist only for `tts_only_corpus` scenarios.
        tts_silent_entries: None,
    };

    let scoring = scenario.expected.as_ref().map(|exp| score(exp, &metrics));

    let all_failed = failed_reps > 0 && latencies.is_empty();
    let status = if all_failed {
        ScenarioRunStatus::Error
    } else if scoring.as_ref().is_some_and(|s| !s.expectations_met) {
        ScenarioRunStatus::Failed
    } else {
        ScenarioRunStatus::Passed
    };

    let error_message = if all_failed {
        // All repetitions failed -- report the last error.
        last_error.map(|e| format!("{failed_reps}/{repetitions} repetitions failed: {e}"))
    } else if failed_reps > 0 {
        // Partial failure -- still passed but note the flakiness.
        last_error.map(|e| format!("{failed_reps}/{repetitions} repetitions failed (partial): {e}"))
    } else {
        None
    };

    ScenarioResult {
        scenario_id: scenario.id.clone(),
        scenario_name: scenario.name.clone(),
        session_ids,
        status,
        metrics,
        latency_samples: latencies.clone(),
        scoring,
        error_message,
        repetitions_failed: (failed_reps > 0).then_some(failed_reps),
        tts_entries: None,
        turns: None,
        barge_in: None,
        synthetic_caller: None,
        gym: None,
    }
}

const MAX_CONNECT_RETRIES: u32 = 2;

/// Delay between retry attempts.
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Wrapper around [`run_single`] that retries transient connection failures.
///
/// Connection errors (WS timeout, TCP refused) are retried up to
/// [`MAX_CONNECT_RETRIES`] times.  Non-connection errors (server errors,
/// protocol failures) are returned immediately.
async fn run_single_with_retry(
    ws_url: &str,
    experiment_id: &str,
    scenario_id: &str,
    scenario: &TestScenario,
    config: &RunnerConfig,
) -> anyhow::Result<RunOutcome> {
    let mut last_err = None;
    for attempt in 0..=MAX_CONNECT_RETRIES {
        match run_single(ws_url, experiment_id, scenario_id, scenario, config).await {
            Ok(outcome) => return Ok(outcome),
            Err(e) => {
                let msg = format!("{e}");
                let is_transient = msg.contains("connect timeout")
                    || msg.contains("Connection refused")
                    || msg.contains("SessionReady timeout")
                    || msg.contains("WS closed before SessionReady");
                if !is_transient || attempt == MAX_CONNECT_RETRIES {
                    return Err(e);
                }
                warn!(
                    attempt = attempt + 1,
                    max_retries = MAX_CONNECT_RETRIES,
                    error = %e,
                    "transient connection error, retrying"
                );
                last_err = Some(e);
                sleep(RETRY_DELAY).await;
            }
        }
    }
    // Should not reach here, but just in case:
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("retry loop exhausted")))
}

/// Execute a single repetition: WS connect -> inject -> collect -> close.
async fn run_single(
    ws_url: &str,
    experiment_id: &str,
    scenario_id: &str,
    scenario: &TestScenario,
    config: &RunnerConfig,
) -> anyhow::Result<RunOutcome> {
    let wall_start = Instant::now();

    // ── Connect ─────────────────────────────────────────────
    let ws_stream = connect_worker(ws_url).await?;

    let (mut sink, mut stream) = ws_stream.split();

    // ── Send Configure (minimal handshake) ──────────────────
    let configure_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "configure",
            "audio_format": "pcm_i16_le",
            "sample_rate": 16000,
            "tags": ["benchmark"]
        }
    });
    sink.send(Message::Text(configure_msg.to_string().into()))
        .await?;

    // ── Wait for SessionReady ───────────────────────────────
    let mut session_id: Option<String> = None;
    let ready = timeout(Duration::from_secs(10), async {
        while let Some(msg) = stream.next().await {
            if let Ok(Message::Text(text)) = msg
                && let Ok(env) = serde_json::from_str::<JsonEnvelope>(&text)
                && env.msg_type == "control"
                && let Some(action) = env.payload.get("action").and_then(|v| v.as_str())
                && action == "session_ready"
            {
                session_id = env
                    .payload
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
                return Ok(());
            }
        }
        Err(anyhow::anyhow!("WS closed before SessionReady"))
    })
    .await;

    match ready {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => anyhow::bail!("SessionReady timeout"),
    }

    // ── Send InjectAudio ────────────────────────────────────
    let source = match &scenario.input {
        ScenarioInput::AudioFile { path } => path.display().to_string(),
        ScenarioInput::TextPrompt { text } => {
            // For text-only scenarios, send as a text message and skip injection.
            let text_msg = serde_json::json!({
                "type": "message",
                "payload": {
                    "kind": "text",
                    "text": text,
                    "role": "user",
                    "message_id": format!("bench-{scenario_id}")
                }
            });
            sink.send(Message::Text(text_msg.to_string().into()))
                .await?;
            return collect_outcome(
                &mut stream,
                session_id,
                config.scenario_timeout,
                config.settle_timeout,
                wall_start,
                true,
            )
            .await;
        }
        ScenarioInput::TtsOnlyCorpus { .. } => {
            // Routed via `run_tts_only_scenario` from `run_scenario`; this
            // path is unreachable in practice but the match must be total.
            anyhow::bail!("tts_only_corpus scenarios are not driven via run_single");
        }
        ScenarioInput::Conversation { .. } | ScenarioInput::BargeIn { .. } => {
            // Routed via dedicated runners from `run_scenario`; not
            // reachable here.
            anyhow::bail!("conversation/barge_in scenarios are not driven via run_single");
        }
        ScenarioInput::SyntheticCaller { .. } => {
            // Routed via `run_synthetic_caller_scenario` from
            // `run_scenario`; not reachable here.
            anyhow::bail!("synthetic_caller scenarios are not driven via run_single");
        }
        ScenarioInput::GymTask { .. } => {
            anyhow::bail!("gym_task scenarios are not driven via run_single");
        }
    };

    let inject_msg = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "inject_audio",
            "source": source,
            "scenario_id": scenario_id,
            "experiment_id": experiment_id,
            "speed": config.inject_speed
        }
    });
    sink.send(Message::Text(inject_msg.to_string().into()))
        .await?;

    collect_outcome(
        &mut stream,
        session_id,
        config.scenario_timeout,
        config.settle_timeout,
        wall_start,
        false,
    )
    .await
}

/// The benchmark's WebSocket to the worker under test, opened within 10 s.
async fn connect_worker<R>(
    request: R,
) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>
where
    R: tokio_tungstenite::tungstenite::client::IntoClientRequest + Unpin,
{
    #[expect(
        clippy::disallowed_methods,
        reason = "the benchmark dials the voice-pipes worker it measures, not a vendor, and sends it no credential"
    )]
    let connect = tokio_tungstenite::connect_async(request);
    let (ws_stream, _response) = timeout(Duration::from_secs(10), connect)
        .await
        .map_err(|_elapsed| anyhow::anyhow!("WS connect timeout"))??;
    Ok(ws_stream)
}
