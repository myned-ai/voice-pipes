//! Audio device I/O -- microphone capture and speaker playback via cpal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tracing::{info, warn};

use super::AudioFrame;

/// RMS amplitude considered the start of speech.
const SPEECH_THRESHOLD: f64 = 200.0;

/// Number of pre-roll chunks to keep before speech onset so the
/// beginning of words isn't clipped.
const PRE_ROLL_CHUNKS: usize = 5;

/// Record audio from the default input device for `duration` seconds.
///
/// Returns an `AudioFrame` captured at the device's preferred sample rate
/// (typically 44100 or 48000 Hz), mono.
pub fn record_microphone(duration: Duration) -> Result<AudioFrame, anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no default input device found"))?;

    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let sample_format = config.sample_format();

    info!(
        device = device.to_string(),
        sample_rate,
        channels,
        format = ?sample_format,
        duration_s = duration.as_secs_f32(),
        "recording from microphone"
    );

    let (tx, rx) = mpsc::channel::<Vec<i16>>();
    let err_fn = |err| warn!("input stream error: {err}");

    let stream = match sample_format {
        cpal::SampleFormat::I16 => {
            let tx = tx.clone();
            device.build_input_stream(
                config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let _ = tx.send(data.to_vec());
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::F32 => {
            let tx = tx.clone();
            device.build_input_stream(
                config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let samples: Vec<i16> = data
                        .iter()
                        .map(|&s| {
                            let clamped = s.clamp(-1.0, 1.0);
                            (clamped * i16::MAX as f32) as i16
                        })
                        .collect();
                    let _ = tx.send(samples);
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("unsupported sample format: {other:?}"),
    };

    stream.play()?;
    std::thread::sleep(duration);
    drop(stream);

    // Collect all captured chunks
    let mut all_samples = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        all_samples.extend(chunk);
    }

    // Mix to mono if multi-channel
    let mono_samples = if channels > 1 {
        mix_to_mono(&all_samples, channels)
    } else {
        all_samples
    };

    let captured_duration = mono_samples.len() as f32 / sample_rate as f32;
    info!(
        samples = mono_samples.len(),
        sample_rate,
        captured_duration_s = format!("{captured_duration:.2}"),
        "microphone capture complete"
    );

    Ok(AudioFrame {
        samples: mono_samples,
        sample_rate,
        channels: 1,
    })
}

/// Play an `AudioFrame` on the default output device. Blocks until playback finishes.
/// Resamples to the device's preferred rate and duplicates mono to match device channels.
pub fn play_speaker(frame: &AudioFrame) -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device found"))?;

    // Query the device's preferred config (rate + channels)
    let default_output_config = device.default_output_config()?;
    let device_rate = default_output_config.sample_rate();
    let device_channels = default_output_config.channels();

    // Resample if the frame rate differs from the device rate
    let mono_samples = if frame.sample_rate == device_rate {
        frame.samples.clone()
    } else {
        let resampled = super::resample::resample(
            AudioFrame {
                samples: frame.samples.clone(),
                sample_rate: frame.sample_rate,
                channels: frame.channels,
            },
            device_rate,
        )?;
        info!(
            from_rate = frame.sample_rate,
            to_rate = device_rate,
            "resampled for output device"
        );
        resampled.samples
    };

    let duration_s = mono_samples.len() as f32 / device_rate as f32;
    info!(
        device = device.to_string(),
        sample_rate = device_rate,
        channels = device_channels,
        samples = mono_samples.len(),
        duration_s = format!("{duration_s:.2}"),
        "playing on speaker"
    );

    let config = cpal::StreamConfig {
        channels: device_channels,
        sample_rate: device_rate,
        buffer_size: cpal::BufferSize::Default,
    };

    let ch = device_channels as usize;
    // Total interleaved output samples
    let total_frames = mono_samples.len();
    let cursor = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let cursor_c = std::sync::Arc::clone(&cursor);
    let done_c = std::sync::Arc::clone(&done);

    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let pos = cursor_c.load(std::sync::atomic::Ordering::Relaxed);
            let mut frame_idx = pos;
            for chunk in data.chunks_exact_mut(ch) {
                let val = if frame_idx < total_frames {
                    mono_samples[frame_idx] as f32 / i16::MAX as f32
                } else {
                    0.0
                };
                for out in chunk.iter_mut() {
                    *out = val;
                }
                frame_idx += 1;
            }
            cursor_c.store(frame_idx, std::sync::atomic::Ordering::Relaxed);
            if frame_idx >= total_frames {
                done_c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
        |err| warn!("output stream error: {err}"),
        None,
    )?;

    stream.play()?;

    // Wait for playback to finish
    while !done.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
    // Small tail to let the audio buffer drain
    std::thread::sleep(Duration::from_millis(100));

    drop(stream);
    info!("playback complete");
    Ok(())
}

/// Record a single utterance from the default input device.
///
/// Waits for speech (amplitude above threshold), then records until
/// silence is detected (amplitude below threshold for `silence_ms`).
/// Returns `None` if the stop flag was set before speech was detected.
pub fn record_utterance(
    stop: Arc<AtomicBool>,
    silence_ms: u64,
) -> Result<Option<AudioFrame>, anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no default input device found"))?;

    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let sample_format = config.sample_format();

    let (tx, rx) = mpsc::channel::<Vec<i16>>();
    let err_fn = |err| warn!("input stream error: {err}");

    let stream = match sample_format {
        cpal::SampleFormat::I16 => {
            let tx = tx.clone();
            device.build_input_stream(
                config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let _ = tx.send(data.to_vec());
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::F32 => {
            let tx = tx.clone();
            device.build_input_stream(
                config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let samples: Vec<i16> = data
                        .iter()
                        .map(|&s| {
                            let clamped = s.clamp(-1.0, 1.0);
                            (clamped * i16::MAX as f32) as i16
                        })
                        .collect();
                    let _ = tx.send(samples);
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("unsupported sample format: {other:?}"),
    };

    stream.play()?;

    const POLL_MS: u64 = 10;

    let silence_limit = (silence_ms as f64 / 1000.0 * sample_rate as f64) as usize;
    let mut all_samples: Vec<i16> = Vec::new();
    let mut speech_detected = false;
    let mut silent_count: usize = 0;

    loop {
        if stop.load(Ordering::Relaxed) {
            drop(stream);
            return Ok(None);
        }

        let mut got_data = false;
        while let Ok(chunk) = rx.try_recv() {
            got_data = true;
            let rms = compute_rms(&chunk);

            if speech_detected {
                all_samples.extend(&chunk);
                if rms > SPEECH_THRESHOLD {
                    silent_count = 0;
                } else {
                    silent_count += chunk.len() / channels as usize;
                }

                if silent_count >= silence_limit {
                    drop(stream);

                    let mono = if channels > 1 {
                        mix_to_mono(&all_samples, channels)
                    } else {
                        all_samples
                    };

                    let dur = mono.len() as f32 / sample_rate as f32;
                    info!(
                        samples = mono.len(),
                        duration_s = format!("{dur:.2}"),
                        "utterance captured"
                    );

                    return Ok(Some(AudioFrame {
                        samples: mono,
                        sample_rate,
                        channels: 1,
                    }));
                }
            } else if rms > SPEECH_THRESHOLD {
                speech_detected = true;
                info!(rms = format!("{rms:.0}"), "speech detected");
                all_samples.extend(&chunk);
                silent_count = 0;
            }
        }

        if !got_data {
            std::thread::sleep(Duration::from_millis(POLL_MS));
        }
    }
}

pub use super::compute_rms;

// ── Streaming mic capture ───────────────────────────────────

use crate::pipeline::AudioChunk;

/// Stream microphone audio as fixed-size chunks via a tokio mpsc channel.
///
/// Runs on a **blocking** thread (cpal callbacks are sync).
/// Emits `AudioChunk`s every `chunk_ms` milliseconds (mono).
/// Sends **all** captured audio -- no VAD or silence filtering.
/// VAD should be applied downstream via `pipeline::vad_filter_task`.
///
/// Returns when `stop` is set to `true` or when the receiver is dropped.
pub fn mic_capture_raw(
    tx: tokio::sync::mpsc::Sender<AudioChunk>,
    stop: Arc<AtomicBool>,
    chunk_ms: u32,
) -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no default input device found"))?;

    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let sample_format = config.sample_format();

    info!(
        device = device.to_string(),
        sample_rate, channels, chunk_ms, "starting raw mic capture"
    );

    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let err_fn = |err| warn!("input stream error: {err}");

    let stream = build_cpal_input_stream(&device, &config.into(), sample_format, raw_tx, err_fn)?;
    stream.play()?;

    let chunk_samples = (sample_rate as u64 * chunk_ms as u64 / 1000) as usize;
    let mut accumulator: Vec<i16> = Vec::with_capacity(chunk_samples * 2);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let mut got_data = false;
        while let Ok(data) = raw_rx.try_recv() {
            got_data = true;
            let mono = if channels > 1 {
                mix_to_mono(&data, channels)
            } else {
                data
            };
            accumulator.extend(&mono);
        }

        while accumulator.len() >= chunk_samples {
            let samples: Vec<i16> = accumulator.drain(..chunk_samples).collect();
            let chunk = AudioChunk {
                samples,
                sample_rate,
                channels: 1,
                is_end_of_utterance: false,
                caption: None,
                utterance_id: 0,
                full_text: None,
                extras: None,
                last_speech_at: None,
                synth_duration: None,
            };
            if tx.blocking_send(chunk).is_err() {
                info!("raw mic capture: downstream channel closed");
                drop(stream);
                return Ok(());
            }
        }

        if !got_data {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    drop(stream);
    info!("raw mic capture stopped");
    Ok(())
}

/// Emits `AudioChunk`s every `chunk_ms` milliseconds (mono).
/// Chunks during silence are suppressed; `is_end_of_utterance` is set when a
/// speech -> silence transition exceeds `silence_ms`.
///
/// **Deprecated**: prefer `mic_capture_raw` + `pipeline::vad_filter_task` so
/// the same VAD logic is shared between local and transport paths.
///
/// Returns when `stop` is set to `true` or when the receiver is dropped.
pub fn mic_stream(
    tx: tokio::sync::mpsc::Sender<AudioChunk>,
    stop: Arc<AtomicBool>,
    chunk_ms: u32,
    silence_ms: u64,
    mut vad: Box<dyn crate::vad::segmenter::VadBackend>,
) -> Result<(), anyhow::Error> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow::anyhow!("no default input device found"))?;

    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let sample_format = config.sample_format();

    info!(
        device = device.to_string(),
        sample_rate, channels, chunk_ms, silence_ms, "starting mic stream"
    );

    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let err_fn = |err| warn!("input stream error: {err}");

    let stream = build_cpal_input_stream(&device, &config.into(), sample_format, raw_tx, err_fn)?;
    stream.play()?;

    let chunk_samples = (sample_rate as u64 * chunk_ms as u64 / 1000) as usize;
    let silence_limit = (silence_ms as f64 / 1000.0 * sample_rate as f64) as usize;

    info!(vad = vad.name(), "mic stream VAD backend");

    run_mic_stream_loop(
        &raw_rx,
        &tx,
        &stop,
        sample_rate,
        channels,
        chunk_samples,
        silence_limit,
        vad.as_mut(),
    );

    drop(stream);
    info!("mic stream stopped");
    Ok(())
}

/// Core mic-stream logic, separated from device I/O for testability.
fn run_mic_stream_loop(
    raw_rx: &mpsc::Receiver<Vec<i16>>,
    tx: &tokio::sync::mpsc::Sender<AudioChunk>,
    stop: &AtomicBool,
    sample_rate: u32,
    channels: u16,
    chunk_samples: usize,
    silence_limit: usize,
    vad: &mut dyn crate::vad::segmenter::VadBackend,
) {
    use std::collections::VecDeque;

    let mut accumulator: Vec<i16> = Vec::with_capacity(chunk_samples * 2);
    let mut speech_active = false;
    let mut silent_samples: usize = 0;
    let mut forwarded_non_speech_chunks: u64 = 0;

    // Circular buffer of recent silent chunks so we can prepend them
    // when speech starts, avoiding clipped word onsets.
    let mut pre_roll: VecDeque<Vec<i16>> = VecDeque::with_capacity(PRE_ROLL_CHUNKS + 1);

    // ── Speech onset confirmation ────────────────────────────
    // Buffer speech chunks until we see MIN_ONSET_CHUNKS consecutive
    // speech frames.  If silence arrives before that, the buffered
    // chunks are discarded (noise spike).
    const MIN_ONSET_CHUNKS: usize = 3; // 3 x 20 ms = 60 ms
    let mut onset_buf: VecDeque<Vec<i16>> = VecDeque::new();
    let mut onset_speech_count: usize = 0;

    // Periodic VAD diagnostic logging (info level) so users can tune thresholds.
    let mut vad_diag_counter: u64 = 0;
    let mut vad_max_prob: f32 = 0.0;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // Drain cpal callback data
        let mut got_data = false;
        while let Ok(data) = raw_rx.try_recv() {
            got_data = true;
            let mono = if channels > 1 {
                mix_to_mono(&data, channels)
            } else {
                data
            };
            accumulator.extend(&mono);
        }

        // Emit fixed-size chunks
        while accumulator.len() >= chunk_samples {
            let chunk_data: Vec<i16> = accumulator.drain(..chunk_samples).collect();

            let is_speech = match vad.process_chunk(&chunk_data, sample_rate) {
                Ok(r) => {
                    if r.speech_prob > vad_max_prob {
                        vad_max_prob = r.speech_prob;
                    }
                    vad_diag_counter += 1;
                    // Log peak probability every ~5 s (250 x 20 ms)
                    if vad_diag_counter.is_multiple_of(250) {
                        tracing::info!(
                            max_prob = format!("{:.3}", vad_max_prob),
                            vad = vad.name(),
                            "VAD diagnostic (peak prob last 5 s)"
                        );
                        vad_max_prob = 0.0;
                    }
                    r.is_speech
                }
                Err(e) => {
                    tracing::error!(error = %e, "VAD error");
                    false // on VAD error, treat as silence
                }
            };

            // ── Onset confirmation state machine ─────────────
            if !speech_active {
                if is_speech {
                    onset_buf.push_back(chunk_data.clone());
                    onset_speech_count += 1;

                    if onset_speech_count >= MIN_ONSET_CHUNKS {
                        // Confirmed real speech -- activate and flush
                        // the pre-roll + onset buffers.
                        speech_active = true;
                        forwarded_non_speech_chunks = 0;
                        tracing::debug!(
                            onset_chunks = onset_speech_count,
                            "speech onset confirmed"
                        );

                        for pre in pre_roll.drain(..) {
                            let chunk = AudioChunk {
                                samples: pre,
                                sample_rate,
                                channels: 1,
                                is_end_of_utterance: false,
                                caption: None,
                                utterance_id: 0,
                                full_text: None,
                                extras: None,
                                last_speech_at: None,
                                synth_duration: None,
                            };
                            if tx.blocking_send(chunk).is_err() {
                                return;
                            }
                        }
                        for buf in onset_buf.drain(..) {
                            let chunk = AudioChunk {
                                samples: buf,
                                sample_rate,
                                channels: 1,
                                is_end_of_utterance: false,
                                caption: None,
                                utterance_id: 0,
                                full_text: None,
                                extras: None,
                                last_speech_at: None,
                                synth_duration: None,
                            };
                            if tx.blocking_send(chunk).is_err() {
                                return;
                            }
                        }
                        onset_speech_count = 0;
                    }
                } else {
                    // Silence during onset -- discard buffered chunks
                    // as a noise spike.
                    if onset_speech_count > 0 {
                        tracing::debug!(
                            onset_chunks = onset_speech_count,
                            "onset aborted (noise spike)"
                        );
                        onset_buf.clear();
                        onset_speech_count = 0;
                    }
                    // Maintain pre-roll ring buffer.
                    if pre_roll.len() >= PRE_ROLL_CHUNKS {
                        pre_roll.pop_front();
                    }
                    pre_roll.push_back(chunk_data);
                }
                continue;
            }

            // ── Active speech tracking ───────────────────────
            if is_speech {
                silent_samples = 0;
            } else {
                silent_samples += chunk_data.len();
            }

            let is_end = silent_samples >= silence_limit;

            // Once speech is active, forward contiguous audio. Dropping
            // transient low-energy chunks inside an utterance can clip
            // consonants and hurt STT accuracy for natural/faster speech.
            if speech_active {
                if !is_speech {
                    forwarded_non_speech_chunks += 1;
                }
                let chunk = AudioChunk {
                    samples: chunk_data,
                    sample_rate,
                    channels: 1,
                    is_end_of_utterance: false,
                    caption: None,
                    utterance_id: 0,
                    full_text: None,
                    extras: None,
                    last_speech_at: None,
                    synth_duration: None,
                };
                if tx.blocking_send(chunk).is_err() {
                    return;
                }
            }

            // When enough silence has elapsed, send a zero-length
            // end-of-utterance sentinel so the pipeline knows the
            // utterance is finished.
            if is_end {
                tracing::info!(
                    forwarded_non_speech_chunks,
                    "mic stream: end-of-utterance after forwarding active speech window"
                );
                let sentinel = AudioChunk {
                    samples: Vec::new(),
                    sample_rate,
                    channels: 1,
                    is_end_of_utterance: true,
                    caption: None,
                    utterance_id: 0,
                    full_text: None,
                    extras: None,
                    last_speech_at: None,
                    synth_duration: None,
                };
                if tx.blocking_send(sentinel).is_err() {
                    return;
                }
                speech_active = false;
                silent_samples = 0;
                vad.reset();
            }
        }

        if !got_data {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Build a cpal input stream that sends raw i16 samples to `raw_tx`.
fn build_cpal_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    raw_tx: mpsc::Sender<Vec<i16>>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, anyhow::Error> {
    match sample_format {
        cpal::SampleFormat::I16 => {
            let tx = raw_tx;
            Ok(device.build_input_stream(
                *config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let _ = tx.send(data.to_vec());
                },
                err_fn,
                None,
            )?)
        }
        cpal::SampleFormat::F32 => {
            let tx = raw_tx;
            Ok(device.build_input_stream(
                *config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let samples: Vec<i16> = data
                        .iter()
                        .map(|&s| {
                            let clamped = s.clamp(-1.0, 1.0);
                            (clamped * i16::MAX as f32) as i16
                        })
                        .collect();
                    let _ = tx.send(samples);
                },
                err_fn,
                None,
            )?)
        }
        other => Err(anyhow::anyhow!("unsupported sample format: {other:?}")),
    }
}

/// List available audio devices (for diagnostics).
pub fn list_devices() {
    let host = cpal::default_host();

    if let Some(dev) = host.default_input_device() {
        info!(device = dev.to_string(), "default input device");
    }
    if let Some(dev) = host.default_output_device() {
        info!(device = dev.to_string(), "default output device");
    }
}

fn mix_to_mono(interleaved: &[i16], channels: u16) -> Vec<i16> {
    let ch = channels as usize;
    interleaved
        .chunks(ch)
        .map(|frame| {
            let sum: i32 = frame.iter().map(|&s| s as i32).sum();
            (sum / ch as i32) as i16
        })
        .collect()
}

#[cfg(test)]
#[path = "tests/io.rs"]
mod tests;
