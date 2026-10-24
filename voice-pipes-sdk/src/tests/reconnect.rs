use super::*;

fn transient() -> SttError {
    SttError::NetworkError("boom".into())
}

/// A session that connected and died at once does not restore the
/// counter: the same failure repeated `max_attempts` times exhausts it.
#[test]
fn accepted_but_immediately_failing_sessions_exhaust_the_budget() {
    let mut budget = ReconnectBudget::new(3);
    let now = Instant::now();
    let session = SessionOutcomeInfo {
        established_at: Some(now),
        delivered_transcript: false,
    };
    assert_eq!(budget.on_failure(&session, now, &transient()).unwrap(), 1);
    assert_eq!(budget.on_failure(&session, now, &transient()).unwrap(), 2);
    assert_eq!(budget.on_failure(&session, now, &transient()).unwrap(), 3);
    let err = budget.on_failure(&session, now, &transient()).unwrap_err();
    assert!(
        matches!(err, SttError::ReconnectExhausted { attempts: 4, .. }),
        "{err}"
    );
    assert!(!err.is_transient(), "exhaustion is permanent");
}

/// A session that lived long enough, or delivered a transcript, restores
/// the counter, so an occasional drop on a long-lived stream keeps
/// reconnecting.
#[test]
fn healthy_sessions_restore_the_counter() {
    let mut budget = ReconnectBudget::new(2);
    let start = Instant::now();
    let short = SessionOutcomeInfo {
        established_at: Some(start),
        delivered_transcript: false,
    };
    assert_eq!(budget.on_failure(&short, start, &transient()).unwrap(), 1);
    assert_eq!(budget.on_failure(&short, start, &transient()).unwrap(), 2);

    let long_lived = SessionOutcomeInfo {
        established_at: Some(start),
        delivered_transcript: false,
    };
    let later = start + HEALTHY_SESSION_AGE;
    assert_eq!(
        budget.on_failure(&long_lived, later, &transient()).unwrap(),
        1,
        "a healthy session resets the count"
    );

    let productive = SessionOutcomeInfo {
        established_at: Some(later),
        delivered_transcript: true,
    };
    assert_eq!(
        budget.on_failure(&productive, later, &transient()).unwrap(),
        1
    );
}

#[test]
fn success_resets_the_counter() {
    let mut budget = ReconnectBudget::new(1);
    let now = Instant::now();
    let s = SessionOutcomeInfo::default();
    assert_eq!(budget.on_failure(&s, now, &transient()).unwrap(), 1);
    budget.on_success();
    assert_eq!(budget.on_failure(&s, now, &transient()).unwrap(), 1);
}

/// Healthy sessions cannot buy unlimited reconnects: the per-window cap
/// still ends the run.
#[test]
fn window_cap_ends_a_reconnect_storm() {
    let mut budget = ReconnectBudget::new(3);
    let start = Instant::now();
    let productive = SessionOutcomeInfo {
        established_at: Some(start),
        delivered_transcript: true,
    };
    let mut now = start;
    for _ in 0..MAX_RECONNECTS_PER_WINDOW {
        now += Duration::from_secs(1);
        assert_eq!(
            budget.on_failure(&productive, now, &transient()).unwrap(),
            1
        );
    }
    now += Duration::from_secs(1);
    let err = budget
        .on_failure(&productive, now, &transient())
        .unwrap_err();
    assert!(matches!(err, SttError::ReconnectExhausted { .. }), "{err}");

    // Once the window has moved on, reconnects are allowed again.
    let mut fresh = ReconnectBudget::new(3);
    for _ in 0..MAX_RECONNECTS_PER_WINDOW {
        fresh.on_failure(&productive, start, &transient()).unwrap();
    }
    let much_later = start + RECONNECT_WINDOW + Duration::from_secs(1);
    assert!(
        fresh
            .on_failure(&productive, much_later, &transient())
            .is_ok()
    );
}

#[test]
fn backoff_doubles_caps_and_never_collapses_to_zero() {
    let base = Duration::from_secs(1);
    let max = Duration::from_secs(30);
    assert_eq!(backoff_delay(base, max, 1), Duration::from_secs(1));
    assert_eq!(backoff_delay(base, max, 2), Duration::from_secs(2));
    assert_eq!(backoff_delay(base, max, 3), Duration::from_secs(4));
    assert_eq!(backoff_delay(base, max, 6), max);
    assert_eq!(
        backoff_delay(base, max, 33),
        max,
        "attempt 33 must not wrap to zero"
    );
    assert_eq!(backoff_delay(base, max, u32::MAX), max);
}

/// A jittered delay lands in the upper half of the plain one, and repeated
/// draws do not all agree.
#[test]
fn jitter_stays_in_the_upper_half_and_varies() {
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(10);
    let plain = backoff_delay(base, max, 3);
    let draws: Vec<Duration> = (0..32).map(|_| backoff_with_jitter(base, max, 3)).collect();
    assert!(
        draws.iter().all(|d| *d >= plain / 2 && *d <= plain),
        "{draws:?}"
    );
    assert!(draws.iter().any(|d| *d != draws[0]), "{draws:?}");
}
