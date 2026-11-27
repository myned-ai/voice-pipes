//! Voice intonation (`YIN` `F0` pitch tracking, semitone range, jitter) and
//! dynamics (dynamic range, crest factor, shimmer, syllable modulation rate).

use serde::{Deserialize, Serialize};

use crate::audio_verify::acoustic::{percentile, short_time_rms};

/// Voice intonation metrics derived from `YIN` fundamental frequency (`F0`) tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntonationMetrics {
    /// Fraction of frames with confident voiced pitch (`60..500 Hz`).
    pub voiced_frame_ratio: f64,
    /// Median fundamental frequency (`F0`) in Hz (speaker pitch register).
    pub median_f0_hz: Option<f64>,
    /// Pitch standard deviation in semitones (expressiveness vs. robotic monotone).
    pub f0_std_semitones: f64,
    /// 95th − 5th percentile pitch span in semitones.
    pub f0_range_semitones: f64,
    /// Mean relative period-to-period pitch jitter (%).
    pub jitter_pct: f64,
    /// Pitch contour in Hz at 10 ms hop (`None` = unvoiced).
    #[serde(skip_serializing)]
    pub f0_contour_hz: Vec<Option<f64>>,
}

/// Vocal loudness and cadence dynamics metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicsMetrics {
    /// Voiced dynamic range (`P95 - P10` in dB).
    pub dynamic_range_db: f64,
    /// Peak-to-RMS crest factor in dB.
    pub crest_factor_db: f64,
    /// Relative frame-to-frame amplitude shimmer (%) on voiced segments.
    pub shimmer_pct: f64,
    /// Estimated syllable / energy-peak rate in Hz (natural speech ≈ `3..6 Hz`).
    pub syllable_rate_hz: f64,
    /// Short-time RMS contour in dB at 10 ms hop (used for golden envelope correlation).
    #[serde(skip_serializing)]
    pub rms_contour_db: Vec<f64>,
}

/// Extract voice intonation (`YIN` `F0`) and dynamics metrics from mono `f32` audio.
pub fn analyze_prosody(samples: &[f32], sample_rate: u32) -> (IntonationMetrics, DynamicsMetrics) {
    let f0_contour_hz = yin_pitch_track(samples, sample_rate);
    let intonation = summarize_intonation(f0_contour_hz);
    let dynamics = analyze_dynamics(samples, sample_rate);
    (intonation, dynamics)
}

/// Pure-Rust `YIN` pitch estimator (CMNDF with threshold `0.15`, parabolic interpolation)
/// across `60..500 Hz` at 10 ms hop.
fn yin_pitch_track(samples: &[f32], sample_rate: u32) -> Vec<Option<f64>> {
    let sr = usize::try_from(sample_rate).unwrap_or(16_000);
    let hop = (sr * 10 / 1000).max(16);
    let tau_min = (sr / 500).max(2);
    let tau_max = (sr / 60).max(tau_min + 2);
    let win = tau_max * 2;
    if samples.len() < win {
        return Vec::new();
    }

    let mut contour = Vec::new();
    let mut cmndf = vec![0.0_f64; tau_max + 1];
    let mut start = 0usize;
    while start + win <= samples.len() {
        let Some(frame) = samples.get(start..start + win) else {
            break;
        };
        let rms = (frame
            .iter()
            .map(|&x| f64::from(x) * f64::from(x))
            .sum::<f64>()
            / (win as f64))
            .sqrt();
        if rms < 0.015 {
            contour.push(None);
            start += hop;
            continue;
        }
        let pitch = yin_frame_f0(frame, tau_min, tau_max, sample_rate, &mut cmndf);
        contour.push(pitch);
        start += hop;
    }
    contour
}

fn yin_frame_f0(
    frame: &[f32],
    tau_min: usize,
    tau_max: usize,
    sample_rate: u32,
    cmndf: &mut [f64],
) -> Option<f64> {
    let half = frame.len() / 2;
    let max_tau = tau_max.min(half);
    if let Some(first) = cmndf.first_mut() {
        *first = 1.0;
    }
    let mut running_sum = 0.0_f64;
    for tau in 1..=max_tau {
        let mut diff = 0.0_f64;
        for i in 0..half {
            let a = f64::from(frame.get(i).copied().unwrap_or(0.0));
            let b = f64::from(frame.get(i + tau).copied().unwrap_or(0.0));
            let d = a - b;
            diff += d * d;
        }
        running_sum += diff;
        let val = if running_sum > 1e-12 {
            diff * (tau as f64) / running_sum
        } else {
            1.0
        };
        if let Some(slot) = cmndf.get_mut(tau) {
            *slot = val;
        }
    }

    // Find first local minimum below 0.15 threshold.
    let mut tau = tau_min;
    let mut found = None;
    while tau < max_tau {
        let cur = cmndf.get(tau).copied().unwrap_or(1.0);
        if cur < 0.15 {
            while tau + 1 < max_tau && cmndf.get(tau + 1).copied().unwrap_or(1.0) < cur {
                tau += 1;
            }
            found = Some(tau);
            break;
        }
        tau += 1;
    }
    let best_tau = found?;
    let refined = if best_tau > 1 && best_tau < max_tau {
        let s0 = cmndf.get(best_tau - 1).copied().unwrap_or(1.0);
        let s1 = cmndf.get(best_tau).copied().unwrap_or(1.0);
        let s2 = cmndf.get(best_tau + 1).copied().unwrap_or(1.0);
        let denom = 2.0 * (2.0 * s1 - s2 - s0);
        if denom.abs() > 1e-9 {
            (best_tau as f64) + (s2 - s0) / denom
        } else {
            best_tau as f64
        }
    } else {
        best_tau as f64
    };
    if refined <= 0.0 {
        return None;
    }
    let hz = f64::from(sample_rate) / refined;
    (60.0..=500.0).contains(&hz).then_some(hz)
}

