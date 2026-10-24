use super::*;

/// A wire-supplied frame with `channels = 0` (proto3 default)
/// must resample as mono, not divide by zero.
#[test]
fn zero_channel_frame_is_treated_as_mono() {
    let frame = AudioFrame {
        samples: vec![1000; 480],
        sample_rate: 48_000,
        channels: 0,
    };
    let out = resample(frame, 16_000).unwrap();
    assert_eq!(out.sample_rate, 16_000);
    assert!(
        (out.samples.len() as i64 - 160).abs() <= 1,
        "{}",
        out.samples.len()
    );
}

fn tone(freq_hz: f64, rate: u32, secs: f64, amp: f64) -> Vec<i16> {
    let n = (rate as f64 * secs) as usize;
    (0..n)
        .map(|i| {
            (amp * (2.0 * std::f64::consts::PI * freq_hz * i as f64 / rate as f64).sin()) as i16
        })
        .collect()
}

/// RMS of the middle 80 % (skips the kernel's edge taper).
fn rms_core(samples: &[i16]) -> f64 {
    let n = samples.len();
    let core = &samples[n / 10..n - n / 10];
    (core.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / core.len() as f64).sqrt()
}

/// Content above the *output* Nyquist must be rejected when downsampling:
/// a decimator has to attenuate a 10 kHz tone at 48 kHz by well over
/// 40 dB on its way to 16 kHz, instead of aliasing it into the speech band.
#[test]
fn downsampling_rejects_content_above_output_nyquist() {
    let input = tone(10_000.0, 48_000, 0.5, 10_000.0);
    let in_rms = rms_core(&input);
    let out = resample(
        AudioFrame {
            samples: input,
            sample_rate: 48_000,
            channels: 1,
        },
        16_000,
    )
    .unwrap();
    let db = 20.0 * (rms_core(&out.samples) / in_rms).log10();
    assert!(db < -40.0, "10 kHz leaked through 48->16 kHz at {db:.1} dB");

    // The f32 path (Silero front-end) uses the same kernel.
    let input_f: Vec<f32> = tone(10_000.0, 48_000, 0.5, 10_000.0)
        .into_iter()
        .map(|s| s as f32 / 32768.0)
        .collect();
    let out_f = resample_f32(&input_f, 48_000, 16_000).unwrap();
    let n = out_f.len();
    let core = &out_f[n / 10..n - n / 10];
    let rms_f = (core.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / core.len() as f64).sqrt();
    let db_f = 20.0 * (rms_f / (10_000.0 / 32768.0 / 2f64.sqrt())).log10();
    assert!(db_f < -40.0, "f32 path leaked 10 kHz at {db_f:.1} dB");
}

/// The pass-band is untouched by the cutoff scaling.
#[test]
fn downsampling_keeps_the_pass_band_flat() {
    for freq in [300.0, 1_000.0, 3_000.0] {
        let input = tone(freq, 48_000, 0.5, 10_000.0);
        let in_rms = rms_core(&input);
        let out = resample(
            AudioFrame {
                samples: input,
                sample_rate: 48_000,
                channels: 1,
            },
            16_000,
        )
        .unwrap();
        let db = 20.0 * (rms_core(&out.samples) / in_rms).log10();
        assert!(
            db.abs() < 0.5,
            "{freq} Hz changed by {db:.2} dB across 48->16 kHz"
        );
    }
}

/// The streaming path must match the batch path for the stretched kernel
/// too, including the smallest chunks (where a too-short history would
/// drop the outermost tap).
#[test]
fn streaming_matches_batch_for_48k_to_16k_at_tiny_chunks() {
    let input = tone(1_000.0, 48_000, 0.25, 8_000.0);
    let batch = resample(
        AudioFrame {
            samples: input.clone(),
            sample_rate: 48_000,
            channels: 1,
        },
        16_000,
    )
    .unwrap()
    .samples;
    for chunk in [1usize, 7, 160, 1024] {
        let mut r = StreamingResampler::new(48_000, 16_000).unwrap();
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            out.extend(r.process_chunk(c));
        }
        out.extend(r.flush());
        assert_eq!(out.len(), batch.len(), "length differs for chunk {chunk}");
        let worst = out
            .iter()
            .zip(&batch)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 1,
            "chunk {chunk}: streaming differs from batch by {worst} LSB"
        );
    }
}

#[test]
fn identity_passthrough() {
    let frame = AudioFrame {
        samples: vec![100, -200, 300],
        sample_rate: 16000,
        channels: 1,
    };
    let out = resample(frame.clone(), 16000).unwrap();
    assert_eq!(out.samples, frame.samples);
    assert_eq!(out.sample_rate, 16000);
}

