//! High-performance sample-rate conversion for the voice pipeline.
//!
//! Designed as an insertable pipeline stage -- call [`resample`] on any
//! `AudioFrame` to convert it to a target sample rate.  The inner loop is
//! structured for auto-vectorization (no data-dependent branches, contiguous
//! memory access, `f32` arithmetic).
//!
//! Algorithm: **windowed-sinc interpolation** with a Kaiser-windowed kernel.
//! Quality-wise this sits between libsamplerate `SRC_SINC_FASTEST` and
//! `SRC_SINC_MEDIUM_QUALITY`.

#![expect(
    clippy::needless_range_loop,
    reason = "DSP kernels index several buffers by one position; iterator chains obscure the math"
)]

use std::ops::RangeInclusive;

use super::AudioFrame;

/// Sample rates the resamplers convert between. Zero divides by zero or
/// never finishes, and a rate far below any audio rate multiplies a buffer
/// into gigabytes; every rate in real use is inside.
pub const SUPPORTED_RATES: RangeInclusive<u32> = 1_000..=384_000;

/// A conversion the resamplers refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ResampleError {
    /// A rate outside [`SUPPORTED_RATES`].
    #[error("cannot resample at {rate} Hz: supported rates are 1000 to 384000 Hz")]
    UnsupportedRate {
        /// The rate refused.
        rate: u32,
    },
}

/// `Ok` when both rates are ones the resamplers convert between.
fn check_rates(from: u32, to: u32) -> Result<(), ResampleError> {
    for rate in [from, to] {
        if !SUPPORTED_RATES.contains(&rate) {
            return Err(ResampleError::UnsupportedRate { rate });
        }
    }
    Ok(())
}

/// Number of zero-crossings on each side of the sinc kernel.
/// 8 gives good quality for speech; higher = better rejection but slower.
const SINC_ZEROS: usize = 8;

/// Oversampling factor for the precomputed sinc table.
/// Higher = finer fractional-delay resolution.  64 is plenty for speech.
const TABLE_OVERSAMPLING: usize = 64;

/// Total taps per side: `SINC_ZEROS * TABLE_OVERSAMPLING`.
const TABLE_HALF_LEN: usize = SINC_ZEROS * TABLE_OVERSAMPLING;

/// Precomputed sinc.Kaiser table (one wing, length = `TABLE_HALF_LEN` + 1).
/// Built at compile time so there's no startup cost.
struct SincTable {
    table: [f32; TABLE_HALF_LEN + 1],
}

impl SincTable {
    const fn new() -> Self {
        let mut table = [0.0f32; TABLE_HALF_LEN + 1];
        let beta = 6.0; // Kaiser beta -- good tradeoff for speech
        let bessel_beta = bessel_i0_const(beta);
        let mut i = 0;
        while i <= TABLE_HALF_LEN {
            let x = i as f64 / TABLE_OVERSAMPLING as f64; // 0.0 .. SINC_ZEROS
            let sinc = if i == 0 {
                1.0
            } else {
                let px = std::f64::consts::PI * x;
                sin_approx(px) / px
            };
            // Kaiser window: I0(beta * sqrt(1 - (x/N)^2)) / I0(beta)
            let r = x / SINC_ZEROS as f64;
            let arg = beta * sqrt_approx(1.0 - r * r);
            let kaiser = bessel_i0_const(arg) / bessel_beta;
            table[i] = (sinc * kaiser) as f32;
            i += 1;
        }
        Self { table }
    }

    /// Linearly interpolate the sinc kernel at fractional table position `pos`.
    /// `pos` is in units of table samples (0.0 .. `TABLE_HALF_LEN`).
    #[expect(
        clippy::inline_always,
        reason = "measured hot path in the resampler inner loop"
    )]
    #[inline(always)]
    fn lookup(&self, pos: f64) -> f32 {
        let idx = pos as usize;
        if idx >= TABLE_HALF_LEN {
            return 0.0;
        }
        let frac = pos - idx as f64;
        let a = self.table[idx];
        let b = self.table[idx + 1];
        a + (b - a) * frac as f32
    }
}

