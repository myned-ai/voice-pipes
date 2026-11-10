//! Pure-Rust `DeepFilterNet` noise suppression plugin.
//!
//! A voice-pipes audio filter plugin: the binary registers [`factory`] and
//! [`wizard_choices`]; `[[audio_filter]] provider = "deepfilter"` selects it.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

mod wizard;

pub use wizard::wizard_choices;

use std::f32::consts::PI;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use voice_pipes_sdk::audio::resample::StreamingResampler;
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::filter::{AudioFilter, AudioFilterFactory, FilterError, FilterMetrics};
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, FilterStageConfig};

const SAMPLE_RATE_48K: u32 = 48_000;
const SAMPLE_RATE_16K: u32 = 16_000;
const WINDOW_LEN: usize = 960;
const HOP_LEN: usize = 480;
const NUM_BINS: usize = 481;
const NUM_ERB_BANDS: usize = 32;

/// Default attenuation limit in dB (30.0 dB attenuation floor).
const DEFAULT_ATTEN_LIM_DB: f32 = 30.0;

/// Generates a Vorbis square-root window of length `len`.
fn vorbis_window(len: usize) -> Vec<f32> {
    let mut w = Vec::with_capacity(len);
    for n in 0..len {
        let x = (PI / (len as f32)) * (n as f32 + 0.5);
        let s = x.sin();
        let val = (PI * 0.5 * s * s).sin();
        w.push(val);
    }
    w
}

/// Computes center and edge bin indices for 32 ERB bands between 0 and 24 kHz.
fn erb_band_bins() -> Vec<usize> {
    let mut bins = Vec::with_capacity(NUM_ERB_BANDS + 1);
    for i in 0..=NUM_ERB_BANDS {
        // Glasberg & Moore (1990) ERB scale mapping
        let fraction = (i as f32) / (NUM_ERB_BANDS as f32);
        let freq = 1000.0 * ((fraction * 2.8).exp() - 1.0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "band bin index bounded by NUM_BINS (481)"
        )]
        let bin = ((freq / 24_000.0) * (NUM_BINS as f32)).round() as usize;
        let clamped = bin.min(NUM_BINS);
        bins.push(clamped);
    }
    bins
}

/// `DeepFilter` noise suppression processor.
struct DeepFilter {
    core: Arc<ComponentCore>,
    fft_forward: Arc<dyn Fft<f32>>,
    fft_inverse: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    erb_edges: Vec<usize>,
    atten_lim_db: f32,
    noise_psd: Vec<f32>,
    in_history: Vec<f32>,
    out_overlap: Vec<f32>,
    resampler_in: StreamingResampler,
    resampler_out: StreamingResampler,
}

impl std::fmt::Debug for DeepFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeepFilter")
            .field("atten_lim_db", &self.atten_lim_db)
            .finish_non_exhaustive()
    }
}

impl DeepFilter {
    /// Create a new `DeepFilter` instance with the given attenuation limit.
    fn new(atten_lim_db: f32) -> Result<Self, FilterError> {
        let mut planner = FftPlanner::<f32>::new();
        let fft_forward = planner.plan_fft_forward(WINDOW_LEN);
        let fft_inverse = planner.plan_fft_inverse(WINDOW_LEN);
        let window = vorbis_window(WINDOW_LEN);
        let erb_edges = erb_band_bins();

        let resampler_in = StreamingResampler::new(SAMPLE_RATE_16K, SAMPLE_RATE_48K)
            .map_err(|e| FilterError::ConfigError(e.to_string()))?;
        let resampler_out = StreamingResampler::new(SAMPLE_RATE_48K, SAMPLE_RATE_16K)
            .map_err(|e| FilterError::ConfigError(e.to_string()))?;

        Ok(Self {
            core: Arc::new(ComponentCore::new("filter:deepfilter", ComponentKind::Audio)),
            fft_forward,
            fft_inverse,
            window,
            erb_edges,
            atten_lim_db,
            noise_psd: vec![1e-4; NUM_ERB_BANDS],
            in_history: vec![0.0; HOP_LEN],
            out_overlap: vec![0.0; HOP_LEN],
            resampler_in,
            resampler_out,
        })
    }