#[test]
fn empty_frame() {
    let frame = AudioFrame {
        samples: vec![],
        sample_rate: 44100,
        channels: 1,
    };
    let out = resample(frame, 16000).unwrap();
    assert!(out.samples.is_empty());
}

#[test]
fn downsample_22050_to_16000() {
    // 1 second of silence with a spike in the middle
    let n = 22050;
    let mut samples = vec![0i16; n];
    samples[n / 2] = 10000;
    let frame = AudioFrame {
        samples,
        sample_rate: 22050,
        channels: 1,
    };
    let out = resample(frame, 16000).unwrap();
    assert_eq!(out.sample_rate, 16000);
    // Output length should be approximately 16000
    assert!((out.samples.len() as i32 - 16000).unsigned_abs() <= 1);
    // Spike energy should be preserved (somewhere near the middle)
    let max_abs = out.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(max_abs > 1000, "spike energy should be preserved");
}

#[test]
fn upsample_16000_to_22050() {
    let n = 16000;
    let mut samples = vec![0i16; n];
    samples[n / 2] = 10000;
    let frame = AudioFrame {
        samples,
        sample_rate: 16000,
        channels: 1,
    };
    let out = resample(frame, 22050).unwrap();
    assert_eq!(out.sample_rate, 22050);
    assert!((out.samples.len() as i32 - 22050).unsigned_abs() <= 1);
    let max_abs = out.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(max_abs > 1000, "spike energy should be preserved");
}

#[test]
fn stereo_resample() {
    // 100 frames of stereo at 44100 -> 22050 should give ~50 frames
    let frames_in = 100;
    let mut samples = Vec::with_capacity(frames_in * 2);
    for i in 0..frames_in {
        samples.push(i as i16 * 100); // left
        samples.push(-(i as i16) * 100); // right
    }
    let frame = AudioFrame {
        samples,
        sample_rate: 44100,
        channels: 2,
    };
    let out = resample(frame, 22050).unwrap();
    assert_eq!(out.sample_rate, 22050);
    assert_eq!(out.channels, 2);
    assert_eq!(out.samples.len() % 2, 0);
    let frames_out = out.samples.len() / 2;
    assert!((frames_out as i32 - 50).unsigned_abs() <= 1);
}

#[test]
fn resample_f32_identity() {
    let input = vec![0.5, -0.5, 0.25];
    let out = resample_f32(&input, 16000, 16000).unwrap();
    assert_eq!(out, input);
}

#[test]
fn resample_f32_downsample() {
    // 1 second at 22050 -> 16000
    let n = 22050;
    let input: Vec<f32> = (0..n).map(|i| i as f32 / n as f32 * 2.0 - 1.0).collect();
    let out = resample_f32(&input, 22050, 16000).unwrap();
    assert!((out.len() as i32 - 16000).unsigned_abs() <= 1);
}

#[test]
fn sinc_table_center_is_one() {
    assert!((SINC.table[0] - 1.0).abs() < 1e-6);
}

#[test]
fn sinc_table_decays() {
    // Each zero-crossing should have lower amplitude
    for z in 1..SINC_ZEROS {
        let pos = z * TABLE_OVERSAMPLING;
        assert!(
            SINC.table[pos].abs() < 0.01,
            "sinc should be near zero at zero-crossing {z}"
        );
    }
}

#[test]
fn dc_preservation() {
    // Constant signal should stay constant after resampling
    let frame = AudioFrame {
        samples: vec![1000i16; 22050],
        sample_rate: 22050,
        channels: 1,
    };
    let out = resample(frame, 16000).unwrap();
    // All samples in the middle should be very close to 1000
    let mid = &out.samples[100..out.samples.len() - 100];
    for &s in mid {
        assert!(
            (s - 1000).unsigned_abs() <= 1,
            "DC should be preserved, got {s}"
        );
    }
}

// ── StreamingResampler tests ────────────────────────────

/// A streaming resampler's output, filter tail included, is the same
/// sample for sample however its input is chunked, down and up.
#[test]
fn streaming_output_does_not_depend_on_the_chunking() {
    for (from, to) in [(48_000, 16_000), (16_000, 24_000), (22_050, 16_000)] {
        let input = tone(700.0, from, 0.01, 9_000.0);
        let out = crate::testing::chunking_invariant(&input, |chunks| {
            let mut r = StreamingResampler::new(from, to).unwrap();
            let mut out = Vec::new();
            for chunk in chunks {
                out.extend(r.process_chunk(chunk));
            }
            out.extend(r.flush());
            out
        });
        assert!(!out.is_empty(), "{from} -> {to}");
    }
}

