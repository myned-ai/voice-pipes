//! `AnnotatorHub` -- the per-session tee/batch/async runner behind every
//! annotate-mode [`AudioAnnotator`](super::AudioAnnotator).
//!
//! ```text
//! runner ──► out_tx ──► pacer ──► WebSocket          (audio: unchanged)
//!    │ tee: hubs.offer(&chunk)  -- one lock + push, never awaits
//!    ▼
//! AnnotatorHub task (one per configured annotator)
//!    batch by utterance/offset ──► annotator.run() stream ──► annotations
//!    ◄── AudioAnnotation ────────────────────────────────────────┘
//!    └──► ServerMessage::Annotation ──► out_tx (passes the pacer immediately)
//! ```
//!
//! Guarantees:
//! * **Audio never waits.** The tee is `try`-style: on overflow the *oldest*
//!   queued chunk is dropped and counted (`dropped_chunks`), never the caller.
//! * **Bounded work.** At most `max_in_flight` segments outstanding; beyond
//!   that audio keeps accumulating into bigger batches, not more latency.
//! * **Failure isolation.** A segment without an annotation after
//!   `timeout_ms` is a timeout; `failures` consecutive timeouts open the
//!   breaker for `cooldown_ms` (one `warn!`), after which a single probe
//!   segment is let through (half-open). While open the client simply
//!   receives no annotations.
//! * **Barge-in.** `cancel(utterance_id)` drops the pending batch, forwards a
//!   `Cancel` to the annotator, and discards late annotations for that
//!   utterance.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc};
// tokio's Instant so the paused test clock drives timeouts and cooldowns.
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::config::AnnotatorConfig;
use crate::pipeline::metrics::TurnComponentMetrics;
use crate::pipeline::types::AudioChunk;
use crate::transport::ServerMessage;

use super::{
    AnnotatorCommand, AnnotatorExtras, AnnotatorSpec, AudioAnnotation, AudioAnnotator,
    AudioSegment, WARMUP_UTTERANCE_ID,
};

/// Tee queue depth (chunks). ~10 s of 40 ms chunks -- generous for a burst,
/// small enough that a stalled hub cannot hold much memory.
pub const TEE_CAPACITY: usize = 256;
/// Command / annotation channel depth between hub and annotator.
const STREAM_CAPACITY: usize = 64;
/// Per-utterance metric entries retained until the runner collects them.
const METRICS_RETAIN_UTTERANCES: usize = 16;
/// Cancelled utterance ids remembered for late-annotation suppression.
const CANCELLED_RETAIN: usize = 8;
/// Timeout sweep granularity while segments are in flight.
const SWEEP_INTERVAL: Duration = Duration::from_millis(25);

// ── Shared state between the tee (hot path) and the hub task ──

enum TeeItem {
    Chunk {
        utterance_id: u32,
        samples: Vec<i16>,
        sample_rate: u32,
        channels: u16,
        end_of_utterance: bool,
    },
    Cancel(u32),
}

/// Per-utterance counters, collected by the runner at turn end.
#[derive(Debug, Default, Clone)]
pub struct TurnAnnotatorMetrics {
    pub segments: u64,
    pub annotations: u64,
    pub latencies_ms: Vec<u64>,
    pub lag_max_ms: Option<u64>,
    pub dropped_chunks: u64,
    pub timeouts: u64,
    pub skipped_segments: u64,
    /// Serialized `extras` bytes forwarded this turn. Excludes the
    /// `audio_annotation` envelope, whose cost is fixed per frame and so is
    /// `annotations × envelope_size` -- keeping the count here off the wire
    /// codec: this is the measurement the annotation-fidelity ladder is calibrated on.
    pub annotation_bytes: u64,
    /// Audio duration the counted annotations actually describe (max
    /// `start_ms + duration_ms`), for a bitrate that does not vary with turn
    /// length. Deliberately NOT the audio emitted so far: annotations trail
    /// the audio by the pipeline lag, so that would inflate the denominator.
    pub annotated_audio_ms: u64,
}

impl TurnAnnotatorMetrics {
    fn latency_p50(&self) -> Option<u64> {
        if self.latencies_ms.is_empty() {
            return None;
        }
        let mut v = self.latencies_ms.clone();
        v.sort_unstable();
        Some(v[v.len() / 2])
    }

