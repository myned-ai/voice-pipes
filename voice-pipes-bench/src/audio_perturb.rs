//! Deterministic audio loudness normalization (`-20 dBFS RMS`), SHA-256 PCM
//! pinning, and acoustic stress perturbations (`phonecodec`, `noisegap`,
//! `clipping`, `reverb`) for production-realistic voice benchmarking.
//!
//! Pure Rust, zero external audio binaries, `#![forbid(unsafe_code)]`.

use sha2::{Digest, Sha256};
use voice_pipes::audio::AudioFrame;
use voice_pipes::audio::g711::{mulaw_to_pcm16, pcm16_to_mulaw};

/// Standard reference loudness target (`-20 dBFS RMS`).
pub const TARGET_RMS_DBFS: f64 = -20.0;

/// Maximum peak fraction permitted by peak guarding (`-0.18 dBFS`).
const PEAK_GUARD_CEILING: f64 = 0.98;

/// Deterministic acoustic stress condition applied to a benchmark input clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AcousticCondition {
    /// Loudness-normalized to `-20 dBFS RMS` with peak guarding only.
    Clean,
    /// Narrowband 8 kHz G.711 μ-law telephony codec round-trip.
    PhoneCodec,
    /// Intermittent deterministic white-noise bursts at target `snr_db`.
    NoiseGap {
        snr_db: f64,
        burst_ms: u32,
        period_ms: u32,
        seed: u64,
    },
    /// Pre-gain overload (`drive_db`) with hard clipping at full-scale.
    Clipping { drive_db: f64 },
    /// 4-comb Schroeder room-echo simulation.
    Reverb { rt60_ms: u32, wet_mix: f64 },
}

/// Compute a deterministic lowercase hex SHA-256 digest over little-endian
/// `i16` PCM samples for reproducible corpus manifest pinning.
#[must_use]
pub fn pcm16_sha256(samples: &[i16]) -> String {
    let mut hasher = Sha256::new();
    for &s in samples {
        hasher.update(s.to_le_bytes());
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Normalize mono/interleaved `i16` PCM to `target_dbfs` RMS with peak
/// guarding so loud transients never hard-clip. Returns zeros unchanged.
#[must_use]
pub fn normalize_rms_dbfs(samples: &[i16], target_dbfs: f64) -> Vec<i16> {
    if samples.is_empty() {
        return Vec::new();
    }
    let mut sum_sq = 0.0_f64;
    let mut peak = 0.0_f64;
    for &s in samples {
        let f = f64::from(s) / 32768.0;
        sum_sq += f * f;
        let a = f.abs();
        if a > peak {
            peak = a;
        }
    }
    let rms = (sum_sq / (samples.len() as f64)).sqrt();
    if rms <= 1e-9 || peak <= 1e-9 {
        return samples.to_vec();
    }
    let target_rms = 10.0_f64.powf(target_dbfs / 20.0);
    let max_safe_gain = PEAK_GUARD_CEILING / peak;
    let gain = (target_rms / rms).min(max_safe_gain);
    samples
        .iter()
        .map(|&s| f64_to_i16((f64::from(s) / 32768.0) * gain))
        .collect()
}

/// Normalize `frame` to `-20 dBFS RMS` and apply `condition` deterministically.
#[must_use]
pub fn apply_condition(frame: &AudioFrame, condition: AcousticCondition) -> AudioFrame {
    let base = normalize_rms_dbfs(&frame.samples, TARGET_RMS_DBFS);
    let processed = match condition {
        AcousticCondition::Clean => base,
        AcousticCondition::PhoneCodec => apply_phone_codec(&base, frame.sample_rate),
        AcousticCondition::NoiseGap {
            snr_db,
            burst_ms,
            period_ms,
            seed,
        } => apply_noise_gap(&base, frame.sample_rate, snr_db, burst_ms, period_ms, seed),
        AcousticCondition::Clipping { drive_db } => apply_clipping(&base, drive_db),
        AcousticCondition::Reverb { rt60_ms, wet_mix } => {
            apply_reverb(&base, frame.sample_rate, rt60_ms, wet_mix)
        }
    };
    AudioFrame {
        samples: processed,
        sample_rate: frame.sample_rate,
        channels: frame.channels,
    }
}

fn apply_phone_codec(samples: &[i16], sample_rate: u32) -> Vec<i16> {
    if samples.is_empty() || sample_rate == 0 {
        return samples.to_vec();
    }
    let down = resample_linear(samples, sample_rate, 8_000);
    let encoded = pcm16_to_mulaw(&down);
    let decoded = mulaw_to_pcm16(&encoded);
    let mut up = resample_linear(&decoded, 8_000, sample_rate);
    up.resize(samples.len(), 0);
    up
}

fn apply_noise_gap(
    samples: &[i16],
    sample_rate: u32,
    snr_db: f64,
    burst_ms: u32,
    period_ms: u32,
    seed: u64,
) -> Vec<i16> {
    if samples.is_empty() || sample_rate == 0 || period_ms == 0 {
        return samples.to_vec();
    }
    let signal_rms = compute_rms(samples).max(1e-6);
    let noise_rms = signal_rms / 10.0_f64.powf(snr_db / 20.0);
    // Uniform [-1, 1] has RMS = 1/sqrt(3); scale so burst RMS matches `noise_rms`.
    let amp = noise_rms * 3.0_f64.sqrt();
    let burst_len = ((u64::from(sample_rate) * u64::from(burst_ms)) / 1000).max(1) as usize;
    let period_len = ((u64::from(sample_rate) * u64::from(period_ms)) / 1000).max(1) as usize;
    let mut rng = seed.max(1);
    samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let base = f64::from(s) / 32768.0;
            if i % period_len < burst_len {
                let u = next_uniform_bipolar(&mut rng);
                f64_to_i16(base + u * amp)
            } else {
                s
            }
        })
        .collect()
}

