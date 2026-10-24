//! Periodic task that flips component health between Healthy and Degraded
//! based on rolling telemetry from the registry.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use super::{Component, ComponentRegistry, HealthState, HealthStatus};

/// Configuration for the periodic [`AutoDegrader`] task.
#[derive(Debug, Clone)]
pub struct AutoDegradeConfig {
    /// How often to evaluate every component. Default: 10 s.
    pub interval: Duration,
    /// Minimum number of new operations (processed + errors) observed since
    /// the previous tick before error-rate is considered. Below this we skip
    /// the error-rate check to avoid flapping on tiny samples.
    pub min_samples: u64,
    /// Error rate over the most-recent tick window above which a component
    /// is marked Degraded. Range 0.0..=1.0.
    pub error_rate_threshold: f32,
    /// p99 latency (ms) above which a component is marked Degraded even
    /// when error rate is fine.
    pub p99_threshold_ms: u64,
}

impl Default for AutoDegradeConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            min_samples: 5,
            error_rate_threshold: 0.2,
            p99_threshold_ms: 5_000,
        }
    }
}

/// Per-component bookkeeping the [`AutoDegrader`] keeps between ticks so it
/// can compute deltas from the cumulative counters in [`ComponentTelemetry`].
///
/// [`ComponentTelemetry`]: super::ComponentTelemetry
#[derive(Debug, Default, Clone)]
struct ComponentBaseline {
    processed: u64,
    errors: u64,
}

/// Periodic task that flips component health between Healthy and Degraded
/// based on rolling telemetry from the registry.
///
/// Behaviour:
/// - `NotConfigured` and `Unhealthy` components are left alone -- those are
///   set by provider lifecycle code (boot, connection lost) and the
///   auto-degrader has no business overriding them.
/// - For everything else, looks at the **delta** of `processed`/`errors`
///   since the last tick plus the current `latency_ms.p99`. If error-rate
///   exceeds the threshold (and we have at least `min_samples` new ops), or
///   p99 exceeds the latency threshold, mark Degraded with a reason. Else
///   mark Healthy.
///
/// The task is purely advisory: it only calls `Component::set_health`,
/// which providers may choose to ignore.
pub struct AutoDegrader {
    registry: ComponentRegistry,
    config: AutoDegradeConfig,
    baselines: DashMap<String, ComponentBaseline>,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "Debug intentionally omits lock/ring internals"
)]
impl fmt::Debug for AutoDegrader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AutoDegrader")
            .field("config", &self.config)
            .field("tracked", &self.baselines.len())
            .finish()
    }
}

impl AutoDegrader {
    pub fn new(registry: ComponentRegistry, config: AutoDegradeConfig) -> Self {
        Self {
            registry,
            config,
            baselines: DashMap::new(),
        }
    }

    /// Spawn the periodic task. Returns the [`tokio::task::JoinHandle`].
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.config.interval);
            // Skip the immediate first tick so providers have time to settle.
            interval.tick().await;
            loop {
                interval.tick().await;
                self.run_once();
            }
        })
    }

    /// Single evaluation pass. Public for tests.
    pub fn run_once(&self) {
        for component in self.registry.snapshot() {
            self.evaluate(component.as_ref());
        }
    }

    fn evaluate(&self, component: &dyn Component) {
        let current = component.health();
        match current.state {
            HealthState::Healthy | HealthState::Degraded => {}
            // Don't override lifecycle-set states, or ones this build does
            // not know.
            _ => return,
        }

        let telemetry = component.telemetry();
        let id = component.id().to_string();

        let mut baseline = self.baselines.entry(id.clone()).or_default();
        let delta_processed = telemetry.processed_total.saturating_sub(baseline.processed);
        let delta_errors = telemetry.errors_total.saturating_sub(baseline.errors);
        let total_ops = delta_processed.saturating_add(delta_errors);
        baseline.processed = telemetry.processed_total;
        baseline.errors = telemetry.errors_total;
        drop(baseline);

        // Decide target state.
        let mut reason: Option<String> = None;

        if total_ops >= self.config.min_samples {
            let rate = delta_errors as f32 / total_ops as f32;
            if rate >= self.config.error_rate_threshold {
                reason = Some(format!(
                    "error rate {:.0}% over last {} ops",
                    rate * 100.0,
                    total_ops
                ));
            }
        }

        if reason.is_none() && telemetry.latency_ms.p99 >= self.config.p99_threshold_ms {
            reason = Some(format!(
                "p99 latency {} ms >= {} ms",
                telemetry.latency_ms.p99, self.config.p99_threshold_ms
            ));
        }

        match (current.state, reason) {
            (HealthState::Healthy, Some(why)) => {
                component.set_health(HealthStatus::degraded(why));
            }
            (HealthState::Degraded, None) => {
                component.set_health(HealthStatus::healthy());
            }
            (HealthState::Degraded, Some(why)) => {
                // Update reason if it changed; set_health is a no-op when
                // state+reason match.
                component.set_health(HealthStatus::degraded(why));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "tests/auto_degrade.rs"]
mod tests;