    fn latency_max(&self) -> Option<u64> {
        self.latencies_ms.iter().copied().max()
    }

    /// Annotation throughput in kbit/s over the audio it describes, or `None`
    /// when this turn annotated no audio. This is the number that decides
    /// whether annotations or audio dominate the downlink.
    fn annotation_kbps(&self) -> Option<f64> {
        if self.annotated_audio_ms == 0 {
            return None;
        }
        Some((self.annotation_bytes as f64 * 8.0) / self.annotated_audio_ms as f64)
    }

    /// Wire-shaped extras for the `annotator:<name>` turn component.
    pub fn extras(&self, breaker_open: bool) -> AnnotatorExtras {
        AnnotatorExtras {
            segments: self.segments,
            annotations: self.annotations,
            latency_p50_ms: self.latency_p50(),
            latency_max_ms: self.latency_max(),
            lag_max_ms: self.lag_max_ms,
            dropped_chunks: self.dropped_chunks,
            timeouts: self.timeouts,
            skipped_segments: self.skipped_segments,
            annotation_bytes: self.annotation_bytes,
            annotation_kbps: self.annotation_kbps(),
            breaker_open,
        }
    }
}

#[derive(Default)]
struct MetricsState {
    per_utterance: HashMap<u32, TurnAnnotatorMetrics>,
    /// Insertion order for bounded retention.
    order: VecDeque<u32>,
    breaker_open: bool,
}

impl MetricsState {
    fn entry(&mut self, utterance_id: u32) -> &mut TurnAnnotatorMetrics {
        if !self.per_utterance.contains_key(&utterance_id) {
            self.order.push_back(utterance_id);
            while self.order.len() > METRICS_RETAIN_UTTERANCES {
                if let Some(old) = self.order.pop_front() {
                    self.per_utterance.remove(&old);
                }
            }
        }
        self.per_utterance.entry(utterance_id).or_default()
    }
}

struct Shared {
    queue: Mutex<VecDeque<TeeItem>>,
    notify: Notify,
    closed: AtomicBool,
    metrics: Mutex<MetricsState>,
}

impl Shared {
    fn push(&self, item: TeeItem) {
        let dropped = {
            let mut q = self.queue.lock();
            q.push_back(item);
            if q.len() > TEE_CAPACITY {
                q.pop_front()
            } else {
                None
            }
        };
        if let Some(TeeItem::Chunk { utterance_id, .. }) = dropped {
            self.metrics.lock().entry(utterance_id).dropped_chunks += 1;
        }
        self.notify.notify_one();
    }
}

/// Handle to one hub task. Dropping it closes the tee; the task then drains
/// what it has and ends the annotator stream.
struct HubHandle {
    name: String,
    shared: Arc<Shared>,
}

impl Drop for HubHandle {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.notify.notify_one();
    }
}

/// All hubs of one session (one per configured annotate-mode processor).
/// Empty when nothing is configured -- every method is then a no-op, so the
/// runners call it unconditionally.
#[derive(Default)]
pub struct AnnotatorHubs {
    hubs: Vec<HubHandle>,
}