// ── Const-compatible math helpers ──────────────────────────────

/// sin(x) via Taylor series -- accurate for the range we need.
const fn sin_approx(x: f64) -> f64 {
    // Reduce to [-π, π]
    let mut v = x;
    while v > std::f64::consts::PI {
        v -= 2.0 * std::f64::consts::PI;
    }
    while v < -std::f64::consts::PI {
        v += 2.0 * std::f64::consts::PI;
    }
    // Taylor: x - x^3/6 + x^5/120 - x^7/5040 + x^9/362880 - x^11/39916800
    let x2 = v * v;
    let x3 = v * x2;
    let x5 = x3 * x2;
    let x7 = x5 * x2;
    let x9 = x7 * x2;
    let x11 = x9 * x2;
    v - x3 / 6.0 + x5 / 120.0 - x7 / 5040.0 + x9 / 362_880.0 - x11 / 39_916_800.0
}

/// sqrt(x) via Newton's method -- const-compatible.
const fn sqrt_approx(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut guess = x;
    let mut i = 0;
    while i < 20 {
        guess = f64::midpoint(guess, x / guess);
        i += 1;
    }
    guess
}

/// Modified Bessel I0(x) -- const-compatible power-series (25 terms, plenty for beta<=12).
const fn bessel_i0_const(x: f64) -> f64 {
    let half_x = x / 2.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut k = 1;
    while k <= 25 {
        term *= (half_x / k as f64) * (half_x / k as f64);
        sum += term;
        k += 1;
    }
    sum
}

static SINC: SincTable = SincTable::new();

/// Tap range and table scaling for one conversion ratio (`from / to`).
///
/// When downsampling (`ratio > 1`) the kernel must be stretched by
/// `ratio` so its cutoff sits at the *output* Nyquist, and the window must
/// cover `SINC_ZEROS * ratio` input samples per side. The previous code
/// evaluated `sinc(distance)` in input-sample units for every ratio, i.e. an
/// interpolator whose cutoff was always the *input* Nyquist: every
/// 48 kHz -> 16 kHz path folded 8..24 kHz content (fricatives, keyboard
/// clicks, HVAC hiss) into the speech band. Upsampling keeps the plain
/// kernel: the input is already band-limited below the output Nyquist.
#[derive(Debug, Clone, Copy)]
struct Kernel {
    /// Taps on each side of the centre, in input samples.
    half_taps: isize,
    /// Multiplier from input-sample distance to sinc-table position.
    table_scale: f64,
}

impl Kernel {
    fn for_ratio(ratio: f64) -> Self {
        if ratio > 1.0 {
            Self {
                half_taps: (SINC_ZEROS as f64 * ratio).ceil() as isize,
                table_scale: TABLE_OVERSAMPLING as f64 / ratio,
            }
        } else {
            Self {
                half_taps: SINC_ZEROS as isize,
                table_scale: TABLE_OVERSAMPLING as f64,
            }
        }
    }

    /// History a streaming resampler must keep so every tap of a deferred
    /// output sample is still reachable after the smallest possible chunk.
    fn history_len(self) -> usize {
        2 * self.half_taps as usize + 1
    }
}

// ── Public API ──────────────────────────────────────────────

