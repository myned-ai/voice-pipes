//! Layer 1 DSP: acoustic sanity metrics, noise (`SNR` / `HNR`), spectral flatness,
//! and digital crackle / click / pop detection with chunk-boundary attribution.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use serde::{Deserialize, Serialize};

/// Core acoustic & noise metrics computed directly from a mono waveform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcousticMetrics {
    pub sample_rate: u32,
    pub duration_s: f64,
    pub rms_dbfs: f64,
    pub peak_abs: f64,
    pub clipping_pct: f64,
    pub dc_offset: f64,
    pub active_speech_ratio: f64,
    pub leading_silence_ms: f64,
    /// Fraction of spectral energy inside wideband human speech (`80..4000 Hz`).
    pub speech_band_ratio: f64,
    /// Fraction of spectral energy above `6000 Hz` (screech / hiss / aliasing).
    pub high_freq_ratio: f64,
    /// Mean Wiener entropy (spectral flatness) on voiced/active frames (`0` = tonal, `1` = noise).
    pub spectral_flatness: f64,
    /// Segmental speech-to-noise ratio (90th vs 10th percentile frame RMS, in dB).
    pub segmental_snr_db: f64,
    /// Mean Harmonics-to-Noise Ratio (`HNR`) on voiced frames, in dB.
    pub hnr_db: f64,
    /// Digital crackle / click / pop impulse diagnostics.
    pub crackle: CrackleMetrics,
}

/// Digital crackle / click / pop impulse metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrackleMetrics {
    /// Total detected sharp discontinuity impulses (debounced to 2 ms).
    pub crackle_count: usize,
    /// Crackles per second of audio.
    pub crackles_per_sec: f64,
    /// Crackles occurring within ±1.5 ms of an 80 ms (`Mimi`) or 50 ms (pacer) chunk seam.
    pub boundary_crackle_count: usize,
    /// Timestamp (ms) of the sharpest detected crackle, if any.
    pub worst_crackle_ms: Option<f64>,
    /// Magnitude of the sharpest 2nd-derivative impulse (`|x[n+1] - 2x[n] + x[n-1]|`).
    pub worst_crackle_magnitude: f64,
}

/// Analyze mono `f32` samples in `[-1.0, 1.0]` at `sample_rate`.
pub fn analyze_acoustics(samples: &[f32], sample_rate: u32) -> AcousticMetrics {
    let n = samples.len();
    let sr = f64::from(sample_rate.max(1));
    let duration_s = (n as f64) / sr;
    if n == 0 {
        return empty_metrics(sample_rate);
    }

    let (rms, peak_abs, dc_offset, clipped) = basic_amplitude_stats(samples);
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let clipping_pct = 100.0 * (clipped as f64) / (n as f64);

    let frame_rms = short_time_rms(samples, sample_rate, 20, 10);
    let (active_speech_ratio, leading_silence_ms, segmental_snr_db) =
        activity_and_snr(&frame_rms, 10.0);
    let (speech_band_ratio, high_freq_ratio, spectral_flatness) =
        spectral_stats(samples, sample_rate);
    let hnr_db = compute_mean_hnr_db(samples, sample_rate);
    let crackle = detect_crackles(samples, sample_rate);

    AcousticMetrics {
        sample_rate,
        duration_s,
        rms_dbfs,
        peak_abs,
        clipping_pct,
        dc_offset,
        active_speech_ratio,
        leading_silence_ms,
        speech_band_ratio,
        high_freq_ratio,
        spectral_flatness,
        segmental_snr_db,
        hnr_db,
        crackle,
    }
}

fn empty_metrics(sample_rate: u32) -> AcousticMetrics {
    AcousticMetrics {
        sample_rate,
        duration_s: 0.0,
        rms_dbfs: -120.0,
        peak_abs: 0.0,
        clipping_pct: 0.0,
        dc_offset: 0.0,
        active_speech_ratio: 0.0,
        leading_silence_ms: 0.0,
        speech_band_ratio: 0.0,
        high_freq_ratio: 0.0,
        spectral_flatness: 1.0,
        segmental_snr_db: 0.0,
        hnr_db: 0.0,
        crackle: CrackleMetrics {
            crackle_count: 0,
            crackles_per_sec: 0.0,
            boundary_crackle_count: 0,
            worst_crackle_ms: None,
            worst_crackle_magnitude: 0.0,
        },
    }
}

fn basic_amplitude_stats(samples: &[f32]) -> (f64, f64, f64, usize) {
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut peak = 0.0_f64;
    let mut clipped = 0usize;
    for &s in samples {
        let v = f64::from(s);
        sum += v;
        sum_sq += v * v;
        let a = v.abs();
        if a > peak {
            peak = a;
        }
        if a >= 0.9999 {
            clipped += 1;
        }
    }
    let n = samples.len().max(1) as f64;
    ((sum_sq / n).sqrt(), peak, sum / n, clipped)
}