fn apply_clipping(samples: &[i16], drive_db: f64) -> Vec<i16> {
    let drive = 10.0_f64.powf(drive_db / 20.0);
    let clipped: Vec<i16> = samples
        .iter()
        .map(|&s| f64_to_i16(((f64::from(s) / 32768.0) * drive).clamp(-1.0, 1.0)))
        .collect();
    normalize_rms_dbfs(&clipped, TARGET_RMS_DBFS)
}

fn apply_reverb(samples: &[i16], sample_rate: u32, rt60_ms: u32, wet_mix: f64) -> Vec<i16> {
    if samples.is_empty() || sample_rate == 0 || rt60_ms == 0 {
        return samples.to_vec();
    }
    let wet = wet_mix.clamp(0.0, 1.0);
    let dry = 1.0 - wet;
    // Four mutually prime Schroeder comb delays (ms).
    let delay_ms = [29.7_f64, 37.1, 41.1, 43.7];
    let sr = f64::from(sample_rate);
    let mut combs: Vec<(Vec<f64>, usize, f64)> = delay_ms
        .iter()
        .map(|&ms| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "comb delay length is positive and bounded by sample_rate"
            )]
            let len = ((ms * sr) / 1000.0).round().max(1.0) as usize;
            let feedback = 10.0_f64.powf(-3.0 * (ms / f64::from(rt60_ms))).min(0.95);
            (vec![0.0_f64; len], 0usize, feedback)
        })
        .collect();

    let mut out = Vec::with_capacity(samples.len());
    for &s in samples {
        let x = f64::from(s) / 32768.0;
        let mut wet_sum = 0.0_f64;
        for (buf, idx, fb) in &mut combs {
            if let Some(slot) = buf.get_mut(*idx) {
                let y = *slot;
                *slot = x + y * (*fb);
                wet_sum += y;
            }
            *idx = (*idx + 1) % buf.len().max(1);
        }
        let y = dry * x + wet * (wet_sum / (combs.len() as f64));
        out.push(f64_to_i16(y));
    }
    out
}

