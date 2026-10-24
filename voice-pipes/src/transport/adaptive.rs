//! Adaptive-quality controller -- fuses link signals into a [`QualityTier`].
//!
//! Consumers (the annotator hub, the wire encoder, the client) act on the
//! tier; everything else in [`NetworkConditions`] is diagnostics. The
//! controller itself is pure logic with no I/O: observations go in, a tier
//! comes out, and the result is published on a `watch` channel. That is
//! deliberate -- it makes the hysteresis testable against synthetic input with
//! a paused clock, so a bug here can never be confused with a bad signal.
//!
//!
//! # The two rules that matter
//!
//! **Downgrade fast, upgrade slow.** `downgrade_observations` consecutive bad
//! samples drop the tier (one bad sample is noise); climbing back out needs a
//! sustained clean window and happens **one tier at a time**. Oscillating
//! between rates is worse for a listener than sitting one tier too low.
//!
//! **A downgrade may skip; an upgrade may not.** A link that collapses should
//! land on `Critical` immediately rather than walking down a step at a time,
//! but a link that has been quiet for 30 s has only proven it can carry the
//! next tier up, not the best one.

use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, Instant};

use crate::config::AdaptiveQualityConfig;
use crate::transport::{ConnectionQuality, NetworkConditions, QualityTier};

/// Observations are small and infrequent (a few per turn at most). This is
/// headroom, not a queue we expect to use.
const OBSERVATION_BUF: usize = 32;

/// A cheap, cloneable handle for feeding the controller from anywhere.
///
/// Reporting a signal must never block or fail the thing being measured, so
/// every send is best-effort: a full buffer drops the observation rather than
/// stalling the audio path, and a closed channel (session ending) is silent.
#[derive(Debug, Clone)]
pub struct ObservationSink {
    tx: mpsc::Sender<Observation>,
}

impl ObservationSink {
    /// Report one reading. Never blocks; never errors.
    pub fn observe(&self, o: Observation) {
        let _ = self.tx.try_send(o);
    }

    /// A sink that discards everything -- for tests and for transports with no
    /// adaptive plumbing.
    pub fn disconnected() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self { tx }
    }
}

/// One reading of the link, before fusion.
///
/// Every field is optional because the signals arrive independently and at
/// different rates: an observation carrying only `send_latency_ms` is normal
/// and useful. Absent fields simply do not vote.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Observation {
    /// S1 -- ping/pong bucket.
    pub quality: Option<ConnectionQuality>,
    /// S1 -- smoothed RTT.
    pub rtt_ms: Option<u64>,
    /// S2/S3 -- arrival wall time over ideal audio duration.
    pub arrival_rtf: Option<f32>,
    /// S2/S3 -- underruns since the previous observation.
    pub underruns: Option<u32>,
    /// S4 -- server socket-write latency.
    pub send_latency_ms: Option<u64>,
    /// S5 -- client uplink backlog watermark.
    pub uplink_backlog_bytes: Option<u64>,
}

/// The independent signals an observation can carry. Each keeps its own
/// downgrade streak: signals arrive at very different rates (a socket-write
/// sample per frame, a playback report per turn), and a fast signal
/// reporting "fine" says nothing about a slow one reporting "bad".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// S1 -- ping/pong quality bucket.
    Rtt,
    /// S2/S3 -- arrival real-time factor.
    Arrival,
    /// S2/S3 -- playback underruns.
    Underruns,
    /// S4 -- server socket-write latency.
    SendLatency,
}

const SIGNALS: [Signal; 4] = [
    Signal::Rtt,
    Signal::Arrival,
    Signal::Underruns,
    Signal::SendLatency,
];

/// Consecutive degraded votes from one signal and the worst tier they asked
/// for.
#[derive(Debug, Clone, Copy, Default)]
struct Streak {
    count: u32,
    worst: QualityTier,
}

/// Fuses observations into a tier and publishes the result.
///
/// One per session. Cheap to construct; holds no I/O handles beyond the
/// `watch` sender.
#[derive(Debug)]
pub struct AdaptiveController {
    cfg: AdaptiveQualityConfig,
    tx: watch::Sender<NetworkConditions>,
    current: QualityTier,
    /// One downgrade streak per signal, indexed like [`SIGNALS`]. A signal's
    /// streak is reset only by a sample of that same signal that no longer
    /// asks for a worse tier; samples of other signals leave it alone.
    streaks: [Streak; 4],
    /// When the link last became better than `current`. `None` while the
    /// current tier is still being indicated.
    clean_since: Option<Instant>,
    /// The last published view. Observations are sparse, so each one only
    /// overwrites the fields it carries; the rest keep their previous value
    /// instead of reverting to "unknown".
    last: NetworkConditions,
}

