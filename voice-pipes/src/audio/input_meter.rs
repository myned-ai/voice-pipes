//! Per-turn input audio quality: speech level, noise floor, SNR, clipping.
//!
//! The meter answers "was the user's microphone the problem, or the
//! model?" without a second pass over the audio. Callers already walk
//! every uplink chunk through a VAD; they hand the same slice and the
//! VAD's verdict to [`InputAudioMeter::feed`], which folds three
//! operations per sample into running sums. Nothing here gates, delays
//! or copies audio, and the only `log10` happens once per turn in the
//! [`InputAudioStats`] accessors.
//!
//! Levels are dBFS of the normalised signal (`20·log10(rms / 32768)`),
//! the same `0..1` scale as the `level` annotator
//! (`pipeline/annotate.rs`) — never the raw-i16 RMS that
//! [`crate::vad::segmenter::EnergyVad`] thresholds on (its default `200.0`
//! is about −44 dBFS). Values are as the server received them, i.e.
//! after any browser AGC.
//!
//! Speech frames pool into the current segment; non-speech frames feed
//! a rolling ~1 s noise window that survives [`InputAudioMeter::take`],
//! so the floor is always the most recent quiet audio (pauses between
//! words, the VAD hangover, idle time before the user spoke) and the
//! agent's own voice leaking into a speakerphone mic ages out quickly.

use std::collections::VecDeque;

use serde_json::{Map, Value};

/// i16 full scale as a float (the magnitude of `i16::MIN`).
const FULL_SCALE: f64 = 32_768.0;
/// A sample at or above this magnitude counts as clipped (≈ −0.09 dBFS).
/// Slightly under full scale because codecs and resamplers rarely leave a
/// clipped waveform at exactly ±32767.
pub const CLIP_THRESHOLD: u16 = 32_440;
/// Digital silence has no finite dBFS; report this floor instead.
pub const DBFS_FLOOR: f64 = -100.0;
/// Length of the rolling noise window, in audio time.
pub const NOISE_WINDOW_MS: u64 = 1_000;

/// Wire keys, in the order they are written. On the `realtime`
/// component's `extras` for S2S turns and the `stt` component's `extras`
/// for cascade turns — the admin reader projects both the same way.
pub const EXTRAS_KEYS: [&str; 8] = [
    "input_speech_dbfs",
    "input_peak_dbfs",
    "input_noise_dbfs",
    "input_snr_db",
    "input_clipped_ratio",
    "input_speech_ratio",
    "input_speech_ms",
    "input_level_source",
];

fn to_dbfs(linear: f64) -> f64 {
    if linear > 0.0 {
        (20.0 * linear.log10()).max(DBFS_FLOOR)
    } else {
        DBFS_FLOOR
    }
}

fn dbfs_from_sum(sum_sq: f64, samples: u64) -> f64 {
    if samples == 0 {
        return DBFS_FLOOR;
    }
    to_dbfs((sum_sq / samples as f64).sqrt() / FULL_SCALE)
}

fn round_to(v: f64, decimals: i32) -> f64 {
    let m = 10f64.powi(decimals);
    (v * m).round() / m
}

/// One user segment's measurements. Holds raw sums rather than dB so two
/// segments of the same utterance (the user paused past the VAD
/// hangover) can be merged exactly with [`Self::merge_earlier`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputAudioStats {
    /// Which classifier decided speech vs not (`"vp_energy"`, …).
    pub source: &'static str,
    speech_sum_sq: f64,
    speech_samples: u64,
    /// Samples from the segment's first speech frame to the take,
    /// including in-segment pauses and the hangover tail.
    segment_samples: u64,
    peak: u16,
    clipped: u64,
    speech_us: u64,
    noise_sum_sq: f64,
    noise_samples: u64,
}

impl InputAudioStats {
    /// Pooled RMS of the speech frames, dBFS.
    pub fn speech_dbfs(&self) -> f64 {
        dbfs_from_sum(self.speech_sum_sq, self.speech_samples)
    }

