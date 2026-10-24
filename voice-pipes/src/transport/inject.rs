//! Audio file injection -- feeds a WAV file into the pipeline as if it
//! were live microphone audio, enabling deterministic benchmarking.
//!
//! The injector reads a WAV file, chunks it into pipeline-sized frames,
//! and sends them at real-time rate (or as-fast-as-possible when `speed`
//! is 0.0) through the same `audio_tx` channel used by live transport.

use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::audio::wav_reader::read_wav;
use crate::pipeline::AudioChunk;

/// Default chunk duration in milliseconds when feeding audio to the pipeline.
const CHUNK_DURATION_MS: u64 = 20;

/// Process-wide switch for WS-triggered audio injection.
///
/// `inject_audio` reads a client-supplied filesystem path, so a remote
/// caller could exfiltrate any file the process can read. The serve
/// command resolves this at startup: enabled in dev (no client auth,
/// matching today's bench workflows), disabled once `VP_CLIENT_TOKEN`
/// enforcement is on, and overridable either way with
/// `VP_ENABLE_AUDIO_INJECTION=1|0`. Defaults to enabled for
/// non-serve entrypoints (CLI/tests), which are inherently local.
pub static INJECTION_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Resolve and store the injection policy. Called once from serve
/// startup; see [`INJECTION_ENABLED`].
pub fn configure_injection(client_auth_enabled: bool) {
    let allowed = match std::env::var("VP_ENABLE_AUDIO_INJECTION").ok().as_deref() {
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true") => true,
        Some(_) => false,
        None => !client_auth_enabled,
    };
    INJECTION_ENABLED.store(allowed, std::sync::atomic::Ordering::Relaxed);
    if !allowed {
        info!("WS audio injection disabled (set VP_ENABLE_AUDIO_INJECTION=1 to allow)");
    }
}

/// Result of an audio injection run.
#[derive(Debug, Clone)]
pub struct InjectionResult {
    /// Total number of chunks sent.
    pub chunks_sent: u64,
    /// Total number of samples sent.
    pub samples_sent: u64,
    /// Duration of the source audio in seconds.
    pub audio_duration_s: f64,
    /// Source file path.
    pub source: String,
}

/// Inject a WAV file into the pipeline's audio channel.
///
/// Reads the entire WAV file, splits into `CHUNK_DURATION_MS`-sized
/// chunks, and sends them through `audio_tx` at the specified speed.
///
/// - `speed = 1.0`: real-time playback (20 ms of audio every 20 ms)
/// - `speed = 0.0`: as-fast-as-possible (no inter-chunk delay)
/// - `speed = 2.0`: double speed (20 ms of audio every 10 ms)
pub async fn inject_audio_file(
    source: &Path,
    audio_tx: &mpsc::Sender<AudioChunk>,
    speed: f32,
) -> Result<InjectionResult, anyhow::Error> {
    let source_str = source.display().to_string();
    info!(source = %source_str, speed, "starting audio injection");

    let frame = read_wav(source)?;

    let sample_rate = frame.sample_rate;
    let channels = frame.channels;
    let total_samples = frame.samples.len();
    let samples_per_chunk =
        (sample_rate as u64 * CHUNK_DURATION_MS / 1000) as usize * channels as usize;

    if samples_per_chunk == 0 {
        anyhow::bail!("invalid sample rate or channels: sr={sample_rate}, ch={channels}");
    }

    let audio_duration_s = total_samples as f64 / (sample_rate as f64 * channels as f64);
    let chunk_delay = if speed <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_millis((CHUNK_DURATION_MS as f64 / speed as f64) as u64)
    };

    let chunks: Vec<Vec<i16>> = frame
        .samples
        .chunks(samples_per_chunk)
        .map(<[i16]>::to_vec)
        .collect();

    let num_chunks = chunks.len() as u64;
    debug!(
        num_chunks,
        samples_per_chunk,
        audio_duration_s,
        ?chunk_delay,
        "chunked audio file for injection"
    );

    for (i, samples) in chunks.into_iter().enumerate() {
        let is_last = i as u64 == num_chunks - 1;
        let chunk = AudioChunk {
            samples,
            sample_rate,
            channels,
            is_end_of_utterance: is_last,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        };

        if audio_tx.send(chunk).await.is_err() {
            warn!(chunk_index = i, "audio channel closed during injection");
            return Ok(InjectionResult {
                chunks_sent: i as u64,
                samples_sent: i as u64 * samples_per_chunk as u64,
                audio_duration_s,
                source: source_str,
            });
        }

        if !chunk_delay.is_zero() && !is_last {
            tokio::time::sleep(chunk_delay).await;
        }
    }

    info!(
        chunks_sent = num_chunks,
        audio_duration_s,
        source = %source_str,
        "audio injection complete"
    );

    Ok(InjectionResult {
        chunks_sent: num_chunks,
        samples_sent: total_samples as u64,
        audio_duration_s,
        source: source_str,
    })
}

#[cfg(test)]
#[path = "tests/inject.rs"]
mod tests;
