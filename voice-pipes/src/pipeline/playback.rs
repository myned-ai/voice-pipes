#![expect(
    clippy::needless_range_loop,
    reason = "DSP kernels index several buffers by one position; iterator chains obscure the math"
)]

use tokio::sync::mpsc;

use crate::audio::resample::resample;

use super::types::AudioChunk;

/// Playback task: receives audio chunks and plays them on a persistent
/// cpal output stream.  A ring buffer bridges the async receiver and the
/// real-time audio callback, eliminating clicks between chunks.
pub async fn playback_task(mut audio_rx: mpsc::Receiver<AudioChunk>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        tracing::error!("no default output device");
        return;
    };
    let default_cfg = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "no default output config");
            return;
        }
    };
    let device_rate = default_cfg.sample_rate();
    let device_channels = default_cfg.channels() as usize;

    // Shared lock-free SPSC ring buffer.
    let buffer_capacity = device_rate as usize * device_channels;
    let (mut prod, mut cons) = rtrb::RingBuffer::new(buffer_capacity);

    let stream_config = cpal::StreamConfig {
        channels: default_cfg.channels(),
        sample_rate: device_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = match device.build_output_stream(
        stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for sample in data.iter_mut() {
                *sample = cons.pop().unwrap_or(0.0);
            }
        },
        |err| tracing::warn!(error = %err, "output stream error"),
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to build output stream");
            return;
        }
    };

    if let Err(e) = stream.play() {
        tracing::error!(error = %e, "failed to start playback");
        return;
    }

    tracing::info!(
        device = device.to_string(),
        device_rate,
        device_channels,
        "playback stream started"
    );

    // Feed received chunks into the ring buffer.
    while let Some(chunk) = audio_rx.recv().await {
        if chunk.samples.is_empty() {
            continue;
        }

        let device_samples = process_chunk_for_device(&chunk, device_channels, device_rate);

        let mut written = 0;
        while written < device_samples.len() {
            if let Ok(()) = prod.push(device_samples[written]) {
                written += 1;
            } else {
                // Buffer is full; yield to let CPAL read some samples
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        }
    }

    // Drain remaining audio before closing.
    loop {
        if prod.slots() == buffer_capacity {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // Small tail for hardware buffer drain.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    drop(stream);
    tracing::info!("playback stream stopped");
}

pub(crate) fn process_chunk_for_device(
    chunk: &AudioChunk,
    device_channels: usize,
    device_rate: u32,
) -> Vec<f32> {
    if chunk.samples.is_empty() {
        return Vec::new();
    }
    let samples = if chunk.sample_rate == device_rate {
        chunk.samples.clone()
    } else {
        // A chunk at a rate that cannot be converted is not played at the
        // wrong speed: it is skipped.
        match resample(chunk.to_frame(), device_rate) {
            Ok(frame) => frame.samples,
            Err(e) => {
                tracing::debug!(error = %e, "playback chunk skipped");
                return Vec::new();
            }
        }
    };

    let mut out = Vec::with_capacity(samples.len() * device_channels);
    // A zero channel count (`AudioChunk::default()`) would make
    // `chunks_exact(0)` panic; treat it as mono.
    let input_channels = usize::from(chunk.channels.max(1));

    if input_channels == device_channels {
        for &s in &samples {
            out.push(s as f32 / i16::MAX as f32);
        }
    } else if input_channels == 1 {
        // Mono to multi-channel expansion
        for &s in &samples {
            let val = s as f32 / i16::MAX as f32;
            for _ in 0..device_channels {
                out.push(val);
            }
        }
    } else if device_channels == 1 {
        // Multi-channel to mono mixdown
        for frame in samples.chunks_exact(input_channels) {
            let sum: f32 = frame.iter().map(|&s| s as f32).sum();
            let avg = (sum / input_channels as f32) / i16::MAX as f32;
            out.push(avg);
        }
    } else {
        // Other channel mismatch: pad or truncate
        for frame in samples.chunks_exact(input_channels) {
            for i in 0..device_channels {
                if i < input_channels {
                    out.push(frame[i] as f32 / i16::MAX as f32);
                } else {
                    out.push(0.0);
                }
            }
        }
    }
    out
}