    /// Loudest speech sample, dBFS.
    pub fn peak_dbfs(&self) -> f64 {
        to_dbfs(f64::from(self.peak) / FULL_SCALE)
    }

    /// Pooled RMS of the noise window at the take, dBFS. `None` when no
    /// non-speech frame had been seen yet.
    pub fn noise_dbfs(&self) -> Option<f64> {
        (self.noise_samples > 0).then(|| dbfs_from_sum(self.noise_sum_sq, self.noise_samples))
    }

    /// Speech level over the noise floor, dB.
    pub fn snr_db(&self) -> Option<f64> {
        self.noise_dbfs().map(|n| self.speech_dbfs() - n)
    }

    /// Clipped speech samples ÷ speech samples.
    pub fn clipped_ratio(&self) -> f64 {
        if self.speech_samples == 0 {
            0.0
        } else {
            self.clipped as f64 / self.speech_samples as f64
        }
    }

    /// Speech samples ÷ samples since the segment's first speech frame.
    /// Near 1.0 on a segment that never went quiet (constant noise the
    /// VAD calls speech); the hangover tail keeps it below 1.0 normally.
    pub fn speech_ratio(&self) -> f64 {
        if self.segment_samples == 0 {
            0.0
        } else {
            self.speech_samples as f64 / self.segment_samples as f64
        }
    }

    /// Audio time the VAD classified as speech, ms.
    pub fn speech_ms(&self) -> u64 {
        self.speech_us / 1_000
    }

    /// Fold an earlier segment of the same utterance into this one:
    /// speech sums add, the peak is the max, and the noise floor stays
    /// this (newer) segment's unless it has none.
    pub fn merge_earlier(&mut self, earlier: &Self) {
        self.speech_sum_sq += earlier.speech_sum_sq;
        self.speech_samples += earlier.speech_samples;
        self.segment_samples += earlier.segment_samples;
        self.peak = self.peak.max(earlier.peak);
        self.clipped += earlier.clipped;
        self.speech_us += earlier.speech_us;
        if self.noise_samples == 0 {
            self.noise_sum_sq = earlier.noise_sum_sq;
            self.noise_samples = earlier.noise_samples;
        }
    }

    /// Write the [`EXTRAS_KEYS`] into a turn component's `extras`.
    /// `input_noise_dbfs` / `input_snr_db` are omitted (not `null`) when
    /// there was no noise sample, matching the other optional extras.
    pub fn write_extras(&self, extras: &mut Map<String, Value>) {
        extras.insert(
            "input_speech_dbfs".into(),
            round_to(self.speech_dbfs(), 1).into(),
        );
        extras.insert(
            "input_peak_dbfs".into(),
            round_to(self.peak_dbfs(), 1).into(),
        );
        if let Some(noise) = self.noise_dbfs() {
            extras.insert("input_noise_dbfs".into(), round_to(noise, 1).into());
        }
        if let Some(snr) = self.snr_db() {
            extras.insert("input_snr_db".into(), round_to(snr, 1).into());
        }
        extras.insert(
            "input_clipped_ratio".into(),
            round_to(self.clipped_ratio(), 4).into(),
        );
        extras.insert(
            "input_speech_ratio".into(),
            round_to(self.speech_ratio(), 3).into(),
        );
        extras.insert("input_speech_ms".into(), self.speech_ms().into());
        extras.insert("input_level_source".into(), self.source.into());
    }
}

/// Running accumulator fed by a VAD caller. See the module docs.
#[derive(Debug)]
pub struct InputAudioMeter {
    source: &'static str,
    in_segment: bool,
    speech_sum_sq: f64,
    speech_samples: u64,
    segment_samples: u64,
    peak: u16,
    clipped: u64,
    speech_us: u64,
    /// `(sum_sq, samples)` per non-speech chunk, oldest first.
    noise: VecDeque<(f64, u64)>,
    noise_sum_sq: f64,
    noise_samples: u64,
}

