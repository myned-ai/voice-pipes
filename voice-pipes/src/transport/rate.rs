//! Inbound audio rate limiting.
//!
//! A client that streams audio much faster than realtime is either
//! hostile or broken; without a limiter it drives STT buffering and CPU
//! without bound. The bucket is deliberately generous — real microphones
//! produce exactly 1× realtime, so headroom only needs to absorb network
//! jitter, reconnect replays, and batched delivery.

use std::time::Instant;

/// Sustained admission rate as a multiple of realtime.
const RATE_HEADROOM: f64 = 2.0;

/// Burst allowance, in seconds of audio.
const BURST_SECONDS: f64 = 10.0;

/// Token bucket over PCM samples: each admitted sample consumes one
/// token; tokens refill at `RATE_HEADROOM ×` the stream's sample rate up
/// to a `BURST_SECONDS` ceiling. `tokens` may go negative (debt) after an
/// oversized single frame is admitted — see [`Self::admit`].
#[derive(Debug)]
pub struct AudioRateLimiter {
    sample_rate: u32,
    refill_per_sec: f64,
    burst: f64,
    tokens: f64,
    last_refill: Instant,
}

impl AudioRateLimiter {
    pub fn new(sample_rate: u32) -> Self {
        let sr = f64::from(sample_rate.max(1));
        let burst = sr * BURST_SECONDS;
        Self {
            sample_rate,
            refill_per_sec: sr * RATE_HEADROOM,
            burst,
            tokens: burst,
            last_refill: Instant::now(),
        }
    }

    /// The sample rate this limiter's budget was sized for. Callers
    /// rebuild the limiter when the session's negotiated rate changes.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Admit or reject a chunk of `samples` PCM samples. Rejected chunks
    /// consume no tokens (the caller drops them).
    ///
    /// A single frame may legally exceed the burst ceiling (the 1 MiB
    /// frame cap allows ~11 s of 48 kHz audio vs the 10 s burst), so the
    /// admission check charges at most `burst`: an oversized frame passes
    /// only on a *full* bucket, drives the bucket into debt for its true
    /// cost, and sustained oversized frames therefore still cannot exceed
    /// the refill rate. Without the clamp such a frame was permanently
    /// unadmittable and silently lost on every retry.
    pub fn admit(&mut self, samples: usize) -> bool {
        self.admit_at(samples, Instant::now())
    }

    fn admit_at(&mut self, samples: usize, now: Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.burst);
        self.last_refill = now;
        let charge = (samples as f64).min(self.burst);
        if self.tokens >= charge {
            // Debit the true cost, not the clamped charge: an oversized
            // frame leaves the bucket in debt so follow-up audio waits
            // until the refill has paid it back.
            self.tokens -= samples as f64;
            true
        } else {
            false
        }
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/rate.rs"]
mod tests;