impl AdaptiveController {
    /// Create a controller and its receiver. The initial published value is
    /// [`QualityTier::Full`], which is also where a disabled controller stays.
    pub fn new(cfg: AdaptiveQualityConfig) -> (Self, watch::Receiver<NetworkConditions>) {
        let (tx, rx) = watch::channel(NetworkConditions::default());
        (
            Self {
                cfg,
                tx,
                current: QualityTier::Full,
                streaks: [Streak::default(); 4],
                clean_since: None,
                last: NetworkConditions::default(),
            },
            rx,
        )
    }

    /// A receiver for consumers that subscribe after construction.
    pub fn subscribe(&self) -> watch::Receiver<NetworkConditions> {
        self.tx.subscribe()
    }

    /// Run the controller in its own task, fed by a channel.
    ///
    /// Signals originate in places that cannot share a `&mut` -- the runner's
    /// select loop, the WebSocket write path, a client control frame -- so the
    /// controller owns itself in a task and everyone else holds a cheap
    /// [`ObservationSink`]. That also keeps `observe` synchronous and
    /// lock-free, which is what makes the unit tests above possible.
    ///
    /// The task ends when every sink is dropped, i.e. when the session ends.
    pub fn spawn(
        cfg: AdaptiveQualityConfig,
    ) -> (ObservationSink, watch::Receiver<NetworkConditions>) {
        let (mut controller, rx) = Self::new(cfg);
        let (tx, mut obs_rx) = mpsc::channel::<Observation>(OBSERVATION_BUF);
        tokio::spawn(async move {
            while let Some(o) = obs_rx.recv().await {
                controller.observe(o);
            }
        });
        (ObservationSink { tx }, rx)
    }

    /// Current tier, without waiting for a publish.
    pub fn tier(&self) -> QualityTier {
        self.current
    }

    /// The tier one signal of this observation asks for, or `None` when the
    /// observation does not carry that signal.
    ///
    /// A signal that is absent does not vote. `Full` means the signal is
    /// present and healthy -- that is evidence, unlike absence.
    fn vote(&self, signal: Signal, o: &Observation) -> Option<QualityTier> {
        match signal {
            Signal::Arrival => o.arrival_rtf.map(|rtf| {
                if rtf >= self.cfg.critical_arrival_rtf {
                    QualityTier::Critical
                } else if rtf >= self.cfg.minimal_arrival_rtf {
                    QualityTier::Minimal
                } else if rtf >= self.cfg.reduced_arrival_rtf {
                    QualityTier::Reduced
                } else {
                    QualityTier::Full
                }
            }),
            Signal::SendLatency => o.send_latency_ms.map(|ms| {
                if ms >= self.cfg.critical_send_latency_ms {
                    QualityTier::Critical
                } else if ms >= self.cfg.minimal_send_latency_ms {
                    QualityTier::Minimal
                } else if ms >= self.cfg.reduced_send_latency_ms {
                    QualityTier::Reduced
                } else {
                    QualityTier::Full
                }
            }),
            Signal::Underruns => o.underruns.map(|u| {
                if u >= self.cfg.minimal_underruns {
                    QualityTier::Minimal
                } else if u >= self.cfg.reduced_underruns {
                    QualityTier::Reduced
                } else {
                    QualityTier::Full
                }
            }),
            // S1 is the coarse floor: `Poor` is worth one tier, but it is far
            // too slow (15 s ping interval, 10-sample EMA) to drive anything
            // sharper. `Lost` deliberately does not vote -- the session is
            // being torn down by the heartbeat watchdog and there is nothing
            // left to adapt.
            Signal::Rtt => match o.quality {
                Some(ConnectionQuality::Poor) => Some(QualityTier::Reduced),
                Some(ConnectionQuality::Excellent | ConnectionQuality::Good) => {
                    Some(QualityTier::Full)
                }
                Some(ConnectionQuality::Lost) | None => None,
            },
        }
    }

    /// The worst tier this observation's signals indicate, and whether any
    /// signal voted at all.
    ///
    /// Each signal votes independently and the worst vote wins: a link
    /// failing any one test is degraded regardless of how healthy the others
    /// look.
    fn indicated(&self, o: &Observation) -> Option<QualityTier> {
        SIGNALS.iter().filter_map(|s| self.vote(*s, o)).max()
    }

    /// How long the link must stay clean before climbing out of `tier`.
    fn clean_window(&self, tier: QualityTier) -> Duration {
        Duration::from_secs(match tier {
            QualityTier::Full => 0,
            QualityTier::Reduced => self.cfg.reduced_clean_secs,
            QualityTier::Minimal => self.cfg.minimal_clean_secs,
            QualityTier::Critical => self.cfg.critical_clean_secs,
        })
    }