impl InputAudioMeter {
    pub fn new(source: &'static str) -> Self {
        Self {
            source,
            in_segment: false,
            speech_sum_sq: 0.0,
            speech_samples: 0,
            segment_samples: 0,
            peak: 0,
            clipped: 0,
            speech_us: 0,
            // ~1 s of 2.7 ms browser frames without regrowing.
            noise: VecDeque::with_capacity(512),
            noise_sum_sq: 0.0,
            noise_samples: 0,
        }
    }

    /// Fold one chunk in. `is_speech` is the caller's VAD verdict for
    /// exactly this chunk. Empty chunks and a zero rate are no-ops.
    pub fn feed(&mut self, samples: &[i16], sample_rate: u32, is_speech: bool) {
        if samples.is_empty() || sample_rate == 0 {
            return;
        }
        let n = samples.len() as u64;
        if is_speech {
            self.in_segment = true;
            let mut sum_sq = 0.0;
            let mut peak = self.peak;
            let mut clipped = 0u64;
            for &s in samples {
                let v = f64::from(s);
                sum_sq += v * v;
                let a = s.unsigned_abs();
                peak = peak.max(a);
                clipped += u64::from(a >= CLIP_THRESHOLD);
            }
            self.speech_sum_sq += sum_sq;
            self.speech_samples += n;
            self.peak = peak;
            self.clipped += clipped;
            self.speech_us += n * 1_000_000 / u64::from(sample_rate);
        } else {
            let sum_sq: f64 = samples
                .iter()
                .map(|&s| {
                    let v = f64::from(s);
                    v * v
                })
                .sum();
            self.push_noise(sum_sq, n, sample_rate);
        }
        if self.in_segment {
            self.segment_samples += n;
        }
    }

    fn push_noise(&mut self, sum_sq: f64, n: u64, sample_rate: u32) {
        self.noise.push_back((sum_sq, n));
        self.noise_sum_sq += sum_sq;
        self.noise_samples += n;
        let window = u64::from(sample_rate) * NOISE_WINDOW_MS / 1_000;
        // Evict while the rest still covers the window.
        while let Some(&(old_sq, old_n)) = self.noise.front() {
            if self.noise_samples - old_n < window {
                break;
            }
            self.noise.pop_front();
            self.noise_sum_sq = (self.noise_sum_sq - old_sq).max(0.0);
            self.noise_samples -= old_n;
        }
    }

    /// The current segment's stats, then reset the segment (the noise
    /// window keeps rolling). `None` when no speech frame was fed.
    pub fn take(&mut self) -> Option<InputAudioStats> {
        let stats = (self.speech_samples > 0).then_some(InputAudioStats {
            source: self.source,
            speech_sum_sq: self.speech_sum_sq,
            speech_samples: self.speech_samples,
            segment_samples: self.segment_samples,
            peak: self.peak,
            clipped: self.clipped,
            speech_us: self.speech_us,
            noise_sum_sq: self.noise_sum_sq,
            noise_samples: self.noise_samples,
        });
        self.discard();
        stats
    }

    /// Drop the current segment (a click the caller rejected) without
    /// producing stats. The noise window is kept.
    pub fn discard(&mut self) {
        self.in_segment = false;
        self.speech_sum_sq = 0.0;
        self.speech_samples = 0;
        self.segment_samples = 0;
        self.peak = 0;
        self.clipped = 0;
        self.speech_us = 0;
    }

    /// Forget everything, noise window included.
    pub fn reset(&mut self) {
        self.discard();
        self.noise.clear();
        self.noise_sum_sq = 0.0;
        self.noise_samples = 0;
    }
}

#[cfg(test)]
#[path = "tests/input_meter.rs"]
mod tests;
