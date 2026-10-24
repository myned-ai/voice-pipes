use super::*;

fn enabled_cfg() -> AdaptiveQualityConfig {
    AdaptiveQualityConfig {
        enabled: true,
        ..Default::default()
    }
}

/// An observation bad enough to indicate exactly `Reduced`.
fn reduced_sample() -> Observation {
    Observation {
        arrival_rtf: Some(1.2), // >= 1.15, < 1.4
        ..Default::default()
    }
}

fn minimal_sample() -> Observation {
    Observation {
        arrival_rtf: Some(1.5),
        ..Default::default()
    }
}

fn critical_sample() -> Observation {
    Observation {
        arrival_rtf: Some(2.5),
        ..Default::default()
    }
}

fn clean_sample() -> Observation {
    Observation {
        arrival_rtf: Some(1.0),
        quality: Some(ConnectionQuality::Excellent),
        ..Default::default()
    }
}

#[test]
fn tier_downgrades_after_two_bad_observations() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    assert_eq!(
        c.observe(reduced_sample()),
        QualityTier::Full,
        "one bad sample is noise"
    );
    assert_eq!(c.observe(reduced_sample()), QualityTier::Reduced);
}

#[tokio::test(start_paused = true)]
async fn tier_upgrades_only_after_clean_window() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    c.observe(reduced_sample());
    c.observe(reduced_sample());
    assert_eq!(c.tier(), QualityTier::Reduced);

    c.observe(clean_sample()); // starts the window
    tokio::time::advance(Duration::from_secs(29)).await;
    assert_eq!(
        c.observe(clean_sample()),
        QualityTier::Reduced,
        "29s < 30s window"
    );

    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(c.observe(clean_sample()), QualityTier::Full);
}

#[tokio::test(start_paused = true)]
async fn tier_climbs_one_step_at_a_time() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    c.observe(critical_sample());
    c.observe(critical_sample());
    assert_eq!(c.tier(), QualityTier::Critical);

    // One long clean stretch buys exactly one step, not a jump to Full.
    c.observe(clean_sample());
    tokio::time::advance(Duration::from_secs(61)).await;
    assert_eq!(c.observe(clean_sample()), QualityTier::Minimal);

    tokio::time::advance(Duration::from_secs(46)).await;
    assert_eq!(c.observe(clean_sample()), QualityTier::Reduced);

    tokio::time::advance(Duration::from_secs(31)).await;
    assert_eq!(c.observe(clean_sample()), QualityTier::Full);
}

/// A signal sitting astride a threshold must not ratchet the tier down:
/// every good sample breaks the streak, so alternating input never
/// accumulates the consecutive agreement a downgrade needs.
#[tokio::test(start_paused = true)]
async fn no_oscillation_under_borderline_input() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    for _ in 0..20 {
        c.observe(reduced_sample());
        c.observe(clean_sample());
    }
    assert_eq!(
        c.tier(),
        QualityTier::Full,
        "alternating input must not degrade"
    );
}

/// S2 is independent of S1: a healthy ping bucket does not veto the
/// client's own report that audio is arriving too slowly.
#[test]
fn arrival_rtf_alone_triggers_reduced() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    let o = Observation {
        quality: Some(ConnectionQuality::Good),
        arrival_rtf: Some(1.2),
        ..Default::default()
    };
    c.observe(o);
    assert_eq!(c.observe(o), QualityTier::Reduced);
}

#[test]
fn disabled_config_never_leaves_full() {
    let (mut c, _rx) = AdaptiveController::new(AdaptiveQualityConfig::default());
    for _ in 0..10 {
        c.observe(critical_sample());
    }
    assert_eq!(c.tier(), QualityTier::Full);
}

/// Each band maps to its own tier: a signal between the `Reduced` and
/// `Critical` thresholds must land on `Minimal` exactly, neither rounding
/// down to the mildest tier nor panicking to the worst.
#[test]
fn each_threshold_band_maps_to_its_own_tier() {
    for (sample, expected) in [
        (reduced_sample(), QualityTier::Reduced),
        (minimal_sample(), QualityTier::Minimal),
        (critical_sample(), QualityTier::Critical),
    ] {
        let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
        c.observe(sample);
        assert_eq!(c.observe(sample), expected, "for {sample:?}");
    }
}

/// A collapsing link lands on the tier its signals indicate rather than
/// walking down one step per observation -- downgrades may skip.
#[test]
fn downgrade_may_skip_tiers() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    c.observe(critical_sample());
    assert_eq!(c.observe(critical_sample()), QualityTier::Critical);
}

/// The worst signal wins: one failing test degrades the session however
/// healthy the others look.
#[test]
fn worst_signal_decides_the_tier() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    let o = Observation {
        quality: Some(ConnectionQuality::Excellent),
        arrival_rtf: Some(1.0),
        send_latency_ms: Some(3000), // critical
        ..Default::default()
    };
    c.observe(o);
    assert_eq!(c.observe(o), QualityTier::Critical);
}

/// `Lost` means the heartbeat watchdog is tearing the session down; there
/// is nothing left to adapt, so it must not stampede the tier on the way
/// out.
#[test]
fn lost_connection_does_not_vote() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    let o = Observation {
        quality: Some(ConnectionQuality::Lost),
        ..Default::default()
    };
    c.observe(o);
    assert_eq!(c.observe(o), QualityTier::Full);
}