impl std::fmt::Debug for AnnotatorHubs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnotatorHubs")
            .field(
                "annotators",
                &self
                    .hubs
                    .iter()
                    .map(|h| h.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

static NO_HUBS: AnnotatorHubs = AnnotatorHubs { hubs: Vec::new() };

impl AnnotatorHubs {
    /// Spawn one hub task per spec for a session. `output_rate` is the
    /// negotiated output sample rate, used for the warm-up segment.
    pub fn spawn(
        specs: &[AnnotatorSpec],
        session_id: &str,
        out_tx: mpsc::Sender<ServerMessage>,
        output_rate: u32,
    ) -> Self {
        let hubs = specs
            .iter()
            .map(|spec| {
                let shared = Arc::new(Shared {
                    queue: Mutex::new(VecDeque::with_capacity(TEE_CAPACITY)),
                    notify: Notify::new(),
                    closed: AtomicBool::new(false),
                    metrics: Mutex::new(MetricsState::default()),
                });
                let name = spec.annotator.name().to_string();
                tokio::spawn(run_hub(
                    Arc::clone(&shared),
                    Arc::clone(&spec.annotator),
                    spec.config.clone(),
                    session_id.to_string(),
                    out_tx.clone(),
                    output_rate,
                ));
                HubHandle { name, shared }
            })
            .collect();
        Self { hubs }
    }

    /// The shared empty instance for sessions (and tests) without annotators.
    pub fn none() -> &'static Self {
        &NO_HUBS
    }

    pub fn is_empty(&self) -> bool {
        self.hubs.is_empty()
    }

    /// Tee point: copy an outbound chunk to every hub. Never blocks.
    pub fn offer(&self, chunk: &AudioChunk) {
        if self.hubs.is_empty() {
            return;
        }
        if chunk.samples.is_empty() && !chunk.is_end_of_utterance {
            return;
        }
        for hub in &self.hubs {
            hub.shared.push(TeeItem::Chunk {
                utterance_id: chunk.utterance_id,
                samples: chunk.samples.clone(),
                sample_rate: chunk.sample_rate,
                channels: chunk.channels.max(1),
                end_of_utterance: chunk.is_end_of_utterance,
            });
        }
    }

    /// Barge-in / flush: drop pending work for `utterance_id`.
    pub fn cancel(&self, utterance_id: u32) {
        for hub in &self.hubs {
            hub.shared.push(TeeItem::Cancel(utterance_id));
        }
    }

    /// Take the per-turn metrics of every hub as turn components
    /// (`component_id = "annotator:<name>"`, `kind = "annotator"`). Empty when
    /// no annotator is configured, so the snapshot is unchanged for them.
    pub fn turn_components(&self, utterance_id: u32) -> Vec<TurnComponentMetrics> {
        self.hubs
            .iter()
            .map(|hub| {
                let (m, breaker_open) = {
                    let mut ms = hub.shared.metrics.lock();
                    let m = ms.per_utterance.remove(&utterance_id).unwrap_or_default();
                    ms.order.retain(|u| *u != utterance_id);
                    (m, ms.breaker_open)
                };
                let extras = serde_json::to_value(m.extras(breaker_open))
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                TurnComponentMetrics {
                    component_id: format!("annotator:{}", hub.name),
                    kind: "annotator".to_string(),
                    started_at_ms: 0,
                    finished_at_ms: None,
                    latency_ms: m.latency_p50(),
                    provider: Some(hub.name.clone()),
                    cold_start: false,
                    error: None,
                    extras,
                }
            })
            .collect()
    }
}

// ── Hub task ────────────────────────────────────────────────

/// Audio accumulating towards the next segment of one utterance.
struct Batch {
    utterance_id: u32,
    sample_rate: u32,
    channels: u16,
    samples: Vec<i16>,
    /// Utterance offset of `samples[0]`.
    start_ms: u64,
    /// Whether the utterance's first segment has been released
    /// (`first_batch_ms` applies before, `batch_ms` after).
    first_released: bool,
}

impl Batch {
    fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        (self.samples.len() as u64 / u64::from(self.channels.max(1))) * 1000
            / u64::from(self.sample_rate)
    }
}

struct Pending {
    utterance_id: u32,
    end_ms: u64,
    sent_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breaker {
    Closed,
    Open {
        until: Instant,
    },
    /// One probe segment in flight after the cooldown.
    HalfOpen,
}

struct Hub {
    name: String,
    cfg: AnnotatorConfig,
    shared: Arc<Shared>,
    out_tx: mpsc::Sender<ServerMessage>,
    cmd_tx: mpsc::Sender<AnnotatorCommand>,
    batch: Option<Batch>,
    /// Audio emitted (ms) per utterance, for `start_ms` and lag accounting.
    emitted_ms: HashMap<u32, u64>,
    emitted_order: VecDeque<u32>,
    pending: VecDeque<Pending>,
    cancelled: VecDeque<u32>,
    consecutive_failures: u32,
    breaker: Breaker,
}

impl Hub {
    fn metrics_for(&self, utterance_id: u32, f: impl FnOnce(&mut TurnAnnotatorMetrics)) {
        let mut ms = self.shared.metrics.lock();
        f(ms.entry(utterance_id));
    }

    fn set_breaker(&mut self, b: Breaker) {
        self.breaker = b;
        self.shared.metrics.lock().breaker_open = !matches!(b, Breaker::Closed);
    }

