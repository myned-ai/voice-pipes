//! Connection health monitor -- ping/pong tracking and quality scoring.
//!
//! Tracks round-trip time, missed pongs, and derives a
//! [`ConnectionQuality`] score that consumers can subscribe to
//! via a `tokio::sync::watch` channel.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::watch;

use super::ConnectionQuality;

// ── Configuration ───────────────────────────────────────────

/// Configuration for the health monitor.
#[derive(Debug, Clone)]
pub struct HealthConfig {
    /// Interval between application-level pings.
    pub ping_interval: Duration,
    /// Number of consecutive missed pongs before declaring lost.
    pub max_missed_pongs: u32,
    /// Maximum number of RTT samples to keep for averaging.
    pub rtt_window: usize,
    /// Thresholds for quality scoring.
    pub thresholds: QualityThresholds,
}

/// Default ping interval: 15 seconds.
const DEFAULT_PING_INTERVAL_SECS: u64 = 15;

/// Default maximum missed pongs before declaring lost.
const DEFAULT_MAX_MISSED_PONGS: u32 = 2;

/// Default RTT averaging window size.
const DEFAULT_RTT_WINDOW: usize = 10;

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(DEFAULT_PING_INTERVAL_SECS),
            max_missed_pongs: DEFAULT_MAX_MISSED_PONGS,
            rtt_window: DEFAULT_RTT_WINDOW,
            thresholds: QualityThresholds::default(),
        }
    }
}

/// RTT thresholds for quality buckets.
///
/// Any RTT above `good_rtt_ms` is `Poor`. `Lost` is not an RTT bucket: it
/// is declared only when `max_missed_pongs` pings go unanswered, because a
/// slow answer and no answer call for different reactions (degrade versus
/// tear down).
#[derive(Debug, Clone)]
pub struct QualityThresholds {
    /// RTT at or below this -> Excellent.
    pub excellent_rtt_ms: u64,
    /// RTT at or below this -> Good; above -> Poor.
    pub good_rtt_ms: u64,
}

/// Default excellent threshold: 100 ms.
const DEFAULT_EXCELLENT_RTT_MS: u64 = 100;

/// Default good threshold: 300 ms.
const DEFAULT_GOOD_RTT_MS: u64 = 300;

impl Default for QualityThresholds {
    fn default() -> Self {
        Self {
            excellent_rtt_ms: DEFAULT_EXCELLENT_RTT_MS,
            good_rtt_ms: DEFAULT_GOOD_RTT_MS,
        }
    }
}

// ── Internal state ──────────────────────────────────────────

/// Mutable health state, protected by a lock.
#[derive(Debug)]
struct HealthState {
    last_pong_at: Instant,
    missed_pongs: u32,
    rtt_samples: VecDeque<u64>,
    rtt_ema_ms: f64,
    quality: ConnectionQuality,
}

/// Smoothing factor for exponential moving average.
/// With a window of 10, alpha ~= 0.18, giving recent samples
/// more weight while smoothing jitter.
const EMA_ALPHA_NUMERATOR: f64 = 2.0;

impl HealthState {
    fn new() -> Self {
        Self {
            last_pong_at: Instant::now(),
            missed_pongs: 0,
            rtt_samples: VecDeque::new(),
            rtt_ema_ms: 0.0,
            quality: ConnectionQuality::Good,
        }
    }

    /// Record a new RTT measurement and update the EMA.
    fn record_rtt(&mut self, rtt_ms: u64, window: usize) {
        self.rtt_samples.push_back(rtt_ms);
        if self.rtt_samples.len() > window {
            self.rtt_samples.pop_front();
        }

        let alpha = EMA_ALPHA_NUMERATOR / (window as f64 + 1.0);
        if self.rtt_ema_ms == 0.0 {
            // First sample -- seed the EMA.
            self.rtt_ema_ms = rtt_ms as f64;
        } else {
            self.rtt_ema_ms = alpha * rtt_ms as f64 + (1.0 - alpha) * self.rtt_ema_ms;
        }
    }