pub(crate) fn short_time_rms(
    samples: &[f32],
    sample_rate: u32,
    win_ms: usize,
    hop_ms: usize,
) -> Vec<f64> {
    let win = (usize::try_from(sample_rate).unwrap_or(16_000) * win_ms / 1000).max(16);
    let hop = (usize::try_from(sample_rate).unwrap_or(16_000) * hop_ms / 1000).max(8);
    let mut out = Vec::new();
    let mut start = 0usize;
    while start + win <= samples.len() {
        if let Some(slice) = samples.get(start..start + win) {
            let sum_sq: f64 = slice.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
            out.push((sum_sq / (win as f64)).sqrt());
        }
        start += hop;
    }
    out
}

fn activity_and_snr(frame_rms: &[f64], hop_ms: f64) -> (f64, f64, f64) {
    if frame_rms.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut sorted = frame_rms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let p10 = percentile(&sorted, 0.10).max(1e-7);
    let p90 = percentile(&sorted, 0.90).max(1e-7);
    let snr_db = (20.0 * (p90 / p10).log10()).clamp(0.0, 80.0);

    let active_thresh = (p90 * 0.12).max(0.008);
    let active_count = frame_rms.iter().filter(|&&r| r >= active_thresh).count();
    let active_ratio = (active_count as f64) / (frame_rms.len() as f64);
    let first_active = frame_rms
        .iter()
        .position(|&r| r >= active_thresh)
        .unwrap_or(frame_rms.len());
    let leading_silence_ms = (first_active as f64) * hop_ms;
    (active_ratio, leading_silence_ms, snr_db)
}

pub(crate) fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "index bounded by sorted.len()"
    )]
    let idx = (((sorted.len() - 1) as f64) * p.clamp(0.0, 1.0)).round() as usize;
    sorted.get(idx).copied().unwrap_or(0.0)
}

fn spectral_stats(samples: &[f32], sample_rate: u32) -> (f64, f64, f64) {
    let fft_len = 1024usize;
    let hop = 512usize;
    if samples.len() < fft_len {
        return (0.0, 0.0, 1.0);
    }
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    let hann: Vec<f64> = (0..fft_len)
        .map(|i| 0.5 * (1.0 - (std::f64::consts::TAU * (i as f64) / (fft_len as f64)).cos()))
        .collect();

    let nyquist_bins = fft_len / 2 + 1;
    let hz_per_bin = f64::from(sample_rate) / (fft_len as f64);

    let mut total_energy = 0.0_f64;
    let mut speech_energy = 0.0_f64;
    let mut hf_energy = 0.0_f64;
    let mut flatness_sum = 0.0_f64;
    let mut active_frames = 0usize;

    let mut buf = vec![Complex::new(0.0_f64, 0.0_f64); fft_len];
    let mut start = 0usize;
    while start + fft_len <= samples.len() {
        let Some(slice) = samples.get(start..start + fft_len) else {
            break;
        };
        let frame_sq: f64 = slice.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        if (frame_sq / (fft_len as f64)).sqrt() < 0.01 {
            start += hop;
            continue;
        }
        for ((slot, &x), &w) in buf.iter_mut().zip(slice.iter()).zip(hann.iter()) {
            *slot = Complex::new(f64::from(x) * w, 0.0);
        }
        fft.process(&mut buf);

        let mut log_sum = 0.0_f64;
        let mut arith_sum = 0.0_f64;
        for (bin, c) in buf.iter().take(nyquist_bins).enumerate().skip(1) {
            let pwr = (c.re * c.re + c.im * c.im).max(1e-18);
            let freq = (bin as f64) * hz_per_bin;
            total_energy += pwr;
            if (80.0..=4000.0).contains(&freq) {
                speech_energy += pwr;
            }
            if freq >= 6000.0 {
                hf_energy += pwr;
            }
            log_sum += pwr.ln();
            arith_sum += pwr;
        }
        let bins = (nyquist_bins.saturating_sub(1).max(1)) as f64;
        let geom = (log_sum / bins).exp();
        let arith = (arith_sum / bins).max(1e-18);
        flatness_sum += (geom / arith).clamp(0.0, 1.0);
        active_frames += 1;
        start += hop;
    }

    if total_energy <= 1e-18 || active_frames == 0 {
        return (0.0, 0.0, 1.0);
    }
    (
        speech_energy / total_energy,
        hf_energy / total_energy,
        flatness_sum / (active_frames as f64),
    )
}

