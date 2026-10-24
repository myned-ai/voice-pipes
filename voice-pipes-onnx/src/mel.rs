//! The pieces of a log-mel front end that every local model shares: a Hann
//! window, a Slaney mel filterbank, and power spectra over a cached FFT
//! plan.
//!
//! Each model keeps the steps its reference implementation defines its own
//! way, which differ between them: pre-emphasis, how the signal is padded,
//! the log's base and floor, and any normalisation. What is here is where
//! they agree, computed once per process instead of on every call.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// Which Hann window: the two conventions the references use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Hann {
    /// `w[i] = 0.5 - 0.5 cos(2πi / (n - 1))`: torch `periodic=False`,
    /// numpy `hanning(n)`.
    Symmetric,
    /// `w[i] = 0.5 - 0.5 cos(2πi / n)`: torch `hann_window(n)`, numpy
    /// `hanning(n + 1)[:-1]`.
    Periodic,
}

/// A Hann window of `len` samples, computed in f64 and stored as f32, as
/// the references do.
pub fn hann(len: usize, kind: Hann) -> Vec<f32> {
    let denom = match kind {
        Hann::Symmetric if len <= 1 => return vec![1.0; len],
        Hann::Symmetric => (len - 1) as f64,
        Hann::Periodic => len as f64,
    };
    (0..len)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / denom).cos()) as f32)
        .collect()
}

/// `window` centred in an `n_fft`-sample frame, zeros either side: a window
/// shorter than the FFT, as `torch.stft` places it.
///
/// # Panics
/// When the window is longer than the frame.
pub fn centred(window: &[f32], n_fft: usize) -> Vec<f32> {
    assert!(window.len() <= n_fft, "a window longer than its frame");
    let offset = (n_fft - window.len()) / 2;
    let mut framed = vec![0.0f32; n_fft];
    framed[offset..offset + window.len()].copy_from_slice(window);
    framed
}

/// Slaney (non-HTK) mel scale: linear to 1 kHz, logarithmic above.
fn hz_to_mel(hz: f64) -> f64 {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f64.ln() / 27.0;
    if hz >= MIN_LOG_HZ {
        min_log_mel + (hz / MIN_LOG_HZ).ln() / logstep
    } else {
        hz / F_SP
    }
}

fn mel_to_hz(mel: f64) -> f64 {
    const F_SP: f64 = 200.0 / 3.0;
    const MIN_LOG_HZ: f64 = 1000.0;
    let min_log_mel = MIN_LOG_HZ / F_SP;
    let logstep = 6.4f64.ln() / 27.0;
    if mel >= min_log_mel {
        MIN_LOG_HZ * (logstep * (mel - min_log_mel)).exp()
    } else {
        F_SP * mel
    }
}

/// The mel filterbank as `librosa.filters.mel(norm="slaney")` builds it,
/// one row per band over the `n_fft / 2 + 1` FFT bins: triangular, and
/// area-normalised by the band's width in hertz. Computed in f64 and
/// stored as f32.
pub fn slaney_filterbank(
    sample_rate: usize,
    n_fft: usize,
    n_mels: usize,
    fmin: f64,
    fmax: f64,
) -> Vec<Vec<f32>> {
    let n_bins = n_fft / 2 + 1;
    let fft_freqs: Vec<f64> = (0..n_bins)
        .map(|k| k as f64 * sample_rate as f64 / n_fft as f64)
        .collect();
    let (lo, hi) = (hz_to_mel(fmin), hz_to_mel(fmax));
    let edges: Vec<f64> = (0..n_mels + 2)
        .map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (n_mels + 1) as f64))
        .collect();
    (0..n_mels)
        .map(|m| {
            let (left, centre, right) = (edges[m], edges[m + 1], edges[m + 2]);
            let enorm = 2.0 / (right - left);
            fft_freqs
                .iter()
                .map(|&f| {
                    let lower = (f - left) / (centre - left);
                    let upper = (right - f) / (right - centre);
                    (lower.min(upper).max(0.0) * enorm) as f32
                })
                .collect()
        })
        .collect()
}

/// Each band's energy in one frame's power spectrum: the dot product of the
/// band's row with the spectrum, summed in bin order.
pub fn mel_energies<'a>(
    filterbank: &'a [Vec<f32>],
    power: &'a [f32],
) -> impl Iterator<Item = f32> + 'a {
    filterbank
        .iter()
        .map(move |row| row.iter().zip(power).map(|(w, p)| w * p).sum())
}

/// A forward FFT plan, shared between every user of its size.
type Plan = Arc<dyn Fft<f32>>;

/// The forward FFT plan for `n_fft`, planned once per process.
fn forward_fft(n_fft: usize) -> Plan {
    static PLANS: OnceLock<Mutex<HashMap<usize, Plan>>> = OnceLock::new();
    let mut plans = PLANS.get_or_init(Mutex::default).lock();
    Arc::clone(
        plans
            .entry(n_fft)
            .or_insert_with(|| FftPlanner::<f32>::new().plan_fft_forward(n_fft)),
    )
}

/// Power spectra of a signal's windowed frames: `|rfft(frame · window)|²`
/// for frames of the window's length every `hop` samples, the signal
/// already padded as the model's reference pads it.
pub struct PowerSpectrogram {
    window: Vec<f32>,
    hop: usize,
    fft: Plan,
}

impl std::fmt::Debug for PowerSpectrogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PowerSpectrogram")
            .field("n_fft", &self.window.len())
            .field("hop", &self.hop)
            .finish_non_exhaustive()
    }
}

impl PowerSpectrogram {
    /// Frames of `window.len()` samples (the FFT size), `hop` apart.
    ///
    /// # Panics
    /// When the window is empty or the hop is zero.
    pub fn new(window: Vec<f32>, hop: usize) -> Self {
        assert!(!window.is_empty() && hop > 0, "an empty window or hop");
        let fft = forward_fft(window.len());
        Self { window, hop, fft }
    }

    /// The FFT size.
    pub fn n_fft(&self) -> usize {
        self.window.len()
    }

    /// How many whole frames a signal of `len` samples holds.
    pub fn frames(&self, len: usize) -> usize {
        if len < self.n_fft() {
            0
        } else {
            1 + (len - self.n_fft()) / self.hop
        }
    }

    /// Call `f(frame, power)` for the first `frames` frames of `signal`,
    /// `power` holding the `n_fft / 2 + 1` bins of that frame.
    ///
    /// # Panics
    /// When `signal` holds fewer than `frames` whole frames.
    pub fn for_each(&self, signal: &[f32], frames: usize, mut f: impl FnMut(usize, &[f32])) {
        assert!(
            frames <= self.frames(signal.len()),
            "more frames than the signal holds"
        );
        let n_fft = self.n_fft();
        let mut buffer = vec![Complex::new(0.0f32, 0.0); n_fft];
        let mut power = vec![0.0f32; n_fft / 2 + 1];
        for t in 0..frames {
            let frame = &signal[t * self.hop..t * self.hop + n_fft];
            for ((c, &s), &w) in buffer.iter_mut().zip(frame).zip(&self.window) {
                *c = Complex::new(s * w, 0.0);
            }
            self.fft.process(&mut buffer);
            for (p, c) in power.iter_mut().zip(&buffer) {
                *p = c.norm_sqr();
            }
            f(t, &power);
        }
    }
}

#[cfg(test)]
#[path = "tests/mel.rs"]
mod tests;
