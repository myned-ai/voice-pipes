//! Layer 3: Golden-reference comparison via 80-bin Log-Mel Filterbank,
//! Sakoe-Chiba banded `DTW` alignment, `MCD` (dB), `LTAS` timbre cosine similarity,
//! `F0` pitch RMSE (semitones), and RMS envelope correlation.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use serde::{Deserialize, Serialize};

use crate::audio_verify::prosody::{DynamicsMetrics, IntonationMetrics};

const N_MELS: usize = 80;
const FFT_SIZE: usize = 512;

/// Quantitative comparison between a candidate recording and a pre-approved golden recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenComparison {
    /// `DTW`-aligned Log-Mel cosine similarity (`0..1`, higher is better; `>= 0.85` strong match).
    pub mel_cosine_similarity: f64,
    /// `DTW`-aligned Mel-Cepstral Distortion (`MCD`) in dB (`< 5` near-identical, `5..8` close).
    pub mcd_db: f64,
    /// Long-Term Average Spectrum (`LTAS`) cosine similarity (speaker timbre match).
    pub ltas_cosine_similarity: f64,
    /// Duration ratio `candidate_s / golden_s`.
    pub duration_ratio: f64,
    /// `DTW`-aligned `F0` pitch RMSE in semitones (when both have voiced frames).
    pub f0_rmse_semitones: Option<f64>,
    /// `DTW`-aligned RMS envelope Pearson correlation (`-1..1`).
    pub rms_envelope_correlation: f64,
}

/// Compare candidate mono audio against a golden reference recording.
pub fn compare_to_golden(
    cand_samples: &[f32],
    cand_sr: u32,
    cand_intonation: &IntonationMetrics,
    cand_dynamics: &DynamicsMetrics,
    gold_samples: &[f32],
    gold_sr: u32,
    gold_intonation: &IntonationMetrics,
    gold_dynamics: &DynamicsMetrics,
) -> GoldenComparison {
    let cand_dur = (cand_samples.len() as f64) / f64::from(cand_sr.max(1));
    let gold_dur = (gold_samples.len() as f64) / f64::from(gold_sr.max(1));
    let duration_ratio = if gold_dur > 0.0 {
        cand_dur / gold_dur
    } else {
        0.0
    };

    let cand_mels = extract_log_mel(cand_samples, cand_sr);
    let gold_mels = extract_log_mel(gold_samples, gold_sr);
    if cand_mels.is_empty() || gold_mels.is_empty() {
        return GoldenComparison {
            mel_cosine_similarity: 0.0,
            mcd_db: 99.0,
            ltas_cosine_similarity: 0.0,
            duration_ratio,
            f0_rmse_semitones: None,
            rms_envelope_correlation: 0.0,
        };
    }

    let ltas_cosine_similarity = cosine_sim(&mean_frame(&cand_mels), &mean_frame(&gold_mels));
    let path = banded_dtw_path(&cand_mels, &gold_mels);
    let (mel_cosine_similarity, mcd_db) = score_aligned_mels(&cand_mels, &gold_mels, &path);
    let f0_rmse_semitones = aligned_f0_rmse(
        &cand_intonation.f0_contour_hz,
        &gold_intonation.f0_contour_hz,
        &path,
    );
    let rms_envelope_correlation = aligned_rms_corr(
        &cand_dynamics.rms_contour_db,
        &gold_dynamics.rms_contour_db,
        &path,
    );

    GoldenComparison {
        mel_cosine_similarity,
        mcd_db,
        ltas_cosine_similarity,
        duration_ratio,
        f0_rmse_semitones,
        rms_envelope_correlation,
    }
}

