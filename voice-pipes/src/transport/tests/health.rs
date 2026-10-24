use super::*;

fn test_config() -> HealthConfig {
    HealthConfig {
        ping_interval: Duration::from_millis(100),
        max_missed_pongs: 2,
        rtt_window: 5,
        thresholds: QualityThresholds {
            excellent_rtt_ms: 50,
            good_rtt_ms: 200,
        },
    }
}

#[test]
fn initial_quality_is_good() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    assert_eq!(monitor.quality(), ConnectionQuality::Good);
}

#[test]
fn excellent_after_low_rtt() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    monitor.record_pong(Duration::from_millis(10));
    assert_eq!(monitor.quality(), ConnectionQuality::Excellent);
}

#[test]
fn good_after_moderate_rtt() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    // Seed with a moderate RTT
    for _ in 0..5 {
        monitor.record_pong(Duration::from_millis(150));
    }
    assert_eq!(monitor.quality(), ConnectionQuality::Good);
}

#[test]
fn poor_after_high_rtt() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    for _ in 0..10 {
        monitor.record_pong(Duration::from_millis(400));
    }
    assert_eq!(monitor.quality(), ConnectionQuality::Poor);
}

#[test]
fn lost_after_missed_pongs() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    monitor.record_pong(Duration::from_millis(10)); // start excellent
    monitor.record_missed_pong();
    assert_ne!(monitor.quality(), ConnectionQuality::Lost); // 1 miss, not lost yet
    monitor.record_missed_pong();
    assert_eq!(monitor.quality(), ConnectionQuality::Lost); // 2 misses = lost
}

#[test]
fn recovery_after_pong() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    monitor.record_missed_pong();
    monitor.record_missed_pong();
    assert_eq!(monitor.quality(), ConnectionQuality::Lost);

    // A successful pong should recover
    monitor.record_pong(Duration::from_millis(20));
    assert_eq!(monitor.quality(), ConnectionQuality::Excellent);
    assert_eq!(monitor.missed_pong_count(), 0);
}

// A pong with a non-echoing payload keeps the connection alive
// (resets the miss counter) but must not fabricate an RTT sample.
#[test]
fn liveness_resets_missed_without_rtt_sample() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    monitor.record_missed_pong();
    assert_eq!(monitor.missed_pong_count(), 1);

    monitor.record_liveness();
    assert_eq!(
        monitor.missed_pong_count(),
        0,
        "liveness clears the miss count"
    );
    assert!(
        monitor.rtt_ms().is_none(),
        "liveness must not add a bogus RTT sample"
    );
    assert_ne!(monitor.quality(), ConnectionQuality::Lost);
}

// A stream of foreign-payload pongs can never drive the connection Lost,
// even though none of them yields an RTT sample.
#[test]
fn repeated_liveness_never_declares_lost() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    for _ in 0..10 {
        monitor.record_missed_pong();
        monitor.record_liveness();
    }
    assert_ne!(monitor.quality(), ConnectionQuality::Lost);
    assert!(monitor.rtt_ms().is_none());
}

#[test]
fn rtt_ema_converges() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    // All 100ms samples
    for _ in 0..20 {
        monitor.record_pong(Duration::from_millis(100));
    }
    let rtt = monitor.rtt_ms().unwrap();
    // EMA should converge near 100
    assert!(
        (90..=110).contains(&rtt),
        "RTT EMA was {rtt}, expected ~100"
    );
}

#[test]
fn no_rtt_before_first_pong() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    assert!(monitor.rtt_ms().is_none());
}

#[test]
fn reset_clears_state() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    monitor.record_missed_pong();
    monitor.record_missed_pong();
    assert_eq!(monitor.quality(), ConnectionQuality::Lost);

    monitor.reset();
    assert_eq!(monitor.quality(), ConnectionQuality::Good);
    assert!(monitor.rtt_ms().is_none());
    assert_eq!(monitor.missed_pong_count(), 0);
}

#[test]
fn watch_receives_quality_changes() {
    let monitor = ConnectionHealthMonitor::new(test_config());
    let mut rx = monitor.quality_rx();

    // Initial value
    assert_eq!(*rx.borrow(), ConnectionQuality::Good);

    // Trigger change to excellent
    monitor.record_pong(Duration::from_millis(10));
    assert!(rx.has_changed().unwrap());
    assert_eq!(*rx.borrow_and_update(), ConnectionQuality::Excellent);
}