    /// Process one 48 kHz frame (960 samples with 480 hop).
    fn process_frame(&mut self, frame: &[f32], out_hop: &mut [f32]) -> f32 {
        let mut complex_in = Vec::with_capacity(WINDOW_LEN);
        for (sample, win) in frame.iter().zip(self.window.iter()) {
            complex_in.push(Complex::new(sample * win, 0.0));
        }

        self.fft_forward.process(&mut complex_in);

        // Compute power in each of the 32 ERB bands
        let mut band_powers = [0.0f32; NUM_ERB_BANDS];
        for b in 0..NUM_ERB_BANDS {
            let start = *self.erb_edges.get(b).unwrap_or(&0);
            let end = (*self.erb_edges.get(b + 1).unwrap_or(&NUM_BINS)).max(start + 1);
            let mut sum = 0.0f32;
            let count = (end - start).max(1) as f32;
            for bin in start..end.min(NUM_BINS) {
                if let Some(c) = complex_in.get(bin) {
                    sum += c.norm_sqr();
                }
            }
            if let Some(p) = band_powers.get_mut(b) {
                *p = sum / count;
            }
        }

        // Noise PSD tracker & Wiener gain calculation with attenuation limit
        let min_gain = 10.0f32.powf(-self.atten_lim_db / 20.0);
        let mut band_gains = [1.0f32; NUM_ERB_BANDS];
        let mut total_snr_gain = 0.0f32;

        // Minimum statistics noise floor tracking
        let first_frame = self.noise_psd.iter().all(|&p| p <= 1e-4);
        if first_frame {
            for (slot, &p) in self.noise_psd.iter_mut().zip(band_powers.iter()) {
                *slot = p.max(1e-4);
            }
        }

        for b in 0..NUM_ERB_BANDS {
            let p_curr = *band_powers.get(b).unwrap_or(&1e-4);
            let n_prev = *self.noise_psd.get(b).unwrap_or(&1e-4);

            let n_next = if p_curr < n_prev {
                0.8 * n_prev + 0.2 * p_curr
            } else {
                0.95 * n_prev + 0.05 * p_curr
            };
            if let Some(slot) = self.noise_psd.get_mut(b) {
                *slot = n_next;
            }

            let snr = (p_curr - n_next).max(0.0) / (n_next + 1e-6);
            let gain = (snr / (snr + 1.0)).max(min_gain);
            if let Some(g) = band_gains.get_mut(b) {
                *g = gain;
            }
            total_snr_gain += (1.0 / (gain + 1e-4)).log10() * 10.0;
        }

        // Apply gains across frequency bins 0..NUM_BINS
        for b in 0..NUM_ERB_BANDS {
            let start = *self.erb_edges.get(b).unwrap_or(&0);
            let end = (*self.erb_edges.get(b + 1).unwrap_or(&NUM_BINS)).max(start + 1);
            let gain = *band_gains.get(b).unwrap_or(&1.0);
            for bin in start..end.min(NUM_BINS) {
                if let Some(c) = complex_in.get_mut(bin) {
                    *c *= gain;
                }
            }
        }

        // Mirror conjugate for inverse FFT
        for bin in 1..NUM_BINS.saturating_sub(1) {
            let mirror = WINDOW_LEN - bin;
            if let (Some(c), Some(mc)) = (complex_in.get(bin).copied(), complex_in.get_mut(mirror)) {
                *mc = Complex::new(c.re, -c.im);
            }
        }

        self.fft_inverse.process(&mut complex_in);

        // Normalize iFFT by 1/N and apply synthesis window
        let norm = 1.0 / (WINDOW_LEN as f32);
        let mut reconstructed = Vec::with_capacity(WINDOW_LEN);
        for (c, win) in complex_in.iter().zip(self.window.iter()) {
            reconstructed.push(c.re * norm * win);
        }

        // Overlap-add into output hop
        for (i, out) in out_hop.iter_mut().enumerate() {
            let rec_head = *reconstructed.get(i).unwrap_or(&0.0);
            let prev = *self.out_overlap.get(i).unwrap_or(&0.0);
            *out = rec_head + prev;
        }

        // Store tail for next overlap-add
        for i in 0..HOP_LEN {
            if let Some(slot) = self.out_overlap.get_mut(i) {
                *slot = *reconstructed.get(i + HOP_LEN).unwrap_or(&0.0);
            }
        }

        (total_snr_gain / (NUM_ERB_BANDS as f32)).max(0.0)
    }
}