/// Mean Harmonics-to-Noise Ratio (`HNR`) in dB over voiced 30 ms frames via normalized autocorrelation.
fn compute_mean_hnr_db(samples: &[f32], sample_rate: u32) -> f64 {
    let sr = usize::try_from(sample_rate).unwrap_or(16_000);
    let win = (sr * 30 / 1000).max(64);
    let hop = (sr * 15 / 1000).max(32);
    let min_lag = (sr / 500).max(2); // 500 Hz
    let max_lag = (sr / 65).min(win / 2); // 65 Hz
    if max_lag <= min_lag || samples.len() < win {
        return 0.0;
    }

    let mut hnr_sum = 0.0_f64;
    let mut count = 0usize;
    let mut start = 0usize;
    while start + win <= samples.len() {
        let Some(frame) = samples.get(start..start + win) else {
            break;
        };
        let energy: f64 = frame.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        if (energy / (win as f64)).sqrt() < 0.015 {
            start += hop;
            continue;
        }
        let mut best_r = 0.0_f64;
        for lag in min_lag..=max_lag {
            let mut num = 0.0_f64;
            let mut den_a = 0.0_f64;
            let mut den_b = 0.0_f64;
            for i in 0..(win - lag) {
                let a = f64::from(frame.get(i).copied().unwrap_or(0.0));
                let b = f64::from(frame.get(i + lag).copied().unwrap_or(0.0));
                num += a * b;
                den_a += a * a;
                den_b += b * b;
            }
            let r = num / (den_a * den_b).sqrt().max(1e-12);
            if r > best_r {
                best_r = r;
            }
        }
        if best_r > 0.35 {
            let clamped = best_r.clamp(0.01, 0.999);
            hnr_sum += 10.0 * (clamped / (1.0 - clamped)).log10();
            count += 1;
        }
        start += hop;
    }
    if count == 0 {
        0.0
    } else {
        hnr_sum / (count as f64)
    }
}

/// Detect sharp digital crackles, clicks, and pops using a 2nd-derivative impulse
/// detector (`d2[n] = x[n+1] - 2*x[n] + x[n-1]`) normalized against local 5 ms MAD,
/// plus chunk-boundary attribution (80 ms Mimi / 50 ms pacer seams).
pub fn detect_crackles(samples: &[f32], sample_rate: u32) -> CrackleMetrics {
    let n = samples.len();
    let sr = f64::from(sample_rate.max(1));
    if n < 8 {
        return CrackleMetrics {
            crackle_count: 0,
            crackles_per_sec: 0.0,
            boundary_crackle_count: 0,
            worst_crackle_ms: None,
            worst_crackle_magnitude: 0.0,
        };
    }

    let mut d2 = Vec::with_capacity(n - 2);
    for i in 1..(n - 1) {
        let prev = f64::from(samples.get(i - 1).copied().unwrap_or(0.0));
        let cur = f64::from(samples.get(i).copied().unwrap_or(0.0));
        let next = f64::from(samples.get(i + 1).copied().unwrap_or(0.0));
        d2.push((next - 2.0 * cur + prev).abs());
    }

    let block = (usize::try_from(sample_rate).unwrap_or(24_000) * 5 / 1000).max(16);
    let debounce = (usize::try_from(sample_rate).unwrap_or(24_000) * 2 / 1000).max(8);
    let mut crackle_count = 0usize;
    let mut boundary_crackle_count = 0usize;
    let mut worst_mag = 0.0_f64;
    let mut worst_ms = None;
    let mut last_event: Option<usize> = None;

    let mut pos = 0usize;
    while pos < d2.len() {
        let end = (pos + block).min(d2.len());
        let Some(window) = d2.get(pos..end) else {
            break;
        };
        let mut sorted = window.to_vec();
        sorted.sort_by(f64::total_cmp);
        let median = percentile(&sorted, 0.50);
        let thresh = (median * 10.0).max(0.22);

        for (off, &mag) in window.iter().enumerate() {
            if mag > thresh {
                let sample_idx = pos + off + 1;
                let debounced =
                    last_event.is_some_and(|prev| sample_idx.saturating_sub(prev) < debounce);
                if !debounced {
                    crackle_count += 1;
                    last_event = Some(sample_idx);
                    let ms = (sample_idx as f64) * 1000.0 / sr;
                    if is_near_chunk_boundary(ms) {
                        boundary_crackle_count += 1;
                    }
                    if mag > worst_mag {
                        worst_mag = mag;
                        worst_ms = Some(ms);
                    }
                }
            }
        }
        pos = end;
    }

    let dur_s = (n as f64) / sr;
    CrackleMetrics {
        crackle_count,
        crackles_per_sec: if dur_s > 0.0 {
            (crackle_count as f64) / dur_s
        } else {
            0.0
        },
        boundary_crackle_count,
        worst_crackle_ms: worst_ms,
        worst_crackle_magnitude: worst_mag,
    }
}

fn is_near_chunk_boundary(ms: f64) -> bool {
    for period in [80.0_f64, 50.0_f64] {
        let rem = ms % period;
        if ms > 10.0 && (rem <= 1.5 || (period - rem) <= 1.5) {
            return true;
        }
    }
    false
}