    /// Feed one reading. Returns the tier after fusion, and publishes the
    /// conditions when anything a consumer cares about changed.
    ///
    /// Downgrades are decided per signal: a signal that keeps asking for a
    /// worse tier for `downgrade_observations` consecutive samples of its own
    /// drops the tier, and only a sample of that same signal can break its
    /// streak. Without that, the socket-write sample the pump reports for
    /// every outbound frame -- almost always healthy -- kept resetting the
    /// streak of the once-per-turn playback report, and a link with 30 percent
    /// underruns never left `Full`. Upgrades still take the whole
    /// observation: the link has to look clean on every signal it reports for
    /// a sustained window.
    pub fn observe(&mut self, o: Observation) -> QualityTier {
        let indicated = self.indicated(&o);

        if self.cfg.enabled
            && let Some(indicated) = indicated
        {
            let needed = self.cfg.downgrade_observations.max(1);
            let mut reached = QualityTier::Full;
            for (i, signal) in SIGNALS.iter().enumerate() {
                let Some(vote) = self.vote(*signal, &o) else {
                    continue;
                };
                let streak = &mut self.streaks[i];
                if vote > self.current {
                    streak.count += 1;
                    streak.worst = streak.worst.max(vote);
                    if streak.count >= needed {
                        reached = reached.max(streak.worst);
                    }
                } else {
                    // This signal no longer asks for a worse tier: its own
                    // streak is over. A borderline signal alternating either
                    // side of a threshold therefore never accumulates the
                    // agreement a downgrade needs.
                    *streak = Streak::default();
                }
            }

            if reached > self.current {
                // Jump straight to the tier the persistent signal asked for
                // -- a collapsing link should not walk down one step per
                // observation. Every streak restarts against the new tier.
                self.current = reached;
                self.streaks = [Streak::default(); 4];
                self.clean_since = None;
            } else if indicated < self.current {
                let now = Instant::now();
                let since = *self.clean_since.get_or_insert(now);
                if now.duration_since(since) >= self.clean_window(self.current) {
                    if let Some(better) = self.current.better() {
                        self.current = better;
                    }
                    // Restart the window: each step up is earned separately,
                    // so recovering from Critical to Full takes all three
                    // windows, not one.
                    self.clean_since = Some(now);
                }
            } else if indicated == self.current {
                // Exactly at the current tier: not degrading, but not
                // evidence of recovery either.
                self.clean_since = None;
            }
        }

        // Merge: an observation carrying only one signal must not erase what
        // the others last reported.
        if let Some(q) = o.quality {
            self.last.quality = q;
        }
        if o.rtt_ms.is_some() {
            self.last.rtt_ms = o.rtt_ms;
        }
        if o.arrival_rtf.is_some() {
            self.last.arrival_rtf = o.arrival_rtf;
        }
        if let Some(u) = o.underruns {
            self.last.underruns_recent = u;
        }
        if o.send_latency_ms.is_some() {
            self.last.send_latency_ms = o.send_latency_ms;
        }
        if o.uplink_backlog_bytes.is_some() {
            self.last.uplink_backlog_bytes = o.uplink_backlog_bytes;
        }
        self.last.tier = self.current;
        // `send` fails only when every receiver is gone, which is a closing
        // session, not an error.
        let _ = self.tx.send(self.last);
        self.current
    }
}

/// Feed the ping/pong health monitor into the controller.
///
/// The monitor lives inside the transport session, which moves into its pump
/// before the controller is fed, so this task bridges the two: every change
/// of the quality bucket becomes an observation carrying the bucket and the
/// smoothed RTT. The task holds the monitor only weakly and ends when the
/// session drops it.
pub fn spawn_health_feed(
    monitor: &std::sync::Arc<crate::transport::health::ConnectionHealthMonitor>,
    sink: ObservationSink,
) {
    let mut quality_rx = monitor.quality_rx();
    let monitor = std::sync::Arc::downgrade(monitor);
    tokio::spawn(async move {
        while quality_rx.changed().await.is_ok() {
            let quality = *quality_rx.borrow_and_update();
            let Some(monitor) = monitor.upgrade() else {
                return;
            };
            sink.observe(Observation {
                quality: Some(quality),
                rtt_ms: monitor.rtt_ms(),
                ..Default::default()
            });
        }
    });
}

/// Tell the client whenever the tier changes.
///
/// Watches `conditions` and emits [`ServerControl::QualityUpdate`] on every
/// **tier** transition — not on every observation, which would be a control
/// frame per RTT sample competing with the audio this is supposed to protect.
///
/// The client uses it to degrade in step (a smaller avatar update rate, a
/// bigger jitter buffer) and to tell the user the connection is poor rather
/// than leaving them to interpret broken audio.
///
/// Ends when the session's `out_tx` closes.
pub fn spawn_quality_reporter(
    mut conditions: watch::Receiver<NetworkConditions>,
    out_tx: mpsc::Sender<crate::transport::ServerMessage>,
) {
    tokio::spawn(async move {
        let mut last = conditions.borrow_and_update().tier;
        while conditions.changed().await.is_ok() {
            let now = conditions.borrow().tier;
            if now == last {
                continue;
            }
            last = now;
            let msg = crate::transport::ServerMessage::Control(
                crate::transport::ServerControl::QualityUpdate {
                    quality: conditions.borrow().quality,
                    tier: now,
                },
            );
            if out_tx.send(msg).await.is_err() {
                return; // session gone
            }
        }
    });
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/adaptive.rs"]
mod tests;
