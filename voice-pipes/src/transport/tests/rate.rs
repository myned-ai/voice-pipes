use super::*;
use std::time::Duration;

#[test]
fn realtime_stream_is_never_dropped() {
    let mut rl = AudioRateLimiter::new(16_000);
    let start = Instant::now();
    // 10 minutes of 100 ms chunks at exactly 1× realtime.
    for i in 0..6_000u64 {
        let now = start + Duration::from_millis(i * 100);
        assert!(rl.admit_at(1_600, now), "chunk {i} dropped at 1x realtime");
    }
}

#[test]
fn firehose_is_throttled_after_burst() {
    let mut rl = AudioRateLimiter::new(16_000);
    let now = Instant::now();
    // Instant delivery (no elapsed time): the 10 s burst covers 100
    // chunks of 100 ms; everything after must be rejected.
    let admitted = (0..1_000).filter(|_| rl.admit_at(1_600, now)).count();
    assert_eq!(admitted, 100, "burst must cap instant audio at 10s worth");
}

#[test]
fn bucket_refills_after_idle() {
    let mut rl = AudioRateLimiter::new(16_000);
    let now = Instant::now();
    while rl.admit_at(1_600, now) {}
    assert!(!rl.admit_at(1_600, now));
    // After 1 s idle the 2× refill covers a 100 ms chunk again.
    let later = now + Duration::from_secs(1);
    assert!(rl.admit_at(1_600, later));
}

/// A codec-legal single frame larger than the burst (1 MiB ≈
/// 524k samples vs the 480k-token burst at 48 kHz) must be admittable
/// from a full bucket — the old `tokens >= samples` check made it
/// permanently unadmittable, silently losing the frame on every retry.
#[test]
fn oversized_single_frame_admitted_from_full_bucket() {
    let mut rl = AudioRateLimiter::new(48_000);
    let now = Instant::now();
    // ~10.9 s of audio in one frame (> 10 s burst): passes on a full bucket.
    assert!(
        rl.admit_at(524_000, now),
        "oversized frame must pass on a full bucket"
    );
    // The bucket is now in debt: an immediate follow-up chunk is rejected.
    assert!(
        !rl.admit_at(4_800, now),
        "bucket must be in debt after an oversized frame"
    );
}

/// Sustained oversized frames still cannot beat the refill
/// rate — each needs a *full* bucket, so their effective throughput is
/// bounded by `RATE_HEADROOM × realtime` like everything else.
#[test]
fn sustained_oversized_frames_bounded_by_refill() {
    let mut rl = AudioRateLimiter::new(48_000);
    let now = Instant::now();
    assert!(rl.admit_at(524_000, now));
    // 1 s later the debt (-44k) has only refilled to ~52k tokens —
    // far from the full bucket an oversized frame requires.
    assert!(!rl.admit_at(524_000, now + Duration::from_secs(1)));
    // ~6 s later the bucket is full again and the next one passes.
    assert!(rl.admit_at(524_000, now + Duration::from_secs(6)));
}

#[test]
fn reports_the_rate_it_was_sized_for() {
    assert_eq!(AudioRateLimiter::new(16_000).sample_rate(), 16_000);
}
