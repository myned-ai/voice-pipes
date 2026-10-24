use super::*;

#[test]
fn core_counters_and_telemetry() {
    let core = ComponentCore::new("tts:test", ComponentKind::Tts);
    core.record_ok(Duration::from_millis(10));
    core.record_ok(Duration::from_millis(30));
    core.record_error("boom");
    core.record_timeout();
    let t = core.telemetry();
    assert_eq!(t.processed_total, 2);
    assert_eq!(t.errors_total, 2, "timeout counts as an error too");
    assert_eq!(t.timeouts_total, 1);
    assert_eq!(t.last_error.as_deref(), Some("timeout"));
    assert_eq!(t.latency_ms.min, 10);
    assert_eq!(t.latency_ms.max, 30);
}

#[test]
fn latency_summary_percentiles() {
    let core = ComponentCore::new("stt:test", ComponentKind::Stt);
    for ms in 1..=100u64 {
        core.record_ok(Duration::from_millis(ms));
    }
    let l = core.telemetry().latency_ms;
    assert_eq!(l.p50, 50);
    assert_eq!(l.p90, 90);
    assert_eq!(l.p99, 99);
    assert_eq!(l.mean, 50);
    assert_eq!(l.min, 1);
    assert_eq!(l.max, 100);
}

#[test]
fn set_health_preserves_since_when_unchanged() {
    let core = ComponentCore::new("x", ComponentKind::Other);
    core.mark_degraded("slow");
    let first = core.health();
    std::thread::sleep(Duration::from_millis(5));
    core.mark_degraded("slow"); // identical state+reason -> no transition
    assert_eq!(core.health().since, first.since);
    core.mark_unhealthy("down"); // real transition
    assert_eq!(core.health().state, HealthState::Unhealthy);
}

#[test]
fn extras_flow_into_telemetry() {
    let core = ComponentCore::new("llm:x", ComponentKind::Text);
    core.set_extra("tokens_per_s", serde_json::json!(42.5));
    core.extras_with(|m| {
        m.insert("chars".into(), serde_json::json!(7));
    });
    let extras = core.telemetry().extras;
    assert_eq!(extras["tokens_per_s"], serde_json::json!(42.5));
    assert_eq!(extras["chars"], serde_json::json!(7));
}

#[test]
fn core_component_delegates() {
    let core = Arc::new(ComponentCore::new_not_configured(
        "vad:x",
        ComponentKind::Vad,
    ));
    let c = CoreComponent::new(Arc::clone(&core))
        .with_error_policy(ErrorPolicy::RetryWithBackoff { max: 3 });
    assert_eq!(c.id(), "vad:x");
    assert_eq!(c.kind(), ComponentKind::Vad);
    assert_eq!(c.health().state, HealthState::NotConfigured);
    assert!(matches!(
        c.error_policy(),
        ErrorPolicy::RetryWithBackoff { max: 3 }
    ));
    c.set_health(HealthStatus::healthy());
    assert_eq!(core.health().state, HealthState::Healthy);
}

// ── Moved from the engine's own component copy ──────────────

#[test]
fn core_records_ok_and_errors_with_last_error() {
    let c = ComponentCore::new("stt:test", ComponentKind::Stt);
    c.record_ok(Duration::from_millis(10));
    c.record_ok(Duration::from_millis(20));
    c.record_error("boom");

    let t = c.telemetry();
    assert_eq!(t.processed_total, 2);
    assert_eq!(t.errors_total, 1);
    assert_eq!(t.timeouts_total, 0);
    assert_eq!(t.last_error.as_deref(), Some("boom"));
    assert_eq!(t.latency_ms.min, 10);
    assert_eq!(t.latency_ms.max, 20);
}

#[test]
fn timeout_increments_both_counters() {
    let c = ComponentCore::new("tts:test", ComponentKind::Tts);
    c.record_timeout();
    let t = c.telemetry();
    assert_eq!(t.timeouts_total, 1);
    assert_eq!(t.errors_total, 1);
    assert_eq!(t.last_error.as_deref(), Some("timeout"));
}

#[test]
fn latency_window_caps_samples() {
    let mut w = LatencyWindow::new(4);
    for v in [10, 20, 30, 40, 50, 60] {
        w.push(v);
    }
    // After overflow, only the last 4 samples remain.
    let s = w.summary();
    assert_eq!(s.min, 30);
    assert_eq!(s.max, 60);
}

#[test]
fn percentile_picks_nearest_rank() {
    let mut w = LatencyWindow::new(10);
    for v in 1..=10u64 {
        w.push(v);
    }
    let s = w.summary();
    assert_eq!(s.min, 1);
    assert_eq!(s.max, 10);
    // Nearest-rank: ceil(0.5*10)=5 -> index 4 -> 5
    assert_eq!(s.p50, 5);
    // ceil(0.9*10)=9 -> index 8 -> 9
    assert_eq!(s.p90, 9);
    assert_eq!(s.p99, 10);
    assert_eq!(s.mean, 5);
}

#[test]
fn health_transitions_update_since() {
    let c = ComponentCore::new("stt:test", ComponentKind::Stt);
    let h0 = c.health();
    assert_eq!(h0.state, HealthState::Healthy);

    // Same state + reason -> no transition (since unchanged).
    c.mark_healthy();
    let h1 = c.health();
    assert_eq!(h1.since, h0.since);

    std::thread::sleep(Duration::from_millis(2));
    c.mark_degraded("slow upstream");
    let h2 = c.health();
    assert_eq!(h2.state, HealthState::Degraded);
    assert_eq!(h2.reason, "slow upstream");
    assert!(h2.since > h0.since);
}

#[test]
fn telemetry_extras_round_trip_through_core() {
    let core = ComponentCore::new("llm:test", ComponentKind::Text);
    core.set_extra("tokens_per_sec", serde_json::json!(42.5));
    core.extras_with(|m| {
        m.insert("model".into(), serde_json::json!("gpt-4o"));
        m.insert("cost_usd".into(), serde_json::json!(0.0021));
    });

    let t = core.telemetry();
    assert_eq!(
        t.extras.get("tokens_per_sec"),
        Some(&serde_json::json!(42.5))
    );
    assert_eq!(t.extras.get("model"), Some(&serde_json::json!("gpt-4o")));
    assert_eq!(t.extras.get("cost_usd"), Some(&serde_json::json!(0.0021)));

    // Round-trip via JSON to confirm wire compatibility.
    let json = serde_json::to_string(&t).unwrap();
    let back: ComponentTelemetry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.extras.len(), 3);
}

#[test]
fn not_configured_constructor() {
    let c = ComponentCore::new_not_configured("tts:absent", ComponentKind::Tts);
    assert_eq!(c.health().state, HealthState::NotConfigured);
}

#[test]
fn health_state_worst_orders_correctly() {
    use HealthState::*;
    assert_eq!(HealthState::worst(Healthy, Degraded), Degraded);
    assert_eq!(HealthState::worst(Degraded, Unhealthy), Unhealthy);
    assert_eq!(HealthState::worst(Unhealthy, Healthy), Unhealthy);
    assert_eq!(HealthState::worst(NotConfigured, Healthy), Healthy);
    assert_eq!(HealthState::worst(NotConfigured, Unhealthy), Unhealthy);
}

#[test]
fn telemetry_empty_extras_are_skipped_in_json() {
    // Old admin backends must not see a noisy `"extras": {}` key.
    let t = ComponentTelemetry::default();
    let s = serde_json::to_string(&t).unwrap();
    assert!(
        !s.contains("extras"),
        "extras should be skipped when empty: {s}"
    );
}
