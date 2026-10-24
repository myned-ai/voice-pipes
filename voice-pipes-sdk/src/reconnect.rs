//! Reconnect budget for hosted streaming STT clients, built in or plugin.
//!
//! A streaming session that dies is reopened with exponential backoff, but
//! not forever: a provider that accepts the connection and then rejects the
//! stream on every attempt (an unsupported sample rate, a model the account
//! cannot use, an exhausted quota phrased as a transient error) would loop
//! connect / fail / back off indefinitely if every accepted connection
//! restored the attempt counter. The budget only restores it for a session
//! that proved itself, and caps how often reconnects may happen at all.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::stt::SttError;

/// A session that lived at least this long counts as healthy and restores
/// the attempt counter when it later fails.
pub const HEALTHY_SESSION_AGE: Duration = Duration::from_secs(10);
/// Hard cap on reconnects within [`RECONNECT_WINDOW`], regardless of how
/// healthy the sessions in between looked.
pub const MAX_RECONNECTS_PER_WINDOW: u32 = 20;
/// The window [`MAX_RECONNECTS_PER_WINDOW`] is counted over.
pub const RECONNECT_WINDOW: Duration = Duration::from_mins(5);

/// What one streaming session achieved before it ended.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct SessionOutcomeInfo {
    /// When the provider accepted the session (`Begin`, response headers).
    pub established_at: Option<Instant>,
    /// At least one transcript reached the pipeline.
    pub delivered_transcript: bool,
}

impl SessionOutcomeInfo {
    /// A session earns a fresh attempt counter by lasting a while or by
    /// producing something; a session that was accepted and then died at
    /// once proves nothing.
    pub fn earned_reset(&self, now: Instant) -> bool {
        self.delivered_transcript
            || self
                .established_at
                .is_some_and(|t| now.duration_since(t) >= HEALTHY_SESSION_AGE)
    }
}

/// Tracks consecutive failed attempts and reconnects per time window.
#[derive(Debug)]
pub struct ReconnectBudget {
    max_attempts: u32,
    attempt: u32,
    recent: VecDeque<Instant>,
}

impl ReconnectBudget {
    /// A budget allowing `max_attempts` consecutive failures.
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            attempt: 0,
            recent: VecDeque::new(),
        }
    }

    /// Current consecutive-failure count (1-based after the first failure).
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The session ended in a way that is not a failure (completed, or a
    /// routine rotation): the consecutive count restarts.
    pub fn on_success(&mut self) {
        self.attempt = 0;
    }

    /// A session failed with a transient error. Returns the attempt number
    /// to reconnect with, or the error to give up with when the budget is
    /// spent: more than `max_attempts` consecutive failures without a
    /// healthy session in between, or more than
    /// [`MAX_RECONNECTS_PER_WINDOW`] reconnects inside [`RECONNECT_WINDOW`].
    pub fn on_failure(
        &mut self,
        session: &SessionOutcomeInfo,
        now: Instant,
        cause: &SttError,
    ) -> Result<u32, SttError> {
        if session.earned_reset(now) {
            self.attempt = 0;
        }
        self.attempt += 1;
        if self.attempt > self.max_attempts {
            return Err(SttError::ReconnectExhausted {
                attempts: self.attempt,
                reason: format!("{cause}"),
            });
        }
        while self
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) > RECONNECT_WINDOW)
        {
            self.recent.pop_front();
        }
        if self.recent.len() as u32 >= MAX_RECONNECTS_PER_WINDOW {
            return Err(SttError::ReconnectExhausted {
                attempts: self.recent.len() as u32,
                reason: format!(
                    "{} reconnects within {} s; last error: {cause}",
                    self.recent.len(),
                    RECONNECT_WINDOW.as_secs()
                ),
            });
        }
        self.recent.push_back(now);
        Ok(self.attempt)
    }
}

/// Exponential backoff for reconnect attempt `attempt` (1-based): `base`
/// doubled per attempt, capped at `max`. The exponent is clamped so a long
/// run of failures cannot overflow the multiplier to zero delay.
pub fn backoff_delay(base: Duration, max: Duration, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(31);
    let multiplier = 1u32 << exponent;
    base.saturating_mul(multiplier).min(max)
}

/// [`backoff_delay`], jittered: a random point in its upper half, so the
/// clients that failed together do not all come back at the same instant.
pub fn backoff_with_jitter(base: Duration, max: Duration, attempt: u32) -> Duration {
    jittered(backoff_delay(base, max, attempt))
}

/// `delay` scaled by a random factor in `[0.5, 1.0]`.
pub fn jittered(delay: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    // A per-process random key (the standard library seeds it from the
    // OS), mixed with the clock: enough spread for backoff, no dependency.
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    let fraction =
        f64::from(u32::try_from(hasher.finish() >> 32).unwrap_or(u32::MAX)) / f64::from(u32::MAX);
    delay.mul_f64(0.5 + fraction / 2.0)
}

#[cfg(test)]
#[path = "tests/reconnect.rs"]
mod tests;
