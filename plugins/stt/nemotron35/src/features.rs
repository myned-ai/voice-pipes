//! Log-mel features as `NemotronAsrStreamingFeatureExtractor` computes them.
//!
//! The encoder was trained on exactly this front-end, so this is a
//! specification to reproduce, not a design to improve: 16 kHz PCM, 0.97
//! pre-emphasis over the whole waveform, a symmetric 400-point Hann window
//! centred in a 512-point frame at a 160-sample hop, power spectrum, a
//! Slaney-scaled and Slaney-normalised 128-band mel filterbank, and
//! `ln(mel + 2^-24)`. No normalisation afterwards. The test in
//! `tests/features.rs` holds every value to the reference extractor's output
//! on two recordings.
//!
//! `center` is what makes chunked extraction equal whole-utterance
//! extraction: the first call pads `n_fft / 2` on both sides as an offline
//! pass would, and every later call is handed audio that already begins
//! `n_fft / 2` samples before its first frame, so it must not pad again.

use voice_pipes_onnx::mel::{
    Hann, PowerSpectrogram, centred, hann, mel_energies, slaney_filterbank,
};

/// The model's sample rate, in Hz.
pub const SAMPLE_RATE_HZ: u32 = 16_000;
pub const SAMPLE_RATE: usize = SAMPLE_RATE_HZ as usize;
pub const N_FFT: usize = 512;
pub const HOP_LENGTH: usize = 160;
pub const WIN_LENGTH: usize = 400;
pub const N_MELS: usize = 128;
pub const PREEMPHASIS: f32 = 0.97;
/// `2^-24`, added before the log so silence has a floor rather than `-inf`.
pub const LOG_ZERO_GUARD: f32 = 5.960_464_5e-8;

pub struct LogMelExtractor {
    spectrogram: PowerSpectrogram,
    filters: Vec<Vec<f32>>,
}

impl std::fmt::Debug for LogMelExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogMelExtractor")
            .field("spectrogram", &self.spectrogram)
            .field("mel_bins", &self.filters.len())
            .finish()
    }
}

impl Default for LogMelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl LogMelExtractor {
    pub fn new() -> Self {
        // A symmetric Hann of `win_length` (torch `periodic=False`, numpy
        // `hanning`), centred in the `n_fft` frame with zeros either side.
        let window = centred(&hann(WIN_LENGTH, Hann::Symmetric), N_FFT);
        Self {
            spectrogram: PowerSpectrogram::new(window, HOP_LENGTH),
            filters: slaney_filterbank(SAMPLE_RATE, N_FFT, N_MELS, 0.0, 8000.0),
        }
    }

    /// Frames of `N_MELS` log-mel values, row-major, for 16 kHz mono PCM in
    /// `[-1, 1]`. Returns the frame count with the flat buffer.
    pub fn extract(&self, pcm: &[f32], center: bool) -> (usize, Vec<f32>) {
        // Pre-emphasis over the waveform as given; the first sample is kept.
        let mut x: Vec<f32> = Vec::with_capacity(pcm.len() + N_FFT);
        if center {
            x.extend(std::iter::repeat_n(0.0, N_FFT / 2));
        }
        if let Some((&first, rest)) = pcm.split_first() {
            x.push(first);
            let mut prev = first;
            for &s in rest {
                x.push(s - PREEMPHASIS * prev);
                prev = s;
            }
        }
        if center {
            x.extend(std::iter::repeat_n(0.0, N_FFT / 2));
        }
        let n_frames = self.spectrogram.frames(x.len());
        let mut out = Vec::with_capacity(n_frames * N_MELS);
        self.spectrogram.for_each(&x, n_frames, |_, power| {
            out.extend(mel_energies(&self.filters, power).map(|mel| (mel + LOG_ZERO_GUARD).ln()));
        });
        (n_frames, out)
    }
}
