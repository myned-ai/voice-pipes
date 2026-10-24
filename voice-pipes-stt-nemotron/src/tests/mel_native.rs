use super::*;

// The filterbank is computed once per params and cached, not rebuilt
// every chunk (the FFT plan is cached by the shared front end).
#[test]
fn the_filterbank_is_reused() {
    let fb1 = cached_filterbank(512, 128, 16_000, 0.0, 8000.0);
    let fb2 = cached_filterbank(512, 128, 16_000, 0.0, 8000.0);
    assert!(
        std::sync::Arc::ptr_eq(&fb1, &fb2),
        "filterbank should be cached"
    );
}

#[test]
fn test_log_mel_spectrogram_sine() {
    let sr = 16_000;
    let freq = 440.0;
    let n_samples = sr;
    let waveform: Vec<f32> = (0..n_samples)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin())
        .collect();
    let mel = log_mel_spectrogram(&waveform, sr, 512, 400, 160, 128, 0.0, 8000.0);
    assert_eq!(mel.len(), 128);
    assert!(!mel[0].is_empty());
    for bin in &mel {
        for v in bin {
            assert!(v.is_finite());
        }
    }
}

#[test]
fn test_preemphasis_first_sample_unchanged() {
    let x = vec![1.0, 0.5, 0.25];
    let y = preemphasis(&x, 0.97);
    assert_eq!(y[0], 1.0);
    assert!((y[1] - (0.5 - 0.97 * 1.0)).abs() < 1e-6);
}

#[test]
fn test_reflect_pad() {
    let x = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    let p = reflect_pad(&x, 2);
    assert_eq!(p, vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0]);
}

/// The trained Nemotron encoder expects RAW log-mel (per `model_config.yaml`'s
/// `normalize: NA`, `pad_to: 0`). Speech content has mean ~ -12 and std ~ 3.4.
/// If this test fails, the default path has likely been re-normalized,
/// which will silently break transcription quality (gibberish output).
#[test]
fn test_raw_mel_distribution_unnormalized() {
    // Synthetic broadband-ish signal (mix of sines) for stable stats
    let sr = 16_000;
    let n = sr; // 1 s
    let waveform: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / sr as f32;
            0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 880.0 * t).sin()
                + 0.1 * (2.0 * std::f32::consts::PI * 1760.0 * t).sin()
        })
        .collect();
    let mel =
        log_mel_spectrogram_opts(&waveform, sr, 512, 400, 160, 128, 0.0, 8000.0, false, false);
    let total: f32 = mel.iter().flatten().sum();
    let count = (mel.len() * mel[0].len()) as f32;
    let mean = total / count;
    // Raw log-mel (with LOG_ZERO_GUARD = 2^-24 ~ -16.6 floor) on speech-like
    // content sits well below 0. If someone re-enables normalization the
    // mean snaps to ~0 and this assertion blows up.
    assert!(
        mean < -3.0,
        "expected raw log-mel mean << 0, got {mean} \u{2014} normalization may have been re-enabled by default"
    );
}

/// A voiced test signal: a modulated tone with a little noise.
fn voiced(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            0.3 * (t * 220.0 * std::f32::consts::TAU).sin() * (1.0 + (t * 3.0).sin())
                + 0.01 * ((i * 7919 % 97) as f32 / 97.0 - 0.5)
        })
        .collect()
}

fn whole(audio: &[f32]) -> Vec<Vec<f32>> {
    log_mel_spectrogram_opts(
        audio, 16_000, 512, 400, 160, 128, 0.0, 8_000.0, false, false,
    )
}

fn part(raw: &[f32], left: usize, ends: bool) -> Vec<Vec<f32>> {
    utterance_log_mel(raw, left, ends, 16_000, 512, 400, 160, 128, 0.0, 8_000.0)
}

/// Frames computed from a stretch of the utterance, with its left context,
/// equal the whole-utterance spectrogram at the same centres: at the start,
/// in the middle, and at the end. Computing each stretch alone (the old
/// streaming path) put up to 12 log units of error in its first frame.
#[test]
fn a_stretch_with_its_context_matches_the_whole_utterance() {
    let audio = voiced(48_000);
    let reference = whole(&audio);
    let check = |got: &[Vec<f32>], first_frame: usize| {
        for (bin, row) in got.iter().enumerate() {
            for (t, v) in row.iter().enumerate() {
                let want = reference[bin][first_frame + t];
                assert!(
                    (v - want).abs() < 1e-3,
                    "bin {bin} frame {}: {v} vs {want}",
                    first_frame + t
                );
            }
        }
    };
    // The start: no left context, reflect-padded as offline.
    check(&part(&audio[..20_000], 0, false), 0);
    // The middle: centres from sample 17 920 (frame 112), with context.
    let start = 112 * 160;
    let left = 257;
    let mid = part(&audio[start - left..start + 20_000], left, false);
    assert!(mid[0].len() > 100, "{} frames", mid[0].len());
    check(&mid, 112);
    // The end: the last frames reflect-padded where the audio ends.
    let tail_start = 280 * 160;
    let end = part(&audio[tail_start - left..], left, true);
    assert_eq!(
        280 + end[0].len(),
        reference[0].len(),
        "the same last frame"
    );
    check(&end, 280);
}

/// The front end against numbers from outside this crate: the reference
/// `NeMo` extractor's output for two fixture recordings, kept by the
/// Nemotron 3.5 crate, whose front end is the same preprocessor (0.97
/// pre-emphasis, a 400-point Hann in a 512-point frame at a 160 hop, a
/// Slaney 128-band filterbank, ln with a 2^-24 guard, no normalisation).
/// Log-mel values span about [-17, 1]; 1e-3 is far below anything the
/// encoder could tell apart and far above f32 accumulation noise.
#[test]
fn the_front_end_matches_the_reference_extractor() {
    for name in ["hello-how-are-you", "quiet-speech"] {
        let wav = format!(
            "{}/../voice-pipes/fixtures/{name}.wav",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut reader = hound::WavReader::open(&wav).expect("fixture wav");
        let pcm: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| f32::from(s.expect("sample")) / 32768.0)
            .collect();
        let data = format!(
            "{}/../voice-pipes-stt-nemotron35/tests/data/{name}.mel128.bin",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&data).expect("reference features");
        let frames = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let want: Vec<f32> = bytes[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let got =
            log_mel_spectrogram_opts(&pcm, 16_000, 512, 400, 160, 128, 0.0, 8_000.0, false, false);
        assert_eq!(got[0].len(), frames, "{name}: frame count");
        for t in 0..frames {
            for (m, row) in got.iter().enumerate() {
                let (g, w) = (row[t], want[t * 128 + m]);
                assert!(
                    (g - w).abs() < 1e-3,
                    "{name}: frame {t} mel {m}: {g} vs {w}"
                );
            }
        }
    }
}