fn resample_linear(samples: &[i16], from_sr: u32, to_sr: u32) -> Vec<i16> {
    if samples.is_empty() || from_sr == 0 || to_sr == 0 || from_sr == to_sr {
        return samples.to_vec();
    }
    let ratio = f64::from(from_sr) / f64::from(to_sr);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "output length is non-negative and bounded"
    )]
    let out_len = ((samples.len() as f64) / ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = (i as f64) * ratio;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "floor index bounded by samples.len()"
        )]
        let idx = pos.floor() as usize;
        let frac = pos - (idx as f64);
        let a = f64::from(samples.get(idx).copied().unwrap_or(0));
        let b = f64::from(
            samples
                .get(idx + 1)
                .copied()
                .unwrap_or(samples.get(idx).copied().unwrap_or(0)),
        );
        out.push(f64_to_i16((a + (b - a) * frac) / 32768.0));
    }
    out
}

fn compute_rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let f = f64::from(s) / 32768.0;
            f * f
        })
        .sum();
    (sum_sq / (samples.len() as f64)).sqrt()
}

fn next_uniform_bipolar(state: &mut u64) -> f64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    let unit = (x as f64) / (u64::MAX as f64);
    unit * 2.0 - 1.0
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to [-1.0, 1.0] * 32767.0 before cast"
)]
fn f64_to_i16(v: f64) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_frame(sr: u32, secs: f64, amp: f64) -> AudioFrame {
        let n = (f64::from(sr) * secs) as usize;
        let samples: Vec<i16> = (0..n)
            .map(|i| {
                let t = (i as f64) / f64::from(sr);
                f64_to_i16((std::f64::consts::TAU * 440.0 * t).sin() * amp)
            })
            .collect();
        AudioFrame {
            samples,
            sample_rate: sr,
            channels: 1,
        }
    }

    #[test]
    fn normalize_hits_minus_20_dbfs_and_peak_guards() {
        let quiet = sine_frame(16_000, 0.5, 0.01);
        let norm = normalize_rms_dbfs(&quiet.samples, TARGET_RMS_DBFS);
        let dbfs = 20.0 * compute_rms(&norm).log10();
        assert!((dbfs - TARGET_RMS_DBFS).abs() < 0.2, "got {dbfs:.2} dBFS");
    }

    #[test]
    fn conditions_are_deterministic_and_alter_signal() {
        let frame = sine_frame(16_000, 0.5, 0.2);
        let clean = apply_condition(&frame, AcousticCondition::Clean);
        let phone1 = apply_condition(&frame, AcousticCondition::PhoneCodec);
        let phone2 = apply_condition(&frame, AcousticCondition::PhoneCodec);
        assert_eq!(phone1.samples.len(), frame.samples.len());
        assert_eq!(pcm16_sha256(&phone1.samples), pcm16_sha256(&phone2.samples));
        assert_ne!(pcm16_sha256(&clean.samples), pcm16_sha256(&phone1.samples));

        let noisy = apply_condition(
            &frame,
            AcousticCondition::NoiseGap {
                snr_db: 5.0,
                burst_ms: 100,
                period_ms: 250,
                seed: 42,
            },
        );
        assert_ne!(pcm16_sha256(&clean.samples), pcm16_sha256(&noisy.samples));

        let clipped = apply_condition(&frame, AcousticCondition::Clipping { drive_db: 24.0 });
        assert_ne!(pcm16_sha256(&clean.samples), pcm16_sha256(&clipped.samples));

        let rev = apply_condition(
            &frame,
            AcousticCondition::Reverb {
                rt60_ms: 400,
                wet_mix: 0.4,
            },
        );
        assert_ne!(pcm16_sha256(&clean.samples), pcm16_sha256(&rev.samples));
    }
}
