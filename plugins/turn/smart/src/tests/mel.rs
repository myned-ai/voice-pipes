use super::*;

/// Deterministic 8 s pure-sine input, reproducible bit-for-bit in Python
/// (float64 math cast to f32) so the reference below is exact.
fn deterministic_audio() -> Vec<f32> {
    (0..WINDOW_SAMPLES)
        .map(|n| {
            let t = n as f64 / SAMPLE_RATE as f64;
            let v = 0.3 * (2.0 * std::f64::consts::PI * 180.0 * t).sin()
                + 0.2 * (2.0 * std::f64::consts::PI * 450.0 * t).sin()
                + 0.1 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin();
            v.clamp(-1.0, 1.0) as f32
        })
        .collect()
}

#[test]
fn matches_whisper_feature_extractor_reference() {
    // Reference from transformers.WhisperFeatureExtractor(feature_size=80,
    // hop_length=160, chunk_length=8, n_fft=400) on `deterministic_audio()`.
    // If this fails, the mel front-end diverged from what the model expects
    // and Smart Turn predictions will be garbage — do NOT loosen blindly.
    let feat = log_mel_features(&deterministic_audio());
    assert_eq!(feat.len(), N_MELS * N_FRAMES);
    let at = |m: usize, fr: usize| feat[m * N_FRAMES + fr];
    // (mel_bin, frame) -> expected
    let refs = [
        (0usize, 0usize, 1.13891f32),
        (0, 1, 0.58272),
        (1, 0, 1.17223),
        (10, 100, 1.10591),
        (40, 400, -0.69809),
        (79, 799, -0.56641),
        (79, 700, -0.69809),
    ];
    for (m, fr, want) in refs {
        let got = at(m, fr);
        assert!(
            (got - want).abs() < 0.02,
            "mel[{m}][{fr}] = {got:.5}, expected {want:.5} (Whisper reference)"
        );
    }
    // Global stats (reference: min -0.69809, max 1.30191, mean -0.28318).
    let min = feat.iter().copied().fold(f32::INFINITY, f32::min);
    let max = feat.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mean = feat.iter().sum::<f32>() / feat.len() as f32;
    assert!((min - -0.69809).abs() < 0.02, "min {min:.5}");
    assert!((max - 1.30191).abs() < 0.02, "max {max:.5}");
    assert!((mean - -0.28318).abs() < 0.02, "mean {mean:.5}");
}

#[test]
fn short_input_is_right_padded_not_crash() {
    // Half a second of audio must still produce a full [80,800] feature map.
    let feat = log_mel_features(&vec![0.01f32; SAMPLE_RATE / 2]);
    assert_eq!(feat.len(), N_MELS * N_FRAMES);
    assert!(feat.iter().all(|v| v.is_finite()));
}
