//! The front-end must produce the encoder's training-time features, which
//! means the reference extractor's output to the last bit that matters.
//!
//! `tests/data/*.mel128.bin` are the reference engine's `LogMelExtractor`
//! run over two fixtures from `voice-pipes/fixtures` (a spoken sentence at a
//! normal level; one near the detection floor, where the log's zero guard
//! matters): a `u32` frame count, a `u32` mel count, then little-endian f32
//! row-major. Regenerate with the reference engine if the fixtures change.

use voice_pipes_stt_nemotron35::features::{LogMelExtractor, N_MELS};

fn fixture_pcm(name: &str) -> Vec<f32> {
    let path = format!(
        "{}/../voice-pipes/fixtures/{name}.wav",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut r = hound::WavReader::open(&path).expect("fixture wav");
    assert_eq!(r.spec().sample_rate, 16_000);
    assert_eq!(r.spec().channels, 1);
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}

fn reference(name: &str) -> (usize, Vec<f32>) {
    let path = format!(
        "{}/tests/data/{name}.mel128.bin",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).expect("reference features");
    let frames = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mels = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    assert_eq!(mels, N_MELS);
    let data = bytes[8..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect::<Vec<_>>();
    assert_eq!(data.len(), frames * mels);
    (frames, data)
}

fn assert_matches_reference(name: &str) {
    let (want_frames, want) = reference(name);
    let (got_frames, got) = LogMelExtractor::new().extract(&fixture_pcm(name), true);
    assert_eq!(got_frames, want_frames, "{name}: frame count");
    let mut worst = 0f32;
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        let d = (g - w).abs();
        if d > worst {
            worst = d;
        }
        // Log-mel values span about [-17, 1]; 1e-3 is far below anything
        // the encoder could distinguish and far above f32 accumulation noise.
        assert!(
            d < 1e-3,
            "{name}: frame {} mel {} got {g} want {w}",
            i / N_MELS,
            i % N_MELS
        );
    }
    eprintln!("{name}: {got_frames} frames, max abs diff {worst:.2e}");
}

#[test]
fn a_spoken_sentence_matches_the_reference_extractor() {
    assert_matches_reference("hello-how-are-you");
}

#[test]
fn near_silent_speech_matches_too_so_the_zero_guard_is_right() {
    assert_matches_reference("quiet-speech");
}

#[test]
fn chunked_extraction_equals_one_pass_over_the_whole_utterance() {
    // What streaming relies on: frames from a chunk handed `n_fft/2` samples
    // of history and extracted without centring are the frames a single
    // centred pass would have produced at those positions.
    use voice_pipes_stt_nemotron35::features::{HOP_LENGTH, N_FFT};
    let pcm = fixture_pcm("hello-how-are-you");
    let ex = LogMelExtractor::new();
    let (all_frames, all) = ex.extract(&pcm, true);
    // Frames 40..72 as a chunk: the centred pass's frame f starts at
    // f*hop - n_fft/2 in the unpadded signal.
    let (f0, f1) = (40usize, 72usize);
    let start = f0 * HOP_LENGTH - N_FFT / 2;
    let end = (f1 - 1) * HOP_LENGTH - N_FFT / 2 + N_FFT;
    let (n, chunk) = ex.extract(&pcm[start..end], false);
    assert_eq!(n, f1 - f0);
    assert!(f1 <= all_frames);
    for (i, (g, w)) in chunk.iter().zip(&all[f0 * N_MELS..f1 * N_MELS]).enumerate() {
        assert!(
            (g - w).abs() < 1e-4,
            "chunk frame {} mel {}: {g} vs {w}",
            i / N_MELS,
            i % N_MELS
        );
    }
}
