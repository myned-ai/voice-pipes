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

use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::filter::{AudioFilter, AudioFilterFactory, FilterError, FilterMetrics};
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, FilterStageConfig};

const SAMPLE_RATE_16K: u32 = 16_000;
/// 20 ms analysis/synthesis window at 16 kHz.
const WINDOW_LEN: usize = 320;
/// 10 ms hop size (50% overlap) at 16 kHz.
const HOP_LEN: usize = 160;
/// Number of positive-frequency FFT bins (`WINDOW_LEN / 2 + 1`).
const NUM_BINS: usize = 161;
/// Number of Equivalent Rectangular Bandwidth (ERB) bands spanning 0..8 kHz.
const NUM_ERB_BANDS: usize = 32;

/// Default attenuation limit in dB (30.0 dB attenuation floor).
const DEFAULT_ATTEN_LIM_DB: f32 = 30.0;

/// Generates a Vorbis square-root window of length `len` satisfying the
/// Princen-Bradley perfect reconstruction condition at 50% overlap.
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

/// Computes strictly monotonic edge bin indices for 32 ERB bands across 0..8 kHz.
fn erb_band_bins() -> Vec<usize> {
    let mut bins = Vec::with_capacity(NUM_ERB_BANDS + 1);
    let erb_max = (1.0_f32 + 0.00437 * 8_000.0).ln();
    for i in 0..=NUM_ERB_BANDS {
        // Glasberg & Moore (1990) ERB scale mapping over 0..8 kHz
        let fraction = (i as f32) / (NUM_ERB_BANDS as f32);
        let freq = ((fraction * erb_max).exp() - 1.0) / 0.00437;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "band bin index bounded by NUM_BINS (161)"
        )]
        let raw_bin = ((freq / 8_000.0) * ((NUM_BINS - 1) as f32)).round() as usize;
        let min_bin = i.min(NUM_BINS);
        let max_bin = NUM_BINS.saturating_sub(NUM_ERB_BANDS.saturating_sub(i));
        bins.push(raw_bin.clamp(min_bin, max_bin));
    }
    if let Some(last) = bins.last_mut() {
        *last = NUM_BINS;
    }
    bins
}

/// Estimates the broadband noise floor from the quieter half of voice bands (bands 3..32).
fn estimate_voice_noise_floor(band_powers: &[f32; NUM_ERB_BANDS]) -> f32 {
    let mut sorted_voice = *band_powers;
    if let Some(voice_slice) = sorted_voice.get_mut(3..NUM_ERB_BANDS) {
        voice_slice.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    }
    let half_count = (NUM_ERB_BANDS - 3) / 2;
    let lower_sum: f32 = sorted_voice.iter().skip(3).take(half_count).sum();
    (lower_sum / (half_count.max(1) as f32)).max(1e-6)
}

/// `DeepFilter` noise suppression processor with pre-allocated zero-allocation
/// hot-path buffers and decision-directed ERB Wiener filtering.
struct DeepFilter {
    core: Arc<ComponentCore>,
    fft_forward: Arc<dyn Fft<f32>>,
    fft_inverse: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    erb_edges: Vec<usize>,
    atten_lim_db: f32,
    min_gain: f32,
    initialized: bool,
    noise_psd: [f32; NUM_ERB_BANDS],
    prev_clean_psd: [f32; NUM_ERB_BANDS],
    in_history: Vec<f32>,
    out_overlap: Vec<f32>,
    fft_buf: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    frame_buf: Vec<f32>,
    out_hop_buf: Vec<f32>,
    in_fifo: Vec<i16>,
    out_fifo: Vec<i16>,
}

impl fmt::Debug for DeepFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeepFilter")
            .field("atten_lim_db", &self.atten_lim_db)
            .finish_non_exhaustive()
    }
}

impl DeepFilter {
    /// Create a new `DeepFilter` instance with the given attenuation limit.
    fn new(atten_lim_db: f32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft_forward = planner.plan_fft_forward(WINDOW_LEN);
        let fft_inverse = planner.plan_fft_inverse(WINDOW_LEN);
        let scratch_len = fft_forward
            .get_inplace_scratch_len()
            .max(fft_inverse.get_inplace_scratch_len());
        let window = vorbis_window(WINDOW_LEN);
        let erb_edges = erb_band_bins();
        let min_gain = 10.0_f32.powf(-atten_lim_db / 20.0);

        Self {
            core: Arc::new(ComponentCore::new("filter:deepfilter", ComponentKind::Audio)),
            fft_forward,
            fft_inverse,
            window,
            erb_edges,
            atten_lim_db,
            min_gain,
            initialized: false,
            noise_psd: [1e-6; NUM_ERB_BANDS],
            prev_clean_psd: [0.0; NUM_ERB_BANDS],
            in_history: vec![0.0; HOP_LEN],
            out_overlap: vec![0.0; HOP_LEN],
            fft_buf: vec![Complex::new(0.0, 0.0); WINDOW_LEN],
            fft_scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            frame_buf: vec![0.0; WINDOW_LEN],
            out_hop_buf: vec![0.0; HOP_LEN],
            in_fifo: Vec::with_capacity(WINDOW_LEN * 2),
            out_fifo: Vec::with_capacity(WINDOW_LEN * 2),
        }
    }