    fn emitted(&mut self, utterance_id: u32) -> &mut u64 {
        if !self.emitted_ms.contains_key(&utterance_id) {
            self.emitted_order.push_back(utterance_id);
            while self.emitted_order.len() > METRICS_RETAIN_UTTERANCES {
                if let Some(old) = self.emitted_order.pop_front() {
                    self.emitted_ms.remove(&old);
                }
            }
        }
        self.emitted_ms.entry(utterance_id).or_insert(0)
    }

    fn is_cancelled(&self, utterance_id: u32) -> bool {
        self.cancelled.contains(&utterance_id)
    }

    /// Fold a teed chunk into the current batch, releasing segments as the
    /// thresholds are met.
    fn on_chunk(
        &mut self,
        utterance_id: u32,
        samples: Vec<i16>,
        sample_rate: u32,
        channels: u16,
        end_of_utterance: bool,
    ) {
        if self.is_cancelled(utterance_id) {
            return;
        }
        // Utterance switch without an EOU (interrupted turn): whatever was
        // accumulating for the old utterance is stale -- drop it.
        if self
            .batch
            .as_ref()
            .is_some_and(|b| b.utterance_id != utterance_id)
        {
            self.batch = None;
        }
        let chunk_ms = if sample_rate == 0 {
            0
        } else {
            (samples.len() as u64 / u64::from(channels.max(1))) * 1000 / u64::from(sample_rate)
        };
        if self.batch.is_none() {
            let start_ms = *self.emitted(utterance_id);
            self.batch = Some(Batch {
                utterance_id,
                sample_rate,
                channels,
                samples: Vec::new(),
                start_ms,
                first_released: false,
            });
        }
        *self.emitted(utterance_id) += chunk_ms;
        let (release, threshold_hit) = {
            let batch = self.batch.as_mut().expect("batch just ensured");
            batch.samples.extend_from_slice(&samples);
            let threshold = if batch.first_released {
                self.cfg.batch_ms
            } else {
                self.cfg.first_batch_ms
            };
            let hit = batch.duration_ms() >= threshold;
            (hit || end_of_utterance, hit)
        };
        if !release {
            return;
        }
        // In-flight bound: keep accumulating (bigger batch) unless this is
        // the utterance end, which must always flush.
        if threshold_hit && !end_of_utterance && self.pending.len() >= self.cfg.max_in_flight {
            return;
        }
        self.release_batch(end_of_utterance);
    }

    fn release_batch(&mut self, end_of_utterance: bool) {
        let Some(batch) = self.batch.take() else {
            return;
        };
        let duration_ms = batch.duration_ms();
        let next_start = batch.start_ms + duration_ms;
        let segment = AudioSegment {
            utterance_id: batch.utterance_id,
            start_ms: batch.start_ms,
            samples: batch.samples,
            sample_rate: batch.sample_rate,
            channels: batch.channels,
            end_of_utterance,
        };
        if !end_of_utterance {
            self.batch = Some(Batch {
                utterance_id: batch.utterance_id,
                sample_rate: batch.sample_rate,
                channels: batch.channels,
                samples: Vec::new(),
                start_ms: next_start,
                first_released: true,
            });
        }
        self.send_segment(segment);
    }

    /// Hand a segment to the annotator stream (`try_send`: the hub never
    /// blocks on a slow annotator; a full command channel counts as a failure).
    fn send_segment(&mut self, segment: AudioSegment) {
        let utterance_id = segment.utterance_id;
        let end_ms = segment.start_ms + segment.duration_ms();
        match self.breaker {
            Breaker::Open { until } if Instant::now() < until => {
                self.metrics_for(utterance_id, |m| m.skipped_segments += 1);
                return;
            }
            Breaker::Open { .. } => {
                debug!(annotator = %self.name, "breaker half-open: probing with next segment");
                self.set_breaker(Breaker::HalfOpen);
            }
            Breaker::HalfOpen if !self.pending.is_empty() => {
                // One probe at a time.
                self.metrics_for(utterance_id, |m| m.skipped_segments += 1);
                return;
            }
            Breaker::HalfOpen | Breaker::Closed => {}
        }
        // Empty EOU segments carry no work to time; forward without tracking.
        let track = !segment.samples.is_empty();
        match self.cmd_tx.try_send(AnnotatorCommand::Segment(segment)) {
            Ok(()) => {
                self.metrics_for(utterance_id, |m| m.segments += 1);
                if track {
                    self.pending.push_back(Pending {
                        utterance_id,
                        end_ms,
                        sent_at: Instant::now(),
                    });
                }
            }
            Err(_) => {
                // Annotator stream saturated or gone: same as a timeout.
                self.record_failure(utterance_id);
            }
        }
    }

