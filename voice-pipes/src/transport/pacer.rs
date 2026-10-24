//! Audio Pacer -- transport-agnostic output flow control.
//!
//! Sits between the pipeline's `out_tx` channel and the session pump,
//! ensuring that audio chunks are sent to the client at a controlled
//! rate slightly above real-time.  This prevents TCP/WebSocket HOL
//! (Head-of-Line) blocking when TTS generates audio much faster than
//! real-time, which would otherwise cause control messages like
//! `ClearAudio` to be delayed behind megabytes of queued audio data.
//!
//! # Architecture
//!
//! ```text
//! Pipeline (out_tx) -> [AudioPacer] -> (paced_rx) -> session_pump -> Transport
//! ```
//!
//! The pacer:
//! - Immediately forwards all non-audio messages (control, text, data)
//! - Buffers audio chunks and releases them at `pace_factor x real-time`
//! - On `ClearAudio`, flushes the internal audio buffer and sends the
//!   `ClearAudio` immediately, ensuring minimal barge-in latency
//! - Configurable per transport: WebSocket uses ~1.5x pace; transports
//!   with independent streams (WebTransport/QUIC) may bypass pacing
//!
//! # Configuration
//!
//! The `PacerConfig` struct controls behavior:
//! - `enabled`: whether pacing is active (default: true for WebSocket)
//! - `pace_factor`: multiplier over real-time (1.5 = 50% faster than playback)
//! - `burst_ms`: initial burst window allowing instant delivery for TTFB

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, trace};

use super::ServerMessage;

// ── Configuration ───────────────────────────────────────────

/// Configuration for the audio pacer.
#[derive(Debug, Clone)]
pub struct PacerConfig {
    /// Whether pacing is enabled.  When false, all messages pass through
    /// immediately (zero-copy forwarding).
    pub enabled: bool,
    /// How much faster than real-time to send audio.
    /// 1.0 = exact real-time, 1.5 = 50% faster, 2.0 = double speed.
    /// Values below 1.0 are clamped to 1.0.
    pub pace_factor: f64,
    /// Initial burst window in milliseconds.  The first `burst_ms` worth
    /// of audio in a turn is sent immediately to minimize TTFB.
    /// After the burst, pacing kicks in.
    pub burst_ms: u64,
    /// Maximum audio duration (ms) to buffer internally.  If the pipeline
    /// produces audio faster than the pacer releases it, this caps memory.
    /// Older chunks beyond this limit are dropped with a warning.
    pub max_buffer_ms: u64,
}

impl Default for PacerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pace_factor: 1.5,
            burst_ms: 500,
            max_buffer_ms: 300_000,
        }
    }
}

impl PacerConfig {
    /// Configuration suitable for WebSocket transport (single ordered stream).
    pub fn websocket() -> Self {
        Self::default()
    }

    /// Configuration for transports with independent streams (e.g., WebTransport/QUIC).
    /// Pacing is disabled since control messages travel on a separate stream.
    pub fn independent_streams() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Bypass pacing entirely -- pass-through mode.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

// ── Pacer state ─────────────────────────────────────────────

/// A buffered audio chunk with its PCM duration pre-computed.
struct PacedChunk {
    message: ServerMessage,
    /// Duration of audio in this chunk (zero for non-audio messages).
    audio_duration: Duration,
}

/// Spawn the audio pacer task.
///
/// Returns a new `mpsc::Receiver<ServerMessage>` that the session pump
/// should read from instead of the raw `out_rx`.  Audio messages are
/// delayed to achieve the configured pace; control/text messages pass
/// through immediately.
///
/// The `suppress_audio` flag is shared with the runner's barge-in logic.
/// When set, the pacer also drops buffered audio (in addition to the
/// session pump's own suppression), ensuring the internal queue is
/// flushed on interruption.
pub fn spawn_pacer(
    config: PacerConfig,
    mut source_rx: mpsc::Receiver<ServerMessage>,
    suppress_audio: Arc<AtomicBool>,
) -> mpsc::Receiver<ServerMessage> {
    // If pacing is disabled, return the source directly (zero overhead).
    if !config.enabled {
        return source_rx;
    }

    let (paced_tx, paced_rx) = mpsc::channel::<ServerMessage>(128);

    tokio::spawn(async move {
        pacer_loop(config, &mut source_rx, &paced_tx, suppress_audio).await;
    });

    paced_rx
}

/// Mutable pacing state for one session.
///
/// Release timing is an absolute schedule, not a per-iteration delay: each
/// released chunk books the next release at `schedule + duration / pace`,
/// and the select loop sleeps until that instant however many source
/// messages arrive in between. Recomputing a relative delay on every wake-up
/// had two effects worth avoiding: while TTS streamed a sentence burst the
/// source arm kept winning and nothing was released until the source went
/// quiet, and the backlog then drained at twice the configured pace because
/// the delay was credited with the wall time the loop had spent elsewhere.
struct PacerState {
    audio_queue: VecDeque<PacedChunk>,
    total_queued: Duration,
    /// Audio still allowed out immediately at the start of the utterance.
    burst_remaining: Duration,
    /// When the front chunk may go out. `None` means "as soon as there is
    /// one": the schedule restarts whenever the queue refills from empty,
    /// and at every utterance boundary.
    next_release: Option<Instant>,
}

impl PacerState {
    fn new(burst: Duration) -> Self {
        Self {
            audio_queue: VecDeque::new(),
            total_queued: Duration::ZERO,
            burst_remaining: burst,
            next_release: None,
        }
    }

