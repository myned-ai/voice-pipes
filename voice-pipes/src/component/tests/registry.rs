use super::ComponentRegistry;
use crate::component::test_support::CoreBacked;
use crate::component::test_support::comp;
use crate::component::{ComponentKind, HealthState};

#[test]
fn registry_register_and_snapshot() {
    let reg = ComponentRegistry::new();
    reg.register(comp("z:a", ComponentKind::Tts));
    reg.register(comp("a:b", ComponentKind::Stt));
    reg.register(comp("m:c", ComponentKind::Text));

    assert_eq!(reg.len(), 3);
    let snap = reg.snapshot();
    let ids: Vec<&str> = snap.iter().map(|c| c.id()).collect();
    // Sorted alphabetically.
    assert_eq!(ids, vec!["a:b", "m:c", "z:a"]);

    assert!(reg.get("a:b").is_some());
    assert!(reg.remove("a:b").is_some());
    assert!(reg.get("a:b").is_none());
    assert_eq!(reg.len(), 2);
}

#[test]
fn registry_aggregate_health_picks_worst() {
    let reg = ComponentRegistry::new();
    let healthy = comp("a:1", ComponentKind::Stt);
    let degraded = comp("b:1", ComponentKind::Tts);
    degraded.0.mark_degraded("slow");
    reg.register(healthy);
    reg.register(std::sync::Arc::<CoreBacked>::clone(&degraded));
    assert_eq!(reg.aggregate_health(), HealthState::Degraded);

    let bad = comp("c:1", ComponentKind::Text);
    bad.0.mark_unhealthy("model not loaded");
    reg.register(bad);
    assert_eq!(reg.aggregate_health(), HealthState::Unhealthy);
}

#[test]
fn registry_concurrent_registration_is_safe() {
    use std::thread;
    let reg = ComponentRegistry::new();
    let mut handles = Vec::new();
    for i in 0..16 {
        let r = reg.clone();
        handles.push(thread::spawn(move || {
            r.register(comp(&format!("stt:{i}"), ComponentKind::Stt));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(reg.len(), 16);
}