    fn record_failure(&mut self, utterance_id: u32) {
        self.metrics_for(utterance_id, |m| m.timeouts += 1);
        self.consecutive_failures += 1;
        let trip = match self.breaker {
            Breaker::HalfOpen => true,
            Breaker::Closed => self.consecutive_failures >= self.cfg.breaker_failures,
            Breaker::Open { .. } => false,
        };
        if trip {
            let cooldown = Duration::from_millis(self.cfg.breaker_cooldown_ms);
            warn!(
                annotator = %self.name,
                failures = self.consecutive_failures,
                cooldown_ms = self.cfg.breaker_cooldown_ms,
                "audio annotator unresponsive; circuit breaker open (audio unaffected)"
            );
            self.set_breaker(Breaker::Open {
                until: Instant::now() + cooldown,
            });
            // Nothing outstanding survives a trip.
            self.pending.clear();
        }
    }

    fn sweep_timeouts(&mut self) {
        let timeout = Duration::from_millis(self.cfg.timeout_ms);
        let now = Instant::now();
        while let Some(p) = self.pending.front()
            && now.duration_since(p.sent_at) >= timeout
        {
            let p = self.pending.pop_front().expect("front exists");
            self.record_failure(p.utterance_id);
        }
    }

    async fn on_annotation(&mut self, a: AudioAnnotation) {
        if a.utterance_id == WARMUP_UTTERANCE_ID || self.is_cancelled(a.utterance_id) {
            return;
        }
        let now = Instant::now();
        let covered_to = a.start_ms + a.duration_ms;
        let mut resolved = Vec::new();
        self.pending.retain(|p| {
            if p.utterance_id == a.utterance_id && p.end_ms <= covered_to {
                resolved.push(now.duration_since(p.sent_at).as_millis() as u64);
                false
            } else {
                true
            }
        });
        if !resolved.is_empty() {
            self.consecutive_failures = 0;
            if self.breaker != Breaker::Closed {
                debug!(annotator = %self.name, "circuit breaker closed");
                self.set_breaker(Breaker::Closed);
            }
        }
        let emitted = self.emitted_ms.get(&a.utterance_id).copied().unwrap_or(0);
        let lag = emitted.saturating_sub(covered_to);
        // Downlink cost of this annotation. `extras` is the variable part; the
        // envelope is fixed per frame and recoverable from `annotations`.
        let extras_bytes = serde_json::to_string(&a.extras).map_or(0, |s| s.len() as u64);
        self.metrics_for(a.utterance_id, |m| {
            m.annotations += 1;
            m.latencies_ms.extend(resolved.iter().copied());
            m.lag_max_ms = Some(m.lag_max_ms.map_or(lag, |l| l.max(lag)));
            m.annotation_bytes += extras_bytes;
            // The audio these bytes actually DESCRIBE, not the audio emitted
            // so far. `emitted` runs ahead of the annotations by the pipeline
            // lag (`lag_max_ms`, often most of a second), so using it inflates
            // the denominator and under-reports the bitrate several-fold.
            // `covered_to` is the far edge of this annotation's span; taking
            // the max avoids double-counting the overlapping or repeated
            // spans a service is free to send.
            m.annotated_audio_ms = m.annotated_audio_ms.max(covered_to);
        });
        let _ = self.out_tx.send(ServerMessage::Annotation(a)).await;
    }

