//! Native Rust STFT + log-mel filterbank matching `NeMo`'s
//! `AudioToMelSpectrogramPreprocessor` (`FilterbankFeatures`) defaults used by
//! Nemotron / `FastConformer` / Conformer ASR models.
//!
//! Matches:
//!   `sample_rate=16000`, `n_fft=512`, `win_length=400`, `hop_length=160`, hann window,
//!   nfilt=128, lowfreq=0, highfreq=sr/2,
//!   preemph=0.97,
//!   center=True with reflect padding,
//!   `mag_power=2.0` (power spectrum),
//!   slaney-normalized mel filterbank (librosa norm='slaney'),
//!   log(x + 2^-24),
//!   per-feature mean/var normalization across time (`normalize='per_feature'`).
//!
//! Dither is intentionally not applied (deterministic inference path).

use voice_pipes_onnx::mel::{
    Hann, PowerSpectrogram, centred, hann, mel_energies, slaney_filterbank,
};

const PREEMPH: f32 = 0.97;
const LOG_ZERO_GUARD: f32 = 1.0 / (1u64 << 24) as f32; // 2^-24
const NORM_EPS: f32 = 1e-5;

/// y[0] = x[0]; y[n] = x[n] - preemph * x[n-1]
fn preemphasis(x: &[f32], preemph: f32) -> Vec<f32> {
    let Some(&first) = x.first() else {
        return Vec::new();
    };
    std::iter::once(first)
        .chain(
            x.iter()
                .zip(x.iter().skip(1))
                .map(|(prev, cur)| cur - preemph * prev),
        )
        .collect()
}

/// Reflect-pad by `pad` samples on each side (torch.stft `center=True`,
/// `pad_mode='reflect'`).
fn reflect_pad(x: &[f32], pad: usize) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let n = x.len();
    let at = |i: usize| x.get(i).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(n + 2 * pad);
    for i in 0..pad {
        out.push(at((pad - i).min(n - 1)));
    }
    out.extend_from_slice(x);
    for i in 0..pad {
        out.push(at(n.saturating_sub(i + 2)));
    }
    out
}

/// Compute log-mel spectrogram matching `NeMo`'s `AudioToMelSpectrogramPreprocessor`.
/// Returns `mel[mel_bin][time_frame]`.
///
/// **Default: raw log-mel** matching the trained model's `model_config.yaml`
/// (`normalize: NA`, `pad_to: 0`). Per-feature normalization and `pad_to=16`
/// are NOT applied -- they would shift the input distribution away from what
/// the encoder was trained on (mean ~ -12, std ~ 3.4) and produce gibberish.
///
/// Set `NEMOTRON_NORMALIZED_MEL=1` to restore the legacy normalized+padded
/// behaviour (debug / back-compat only).
pub fn log_mel_spectrogram(
    waveform: &[f32],
    sample_rate: usize,
    n_fft: usize,
    win_length: usize,
    hop_length: usize,
    n_mels: usize,
    fmin: f32,
    fmax: f32,
) -> Vec<Vec<f32>> {
    let normalized = legacy_normalized_mel_enabled();
    log_mel_spectrogram_opts(
        waveform,
        sample_rate,
        n_fft,
        win_length,
        hop_length,
        n_mels,
        fmin,
        fmax,
        normalized,
        normalized,
    )
}

/// Returns true when the legacy normalized + `pad_to=16` mel path is opted into
/// via `NEMOTRON_NORMALIZED_MEL=1`. Default is false (raw log-mel).
///
/// The env var is read once and cached: the streaming loop called
/// this per audio chunk.
pub fn legacy_normalized_mel_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("NEMOTRON_NORMALIZED_MEL")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    })
}

/// Shared `[mel_bin][freq_bin]` filterbank (see [`cached_filterbank`]).
type Filterbank = std::sync::Arc<Vec<Vec<f32>>>;

/// Slaney-normalised mel filterbank (librosa `norm='slaney'`), cached by its
/// parameters: the streaming loop asks for the same one on every chunk.
fn cached_filterbank(
    n_fft: usize,
    n_mels: usize,
    sample_rate: usize,
    fmin: f32,
    fmax: f32,
) -> Filterbank {
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::OnceLock;
    type Key = (usize, usize, usize, u32, u32);
    static CACHE: OnceLock<Mutex<HashMap<Key, Filterbank>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (n_fft, n_mels, sample_rate, fmin.to_bits(), fmax.to_bits());
    let mut map = cache.lock();
    std::sync::Arc::clone(map.entry(key).or_insert_with(|| {
        std::sync::Arc::new(slaney_filterbank(
            sample_rate,
            n_fft,
            n_mels,
            f64::from(fmin),
            f64::from(fmax),
        ))
    }))
}