/// Diagnostics ride along even while the tier is pinned, so an operator
/// can see what the controller *would* have done before enabling it.
#[test]
fn conditions_are_published_while_disabled() {
    let (mut c, rx) = AdaptiveController::new(AdaptiveQualityConfig::default());
    c.observe(Observation {
        rtt_ms: Some(420),
        arrival_rtf: Some(1.9),
        ..Default::default()
    });
    let seen = *rx.borrow();
    assert_eq!(seen.tier, QualityTier::Full);
    assert_eq!(seen.rtt_ms, Some(420));
    assert_eq!(seen.arrival_rtf, Some(1.9));
}

/// The reporter must be quiet while the tier holds. A control frame per
/// observation would compete with the audio the tier exists to protect.
#[tokio::test(start_paused = true)]
async fn quality_reporter_emits_only_on_tier_changes() {
    use crate::transport::{ServerControl, ServerMessage};
    let (mut c, rx) = AdaptiveController::new(enabled_cfg());
    let (out_tx, mut out_rx) = mpsc::channel(16);
    spawn_quality_reporter(rx, out_tx);

    // Ten clean observations, no transition: nothing to say.
    for _ in 0..10 {
        c.observe(clean_sample());
    }
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;
    assert!(out_rx.try_recv().is_err(), "no frame while the tier holds");

    // Two bad ones drop to Reduced -- exactly one frame.
    c.observe(reduced_sample());
    c.observe(reduced_sample());
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    match out_rx.try_recv() {
        Ok(ServerMessage::Control(ServerControl::QualityUpdate { tier, .. })) => {
            assert_eq!(tier, QualityTier::Reduced);
        }
        other => panic!("expected a QualityUpdate, got {other:?}"),
    }
    assert!(
        out_rx.try_recv().is_err(),
        "exactly one frame per transition"
    );
}

/// Staying exactly at the current tier is not evidence of recovery: the
/// clean window must not accumulate while the link is still bad.
#[tokio::test(start_paused = true)]
async fn holding_at_the_current_tier_does_not_earn_an_upgrade() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    c.observe(reduced_sample());
    c.observe(reduced_sample());
    assert_eq!(c.tier(), QualityTier::Reduced);

    for _ in 0..10 {
        tokio::time::advance(Duration::from_secs(10)).await;
        c.observe(reduced_sample());
    }
    assert_eq!(c.tier(), QualityTier::Reduced, "still degraded, no upgrade");
}

/// Streaks are per signal. The socket-write sample arrives many times
/// between two playback reports and is almost always healthy; it must
/// not reset the playback report's streak, or a link with steady
/// underruns would never leave `Full`.
#[test]
fn send_latency_samples_do_not_reset_an_arrival_streak() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    let healthy_write = Observation {
        send_latency_ms: Some(5),
        ..Default::default()
    };
    c.observe(reduced_sample());
    for _ in 0..25 {
        assert_eq!(c.observe(healthy_write), QualityTier::Full);
    }
    assert_eq!(
        c.observe(reduced_sample()),
        QualityTier::Reduced,
        "the second playback report completes the arrival streak"
    );
}

/// A healthy sample of the *same* signal still breaks its streak.
#[test]
fn a_healthy_sample_of_the_same_signal_breaks_its_streak() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    c.observe(reduced_sample());
    c.observe(Observation {
        arrival_rtf: Some(1.0),
        ..Default::default()
    });
    assert_eq!(c.observe(reduced_sample()), QualityTier::Full);
}

/// The ping/pong bucket alone can degrade the session.
#[test]
fn rtt_only_degradation_reaches_reduced() {
    let (mut c, _rx) = AdaptiveController::new(enabled_cfg());
    let poor = Observation {
        quality: Some(ConnectionQuality::Poor),
        rtt_ms: Some(650),
        ..Default::default()
    };
    assert_eq!(c.observe(poor), QualityTier::Full);
    assert_eq!(c.observe(poor), QualityTier::Reduced);
}

/// Observations are sparse; the published view keeps the last value of
/// every signal instead of reverting the others to unknown, so the
/// `quality_update` frame reports the real ping bucket.
#[test]
fn sparse_observations_keep_previous_diagnostics() {
    let (mut c, rx) = AdaptiveController::new(enabled_cfg());
    c.observe(Observation {
        quality: Some(ConnectionQuality::Poor),
        rtt_ms: Some(700),
        ..Default::default()
    });
    c.observe(Observation {
        send_latency_ms: Some(12),
        ..Default::default()
    });
    let seen = *rx.borrow();
    assert_eq!(seen.quality, ConnectionQuality::Poor);
    assert_eq!(seen.rtt_ms, Some(700));
    assert_eq!(seen.send_latency_ms, Some(12));
}

/// The health feed turns bucket changes into observations carrying the
/// bucket and the smoothed RTT, and stops when the session drops the
/// monitor.
#[tokio::test]
async fn health_feed_reports_bucket_changes_with_rtt() {
    use crate::transport::health::{ConnectionHealthMonitor, HealthConfig};
    let monitor = std::sync::Arc::new(ConnectionHealthMonitor::new(HealthConfig::default()));
    let (tx, mut obs_rx) = mpsc::channel::<Observation>(8);
    spawn_health_feed(&monitor, ObservationSink { tx });

    for _ in 0..10 {
        monitor.record_pong(Duration::from_millis(600));
    }
    let o = tokio::time::timeout(Duration::from_secs(2), obs_rx.recv())
        .await
        .expect("observation")
        .expect("feed alive");
    assert_eq!(o.quality, Some(ConnectionQuality::Poor));
    assert!(o.rtt_ms.is_some_and(|ms| ms > 300));

    drop(monitor);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), obs_rx.recv())
            .await
            .expect("feed must end when the monitor is gone")
            .is_none()
    );
}