fn summarize_intonation(f0_contour_hz: Vec<Option<f64>>) -> IntonationMetrics {
    let total = f0_contour_hz.len().max(1);
    let voiced: Vec<f64> = f0_contour_hz.iter().filter_map(|&v| v).collect();
    let voiced_frame_ratio = (voiced.len() as f64) / (total as f64);
    if voiced.is_empty() {
        return IntonationMetrics {
            voiced_frame_ratio,
            median_f0_hz: None,
            f0_std_semitones: 0.0,
            f0_range_semitones: 0.0,
            jitter_pct: 0.0,
            f0_contour_hz,
        };
    }

    let mut sorted = voiced.clone();
    sorted.sort_by(f64::total_cmp);
    let median_hz = percentile(&sorted, 0.50);
    let semis: Vec<f64> = voiced
        .iter()
        .map(|&hz| 12.0 * (hz / median_hz.max(1.0)).log2())
        .collect();
    let mean_st = semis.iter().sum::<f64>() / (semis.len() as f64);
    let var_st = semis
        .iter()
        .map(|&s| (s - mean_st) * (s - mean_st))
        .sum::<f64>()
        / (semis.len() as f64);

    let mut sorted_st = semis;
    sorted_st.sort_by(f64::total_cmp);
    let range_st = percentile(&sorted_st, 0.95) - percentile(&sorted_st, 0.05);

    // Consecutive-voiced jitter (%).
    let mut jitter_sum = 0.0_f64;
    let mut jitter_n = 0usize;
    for pair in f0_contour_hz.windows(2) {
        if let (Some(a), Some(b)) = (
            pair.first().copied().flatten(),
            pair.get(1).copied().flatten(),
        ) {
            let pa = 1.0 / a;
            let pb = 1.0 / b;
            jitter_sum += (pa - pb).abs() / f64::midpoint(pa, pb);
            jitter_n += 1;
        }
    }
    let jitter_pct = if jitter_n > 0 {
        100.0 * jitter_sum / (jitter_n as f64)
    } else {
        0.0
    };

    IntonationMetrics {
        voiced_frame_ratio,
        median_f0_hz: Some(median_hz),
        f0_std_semitones: var_st.sqrt(),
        f0_range_semitones: range_st.max(0.0),
        jitter_pct,
        f0_contour_hz,
    }
}

fn analyze_dynamics(samples: &[f32], sample_rate: u32) -> DynamicsMetrics {
    let rms_frames = short_time_rms(samples, sample_rate, 25, 10);
    let rms_contour_db: Vec<f64> = rms_frames
        .iter()
        .map(|&r| 20.0 * r.max(1e-6).log10())
        .collect();

    let total_rms = (samples
        .iter()
        .map(|&x| f64::from(x) * f64::from(x))
        .sum::<f64>()
        / (samples.len().max(1) as f64))
        .sqrt()
        .max(1e-9);
    let peak = samples
        .iter()
        .map(|&x| f64::from(x).abs())
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let crest_factor_db = 20.0 * (peak / total_rms).log10();

    let active_db: Vec<f64> = rms_contour_db
        .iter()
        .copied()
        .filter(|&db| db > -40.0)
        .collect();
    let dynamic_range_db = if active_db.len() >= 4 {
        let mut s = active_db;
        s.sort_by(f64::total_cmp);
        (percentile(&s, 0.95) - percentile(&s, 0.10)).max(0.0)
    } else {
        0.0
    };

    // Frame-to-frame shimmer (%) on active frames.
    let mut shim_sum = 0.0_f64;
    let mut shim_n = 0usize;
    for pair in rms_frames.windows(2) {
        let a = pair.first().copied().unwrap_or(0.0);
        let b = pair.get(1).copied().unwrap_or(0.0);
        if a > 0.02 && b > 0.02 {
            shim_sum += (a - b).abs() / f64::midpoint(a, b);
            shim_n += 1;
        }
    }
    let shimmer_pct = if shim_n > 0 {
        100.0 * shim_sum / (shim_n as f64)
    } else {
        0.0
    };

    // Syllable rate: count local RMS peaks separated by >= 100 ms (max 10 Hz).
    let syllable_rate_hz = estimate_syllable_rate(&rms_frames, 10.0);

    DynamicsMetrics {
        dynamic_range_db,
        crest_factor_db,
        shimmer_pct,
        syllable_rate_hz,
        rms_contour_db,
    }
}

fn estimate_syllable_rate(rms_frames: &[f64], hop_ms: f64) -> f64 {
    if rms_frames.len() < 5 {
        return 0.0;
    }
    let mut sorted = rms_frames.to_vec();
    sorted.sort_by(f64::total_cmp);
    let peak_thresh = (percentile(&sorted, 0.85) * 0.35).max(0.02);
    let min_gap = (100.0 / hop_ms).round() as usize;

    let mut peaks = 0usize;
    let mut last_peak: Option<usize> = None;
    for i in 1..(rms_frames.len() - 1) {
        let prev = rms_frames.get(i - 1).copied().unwrap_or(0.0);
        let cur = rms_frames.get(i).copied().unwrap_or(0.0);
        let next = rms_frames.get(i + 1).copied().unwrap_or(0.0);
        if cur >= peak_thresh
            && cur > prev
            && cur >= next
            && last_peak.is_none_or(|lp| i.saturating_sub(lp) >= min_gap)
        {
            peaks += 1;
            last_peak = Some(i);
        }
    }
    let dur_s = (rms_frames.len() as f64) * hop_ms / 1000.0;
    if dur_s > 0.0 {
        (peaks as f64) / dur_s
    } else {
        0.0
    }
}
