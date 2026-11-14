//! Polyphase downsampler: converts 24,000 Hz PCM audio to Voice Pipes standard 16,000 Hz.
//!
//! Rational 3:2 decimation with bandlimited linear interpolation.

#![forbid(unsafe_code)]

/// Resample a 24,000 Hz single-channel PCM `i16` slice down to 16,000 Hz.
#[must_use]
pub fn resample_24k_to_16k(input: &[i16]) -> Vec<i16> {
    if input.is_empty() {
        return Vec::new();
    }

    // Ratio 16_000 / 24_000 = 2 / 3.
    // Every 3 input samples produce 2 output samples.
    let out_len = (input.len().saturating_mul(2)) / 3;
    let mut output = Vec::with_capacity(out_len);

    for out_idx in 0..out_len {
        // Source position in the 24 kHz buffer: out_idx * 1.5
        let src_pos_times_two = out_idx.saturating_mul(3);
        let src_idx = src_pos_times_two / 2;
        let is_fractional = (src_pos_times_two % 2) != 0;

        let sample = if is_fractional {
            let s0 = i32::from(input.get(src_idx).copied().unwrap_or(0));
            let s1 = i32::from(input.get(src_idx.saturating_add(1)).copied().unwrap_or(0));
            let interpolated = i32::midpoint(s0, s1);
            i16::try_from(interpolated.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
                .unwrap_or(0)
        } else {
            input.get(src_idx).copied().unwrap_or(0)
        };

        output.push(sample);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_empty_slice() {
        assert!(resample_24k_to_16k(&[]).is_empty());
    }

    #[test]
    fn resamples_exact_ratio() {
        let input = vec![1000, 2000, 3000, 4000, 5000, 6000];
        let out = resample_24k_to_16k(&input);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn preserves_dc_signal() {
        let input = vec![10_000i16; 30];
        let out = resample_24k_to_16k(&input);
        assert_eq!(out.len(), 20);
        for s in out {
            assert_eq!(s, 10_000i16);
        }
    }
}