    /// Compute mean power in each of the 32 ERB bands from `self.fft_buf`.
    fn compute_band_powers(&self) -> [f32; NUM_ERB_BANDS] {
        let mut band_powers = [0.0_f32; NUM_ERB_BANDS];
        for (b, slot) in band_powers.iter_mut().enumerate() {
            let start = *self.erb_edges.get(b).unwrap_or(&0);
            let end = (*self.erb_edges.get(b + 1).unwrap_or(&NUM_BINS)).max(start + 1);
            let mut sum = 0.0_f32;
            let count = (end - start).max(1) as f32;
            for bin in start..end.min(NUM_BINS) {
                if let Some(c) = self.fft_buf.get(bin) {
                    sum += c.norm_sqr();
                }
            }
            *slot = sum / count;
        }
        band_powers
    }

    /// Update MCRA noise floor and compute decision-directed ERB Wiener gains.
    fn compute_erb_gains(&mut self, band_powers: &[f32; NUM_ERB_BANDS]) -> ([f32; NUM_ERB_BANDS], f32) {
        let noise_floor_est = estimate_voice_noise_floor(band_powers);
        if !self.initialized {
            self.initialized = true;
            for (b, (slot, &p)) in self.noise_psd.iter_mut().zip(band_powers.iter()).enumerate() {
                *slot = if b < 3 || p <= 4.0 * noise_floor_est {
                    p.max(1e-6)
                } else {
                    noise_floor_est
                };
            }
        }

        let mut band_gains = [1.0_f32; NUM_ERB_BANDS];
        let mut total_in_power = 0.0_f32;
        let mut total_out_power = 0.0_f32;

        for b in 0..NUM_ERB_BANDS {
            let p_curr = *band_powers.get(b).unwrap_or(&1e-6);
            let n_prev = *self.noise_psd.get(b).unwrap_or(&1e-6);

            let n_next = if b < 3 {
                0.70 * n_prev + 0.30 * p_curr
            } else if p_curr < n_prev {
                0.80 * n_prev + 0.20 * p_curr
            } else if p_curr > 4.0 * noise_floor_est {
                0.9995 * n_prev + 0.0005 * noise_floor_est
            } else {
                0.85 * n_prev + 0.15 * p_curr
            };
            let n_clamped = n_next.max(1e-6);
            if let Some(slot) = self.noise_psd.get_mut(b) {
                *slot = n_clamped;
            }

            let post_snr = p_curr / n_clamped;
            let inst_snr = (post_snr - 1.0).max(0.0);
            let prev_clean = *self.prev_clean_psd.get(b).unwrap_or(&0.0);
            let prio_snr = 0.85 * (prev_clean / n_clamped) + 0.15 * inst_snr;

            let gain = (prio_snr / (prio_snr + 1.0)).max(self.min_gain);
            if let Some(g) = band_gains.get_mut(b) {
                *g = gain;
            }
            if let Some(clean_slot) = self.prev_clean_psd.get_mut(b) {
                *clean_slot = p_curr * gain * gain;
            }

            total_in_power += p_curr;
            total_out_power += p_curr * gain * gain;
        }

        let snr_db = if total_in_power > 1e-6 {
            (10.0 * (total_in_power / (total_out_power + 1e-9)).log10()).max(0.0)
        } else {
            0.0
        };
        (band_gains, snr_db)
    }