/// Resample an `AudioFrame` to `target_rate` Hz.
///
/// Returns a new frame with the converted samples and updated `sample_rate`.
/// If the frame is already at `target_rate`, it is returned unchanged (zero-copy).
///
/// Handles multi-channel audio by resampling each channel independently.
///
/// # Errors
/// When a rate is outside [`SUPPORTED_RATES`]: a frame whose rate is
/// unknown (zero) cannot be converted.
pub fn resample(frame: AudioFrame, target_rate: u32) -> Result<AudioFrame, ResampleError> {
    if frame.sample_rate == target_rate {
        return Ok(frame);
    }
    check_rates(frame.sample_rate, target_rate)?;
    if frame.samples.is_empty() {
        return Ok(AudioFrame {
            sample_rate: target_rate,
            ..frame
        });
    }
    // `channels == 0` reaches here from wire-supplied frames
    // (proto3 default) and from `AudioChunk::default()`; treat it as mono
    // instead of dividing by zero.
    let channels = usize::from(frame.channels.max(1));
    let src_rate = frame.sample_rate;

    // Deinterleave -> resample each channel -> reinterleave
    let frames_in = frame.samples.len() / channels;
    let ratio = src_rate as f64 / target_rate as f64;
    let frames_out = ((frames_in as f64) / ratio).ceil() as usize;

    if channels == 1 {
        // Fast path: no deinterleave overhead
        let out = resample_channel(&frame.samples, frames_in, frames_out, ratio);
        Ok(AudioFrame {
            samples: out,
            sample_rate: target_rate,
            channels: frame.channels,
        })
    } else {
        // Deinterleave
        let mut ch_bufs: Vec<Vec<i16>> = (0..channels)
            .map(|c| {
                (0..frames_in)
                    .map(|f| frame.samples[f * channels + c])
                    .collect()
            })
            .collect();

        // Resample each channel
        for buf in &mut ch_bufs {
            *buf = resample_channel(buf, frames_in, frames_out, ratio);
        }

        // Reinterleave
        let mut out = Vec::with_capacity(frames_out * channels);
        for f in 0..frames_out {
            for c in 0..channels {
                out.push(ch_bufs[c][f]);
            }
        }

        Ok(AudioFrame {
            samples: out,
            sample_rate: target_rate,
            channels: frame.channels,
        })
    }
}

/// Core resampling loop for a single channel of i16 samples.
///
/// Uses windowed-sinc interpolation from the precomputed `SINC` table.
/// The loop is structured for auto-vectorization: no branches inside the
/// inner tap accumulation, contiguous reads, pure f32 arithmetic.
#[inline(never)] // prevent inlining so the compiler can vectorize the inner loop
fn resample_channel(input: &[i16], frames_in: usize, frames_out: usize, ratio: f64) -> Vec<i16> {
    let mut output = vec![0i16; frames_out];
    let kernel = Kernel::for_ratio(ratio);

    for out_idx in 0..frames_out {
        let center = out_idx as f64 * ratio; // fractional position in input
        let center_floor = center as isize;
        let frac = center - center_floor as f64; // 0.0..1.0

        let mut accum: f64 = 0.0;
        let mut weight_sum: f64 = 0.0;

        // Left wing: taps from center_floor down to center_floor - half_taps + 1
        // Right wing: taps from center_floor + 1 up to center_floor + half_taps
        for tap in -kernel.half_taps..=kernel.half_taps {
            let src_idx = center_floor + tap;
            if src_idx < 0 || src_idx >= frames_in as isize {
                continue;
            }
            let distance = (tap as f64 - frac).abs();
            let table_pos = distance * kernel.table_scale;
            let w = SINC.lookup(table_pos) as f64;
            accum += input[src_idx as usize] as f64 * w;
            weight_sum += w;
        }

        // Normalize to maintain amplitude
        let sample = if weight_sum > 0.0 {
            accum / weight_sum
        } else {
            0.0
        };

        output[out_idx] = sample.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
    }

    output
}