    /// Re-evaluate quality based on current state.
    fn evaluate_quality(&mut self, config: &HealthConfig) -> bool {
        let new_quality = if self.missed_pongs >= config.max_missed_pongs {
            ConnectionQuality::Lost
        } else {
            let rtt = self.rtt_ema_ms as u64;
            if rtt <= config.thresholds.excellent_rtt_ms {
                ConnectionQuality::Excellent
            } else if rtt <= config.thresholds.good_rtt_ms {
                ConnectionQuality::Good
            } else {
                ConnectionQuality::Poor
            }
        };

        let changed = new_quality != self.quality;
        self.quality = new_quality;
        changed
    }
}

// ── Public API ──────────────────────────────────────────────

/// Monitors connection health through ping/pong round-trip times.
///
/// Create one per [`TransportSession`](super::session::TransportSession).
/// Call [`record_pong`](Self::record_pong) when a pong arrives and
/// [`record_missed_pong`](Self::record_missed_pong) when a ping
/// times out.  Subscribe to quality changes via [`quality_rx`](Self::quality_rx).
#[derive(Debug)]
pub struct ConnectionHealthMonitor {
    config: HealthConfig,
    state: Arc<RwLock<HealthState>>,
    quality_tx: watch::Sender<ConnectionQuality>,
    quality_rx: watch::Receiver<ConnectionQuality>,
}

impl ConnectionHealthMonitor {
    /// Create a new health monitor with the given configuration.
    pub fn new(config: HealthConfig) -> Self {
        let (quality_tx, quality_rx) = watch::channel(ConnectionQuality::Good);
        Self {
            config,
            state: Arc::new(RwLock::new(HealthState::new())),
            quality_tx,
            quality_rx,
        }
    }

    /// Record a successful pong with the measured round-trip time.
    pub fn record_pong(&self, rtt: Duration) {
        let rtt_ms = rtt.as_millis() as u64;
        let mut state = self.state.write();
        state.last_pong_at = Instant::now();
        state.missed_pongs = 0;
        state.record_rtt(rtt_ms, self.config.rtt_window);
        if state.evaluate_quality(&self.config) {
            let _ = self.quality_tx.send(state.quality);
        }
    }

    /// Record that a ping went unanswered.
    pub fn record_missed_pong(&self) {
        let mut state = self.state.write();
        state.missed_pongs += 1;
        if state.evaluate_quality(&self.config) {
            let _ = self.quality_tx.send(state.quality);
        }
    }

    /// Record client liveness without an RTT sample. Used when a pong
    /// arrives whose payload doesn't echo one of our timestamps — an
    /// RFC-violating peer or middlebox that strips/rewrites the payload is
    /// still clearly alive, so clear the missed-pong counter, but don't
    /// fabricate an RTT sample that would pollute the EMA.
    pub fn record_liveness(&self) {
        let mut state = self.state.write();
        state.last_pong_at = Instant::now();
        state.missed_pongs = 0;
        if state.evaluate_quality(&self.config) {
            let _ = self.quality_tx.send(state.quality);
        }
    }

    /// Current quality level.
    pub fn quality(&self) -> ConnectionQuality {
        self.state.read().quality
    }

    /// Current smoothed RTT in milliseconds, or `None` if no samples yet.
    pub fn rtt_ms(&self) -> Option<u64> {
        let state = self.state.read();
        if state.rtt_samples.is_empty() {
            None
        } else {
            Some(state.rtt_ema_ms as u64)
        }
    }

    /// Get a `watch::Receiver` for quality changes.
    pub fn quality_rx(&self) -> watch::Receiver<ConnectionQuality> {
        self.quality_rx.clone()
    }

    /// Ping interval from configuration.
    pub fn ping_interval(&self) -> Duration {
        self.config.ping_interval
    }

    /// How long since the last successful pong.
    pub fn time_since_last_pong(&self) -> Duration {
        self.state.read().last_pong_at.elapsed()
    }

    /// Number of consecutive missed pongs.
    pub fn missed_pong_count(&self) -> u32 {
        self.state.read().missed_pongs
    }

    /// Reset state (e.g. after reconnection).
    pub fn reset(&self) {
        let mut state = self.state.write();
        *state = HealthState::new();
        let _ = self.quality_tx.send(ConnectionQuality::Good);
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/health.rs"]
mod tests;