fn extract_log_mel(samples: &[f32], sample_rate: u32) -> Vec<[f64; N_MELS]> {
    let sr = usize::try_from(sample_rate).unwrap_or(16_000);
    let win = (sr * 25 / 1000).clamp(64, FFT_SIZE);
    let hop = (sr * 10 / 1000).max(16);
    if samples.len() < win {
        return Vec::new();
    }

    let filters = mel_filterbank(sample_rate, FFT_SIZE, N_MELS);
    let hann: Vec<f64> = (0..win)
        .map(|i| 0.5 * (1.0 - (std::f64::consts::TAU * (i as f64) / (win as f64)).cos()))
        .collect();
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut buf = vec![Complex::new(0.0_f64, 0.0_f64); FFT_SIZE];
    let mut frames = Vec::new();

    let mut start = 0usize;
    while start + win <= samples.len() {
        let Some(slice) = samples.get(start..start + win) else {
            break;
        };
        for slot in &mut buf {
            *slot = Complex::new(0.0, 0.0);
        }
        for ((slot, &x), &w) in buf.iter_mut().zip(slice.iter()).zip(hann.iter()) {
            *slot = Complex::new(f64::from(x) * w, 0.0);
        }
        fft.process(&mut buf);

        let mut mel = [0.0_f64; N_MELS];
        for (m, filt) in filters.iter().enumerate() {
            let mut energy = 0.0_f64;
            for &(bin, weight) in filt {
                if let Some(c) = buf.get(bin) {
                    energy += (c.re * c.re + c.im * c.im) * weight;
                }
            }
            if let Some(dst) = mel.get_mut(m) {
                *dst = energy.max(1e-10).ln();
            }
        }
        frames.push(mel);
        start += hop;
    }
    frames
}

fn hz_to_mel(hz: f64) -> f64 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f64) -> f64 {
    700.0 * (10.0_f64.powf(mel / 2595.0) - 1.0)
}

fn mel_filterbank(sample_rate: u32, fft_size: usize, n_mels: usize) -> Vec<Vec<(usize, f64)>> {
    let f_min = 80.0_f64;
    let f_max = (f64::from(sample_rate) * 0.5).min(8000.0);
    let m_min = hz_to_mel(f_min);
    let m_max = hz_to_mel(f_max);

    let mut bin_points = Vec::with_capacity(n_mels + 2);
    for i in 0..(n_mels + 2) {
        let mel = m_min + (m_max - m_min) * (i as f64) / ((n_mels + 1) as f64);
        let hz = mel_to_hz(mel);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "FFT bin bounded by fft_size/2"
        )]
        let bin = (((fft_size + 1) as f64) * hz / f64::from(sample_rate))
            .floor()
            .clamp(0.0, (fft_size / 2) as f64) as usize;
        bin_points.push(bin);
    }

    let mut filters = Vec::with_capacity(n_mels);
    for m in 0..n_mels {
        let left = bin_points.get(m).copied().unwrap_or(0);
        let center = bin_points.get(m + 1).copied().unwrap_or(left + 1);
        let right = bin_points.get(m + 2).copied().unwrap_or(center + 1);
        let mut weights = Vec::new();
        for b in left..=right {
            let w = if b < center && center > left {
                ((b - left) as f64) / ((center - left) as f64)
            } else if b >= center && right > center {
                ((right - b) as f64) / ((right - center) as f64)
            } else if b == center {
                1.0
            } else {
                0.0
            };
            if w > 0.0 {
                weights.push((b, w));
            }
        }
        filters.push(weights);
    }
    filters
}

fn banded_dtw_path(cand: &[[f64; N_MELS]], gold: &[[f64; N_MELS]]) -> Vec<(usize, usize)> {
    let n_cand = cand.len();
    let n_gold = gold.len();
    let band = ((n_cand.max(n_gold) / 4).max(n_cand.abs_diff(n_gold) + 16)).min(n_cand.max(n_gold));
    let mut dp = vec![vec![f64::INFINITY; n_gold + 1]; n_cand + 1];
    if let Some(cell) = dp.first_mut().and_then(|r| r.first_mut()) {
        *cell = 0.0;
    }

    for row in 1..=n_cand {
        let center = (row * n_gold) / n_cand.max(1);
        let col_lo = center.saturating_sub(band).max(1);
        let col_hi = (center + band).min(n_gold);
        for col in col_lo..=col_hi {
            let cost = frame_l2(
                cand.get(row - 1).unwrap_or(&[0.0; N_MELS]),
                gold.get(col - 1).unwrap_or(&[0.0; N_MELS]),
            );
            let best_prev = dp
                .get(row - 1)
                .and_then(|r| r.get(col - 1))
                .copied()
                .unwrap_or(f64::INFINITY)
                .min(
                    dp.get(row - 1)
                        .and_then(|r| r.get(col))
                        .copied()
                        .unwrap_or(f64::INFINITY),
                )
                .min(
                    dp.get(row)
                        .and_then(|r| r.get(col - 1))
                        .copied()
                        .unwrap_or(f64::INFINITY),
                );
            if let Some(cell) = dp.get_mut(row).and_then(|r| r.get_mut(col)) {
                *cell = cost + best_prev;
            }
        }
    }

    let mut path = Vec::with_capacity(n_cand.max(n_gold));
    let (mut row, mut col) = (n_cand, n_gold);
    while row > 0 && col > 0 {
        path.push((row - 1, col - 1));
        let d_diag = dp
            .get(row - 1)
            .and_then(|r| r.get(col - 1))
            .copied()
            .unwrap_or(f64::INFINITY);
        let d_up = dp
            .get(row - 1)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(f64::INFINITY);
        let d_left = dp
            .get(row)
            .and_then(|r| r.get(col - 1))
            .copied()
            .unwrap_or(f64::INFINITY);
        if d_diag <= d_up && d_diag <= d_left {
            row -= 1;
            col -= 1;
        } else if d_up <= d_left {
            row -= 1;
        } else {
            col -= 1;
        }
    }
    path.reverse();
    path
}

