//! ITU-T G.711 audio codecs (µ-law and A-law) for PSTN / telephony streams.
//!
//! Pure safe Rust (`#![forbid(unsafe_code)]`) with constant-time decoding and
//! standard ITU-T G.711 companding curves. No vendor-specific code.

/// Bias added during µ-law companding (ITU-T G.711).
const MULAW_BIAS: i32 = 0x84;
/// Maximum linear magnitude before clipping in µ-law encoding.
const MULAW_CLIP: i32 = 32_635;

/// Decode a single ITU-T G.711 µ-law byte into a signed 16-bit PCM sample.
#[inline]
pub const fn mulaw_byte_to_pcm16(byte: u8) -> i16 {
    let complemented = !byte;
    let sign = complemented & 0x80;
    let exponent = ((complemented >> 4) & 0x07) as i32;
    let mantissa = (complemented & 0x0F) as i32;
    let magnitude = (((mantissa << 3) + MULAW_BIAS) << exponent) - MULAW_BIAS;
    if sign != 0 {
        -(magnitude as i16)
    } else {
        magnitude as i16
    }
}

/// Encode a single signed 16-bit PCM sample into an ITU-T G.711 µ-law byte.
#[inline]
pub const fn pcm16_to_mulaw_byte(sample: i16) -> u8 {
    let s = sample as i32;
    let (sign, mut mag) = if s < 0 {
        (0x80u8, (-s).saturating_add(0))
    } else {
        (0x00u8, s)
    };
    if mag > MULAW_CLIP {
        mag = MULAW_CLIP;
    }
    mag += MULAW_BIAS;

    let mut exponent: u8 = 7;
    let mut mask = 0x4000;
    while exponent > 0 && (mag & mask) == 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = ((mag >> (exponent + 3)) & 0x0F) as u8;
    !(sign | (exponent << 4) | mantissa)
}

/// Decode a slice of ITU-T G.711 µ-law bytes into signed 16-bit PCM samples.
pub fn mulaw_to_pcm16(input: &[u8]) -> Vec<i16> {
    input.iter().map(|&b| mulaw_byte_to_pcm16(b)).collect()
}

/// Encode a slice of signed 16-bit PCM samples into ITU-T G.711 µ-law bytes.
pub fn pcm16_to_mulaw(input: &[i16]) -> Vec<u8> {
    input.iter().map(|&s| pcm16_to_mulaw_byte(s)).collect()
}

/// Decode a single ITU-T G.711 A-law byte into a signed 16-bit PCM sample.
#[inline]
pub const fn alaw_byte_to_pcm16(byte: u8) -> i16 {
    let val = byte ^ 0x55;
    let sign = val & 0x80;
    let exponent = ((val >> 4) & 0x07) as i32;
    let mantissa = (val & 0x0F) as i32;
    let magnitude = if exponent == 0 {
        (mantissa << 4) + 8
    } else {
        ((mantissa << 4) + 0x108) << (exponent - 1)
    };
    if sign != 0 {
        magnitude as i16
    } else {
        -(magnitude as i16)
    }
}

/// Encode a single signed 16-bit PCM sample into an ITU-T G.711 A-law byte.
#[inline]
pub const fn pcm16_to_alaw_byte(sample: i16) -> u8 {
    let s = sample as i32;
    let (sign, mut mag) = if s >= 0 { (0x80u8, s) } else { (0x00u8, -s) };
    if mag > 32_767 {
        mag = 32_767;
    }

    let (exponent, mantissa) = if mag < 256 {
        (0u8, (mag >> 4) as u8)
    } else {
        let mut exp: u8 = 7;
        let mut mask = 0x4000;
        while exp > 1 && (mag & mask) == 0 {
            exp -= 1;
            mask >>= 1;
        }
        let man = ((mag >> (exp + 3)) & 0x0F) as u8;
        (exp, man)
    };

    (sign | (exponent << 4) | mantissa) ^ 0x55
}

/// Decode a slice of ITU-T G.711 A-law bytes into signed 16-bit PCM samples.
pub fn alaw_to_pcm16(input: &[u8]) -> Vec<i16> {
    input.iter().map(|&b| alaw_byte_to_pcm16(b)).collect()
}

/// Encode a slice of signed 16-bit PCM samples into ITU-T G.711 A-law bytes.
pub fn pcm16_to_alaw(input: &[i16]) -> Vec<u8> {
    input.iter().map(|&s| pcm16_to_alaw_byte(s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mulaw_silence_roundtrip() {
        // ITU-T G.711 µ-law positive zero is 0xFF.
        assert_eq!(pcm16_to_mulaw_byte(0), 0xFF);
        assert_eq!(mulaw_byte_to_pcm16(0xFF), 0);
        // Negative zero is 0x7F -> decodes to 0.
        assert_eq!(mulaw_byte_to_pcm16(0x7F), 0);
    }

    #[test]
    fn mulaw_sine_wave_high_snr() {
        // 1 kHz sine wave at 8 kHz sample rate (160 samples = 20 ms).
        let original: Vec<i16> = (0..160)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * 1000.0 * (i as f64) / 8000.0;
                (phase.sin() * 16_000.0) as i16
            })
            .collect();
        let encoded = pcm16_to_mulaw(&original);
        assert_eq!(encoded.len(), 160);
        let decoded = mulaw_to_pcm16(&encoded);

        let signal_power: f64 = original.iter().map(|&s| (s as f64).powi(2)).sum();
        let noise_power: f64 = original
            .iter()
            .zip(decoded.iter())
            .map(|(&a, &b)| ((a as f64) - (b as f64)).powi(2))
            .sum();
        let snr_db = 10.0 * (signal_power / noise_power.max(1.0)).log10();
        assert!(
            snr_db > 35.0,
            "expected µ-law SNR > 35 dB, got {snr_db:.2} dB"
        );
    }

    #[test]
    fn alaw_sine_wave_high_snr() {
        let original: Vec<i16> = (0..160)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * 1000.0 * (i as f64) / 8000.0;
                (phase.sin() * 16_000.0) as i16
            })
            .collect();
        let encoded = pcm16_to_alaw(&original);
        assert_eq!(encoded.len(), 160);
        let decoded = alaw_to_pcm16(&encoded);

        let signal_power: f64 = original.iter().map(|&s| (s as f64).powi(2)).sum();
        let noise_power: f64 = original
            .iter()
            .zip(decoded.iter())
            .map(|(&a, &b)| ((a as f64) - (b as f64)).powi(2))
            .sum();
        let snr_db = 10.0 * (signal_power / noise_power.max(1.0)).log10();
        assert!(
            snr_db > 35.0,
            "expected A-law SNR > 35 dB, got {snr_db:.2} dB"
        );
    }

    #[test]
    fn extrema_do_not_panic_or_overflow() {
        for sample in [i16::MIN, i16::MIN + 1, -1, 0, 1, i16::MAX] {
            let mu = pcm16_to_mulaw_byte(sample);
            let _ = mulaw_byte_to_pcm16(mu);
            let al = pcm16_to_alaw_byte(sample);
            let _ = alaw_byte_to_pcm16(al);
        }
    }
}