/// Convenience: resample a slice of f32 samples (mono) -- useful for providers
/// that work in float internally (e.g., Parakeet preprocessor).
///
/// # Errors
/// When a rate is outside [`SUPPORTED_RATES`], as for [`resample`]: a
/// frame's rate comes from a client, and zero would ask for an output of
/// unbounded length.
pub fn resample_f32(
    samples: &[f32],
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<f32>, ResampleError> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }
    check_rates(from_rate, to_rate)?;
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let kernel = Kernel::for_ratio(ratio);
    let frames_in = samples.len();

    let mut output = vec![0.0f32; out_len];

    for out_idx in 0..out_len {
        let center = out_idx as f64 * ratio;
        let center_floor = center as isize;
        let frac = center - center_floor as f64;

        let mut accum: f64 = 0.0;
        let mut weight_sum: f64 = 0.0;

        for tap in -kernel.half_taps..=kernel.half_taps {
            let src_idx = center_floor + tap;
            if src_idx < 0 || src_idx >= frames_in as isize {
                continue;
            }
            let distance = (tap as f64 - frac).abs();
            let table_pos = distance * kernel.table_scale;
            let w = SINC.lookup(table_pos) as f64;
            accum += samples[src_idx as usize] as f64 * w;
            weight_sum += w;
        }

        output[out_idx] = if weight_sum > 0.0 {
            (accum / weight_sum) as f32
        } else {
            0.0
        };
    }

    Ok(output)
}

// ── Streaming resampler ─────────────────────────────────────

/// Stateful resampler for chunk-based streaming.
///
/// Maintains a history buffer (`2 * half_taps + 1` samples, sized for the
/// conversion ratio) and the current
/// output index so that windowed-sinc interpolation works seamlessly across
/// chunk boundaries.  Feed it 10-30 ms chunks via
/// [`process_chunk`](Self::process_chunk) and call [`flush`](Self::flush)
/// after the last chunk.
///
/// Uses the same Kaiser-windowed sinc kernel as the batch [`resample`]
/// function, so audio quality is identical (within +/-1 LSB).
#[derive(Debug)]
pub struct StreamingResampler {
    from_rate: u32,
    to_rate: u32,
    /// `from_rate / to_rate` -- same convention as the batch `resample_channel`.
    ratio: f64,
    /// Kernel geometry for this ratio.
    kernel: Kernel,
    /// Last `kernel.history_len()` input samples (as f64) from previous chunks.
    /// On the first chunk the buffer is logically invalid; taps that land
    /// before global position 0 are skipped, matching batch edge behaviour.
    history: Vec<f64>,
    /// Total number of input samples consumed so far (= start position of
    /// the next chunk in global sample coordinates).
    input_offset: usize,
    /// Next output sample index (monotonically increasing).
    output_index: usize,
}

impl StreamingResampler {
    /// Create a resampler converting `from_rate` Hz to `to_rate` Hz.
    ///
    /// # Errors
    /// When a rate is outside [`SUPPORTED_RATES`], which the filter cannot
    /// be built for.
    pub fn new(from_rate: u32, to_rate: u32) -> Result<Self, ResampleError> {
        if from_rate != to_rate {
            check_rates(from_rate, to_rate)?;
        }
        Ok(Self {
            from_rate,
            to_rate,
            ratio: from_rate as f64 / to_rate as f64,
            kernel: Kernel::for_ratio(from_rate as f64 / to_rate as f64),
            history: vec![0.0; Kernel::for_ratio(from_rate as f64 / to_rate as f64).history_len()],
            input_offset: 0,
            output_index: 0,
        })
    }

    /// Is this a passthrough (same rate)?
    pub fn is_passthrough(&self) -> bool {
        self.from_rate == self.to_rate
    }

    /// Source sample rate.
    pub fn from_rate(&self) -> u32 {
        self.from_rate
    }
    /// Target sample rate.
    pub fn to_rate(&self) -> u32 {
        self.to_rate
    }

