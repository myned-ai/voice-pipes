//! Continuous two-sided session recording.
//!
//! The per-utterance audio in a capture (`NNN_mic_raw.wav`) is what the STT
//! task saw, which is post-VAD and cut into utterances. That is the right
//! artefact for asking "what did the recogniser hear", and the wrong one for
//! anything that reasons about *timing between* the two speakers: the silence
//! either side of a turn has been removed, the two sides are in separate
//! files, and nothing records where they sat relative to each other.
//!
//! Turn-taking models read exactly that removed structure -- who was speaking
//! when, how long the gaps were, whether the two overlapped -- and so does any
//! evaluation of interruption handling. This records it: one stereo WAV per
//! session, channel 0 the user, channel 1 the agent, on a single timeline.
//!
//! Off by default. It records continuously, including silence and including
//! audio no turn ever committed, so it is a storage and privacy cost that has
//! to be opted into.
//!
//! ## Placement
//!
//! Each chunk is placed at its arrival offset from session start, with gaps
//! zero-filled, and never earlier than what is already written on that
//! channel. Arrival time is not capture time -- the agent's audio is teed when
//! it is sent, not when the client plays it -- so this timeline is accurate to
//! transport jitter, not to the sample. That is enough for turn-taking, whose
//! unit is hundreds of milliseconds, and not enough for anything claiming
//! sample alignment between the channels.
//!
//! A dropped chunk (queue full) therefore leaves a silent gap rather than
//! shifting everything after it, which keeps the timeline honest under load.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::audio::AudioFrame;
use crate::audio::resample::resample;
use crate::audio::wav_writer;

use super::store::AuditStore;

/// Bound for the recorder's queue, in chunks. Each item is a copy of one
/// audio chunk; on a full queue the chunk is dropped rather than growing
/// memory without bound behind a store that cannot keep up.
const QUEUE_BOUND: usize = 256;

/// Rate the session file is written at. 16 kHz is what the turn-taking and
/// STT models want, and halves the file against the usual 48 kHz uplink.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Filename the stereo recording is stored under, inside the session's
/// capture directory.
pub const SESSION_AUDIO_FILE: &str = "session_audio.wav";

/// Which speaker a chunk belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The user's microphone, before the VAD gates it.
    User,
    /// What the agent sent to the client, after the inline chain.
    Agent,
}

impl Side {
    const fn index(self) -> usize {
        match self {
            Self::User => 0,
            Self::Agent => 1,
        }
    }
}

enum Item {
    Chunk {
        side: Side,
        samples: Vec<i16>,
        sample_rate: u32,
        channels: u16,
        at: Instant,
    },
    Finish(oneshot::Sender<()>),
}

/// Records both sides of a session onto one timeline.
///
/// Cloneable so the user tee and the agent tee can hold it independently;
/// the recording is written when the last handle is dropped, or earlier if
/// [`SessionRecorder::finish`] is called.
#[derive(Debug, Clone)]
pub struct SessionRecorder {
    tx: mpsc::Sender<Item>,
    warned_full: Arc<AtomicBool>,
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chunk { side, samples, .. } => f
                .debug_struct("Chunk")
                .field("side", side)
                .field("samples", &samples.len())
                .finish_non_exhaustive(),
            Self::Finish(_) => f.write_str("Finish"),
        }
    }
}