fn frame_l2(a: &[f64; N_MELS], b: &[f64; N_MELS]) -> f64 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x - y) * (x - y))
        .sum::<f64>()
        .sqrt()
}

fn cosine_sim(a: &[f64; N_MELS], b: &[f64; N_MELS]) -> f64 {
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = (na * nb).sqrt();
    if denom <= 1e-12 { 0.0 } else { dot / denom }
}

fn mean_frame(frames: &[[f64; N_MELS]]) -> [f64; N_MELS] {
    let mut out = [0.0_f64; N_MELS];
    let n = frames.len().max(1) as f64;
    for f in frames {
        for (dst, &val) in out.iter_mut().zip(f.iter()) {
            *dst += val / n;
        }
    }
    out
}

fn score_aligned_mels(
    a: &[[f64; N_MELS]],
    b: &[[f64; N_MELS]],
    path: &[(usize, usize)],
) -> (f64, f64) {
    if path.is_empty() {
        return (0.0, 99.0);
    }
    let mcd_scale =
        10.0 / std::f64::consts::LN_10 * std::f64::consts::SQRT_2 / (N_MELS as f64).sqrt();
    let mut cos_sum = 0.0_f64;
    let mut mcd_sum = 0.0_f64;
    for &(i, j) in path {
        let fa = a.get(i).unwrap_or(&[0.0; N_MELS]);
        let fb = b.get(j).unwrap_or(&[0.0; N_MELS]);
        cos_sum += cosine_sim(fa, fb);
        mcd_sum += frame_l2(fa, fb) * mcd_scale;
    }
    let len = path.len() as f64;
    (cos_sum / len, mcd_sum / len)
}

fn aligned_f0_rmse(a: &[Option<f64>], b: &[Option<f64>], path: &[(usize, usize)]) -> Option<f64> {
    let mut sq_sum = 0.0_f64;
    let mut n = 0usize;
    for &(i, j) in path {
        if let (Some(fa), Some(fb)) = (a.get(i).copied().flatten(), b.get(j).copied().flatten()) {
            let st_diff = 12.0 * (fa / fb).log2();
            sq_sum += st_diff * st_diff;
            n += 1;
        }
    }
    (n >= 3).then(|| (sq_sum / (n as f64)).sqrt())
}

fn aligned_rms_corr(a: &[f64], b: &[f64], path: &[(usize, usize)]) -> f64 {
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for &(i, j) in path {
        if let (Some(&va), Some(&vb)) = (a.get(i), b.get(j)) {
            xs.push(va);
            ys.push(vb);
        }
    }
    if xs.len() < 3 {
        return 0.0;
    }
    let mx = xs.iter().sum::<f64>() / (xs.len() as f64);
    let my = ys.iter().sum::<f64>() / (ys.len() as f64);
    let mut num = 0.0_f64;
    let mut dx = 0.0_f64;
    let mut dy = 0.0_f64;
    for (&x, &y) in xs.iter().zip(ys.iter()) {
        num += (x - mx) * (y - my);
        dx += (x - mx) * (x - mx);
        dy += (y - my) * (y - my);
    }
    let denom = (dx * dy).sqrt();
    if denom <= 1e-12 { 0.0 } else { num / denom }
}
