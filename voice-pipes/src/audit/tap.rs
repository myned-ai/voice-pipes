//! Audit tap -- captures raw audio around resampling stages.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::audio::AudioFrame;
use crate::audio::resample::resample;
use crate::audio::wav_writer;

use super::store::AuditStore;

/// Which side(s) of a resample operation to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapSide {
    /// Record only the input (before resampling).
    Before,
    /// Record only the output (after resampling).
    After,
    /// Record both input and output.
    Both,
}

/// Buffered write destined for the background writer task.
struct TapWrite {
    key: String,
    data: Vec<u8>,
}

/// An audit tap that wraps [`resample`] and optionally writes WAV snapshots
/// via an [`AuditStore`] before and/or after the conversion.
///
/// Designed to be inserted at any point in the pipeline where sample-rate
/// conversion happens. Each call produces sequentially-numbered WAV files
/// so multiple invocations don't overwrite each other.
///
/// Audio is encoded in-memory and sent to a background writer task so
/// the hot path never blocks on I/O.
/// Bound for the tap's write queue. Each item is an encoded WAV blob,
/// so an unbounded queue grows memory without bound when the store can't keep
/// up. On a full queue the snapshot is dropped (diagnostic capture — dropping a
/// frame is preferable to OOM).
const TAP_QUEUE_BOUND: usize = 64;

#[derive(Debug)]
pub struct AuditTap {
    tx: mpsc::Sender<TapWrite>,
    label: String,
    side: TapSide,
    seq: AtomicU64,
}

impl AuditTap {
    /// Create a new audit tap backed by `store`.
    ///
    /// - `store`: the storage backend for WAV snapshots
    /// - `label`: short tag included in keys (e.g., `"stt_in"`, `"tts_out"`)
    /// - `side`: which side(s) to record
    pub fn spawn(store: Arc<dyn AuditStore>, label: &str, side: TapSide) -> Self {
        let (tx, rx) = mpsc::channel(TAP_QUEUE_BOUND);
        tokio::spawn(tap_writer_task(rx, store));
        Self {
            tx,
            label: label.to_string(),
            side,
            seq: AtomicU64::new(0),
        }
    }

    /// Resample `frame` to `target_rate`, writing audit WAVs according to [`TapSide`].
    ///
    /// # Errors
    /// When the rates cannot be converted; the input snapshot is still
    /// written.
    pub fn resample(
        &self,
        frame: AudioFrame,
        target_rate: u32,
    ) -> Result<AudioFrame, crate::audio::resample::ResampleError> {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);

        if matches!(self.side, TapSide::Before | TapSide::Both) {
            self.write_snapshot(&frame, n, "pre");
        }

        let out = resample(frame, target_rate)?;

        if matches!(self.side, TapSide::After | TapSide::Both) {
            self.write_snapshot(&out, n, "post");
        }

        Ok(out)
    }

    /// Encode and queue a single WAV snapshot. Failures are logged but
    /// never propagate -- audit recording must not break the pipeline.
    fn write_snapshot(&self, frame: &AudioFrame, seq: u64, tag: &str) {
        let key = format!(
            "{seq:04}_{label}_{tag}_{rate}hz.wav",
            label = self.label,
            rate = frame.sample_rate,
        );
        match wav_writer::encode_wav(frame) {
            Ok(data) => match self.tx.try_send(TapWrite { key, data }) {
                Ok(()) => debug!(samples = frame.samples.len(), "audit tap queued"),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("audit tap queue full; dropping snapshot (store not keeping up)");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            },
            Err(e) => warn!(key = %key, error = %e, "audit tap encode failed"),
        }
    }
}

/// Background task that drains the tap channel and writes to the
/// store. Gives up after repeated consecutive failures and discards
/// the rest instead of logging every snapshot.
async fn tap_writer_task(mut rx: mpsc::Receiver<TapWrite>, store: Arc<dyn AuditStore>) {
    let mut health = super::streaming::WriterHealth::default();
    while let Some(write) = rx.recv().await {
        if health.disabled() {
            continue;
        }
        let result = store.put(&write.key, &write.data).await;
        health.observe("audit tap write", &result);
    }
}

#[cfg(test)]
#[path = "tests/tap.rs"]
mod tests;