    /// Process one 16 kHz frame (`self.frame_buf`, 320 samples) into
    /// `self.out_hop_buf` (160 samples) with zero heap allocations.
    fn process_frame(&mut self) -> f32 {
        for ((slot, &sample), &win) in self
            .fft_buf
            .iter_mut()
            .zip(self.frame_buf.iter())
            .zip(self.window.iter())
        {
            *slot = Complex::new(sample * win, 0.0);
        }

        self.fft_forward
            .process_with_scratch(&mut self.fft_buf, &mut self.fft_scratch);

        let band_powers = self.compute_band_powers();
        let (band_gains, snr_db) = self.compute_erb_gains(&band_powers);

        for b in 0..NUM_ERB_BANDS {
            let start = *self.erb_edges.get(b).unwrap_or(&0);
            let end = (*self.erb_edges.get(b + 1).unwrap_or(&NUM_BINS)).max(start + 1);
            let gain = *band_gains.get(b).unwrap_or(&1.0);
            for bin in start..end.min(NUM_BINS) {
                if let Some(c) = self.fft_buf.get_mut(bin) {
                    *c *= gain;
                }
            }
        }

        for bin in 1..NUM_BINS.saturating_sub(1) {
            let mirror = WINDOW_LEN - bin;
            if let (Some(c), Some(mc)) = (
                self.fft_buf.get(bin).copied(),
                self.fft_buf.get_mut(mirror),
            ) {
                *mc = Complex::new(c.re, -c.im);
            }
        }

        self.fft_inverse
            .process_with_scratch(&mut self.fft_buf, &mut self.fft_scratch);

        let norm = 1.0 / (WINDOW_LEN as f32);
        for (i, out) in self.out_hop_buf.iter_mut().enumerate() {
            let c_re = self.fft_buf.get(i).map_or(0.0, |c| c.re);
            let win = *self.window.get(i).unwrap_or(&0.0);
            let prev = *self.out_overlap.get(i).unwrap_or(&0.0);
            *out = c_re * norm * win + prev;
        }

        for i in 0..HOP_LEN {
            let idx = i + HOP_LEN;
            let c_re = self.fft_buf.get(idx).map_or(0.0, |c| c.re);
            let win = *self.window.get(idx).unwrap_or(&0.0);
            if let Some(slot) = self.out_overlap.get_mut(i) {
                *slot = c_re * norm * win;
            }
        }

        snr_db
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

        self.in_fifo.extend_from_slice(samples);

        let mut total_snr_improvement = 0.0_f32;
        let mut hops_processed = 0_usize;

        while self.in_fifo.len() >= HOP_LEN {
            for (dst, &src) in self.frame_buf.iter_mut().zip(self.in_history.iter()) {
                *dst = src;
            }
            if let Some((hop_in, frame_tail)) = self
                .in_fifo
                .get(..HOP_LEN)
                .zip(self.frame_buf.get_mut(HOP_LEN..WINDOW_LEN))
            {
                for ((dst_frame, dst_hist), &s) in frame_tail
                    .iter_mut()
                    .zip(self.in_history.iter_mut())
                    .zip(hop_in.iter())
                {
                    let norm_s = (s as f32) / 32768.0;
                    *dst_frame = norm_s;
                    *dst_hist = norm_s;
                }
            }
            self.in_fifo.drain(..HOP_LEN);

            total_snr_improvement += self.process_frame();
            hops_processed += 1;

            #[expect(
                clippy::cast_possible_truncation,
                reason = "PCM samples clamped to i16 bounds"
            )]
            for &s in &self.out_hop_buf {
                let pcm = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                self.out_fifo.push(pcm);
            }
        }

        let avail = self.out_fifo.len().min(samples.len());
        let pad = samples.len().saturating_sub(avail);
        for dst in samples.iter_mut().take(pad) {
            *dst = 0;
        }
        if let Some((dst_slice, src_slice)) = samples
            .get_mut(pad..pad + avail)
            .zip(self.out_fifo.get(..avail))
        {
            dst_slice.copy_from_slice(src_slice);
        }
        self.out_fifo.drain(..avail);

        let elapsed = started.elapsed();
        self.core.record_ok(elapsed);

        let avg_snr = if hops_processed > 0 {
            total_snr_improvement / (hops_processed as f32)
        } else {
            0.0
        };

        Ok(FilterMetrics::new(elapsed)
            .with_snr_improvement(avg_snr)
            .with_suppressed(false))
    }

    fn reset(&mut self) {
        self.initialized = false;
        self.noise_psd.fill(1e-6);
        self.prev_clean_psd.fill(0.0);
        self.in_history.fill(0.0);
        self.out_overlap.fill(0.0);
        self.in_fifo.clear();
        self.out_fifo.clear();
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
        Ok(Box::new(DeepFilter::new(atten_lim)))
    }
}

/// Returns a boxed [`DeepFilterFactory`].
pub fn factory() -> Box<dyn AudioFilterFactory> {
    Box::new(DeepFilterFactory)
}