/// Like [`log_mel_spectrogram`] but with explicit toggles for per-feature
/// normalization and `pad_to=16` padding.
pub fn log_mel_spectrogram_opts(
    waveform: &[f32],
    sample_rate: usize,
    n_fft: usize,
    win_length: usize,
    hop_length: usize,
    n_mels: usize,
    fmin: f32,
    fmax: f32,
    normalize: bool,
    pad_to_16: bool,
) -> Vec<Vec<f32>> {
    if waveform.is_empty() {
        return vec![Vec::new(); n_mels];
    }

    let signal = preemphasis(waveform, PREEMPH);
    let signal = reflect_pad(&signal, n_fft / 2);

    // A symmetric Hann (`window_fn(win_length, periodic=False)`), centred
    // in the `n_fft` frame.
    let spectrogram = PowerSpectrogram::new(
        centred(&hann(win_length, Hann::Symmetric), n_fft),
        hop_length,
    );
    let n_frames = spectrogram.frames(signal.len());
    let fbanks = cached_filterbank(n_fft, n_mels, sample_rate, fmin, fmax);

    let mut mel_spec = vec![vec![0.0f32; n_frames]; n_mels];
    spectrogram.for_each(&signal, n_frames, |t, power| {
        for (row, e) in mel_spec.iter_mut().zip(mel_energies(&fbanks, power)) {
            if let Some(v) = row.get_mut(t) {
                *v = (e + LOG_ZERO_GUARD).ln();
            }
        }
    });

    // Per-feature normalization across the real (unpadded) frames
    // (normalize='per_feature') with unbiased std (NeMo's torch.std default).
    // NeMo applies normalization BEFORE pad_to and pads with zero (pad_value=0)
    // POST-normalization, so the padded slots become exactly 0 (= the post-
    // normalization mean). Mismatching this corrupts the encoder input on the
    // last chunk(s).
    if normalize && n_frames > 1 {
        for row in &mut mel_spec {
            let mean: f32 = row.iter().sum::<f32>() / n_frames as f32;
            let var: f32 =
                row.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / (n_frames - 1) as f32;
            let std = var.sqrt() + NORM_EPS;
            for v in row.iter_mut() {
                *v = (*v - mean) / std;
            }
        }
    }

    // pad_to=16: pad time dim to next multiple of 16 with **zero** (NeMo
    // `pad_value=0`, applied AFTER normalization).
    let padded_frames = if !pad_to_16 || n_frames == 0 {
        n_frames
    } else {
        n_frames.div_ceil(16) * 16
    };
    if padded_frames > n_frames {
        for row in &mut mel_spec {
            row.resize(padded_frames, 0.0);
        }
    }

    mel_spec
}

/// Part of the raw log-mel [`log_mel_spectrogram`] computes over a whole
/// utterance, from a stretch of it: the frames centred on `raw[left]`,
/// `raw[left + hop]`, and on while their windows fit, in the same
/// `[mel_bin][frame]` layout.
///
/// `left` is how much of the audio before the first centre `raw` holds.
/// At the utterance's start it is 0: the frames are reflect-padded and the
/// first sample is not pre-emphasised, as a whole-utterance pass does.
/// Anywhere else it must be more than `n_fft / 2`, so the first window and
/// its pre-emphasis read real audio rather than padding. With `ends`, the
/// utterance ends where `raw` does and the last frames are reflect-padded
/// there. Frame for frame, the result equals the whole-utterance
/// spectrogram at the same centres, which is what lets a stream feed the
/// encoder exactly what an offline pass would.
pub fn utterance_log_mel(
    raw: &[f32],
    left: usize,
    ends: bool,
    sample_rate: usize,
    n_fft: usize,
    win_length: usize,
    hop_length: usize,
    n_mels: usize,
    fmin: f32,
    fmax: f32,
) -> Vec<Vec<f32>> {
    let pad = n_fft / 2;
    if raw.len() <= left {
        return vec![Vec::new(); n_mels];
    }
    let mut signal = if left == 0 {
        let emphasised = preemphasis(raw, PREEMPH);
        let last = emphasised.len() - 1;
        let mut padded: Vec<f32> = (0..pad)
            .map(|i| emphasised.get((pad - i).min(last)).copied().unwrap_or(0.0))
            .collect();
        padded.extend_from_slice(&emphasised);
        padded
    } else {
        assert!(
            left > pad,
            "{left} samples of left context; more than {pad} needed"
        );
        // Each sample from `left - pad` on, emphasised by the one before.
        raw.iter()
            .zip(raw.iter().skip(1))
            .skip(left - pad - 1)
            .map(|(prev, cur)| cur - PREEMPH * prev)
            .collect()
    };
    if ends {
        // The end mirrors the pre-emphasised audio before it, as the
        // whole-utterance pass's right padding does.
        let n = signal.len();
        let tail: Vec<f32> = (0..pad)
            .map(|i| signal.get(n.saturating_sub(i + 2)).copied().unwrap_or(0.0))
            .collect();
        signal.extend(tail);
    }

    let spectrogram = PowerSpectrogram::new(
        centred(&hann(win_length, Hann::Symmetric), n_fft),
        hop_length,
    );
    let n_frames = spectrogram.frames(signal.len());
    let fbanks = cached_filterbank(n_fft, n_mels, sample_rate, fmin, fmax);
    let mut mel_spec = vec![vec![0.0f32; n_frames]; n_mels];
    spectrogram.for_each(&signal, n_frames, |t, power| {
        for (row, e) in mel_spec.iter_mut().zip(mel_energies(&fbanks, power)) {
            if let Some(v) = row.get_mut(t) {
                *v = (e + LOG_ZERO_GUARD).ln();
            }
        }
    });
    mel_spec
}

#[cfg(test)]
#[path = "tests/mel_native.rs"]
mod tests;