#[test]
fn streaming_passthrough() {
    let mut sr = StreamingResampler::new(16000, 16000).unwrap();
    assert!(sr.is_passthrough());
    let input: Vec<i16> = (0..320).map(|i| (i * 10) as i16).collect();
    let out = sr.process_chunk(&input);
    assert_eq!(out, input);
}

#[test]
fn streaming_vs_batch_downsample() {
    // Compare: batch resample 1 second from 22050->16000
    // vs. StreamingResampler in 20ms chunks.
    let n = 22050;
    let samples: Vec<i16> = (0..n)
        .map(|i| {
            // 440 Hz sine wave
            let t = i as f64 / 22050.0;
            (10000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
        })
        .collect();

    // Batch
    let frame = AudioFrame {
        samples: samples.clone(),
        sample_rate: 22050,
        channels: 1,
    };
    let batch_out = resample(frame, 16000).unwrap();

    // Streaming in 20ms chunks (441 samples @ 22050 Hz)
    let chunk_size = 441;
    let mut sr = StreamingResampler::new(22050, 16000).unwrap();
    let mut streaming_out = Vec::new();
    for chunk in samples.chunks(chunk_size) {
        streaming_out.extend_from_slice(&sr.process_chunk(chunk));
    }
    streaming_out.extend_from_slice(&sr.flush());

    // Lengths should match within 1 sample
    let len_diff = (batch_out.samples.len() as isize - streaming_out.len() as isize).unsigned_abs();
    assert!(
        len_diff <= 1,
        "length mismatch: batch={} streaming={}",
        batch_out.samples.len(),
        streaming_out.len()
    );

    // Compare sample values in the middle (skip edges where boundary effects may differ)
    let compare_len = batch_out.samples.len().min(streaming_out.len());
    let start = 200;
    let end = compare_len.saturating_sub(200);
    let mut max_diff: u16 = 0;
    for i in start..end {
        let diff = (batch_out.samples[i] as i32 - streaming_out[i] as i32).unsigned_abs() as u16;
        if diff > max_diff {
            max_diff = diff;
        }
    }
    assert!(
        max_diff <= 1,
        "streaming vs batch max sample diff = {max_diff} (should be <=1 LSB)"
    );
}

#[test]
fn streaming_vs_batch_upsample() {
    let n = 16000;
    let samples: Vec<i16> = (0..n)
        .map(|i| {
            let t = i as f64 / 16000.0;
            (10000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
        })
        .collect();

    let frame = AudioFrame {
        samples: samples.clone(),
        sample_rate: 16000,
        channels: 1,
    };
    let batch_out = resample(frame, 22050).unwrap();

    let chunk_size = 320; // 20ms @ 16kHz
    let mut sr = StreamingResampler::new(16000, 22050).unwrap();
    let mut streaming_out = Vec::new();
    for chunk in samples.chunks(chunk_size) {
        streaming_out.extend_from_slice(&sr.process_chunk(chunk));
    }
    streaming_out.extend_from_slice(&sr.flush());

    let len_diff = (batch_out.samples.len() as isize - streaming_out.len() as isize).unsigned_abs();
    assert!(
        len_diff <= 1,
        "length mismatch: batch={} streaming={}",
        batch_out.samples.len(),
        streaming_out.len()
    );

    let compare_len = batch_out.samples.len().min(streaming_out.len());
    let start = 200;
    let end = compare_len.saturating_sub(200);
    let mut max_diff: u16 = 0;
    for i in start..end {
        let diff = (batch_out.samples[i] as i32 - streaming_out[i] as i32).unsigned_abs() as u16;
        if diff > max_diff {
            max_diff = diff;
        }
    }
    assert!(
        max_diff <= 1,
        "streaming vs batch max sample diff = {max_diff} (should be <=1 LSB)"
    );
}

#[test]
fn streaming_various_chunk_sizes() {
    // Test with 10ms, 50ms, and 100ms chunks -- all should produce the same output
    let n = 16000; // 1 second @ 16kHz
    let samples: Vec<i16> = (0..n)
        .map(|i| {
            let t = i as f64 / 16000.0;
            (8000.0 * (2.0 * std::f64::consts::PI * 300.0 * t).sin()) as i16
        })
        .collect();

    let target = 22050;
    let chunk_sizes = [160, 800, 1600]; // 10ms, 50ms, 100ms @ 16kHz
    let mut results = Vec::new();

    for &cs in &chunk_sizes {
        let mut sr = StreamingResampler::new(16000, target).unwrap();
        let mut out = Vec::new();
        for chunk in samples.chunks(cs) {
            out.extend_from_slice(&sr.process_chunk(chunk));
        }
        out.extend_from_slice(&sr.flush());
        results.push(out);
    }

    // All chunk sizes should produce same length (within 1)
    for (i, r) in results.iter().enumerate() {
        let len_diff = (results[0].len() as isize - r.len() as isize).unsigned_abs();
        assert!(
            len_diff <= 1,
            "chunk_size {} produced len {} vs {} for chunk_size {}",
            chunk_sizes[i],
            r.len(),
            results[0].len(),
            chunk_sizes[0]
        );
    }

    // And same sample values in the middle
    let compare_len = results.iter().map(std::vec::Vec::len).min().unwrap();
    let start = 200;
    let end = compare_len.saturating_sub(200);
    for i in 1..results.len() {
        let mut max_diff: u16 = 0;
        for j in start..end {
            let diff = (results[0][j] as i32 - results[i][j] as i32).unsigned_abs() as u16;
            if diff > max_diff {
                max_diff = diff;
            }
        }
        assert!(
            max_diff <= 1,
            "chunk_size {} vs {} max diff = {max_diff}",
            chunk_sizes[0],
            chunk_sizes[i]
        );
    }
}

#[test]
fn streaming_single_sample_chunks() {
    // Extreme edge case: feed one sample at a time
    let n = 1000;
    let samples: Vec<i16> = (0..n)
        .map(|i| {
            let t = i as f64 / 16000.0;
            (5000.0 * (2.0 * std::f64::consts::PI * 200.0 * t).sin()) as i16
        })
        .collect();

    let mut sr = StreamingResampler::new(16000, 22050).unwrap();
    let mut out = Vec::new();
    for s in &samples {
        out.extend_from_slice(&sr.process_chunk(&[*s]));
    }
    out.extend_from_slice(&sr.flush());

    // Should produce roughly n * 22050/16000 ~= 1378 samples
    let expected = (n as f64 * 22050.0 / 16000.0).ceil() as usize;
    let len_diff = (out.len() as isize - expected as isize).unsigned_abs();
    assert!(
        len_diff <= 2,
        "expected ~{expected} samples, got {}",
        out.len()
    );
}

#[test]
fn streaming_dc_preservation() {
    let mut sr = StreamingResampler::new(22050, 16000).unwrap();
    let chunk = vec![1000i16; 441]; // 20ms @ 22050
    let mut all_out = Vec::new();
    for _ in 0..50 {
        all_out.extend_from_slice(&sr.process_chunk(&chunk));
    }
    all_out.extend_from_slice(&sr.flush());

    // Middle samples should all be ~1000
    let mid = &all_out[100..all_out.len().saturating_sub(100)];
    for &s in mid {
        assert!(
            (s - 1000).unsigned_abs() <= 1,
            "DC should be preserved in streaming mode, got {s}"
        );
    }
}

#[test]
fn streaming_empty_chunk() {
    let mut sr = StreamingResampler::new(16000, 22050).unwrap();
    let out = sr.process_chunk(&[]);
    assert!(out.is_empty());
    // Should still work after empty chunk
    let out2 = sr.process_chunk(&[1000; 320]);
    assert!(!out2.is_empty());
}

/// The regression: a zero rate made the batch resamplers panic (from 0) or
/// return nothing (to 0), and the streaming one loop forever (from 0) or
/// panic (to 0). All three now refuse a rate outside the supported range, and
/// equal rates still pass through untouched.
#[test]
fn an_unsupported_rate_is_refused_not_a_panic_or_a_hang() {
    let frame = |rate| AudioFrame {
        samples: vec![1; 160],
        sample_rate: rate,
        channels: 1,
    };
    for (from, to) in [(0, 16_000), (16_000, 0), (1, 16_000), (16_000, 999_999)] {
        let refused = if SUPPORTED_RATES.contains(&from) {
            to
        } else {
            from
        };
        assert_eq!(
            resample(frame(from), to).unwrap_err(),
            ResampleError::UnsupportedRate { rate: refused },
            "batch {from} -> {to}"
        );
        assert_eq!(
            StreamingResampler::new(from, to).err(),
            Some(ResampleError::UnsupportedRate { rate: refused }),
            "stream {from} -> {to}"
        );
        assert_eq!(
            resample_f32(&[0.5; 160], from, to).err(),
            Some(ResampleError::UnsupportedRate { rate: refused }),
            "f32 {from} -> {to}"
        );
    }
    assert_eq!(resample_f32(&[0.5; 160], 0, 0).unwrap().len(), 160);
    assert_eq!(resample(frame(0), 0).unwrap().samples.len(), 160);
    assert!(StreamingResampler::new(0, 0).unwrap().is_passthrough());
}
