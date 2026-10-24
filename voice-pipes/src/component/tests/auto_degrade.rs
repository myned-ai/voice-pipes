use std::sync::Arc;
use std::time::Duration;

use super::{AutoDegradeConfig, AutoDegrader};
use crate::component::test_support::{CoreBacked, comp};
use crate::component::{
    Component, ComponentCore, ComponentKind, ComponentRegistry, ComponentTelemetry, HealthState,
    HealthStatus,
};

fn auto_cfg() -> AutoDegradeConfig {
    AutoDegradeConfig {
        interval: Duration::from_secs(1),
        min_samples: 5,
        error_rate_threshold: 0.2,
        p99_threshold_ms: 500,
    }
}

#[test]
fn auto_degrade_marks_degraded_on_high_error_rate() {
    let reg = ComponentRegistry::new();
    let c = comp("stt:bad", ComponentKind::Stt);
    // 3 ok + 5 errors over the tick window -> 5/8 = 62% error rate.
    for _ in 0..3 {
        c.0.record_ok(Duration::from_millis(10));
    }
    for _ in 0..5 {
        c.0.record_error("boom");
    }
    reg.register(std::sync::Arc::<CoreBacked>::clone(&c));

    let degrader = AutoDegrader::new(reg.clone(), auto_cfg());
    degrader.run_once();

    let h = c.0.health();
    assert_eq!(h.state, HealthState::Degraded);
    assert!(h.reason.contains("error rate"), "reason: {}", h.reason);
}

#[test]
fn auto_degrade_skips_when_too_few_samples() {
    let reg = ComponentRegistry::new();
    let c = comp("stt:few", ComponentKind::Stt);
    // 1 ok + 1 error: under min_samples=5, error-rate check is skipped.
    c.0.record_ok(Duration::from_millis(10));
    c.0.record_error("boom");
    reg.register(std::sync::Arc::<CoreBacked>::clone(&c));

    let degrader = AutoDegrader::new(reg.clone(), auto_cfg());
    degrader.run_once();

    // Latency p99 (10 ms) is also under threshold -> stays Healthy.
    assert_eq!(c.0.health().state, HealthState::Healthy);
}

#[test]
fn auto_degrade_marks_degraded_on_high_p99_latency() {
    let reg = ComponentRegistry::new();
    let c = comp("tts:slow", ComponentKind::Tts);
    // All successful but very slow: p99 well over 500 ms threshold.
    for _ in 0..10 {
        c.0.record_ok(Duration::from_millis(800));
    }
    reg.register(std::sync::Arc::<CoreBacked>::clone(&c));

    let degrader = AutoDegrader::new(reg.clone(), auto_cfg());
    degrader.run_once();

    let h = c.0.health();
    assert_eq!(h.state, HealthState::Degraded);
    assert!(h.reason.contains("p99"), "reason: {}", h.reason);
}

#[test]
fn auto_degrade_recovers_to_healthy_when_window_clears() {
    let reg = ComponentRegistry::new();
    let c = comp("stt:flap", ComponentKind::Stt);
    for _ in 0..3 {
        c.0.record_ok(Duration::from_millis(10));
    }
    for _ in 0..5 {
        c.0.record_error("boom");
    }
    reg.register(std::sync::Arc::<CoreBacked>::clone(&c));

    let degrader = AutoDegrader::new(reg.clone(), auto_cfg());
    degrader.run_once();
    assert_eq!(c.0.health().state, HealthState::Degraded);

    // Tick 2: a clean burst since the previous baseline.
    for _ in 0..20 {
        c.0.record_ok(Duration::from_millis(10));
    }
    degrader.run_once();
    assert_eq!(c.0.health().state, HealthState::Healthy);
}

#[test]
fn auto_degrade_leaves_unhealthy_alone() {
    let reg = ComponentRegistry::new();
    let c = comp("stt:dead", ComponentKind::Stt);
    c.0.mark_unhealthy("connection lost");
    // Plenty of fresh successful ops should not flip Unhealthy -> Healthy.
    for _ in 0..20 {
        c.0.record_ok(Duration::from_millis(5));
    }
    reg.register(std::sync::Arc::<CoreBacked>::clone(&c));

    let degrader = AutoDegrader::new(reg.clone(), auto_cfg());
    degrader.run_once();

    assert_eq!(c.0.health().state, HealthState::Unhealthy);
}

#[test]
fn auto_degrade_leaves_not_configured_alone() {
    let reg = ComponentRegistry::new();
    let c = Arc::new(CoreBacked(ComponentCore::new_not_configured(
        "tts:absent",
        ComponentKind::Tts,
    )));
    reg.register(std::sync::Arc::<CoreBacked>::clone(&c));

    let degrader = AutoDegrader::new(reg.clone(), auto_cfg());
    degrader.run_once();

    assert_eq!(c.0.health().state, HealthState::NotConfigured);
}

#[test]
fn auto_degrade_default_set_health_is_noop() {
    // A component that doesn't implement set_health (uses the trait
    // default no-op) must not be touched by the auto-degrader.
    struct Stub;
    impl Component for Stub {
        fn id(&self) -> &'static str {
            "stub:1"
        }
        fn kind(&self) -> ComponentKind {
            ComponentKind::Other
        }
        fn health(&self) -> HealthStatus {
            HealthStatus::healthy()
        }
        fn telemetry(&self) -> ComponentTelemetry {
            let mut telemetry = ComponentTelemetry::default();
            telemetry.errors_total = 100;
            telemetry
        }
    }
    let reg = ComponentRegistry::new();
    reg.register(Arc::new(Stub));
    let degrader = AutoDegrader::new(reg.clone(), auto_cfg());
    // Should not panic; the stub silently drops set_health.
    degrader.run_once();
    assert_eq!(reg.aggregate_health(), HealthState::Healthy);
}
