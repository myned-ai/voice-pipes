use super::*;

fn ring() -> PlaybackRing {
    Arc::new(parking_lot::Mutex::new(VecDeque::new()))
}

#[test]
fn push_bounded_respects_cap_and_reports_drops() {
    let mut buf = VecDeque::new();
    // Cap of 10 ring values, stereo fan-out: 5 source samples fit.
    let samples: Vec<i16> = (0..8).collect();
    let dropped = push_bounded(&mut buf, &samples, 2, 10);
    assert_eq!(buf.len(), 10);
    assert_eq!(dropped, 3);

    // Further pushes drop everything.
    let dropped = push_bounded(&mut buf, &samples, 2, 10);
    assert_eq!(buf.len(), 10);
    assert_eq!(dropped, 8);
}

#[test]
fn push_bounded_under_cap_drops_nothing() {
    let mut buf = VecDeque::new();
    let dropped = push_bounded(&mut buf, &[1, 2, 3], 2, 100);
    assert_eq!(dropped, 0);
    assert_eq!(buf.len(), 6);
}

/// The RT callback must not block when the ring lock is held
/// elsewhere — it emits silence for that quantum instead.
#[test]
fn callback_emits_silence_on_lock_contention() {
    let ring = ring();
    ring.lock().extend([0.5f32; 8]);

    // Hold the lock (parking_lot::Mutex is not reentrant, so try_lock
    // from the same thread returns None, same as cross-thread
    // contention).
    let guard = ring.lock();
    let mut data = [1.0f32; 4];
    playback_callback(&ring, &mut data);
    assert_eq!(data, [0.0; 4], "contended callback must emit silence");
    drop(guard);

    // Uncontended: plays queued samples.
    let mut data = [0.0f32; 4];
    playback_callback(&ring, &mut data);
    assert_eq!(data, [0.5; 4]);
}

/// A producer panicking while holding the ring lock must not
/// take playback down — `parking_lot` cannot poison, so the lock is
/// simply released on unwind and every ring operation keeps working.
#[test]
fn panicking_lock_holder_does_not_break_the_ring() {
    let ring = ring();
    ring.lock().extend([0.25f32; 4]);

    // Panic while holding the lock (previously poisoned a std mutex).
    let ring_p = Arc::clone(&ring);
    let _ = std::thread::spawn(move || {
        let _guard = ring_p.lock();
        panic!("unwind while holding the ring lock");
    })
    .join();

    let mut data = [0.0f32; 2];
    playback_callback(&ring, &mut data);
    assert_eq!(
        data, [0.25; 2],
        "callback must keep playing after a holder panicked"
    );

    // feed/clear/len paths must not panic either.
    let frame = AudioFrame {
        samples: vec![100, 200],
        sample_rate: 48_000,
        channels: 1,
    };
    feed_playback_ring(&ring, &frame, 1);
    clear_playback_ring(&ring);
    assert!(ring.lock().is_empty());
}