    /// Process one chunk of mono i16 samples and return resampled output.
    ///
    /// Output samples whose sinc kernel would extend past the end of this
    /// chunk are *deferred* to the next `process_chunk` / `flush` call,
    /// ensuring the same kernel coverage as the batch resampler.
    pub fn process_chunk(&mut self, input: &[i16]) -> Vec<i16> {
        if self.from_rate == self.to_rate || input.is_empty() {
            self.input_offset += input.len();
            return input.to_vec();
        }

        let input_f64: Vec<f64> = input.iter().map(|&s| s as f64).collect();
        let chunk_len = input.len();
        let chunk_end_global = (self.input_offset + chunk_len) as isize;
        let kernel = self.kernel;
        let hist_len = self.history.len();
        let mut output = Vec::new();

        loop {
            let center = self.output_index as f64 * self.ratio;
            let center_floor = center.floor() as isize;

            // Defer: right-most tap would land at or past the chunk end.
            // This matches the batch resampler which only skips taps at the
            // *signal* boundary, not at arbitrary interior positions.
            if center_floor + kernel.half_taps >= chunk_end_global {
                break;
            }

            let frac = center - center_floor as f64;

            let mut accum: f64 = 0.0;
            let mut weight_sum: f64 = 0.0;

            for tap in -kernel.half_taps..=kernel.half_taps {
                let global_idx = center_floor + tap;

                // Before signal start -> skip (matches batch behavior)
                if global_idx < 0 {
                    continue;
                }

                let global_idx_u = global_idx as usize;

                let sample_val = if global_idx_u >= self.input_offset {
                    // In current chunk
                    input_f64[global_idx_u - self.input_offset]
                } else {
                    // In history
                    let back = self.input_offset - global_idx_u; // 1-based distance
                    if back <= hist_len {
                        self.history[hist_len - back]
                    } else {
                        continue; // older than history -- skip
                    }
                };

                let distance = (tap as f64 - frac).abs();
                let table_pos = distance * kernel.table_scale;
                let w = SINC.lookup(table_pos) as f64;
                accum += sample_val * w;
                weight_sum += w;
            }

            let sample = if weight_sum > 0.0 {
                accum / weight_sum
            } else {
                0.0
            };
            output.push(sample.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16);
            self.output_index += 1;
        }

        // Update history: keep the last `hist_len` samples
        if chunk_len >= hist_len {
            self.history
                .copy_from_slice(&input_f64[chunk_len - hist_len..]);
        } else {
            let shift = hist_len - chunk_len;
            self.history.copy_within(chunk_len..hist_len, 0);
            self.history[shift..].copy_from_slice(&input_f64);
        }

        self.input_offset += chunk_len;
        output
    }

    /// Emit any deferred output samples and reset state for reuse.
    ///
    /// After the last chunk there may be output samples whose right-wing
    /// taps extend past the signal end.  Those taps are skipped (matching
    /// the batch resampler's edge behaviour).
    pub fn flush(&mut self) -> Vec<i16> {
        if self.from_rate == self.to_rate {
            self.history.fill(0.0);
            self.input_offset = 0;
            self.output_index = 0;
            return Vec::new();
        }

        let signal_end = self.input_offset as isize;
        let hist_len = self.history.len();
        let kernel = self.kernel;
        let mut output = Vec::new();

        loop {
            let center = self.output_index as f64 * self.ratio;

            // Stop when center is at or past signal end (same as batch)
            if center >= signal_end as f64 {
                break;
            }

            let center_floor = center.floor() as isize;
            let frac = center - center_floor as f64;

            let mut accum: f64 = 0.0;
            let mut weight_sum: f64 = 0.0;

            for tap in -kernel.half_taps..=kernel.half_taps {
                let global_idx = center_floor + tap;

                if global_idx < 0 || global_idx >= signal_end {
                    continue; // outside signal -> skip (matches batch)
                }

                let global_idx_u = global_idx as usize;
                let back = self.input_offset - global_idx_u;
                if back <= hist_len {
                    let sample_val = self.history[hist_len - back];
                    let distance = (tap as f64 - frac).abs();
                    let table_pos = distance * kernel.table_scale;
                    let w = SINC.lookup(table_pos) as f64;
                    accum += sample_val * w;
                    weight_sum += w;
                }
                // else: beyond history reach -- skip
            }

            let sample = if weight_sum > 0.0 {
                accum / weight_sum
            } else {
                0.0
            };
            output.push(sample.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16);
            self.output_index += 1;
        }

        self.history.fill(0.0);
        self.input_offset = 0;
        self.output_index = 0;
        output
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "../tests/resample.rs"]
mod tests;