    fn clear_audio(&mut self) {
        self.audio_queue.clear();
        self.total_queued = Duration::ZERO;
    }

    /// The utterance ended (or was cut): the next one starts with a fresh
    /// burst and no inherited schedule.
    fn start_utterance(&mut self, burst: Duration) {
        self.burst_remaining = burst;
        self.next_release = None;
    }
}

async fn pacer_loop(
    config: PacerConfig,
    source_rx: &mut mpsc::Receiver<ServerMessage>,
    paced_tx: &mpsc::Sender<ServerMessage>,
    suppress_audio: Arc<AtomicBool>,
) {
    let pace_factor = config.pace_factor.max(1.0);
    let burst_budget = Duration::from_millis(config.burst_ms);
    let max_buffer = Duration::from_millis(config.max_buffer_ms);

    let mut state = PacerState::new(burst_budget);

    loop {
        // With audio queued, wake at the scheduled release; otherwise only
        // for new input. The schedule is absolute, so a source message that
        // arrives first does not push the release back.
        let release_at = state
            .audio_queue
            .front()
            .map(|_| state.next_release.unwrap_or_else(Instant::now));

        tokio::select! {
            biased;

            // Priority: always check for new messages from the pipeline.
            msg = source_rx.recv() => {
                match msg {
                    Some(m) => handle_incoming(
                        m, &config, paced_tx, &mut state, &suppress_audio, max_buffer,
                    ).await,
                    None => break, // source closed
                }
            }

            // Timer: release the front audio chunk.
            () = tokio::time::sleep_until(release_at.unwrap_or_else(Instant::now)),
                if release_at.is_some() =>
            {
                let now = Instant::now();
                if let Some(chunk) = state.audio_queue.pop_front() {
                    state.total_queued = state.total_queued.saturating_sub(chunk.audio_duration);
                    let is_end = matches!(&chunk.message,
                        ServerMessage::Audio(c) if c.is_end_of_utterance);

                    // The chunk just released buys the client `audio_duration`
                    // of playback; at the configured pace the next one is due
                    // after this much wall time.
                    let paced = Duration::from_secs_f64(
                        chunk.audio_duration.as_secs_f64() / pace_factor,
                    );
                    if state.burst_remaining > Duration::ZERO {
                        // Burst phase: the next chunk may follow at once while
                        // budget remains; the chunk that spends the last of it
                        // starts the paced schedule.
                        state.burst_remaining =
                            state.burst_remaining.saturating_sub(chunk.audio_duration);
                        state.next_release = Some(if state.burst_remaining > Duration::ZERO {
                            now
                        } else {
                            now + paced
                        });
                    } else {
                        // Paced phase: booked from the schedule, not from `now`,
                        // so a late wake-up (the loop was busy) does not stretch
                        // the cadence. If the loop fell more than one interval
                        // behind, catch up one chunk at a time from `now` rather
                        // than releasing the whole backlog at once.
                        let floor = now.checked_sub(paced).unwrap_or(now);
                        let base = state.next_release.map_or(now, |t| t.max(floor));
                        state.next_release = Some(base + paced);
                    }
                    if is_end {
                        state.start_utterance(burst_budget);
                    }

                    // Don't send if suppressed (barge-in in progress).
                    if suppress_audio.load(Ordering::Acquire) {
                        trace!("pacer: dropping buffered audio (suppress_audio)");
                    } else if paced_tx.send(chunk.message).await.is_err() {
                        break; // downstream closed
                    }
                }
            }
        }
    }

    // Drain remaining buffered audio on shutdown.
    while let Some(chunk) = state.audio_queue.pop_front() {
        if paced_tx.send(chunk.message).await.is_err() {
            break;
        }
    }
}

/// Handle an incoming message from the pipeline source channel.
async fn handle_incoming(
    msg: ServerMessage,
    config: &PacerConfig,
    paced_tx: &mpsc::Sender<ServerMessage>,
    state: &mut PacerState,
    suppress_audio: &AtomicBool,
    max_buffer: Duration,
) {
    // Identify the message type without borrowing the whole value.
    let is_audio = matches!(msg, ServerMessage::Audio(_));
    let is_clear_audio = matches!(
        msg,
        ServerMessage::Control(super::ServerControl::ClearAudio { .. })
    );

    if is_audio {
        let ServerMessage::Audio(ref chunk) = msg else {
            unreachable!()
        };

        // If audio is suppressed (barge-in), drop immediately.
        if suppress_audio.load(Ordering::Acquire) {
            if !state.audio_queue.is_empty() {
                debug!(
                    flushed = state.audio_queue.len(),
                    "pacer: flushing buffered audio on suppress"
                );
                state.clear_audio();
            }
            return;
        }

        let duration = chunk_audio_duration(chunk);

        // Check buffer limit.
        if state.total_queued + duration > max_buffer {
            debug!(
                total_queued_ms = state.total_queued.as_millis() as u64,
                chunk_ms = duration.as_millis() as u64,
                "pacer: dropping audio chunk, buffer limit reached"
            );

            // If this chunk also signals the end of the utterance, preserve the
            // sentinel so the client knows the audio has completed.
            if chunk.is_end_of_utterance {
                let mut sentinel = chunk.clone();
                sentinel.samples.clear();
                state.audio_queue.push_back(PacedChunk {
                    message: ServerMessage::Audio(sentinel),
                    audio_duration: Duration::ZERO,
                });
            }
            return;
        }

        state.audio_queue.push_back(PacedChunk {
            message: msg,
            audio_duration: duration,
        });
        state.total_queued += duration;
    } else if is_clear_audio {
        // CRITICAL: On ClearAudio, flush all buffered audio first,
        // then forward the ClearAudio immediately.
        if !state.audio_queue.is_empty() {
            debug!(
                flushed = state.audio_queue.len(),
                "pacer: flushing audio buffer on ClearAudio"
            );
            state.clear_audio();
        }
        // The next utterance starts fresh.
        state.start_utterance(Duration::from_millis(config.burst_ms));

        // Forward ClearAudio immediately.
        let _ = paced_tx.send(msg).await;
    } else {
        // All non-audio, non-ClearAudio messages (control, text, data) pass
        // through immediately.  StateChanged{Listening} in particular MUST NOT
        // be held behind buffered audio: for long responses the audio queue may
        // hold tens of seconds of data, and delaying Listening would freeze the
        // client's utteranceId counter, breaking word-reveal and interruptions.
        // The client guards against the spurious-onPlaybackStart side-effect by
        // checking pipelineStateRef (must be "speaking") before adding a placeholder.
        let _ = paced_tx.send(msg).await;
    }
}

/// Compute the playback duration of an audio chunk from its PCM samples.
/// Samples are interleaved, so a stereo chunk holds half as many frames as
/// it has samples.
fn chunk_audio_duration(chunk: &crate::pipeline::AudioChunk) -> Duration {
    if chunk.sample_rate == 0 || chunk.samples.is_empty() {
        return Duration::ZERO;
    }
    let frames = chunk.samples.len() as u64 / u64::from(chunk.channels.max(1));
    let rate = chunk.sample_rate as u64;
    Duration::from_micros(frames * 1_000_000 / rate)
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/pacer.rs"]
mod tests;