impl SessionRecorder {
    /// Start recording. `max_seconds` caps the timeline; past it, audio is
    /// dropped rather than growing the session's memory without bound.
    pub fn spawn(store: Arc<dyn AuditStore>, session_id: String, max_seconds: u32) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_BOUND);
        tokio::spawn(writer_task(rx, store, session_id, max_seconds));
        Self {
            tx,
            warned_full: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Tee one chunk. Never blocks and never fails the caller: this is called
    /// from the session loop and from the outbound audio path.
    pub fn record(&self, side: Side, samples: &[i16], sample_rate: u32, channels: u16) {
        if samples.is_empty() || sample_rate == 0 {
            return;
        }
        let item = Item::Chunk {
            side,
            samples: samples.to_vec(),
            sample_rate,
            channels: channels.max(1),
            at: Instant::now(),
        };
        // A closed queue means the writer has already finished, which is not
        // worth reporting; only a full one loses audio.
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(item) {
            // One warning per session: a full queue usually stays full, and
            // this runs once per audio chunk.
            if !self.warned_full.swap(true, Ordering::Relaxed) {
                warn!(
                    "session audio queue full; dropping chunks (leaves silent gaps, \
                     timeline stays aligned)"
                );
            }
        }
    }

    /// Write the recording now and wait for it to land.
    ///
    /// Dropping every handle writes it too; this exists so session teardown
    /// (and tests) can know the file is there.
    pub async fn finish(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Item::Finish(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

/// Mono i16 at [`TARGET_SAMPLE_RATE`].
fn to_mono_target(samples: Vec<i16>, sample_rate: u32, channels: u16) -> Vec<i16> {
    let mono = if channels <= 1 {
        samples
    } else {
        let n = usize::from(channels);
        samples
            .chunks(n)
            .map(|frame| {
                let sum: i32 = frame.iter().map(|&s| i32::from(s)).sum();
                (sum / frame.len() as i32) as i16
            })
            .collect()
    };
    if sample_rate == TARGET_SAMPLE_RATE {
        return mono;
    }
    // A recording at the wrong rate would misrepresent the session, so
    // audio that cannot be converted is left out of it.
    resample(
        AudioFrame {
            samples: mono,
            sample_rate,
            channels: 1,
        },
        TARGET_SAMPLE_RATE,
    )
    .map_or_else(
        |e| {
            tracing::debug!(error = %e, "session recording: audio left out");
            Vec::new()
        },
        |frame| frame.samples,
    )
}

/// The two channels, grown as audio is placed on them.
#[derive(Default)]
struct Timeline {
    channels: [Vec<i16>; 2],
    dropped_past_cap: bool,
}

impl Timeline {
    fn place(&mut self, side: Side, samples: &[i16], offset: usize, cap: usize) {
        let track = &mut self.channels[side.index()];
        // Never earlier than what is already written: transport jitter can
        // deliver a chunk whose arrival offset predates the previous one, and
        // moving backwards would overwrite real audio.
        let start = offset.max(track.len());
        if cap > 0 && start >= cap {
            self.dropped_past_cap = true;
            return;
        }
        let end = if cap > 0 {
            (start + samples.len()).min(cap)
        } else {
            start + samples.len()
        };
        if end < start + samples.len() {
            self.dropped_past_cap = true;
        }
        if track.len() < start {
            track.resize(start, 0);
        }
        track.extend_from_slice(&samples[..end - start]);
    }

    /// Interleave into one stereo frame, the shorter channel read as silence
    /// past its end.
    ///
    /// Borrows rather than consumes: the recording is written repeatedly while
    /// the session runs, so the timeline has to survive being rendered.
    fn snapshot_stereo(&self) -> AudioFrame {
        let len = self.channels[0].len().max(self.channels[1].len());
        let mut samples = Vec::with_capacity(len * 2);
        for i in 0..len {
            samples.push(self.channels[0].get(i).copied().unwrap_or(0));
            samples.push(self.channels[1].get(i).copied().unwrap_or(0));
        }
        AudioFrame {
            samples,
            sample_rate: TARGET_SAMPLE_RATE,
            channels: 2,
        }
    }
}

/// How often the recording is written while the session is still running.
///
/// It is written repeatedly rather than once at the end because "once at the
/// end" means a session that is killed, crashes, or is closed by shutting the
/// terminal leaves nothing behind -- and the recordings this produces are
/// collected by a person talking, which is exactly the situation where the
/// process does not always exit the way it should.
///
/// The whole file is re-encoded each time, so this trades disk bandwidth for
/// not losing an hour of someone's afternoon. A minute keeps that cost
/// negligible for the session lengths this is used at.
const FLUSH_INTERVAL: Duration = Duration::from_mins(1);

/// How long before the *first* save.
///
/// Short, because the session most likely to be abandoned is the first one
/// someone tries, and a minute of nothing on disk reads as "recording does
/// not work".
const FIRST_FLUSH_AFTER: Duration = Duration::from_secs(10);

/// Encode and store the recording so far.
///
/// `final_write` only changes the logging: an intermediate flush is routine
/// and should not announce itself at `info` on every tick.
async fn write_recording(
    store: &dyn AuditStore,
    session_id: &str,
    timeline: &Timeline,
    final_write: bool,
) {
    let truncated = timeline.dropped_past_cap;
    let frame = timeline.snapshot_stereo();
    if frame.samples.is_empty() {
        if final_write {
            debug!(session_id = %session_id, "no session audio recorded");
        }
        return;
    }
    let seconds = frame.samples.len() as f64 / 2.0 / f64::from(TARGET_SAMPLE_RATE);
    let key = format!("{session_id}/{SESSION_AUDIO_FILE}");
    match wav_writer::encode_wav(&frame) {
        Ok(data) => {
            let bytes = data.len();
            match store.put(&key, &data).await {
                Ok(()) => {
                    if final_write {
                        info!(
                            session_id = %session_id,
                            seconds = format!("{seconds:.1}"),
                            bytes,
                            truncated,
                            "wrote continuous session audio"
                        );
                    } else {
                        debug!(
                            session_id = %session_id,
                            seconds = format!("{seconds:.1}"),
                            "flushed continuous session audio"
                        );
                    }
                }
                Err(e) => {
                    warn!(session_id = %session_id, error = %e, "session audio write failed");
                }
            }
        }
        Err(e) => warn!(session_id = %session_id, error = %e, "session audio encode failed"),
    }
}

async fn writer_task(
    mut rx: mpsc::Receiver<Item>,
    store: Arc<dyn AuditStore>,
    session_id: String,
    max_seconds: u32,
) {
    let cap = max_seconds as usize * TARGET_SAMPLE_RATE as usize;
    let mut timeline = Timeline::default();
    let mut started: Option<Instant> = None;
    let mut done: Option<oneshot::Sender<()>> = None;
    let mut unsaved = false;

    // Short first, then settle. A session that is killed after thirty
    // seconds is exactly the case someone hits while trying the thing out,
    // and waiting a full minute to save anything means their first attempt
    // leaves nothing behind. Later flushes are rarer because the file grows
    // and each one rewrites all of it.
    let mut next_flush = tokio::time::Instant::now() + FIRST_FLUSH_AFTER;

    loop {
        tokio::select! {
            item = rx.recv() => match item {
                Some(Item::Chunk {
                    side,
                    samples,
                    sample_rate,
                    channels,
                    at,
                }) => {
                    let start = *started.get_or_insert(at);
                    let offset = at.saturating_duration_since(start).as_secs_f64().max(0.0);
                    let offset_samples = (offset * f64::from(TARGET_SAMPLE_RATE)) as usize;
                    let mono = to_mono_target(samples, sample_rate, channels);
                    timeline.place(side, &mono, offset_samples, cap);
                    unsaved = true;
                }
                Some(Item::Finish(ack)) => {
                    done = Some(ack);
                    break;
                }
                // Every handle dropped: the session ended without calling
                // `finish`, which is the ordinary path for a worker whose
                // task was simply cancelled.
                None => break,
            },
            () = tokio::time::sleep_until(next_flush) => {
                if unsaved {
                    write_recording(&*store, &session_id, &timeline, false).await;
                    unsaved = false;
                }
                next_flush = tokio::time::Instant::now() + FLUSH_INTERVAL;
            }
        }
    }

    write_recording(&*store, &session_id, &timeline, true).await;

    if let Some(ack) = done {
        let _ = ack.send(());
    }
}

#[cfg(test)]
#[path = "tests/session_audio.rs"]
mod tests;