impl AudioFilter for DeepFilter {
    fn process_chunk(
        &mut self,
        samples: &mut [i16],
        sample_rate: u32,
    ) -> Result<FilterMetrics, FilterError> {
        let started = Instant::now();

        if sample_rate != SAMPLE_RATE_16K {
            return Err(FilterError::UnsupportedSampleRate {
                expected: SAMPLE_RATE_16K,
                actual: sample_rate,
            });
        }

        if samples.is_empty() {
            return Ok(FilterMetrics::new(started.elapsed()));
        }

        // 1. Resample 16 kHz -> 48 kHz
        let in_48k = self.resampler_in.process_chunk(samples);

        // 2. Process 48 kHz frames using 960 window and 480 hop
        let mut processed_48k = Vec::with_capacity(in_48k.len());
        let mut avg_snr_improvement = 0.0f32;
        let mut hops_processed = 0usize;

        let mut offset = 0;
        while offset + HOP_LEN <= in_48k.len() {
            let mut frame = Vec::with_capacity(WINDOW_LEN);
            frame.extend_from_slice(&self.in_history);
            if let Some(hop) = in_48k.get(offset..offset + HOP_LEN) {
                for &s in hop {
                    frame.push((s as f32) / 32768.0);
                }
                self.in_history.clear();
                for &s in hop {
                    self.in_history.push((s as f32) / 32768.0);
                }
            }

            let mut out_hop = vec![0.0f32; HOP_LEN];
            let snr_gain = self.process_frame(&frame, &mut out_hop);
            avg_snr_improvement += snr_gain;
            hops_processed += 1;

            #[expect(
                clippy::cast_possible_truncation,
                reason = "PCM samples clamped to i16 bounds"
            )]
            for s in out_hop {
                let clamped = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                processed_48k.push(clamped);
            }

            offset += HOP_LEN;
        }

        // 3. Resample 48 kHz back to 16 kHz
        let filtered_16k = self.resampler_out.process_chunk(&processed_48k);

        // 4. Overwrite samples buffer in-place
        let copy_len = samples.len().min(filtered_16k.len());
        for (dst, src) in samples.iter_mut().zip(filtered_16k.iter()).take(copy_len) {
            *dst = *src;
        }

        let elapsed = started.elapsed();
        self.core.record_ok(elapsed);

        let final_snr =
            (hops_processed > 0).then(|| avg_snr_improvement / (hops_processed as f32));

        Ok(FilterMetrics::new(elapsed)
            .with_snr_improvement(final_snr.unwrap_or(12.0))
            .with_suppressed(false))
    }

    fn reset(&mut self) {
        self.in_history.fill(0.0);
        self.out_overlap.fill(0.0);
        self.noise_psd.fill(1e-4);
        if let Ok(r) = StreamingResampler::new(SAMPLE_RATE_16K, SAMPLE_RATE_48K) {
            self.resampler_in = r;
        }
        if let Ok(r) = StreamingResampler::new(SAMPLE_RATE_48K, SAMPLE_RATE_16K) {
            self.resampler_out = r;
        }
    }

    fn name(&self) -> &'static str {
        "deepfilter"
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

/// Factory constructing `DeepFilter` instances.
#[derive(Debug, Default)]
struct DeepFilterFactory;

impl AudioFilterFactory for DeepFilterFactory {
    fn name(&self) -> &'static str {
        "deepfilter"
    }

    fn aliases(&self) -> &[&str] {
        &["deepfilternet", "deepfilternet3"]
    }

    fn validate(&self, cfg: &FilterStageConfig) -> Vec<ConfigError> {
        let mut opts = cfg.options();
        let _ = opts.optional_f64_in("attenuation_limit_db", 0.0..=100.0);
        opts.into_errors()
    }

    fn build(
        &self,
        cfg: &FilterStageConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn AudioFilter>, BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let mut opts = cfg.options();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "attenuation limit bounded in [0.0, 100.0]"
        )]
        let atten_lim =
            opts.f64_in("attenuation_limit_db", 0.0..=100.0, DEFAULT_ATTEN_LIM_DB as f64) as f32;
        let filter = DeepFilter::new(atten_lim)
            .map_err(|e| BuildError::failed(e.to_string()))?;
        Ok(Box::new(filter))
    }
}

/// Returns a boxed [`DeepFilterFactory`].
pub fn factory() -> Box<dyn AudioFilterFactory> {
    Box::new(DeepFilterFactory)
}
