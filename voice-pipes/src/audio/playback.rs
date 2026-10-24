use crate::audio::AudioFrame;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tracing::info;

pub const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(10);
pub type PlaybackRing = Arc<parking_lot::Mutex<VecDeque<f32>>>;

/// Cap on queued playback audio, in seconds of device audio.
/// Feeding past the cap drops the newest samples with a warning — a
/// backlog this deep means the producer is far ahead of realtime playback
/// and would otherwise grow the queue without bound.
const MAX_RING_SECONDS: usize = 60;

/// Cached device channel count (set once per `open_playback_stream` call).
static PLAYBACK_DEVICE_CHANNELS: AtomicUsize = AtomicUsize::new(2);
/// Cached device sample rate (set once per `open_playback_stream` call);
/// sizes the ring cap together with `MAX_RING_SECONDS`.
static PLAYBACK_DEVICE_RATE: AtomicUsize = AtomicUsize::new(48_000);

pub fn playback_device_channels() -> usize {
    PLAYBACK_DEVICE_CHANNELS.load(Ordering::Relaxed)
}

/// Open a persistent cpal output stream backed by a ring buffer.
/// Returns (ring, stream, `device_sample_rate`).  On failure the ring/stream
/// are `None` and playback degrades to silence -- the pipeline keeps running.
pub fn open_playback_stream() -> (Option<PlaybackRing>, Option<cpal::Stream>, u32) {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        tracing::error!("no default output device -- playback disabled");
        return (None, None, 0);
    };
    let cfg = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "no default output config");
            return (None, None, 0);
        }
    };

    let device_rate = cfg.sample_rate();
    let device_channels = cfg.channels() as usize;
    PLAYBACK_DEVICE_CHANNELS.store(device_channels, Ordering::Relaxed);
    PLAYBACK_DEVICE_RATE.store(device_rate as usize, Ordering::Relaxed);

    let ring: PlaybackRing = Arc::new(parking_lot::Mutex::new(VecDeque::with_capacity(
        device_rate as usize * device_channels,
    )));

    let ring_cb = Arc::clone(&ring);
    let stream_cfg = cpal::StreamConfig {
        channels: cfg.channels(),
        sample_rate: device_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = match device.build_output_stream(
        stream_cfg,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            playback_callback(&ring_cb, data);
        },
        |err| tracing::warn!(error = %err, "output stream error"),
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to build output stream");
            return (None, None, 0);
        }
    };

    info!(
        device = device.to_string(),
        device_rate, device_channels, "persistent playback stream opened"
    );

    (Some(ring), Some(stream), device_rate)
}

/// Fill one output quantum from the ring.
///
/// Runs on the real-time audio device thread: it must never block on the
/// mutex (a contended `lock()` priority-inverts the RT callback) and must
/// never panic. On contention it emits silence for this quantum.
/// The ring is a `parking_lot::Mutex`, which cannot poison — a producer
/// panicking mid-push simply releases the lock.
fn playback_callback(ring: &PlaybackRing, data: &mut [f32]) {
    match ring.try_lock() {
        Some(mut buf) => fill_from_ring(&mut buf, data),
        None => data.fill(0.0),
    }
}

fn fill_from_ring(buf: &mut VecDeque<f32>, data: &mut [f32]) {
    for sample in data.iter_mut() {
        *sample = buf.pop_front().unwrap_or(0.0);
    }
}

/// Push an `AudioFrame` into the playback ring buffer, bounded by
/// `MAX_RING_SECONDS` of device audio. Samples past the cap are dropped
/// (newest-first) with a warning.
pub fn feed_playback_ring(ring: &PlaybackRing, frame: &AudioFrame, device_channels: usize) {
    let cap = PLAYBACK_DEVICE_RATE
        .load(Ordering::Relaxed)
        .saturating_mul(device_channels.max(1))
        .saturating_mul(MAX_RING_SECONDS);
    let mut buf = ring.lock();
    let dropped = push_bounded(&mut buf, &frame.samples, device_channels, cap);
    if dropped > 0 {
        tracing::warn!(
            dropped_samples = dropped,
            cap,
            "playback ring full (producer far ahead of playback); dropping newest audio"
        );
    }
}

/// Push samples (fanned out to `device_channels`) until the ring holds
/// `cap` values. Returns how many source samples were dropped.
fn push_bounded(
    buf: &mut VecDeque<f32>,
    samples: &[i16],
    device_channels: usize,
    cap: usize,
) -> usize {
    let device_channels = device_channels.max(1);
    for (i, &s) in samples.iter().enumerate() {
        if buf.len() + device_channels > cap {
            return samples.len() - i;
        }
        let val = s as f32 / i16::MAX as f32;
        for _ in 0..device_channels {
            buf.push_back(val);
        }
    }
    0
}

/// Drop all queued samples from the playback ring buffer (used on
/// barge-in / interruption to silence playback immediately).
pub fn clear_playback_ring(ring: &PlaybackRing) {
    ring.lock().clear();
}

/// Wait until the ring buffer is empty (all audio played).
pub async fn drain_playback_ring(ring: &PlaybackRing) {
    loop {
        let len = ring.lock().len();
        if len == 0 {
            break;
        }
        tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/playback.rs"]
mod tests;
