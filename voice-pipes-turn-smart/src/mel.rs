//! Whisper-style log-mel feature extraction for Smart Turn v3.
//!
//! Smart Turn v3 is built on a Whisper encoder: its ONNX input `input_features`
//! is an 80-bin log-mel spectrogram of shape `[batch, 80, 800]` (8 s @ 16 kHz,
//! hop 160), NOT raw audio (that was v2). This reproduces `HuggingFace`'s
//! `WhisperFeatureExtractor` (`feature_size=80`, `n_fft=400`, hop=160, `chunk_length=8`)
//! so the features match what the model was trained on:
//!
//!   1. right-pad / tail-truncate audio to 128000 samples (8 s)
//!   2. reflect-pad by `n_fft/2` (center=True), periodic Hann window
//!   3. power spectrum |rfft|^2 over 400-sample frames, hop 160
//!   4. slaney-normalized mel filterbank (80 mels, 0–8000 Hz)
//!   5. log10(max(mel, 1e-10)); keep the first 800 frames (Whisper drops the last)
//!   6. floor at `global_max` − 8; then (x + 4) / 4
//!
//! The reference test pins the output against `transformers.WhisperFeatureExtractor`
//! for a deterministic input, so a subtly-wrong filterbank/normalization can't
//! silently ship (which would give the classifier garbage features).

use std::sync::LazyLock;

use voice_pipes_onnx::mel::{Hann, PowerSpectrogram, hann, mel_energies, slaney_filterbank};

pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = 400;
pub const HOP: usize = 160;
pub const N_MELS: usize = 80;
pub const N_FRAMES: usize = 800; // 8 s at hop 160 (Whisper drops the 801st frame)
pub const WINDOW_SAMPLES: usize = SAMPLE_RATE * 8; // 128000
const FMIN: f64 = 0.0;
const FMAX: f64 = 8_000.0;
const LOG_CLAMP: f32 = 1e-10;

/// Reflect-pad by `pad` samples each side (numpy `mode="reflect"`, torch
/// `center=True`): edge sample is NOT repeated.
fn reflect_pad(x: &[f32], pad: usize) -> Vec<f32> {
    let n = x.len();
    let at = |i: usize| x.get(i).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(n + 2 * pad);
    for i in 0..pad {
        out.push(at((pad - i).min(n.saturating_sub(1))));
    }
    out.extend_from_slice(x);
    for i in 0..pad {
        out.push(at(n.saturating_sub(i + 2)));
    }
    out
}

/// What every extraction shares: built on first use and kept, since the
/// detector computes features once per pause. Periodic Hann (`torch.hann_window(n)`),
/// slaney-normalised mel filterbank (`mel_scale="slaney"`, 80 mels, 0–8000 Hz).
struct Tables {
    spectrogram: PowerSpectrogram,
    fb: Vec<Vec<f32>>,
}

static TABLES: LazyLock<Tables> = LazyLock::new(|| Tables {
    spectrogram: PowerSpectrogram::new(hann(N_FFT, Hann::Periodic), HOP),
    fb: slaney_filterbank(SAMPLE_RATE, N_FFT, N_MELS, FMIN, FMAX),
});

/// Compute Whisper log-mel `input_features` for Smart Turn v3.
///
/// `samples` is 16 kHz mono f32 in `[-1, 1]`; it's right-padded (or tail-
/// truncated, keeping the most recent audio) to 8 s before extraction. Returns a
/// flat `N_MELS * N_FRAMES` buffer in row-major `[mel][frame]` order — i.e. the
/// `[1, 80, 800]` tensor payload.
pub fn log_mel_features(samples: &[f32]) -> Vec<f32> {
    // 1. Fixed 8 s window: recent audio at the front, zero-padded to length
    //    (WhisperFeatureExtractor padding="max_length"). Keep the tail if longer.
    let (_, src) = samples.split_at(samples.len().saturating_sub(WINDOW_SAMPLES));
    let mut window_buf = vec![0.0f32; WINDOW_SAMPLES];
    for (dst, s) in window_buf.iter_mut().zip(src) {
        *dst = *s;
    }

    // 2. center reflect-pad, then the periodic-Hann power spectra.
    let signal = reflect_pad(&window_buf, N_FFT / 2);
    let Tables { spectrogram, fb } = &*TABLES;

    // Row-major [mel][frame]. Compute only the first N_FRAMES (=800) frames;
    // Whisper produces 801 and drops the last, which is exactly skipping frame 800.
    let mut mel = vec![0.0f32; N_MELS * N_FRAMES];
    spectrogram.for_each(&signal, N_FRAMES, |t, power| {
        for (m, e) in mel_energies(fb, power).enumerate() {
            if let Some(v) = mel.get_mut(m * N_FRAMES + t) {
                *v = e.max(LOG_CLAMP).log10();
            }
        }
    });

    // 3. Whisper normalization: floor at global_max - 8, then (x + 4) / 4.
    let gmax = mel.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let floor = gmax - 8.0;
    for v in &mut mel {
        *v = (v.max(floor) + 4.0) / 4.0;
    }
    mel
}

#[cfg(test)]
#[path = "tests/mel.rs"]
mod tests;