    fn on_cancel(&mut self, utterance_id: u32) {
        if self
            .batch
            .as_ref()
            .is_some_and(|b| b.utterance_id <= utterance_id)
        {
            self.batch = None;
        }
        self.pending.retain(|p| p.utterance_id > utterance_id);
        if !self.cancelled.contains(&utterance_id) {
            self.cancelled.push_back(utterance_id);
            while self.cancelled.len() > CANCELLED_RETAIN {
                self.cancelled.pop_front();
            }
        }
        let _ = self
            .cmd_tx
            .try_send(AnnotatorCommand::Cancel { utterance_id });
    }
}

async fn run_hub(
    shared: Arc<Shared>,
    annotator: Arc<dyn AudioAnnotator>,
    cfg: AnnotatorConfig,
    session_id: String,
    out_tx: mpsc::Sender<ServerMessage>,
    output_rate: u32,
) {
    let name = annotator.name().to_string();
    let mut stream = open_stream(&annotator, &session_id, &cfg, output_rate);
    let mut hub = Hub {
        name: name.clone(),
        cfg: cfg.clone(),
        shared: Arc::clone(&shared),
        out_tx,
        cmd_tx: stream.cmd_tx.clone(),
        batch: None,
        emitted_ms: HashMap::new(),
        emitted_order: VecDeque::new(),
        pending: VecDeque::new(),
        cancelled: VecDeque::new(),
        consecutive_failures: 0,
        breaker: Breaker::Closed,
    };
    let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Drain everything the tee queued since the last wake-up.
        loop {
            let item = shared.queue.lock().pop_front();
            match item {
                Some(TeeItem::Chunk {
                    utterance_id,
                    samples,
                    sample_rate,
                    channels,
                    end_of_utterance,
                }) => {
                    hub.on_chunk(
                        utterance_id,
                        samples,
                        sample_rate,
                        channels,
                        end_of_utterance,
                    );
                }
                Some(TeeItem::Cancel(utterance_id)) => hub.on_cancel(utterance_id),
                None => break,
            }
        }
        if shared.closed.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {
            () = shared.notify.notified() => {}
            ann = stream.ann_rx.recv() => {
                if let Some(a) = ann {
                    hub.on_annotation(a).await;
                } else {
                    // Annotator stream ended while the session is alive:
                    // count it as a failure and re-open after the cooldown.
                    let outcome = match (&mut stream.task).await {
                        Ok(Ok(())) => "ended".to_string(),
                        Ok(Err(e)) => e.to_string(),
                        Err(join) => format!("task panicked: {join}"),
                    };
                    warn!(annotator = %name, outcome, "annotator stream closed; reopening after cooldown");
                    hub.consecutive_failures = hub.cfg.breaker_failures.max(1);
                    hub.record_failure(hub.batch.as_ref().map_or(0, |b| b.utterance_id));
                    tokio::time::sleep(Duration::from_millis(cfg.breaker_cooldown_ms)).await;
                    if shared.closed.load(Ordering::Acquire) {
                        break;
                    }
                    stream = open_stream(&annotator, &session_id, &cfg, output_rate);
                    hub.cmd_tx = stream.cmd_tx.clone();
                    hub.set_breaker(Breaker::HalfOpen);
                }
            }
            _ = sweep.tick(), if !hub.pending.is_empty() => hub.sweep_timeouts(),
        }
    }
    // Session end: closing the command channel ends the annotator stream.
    drop(hub);
    drop(stream.cmd_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), &mut stream.task).await;
    debug!(annotator = %name, session_id = %session_id, "annotator hub stopped");
}

struct Stream {
    cmd_tx: mpsc::Sender<AnnotatorCommand>,
    ann_rx: mpsc::Receiver<AudioAnnotation>,
    task: tokio::task::JoinHandle<Result<(), super::AnnotatorError>>,
}

fn open_stream(
    annotator: &Arc<dyn AudioAnnotator>,
    session_id: &str,
    cfg: &AnnotatorConfig,
    output_rate: u32,
) -> Stream {
    let (cmd_tx, cmd_rx) = mpsc::channel(STREAM_CAPACITY);
    let (ann_tx, ann_rx) = mpsc::channel(STREAM_CAPACITY);
    let annotator = Arc::clone(annotator);
    let session_id = session_id.to_string();
    let task = tokio::spawn(async move { annotator.run(session_id, cmd_rx, ann_tx).await });
    if cfg.warmup {
        let _ = cmd_tx.try_send(AnnotatorCommand::Warmup {
            sample_rate: output_rate,
            channels: 1,
        });
    }
    Stream {
        cmd_tx,
        ann_rx,
        task,
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/hub.rs"]
mod tests;
