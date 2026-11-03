//! Minimal offline Nemotron transcribe — loads sessions + WAV and calls
//! `transcribe_buffered` directly. Useful for A/B testing the
//! `NEMOTRON_NORMALIZED_MEL=1` legacy opt-in against the default raw mel path.
//!
//! Build / usage:
//!
//! ```text
//! cargo build --release -p voice-pipes-stt-nemotron --example offline_transcribe
//! cargo run --release -p voice-pipes-stt-nemotron --example offline_transcribe -- \
//!     <model_dir> <input.wav>
//! ```
//!
//! Toggle preprocessing:
//!
//! ```text
//! (default)                          # raw log-mel, native, mel.onnx bypassed
//! $env:NEMOTRON_NORMALIZED_MEL = "1" # legacy normalized+padded path (uses mel.onnx if present)
//! ```

use std::env;
use std::path::Path;

use voice_pipes_stt_nemotron::streaming::{NemotronSessions, load_metadata, transcribe_buffered};
use voice_pipes_stt_nemotron::vocab::{detokenize, load_vocab};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: nemotron_offline_transcribe <model_dir> <input.wav>");
        std::process::exit(2);
    }
    let model_dir = Path::new(&args[1]);
    let wav_path = Path::new(&args[2]);

    let normalized = env::var("NEMOTRON_NORMALIZED_MEL").unwrap_or_default();
    eprintln!("=== nemotron_offline_transcribe ===");
    eprintln!("  model_dir              = {}", model_dir.display());
    eprintln!("  wav                    = {}", wav_path.display());
    eprintln!("  NEMOTRON_NORMALIZED_MEL = {normalized:?}");

    let mut reader = hound::WavReader::open(wav_path)?;
    let spec = reader.spec();
    eprintln!(
        "  wav spec: sr={} channels={} bits={} format={:?}",
        spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
    );
    if spec.sample_rate != 16_000 {
        return Err(format!("expected 16 kHz WAV, got {} Hz", spec.sample_rate).into());
    }
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
        (hound::SampleFormat::Float, 32) => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        other => return Err(format!("unsupported WAV format {other:?}").into()),
    };
    let mono: Vec<f32> = if spec.channels == 1 {
        samples
    } else {
        let ch = spec.channels as usize;
        samples
            .chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    eprintln!(
        "  samples         = {} ({:.2} s)",
        mono.len(),
        mono.len() as f32 / 16000.0
    );

    eprintln!("loading metadata + sessions");
    let metadata = load_metadata(model_dir)?;
    let mut sessions = NemotronSessions::load(model_dir, 4)?;
    eprintln!(
        "  mel.onnx loaded = {}  (will be {} — default is RAW native mel)",
        sessions.mel.is_some(),
        if normalized == "1" {
            "USED (legacy normalized path)"
        } else {
            "BYPASSED"
        }
    );

    eprintln!("loading vocab");
    let vocab = load_vocab(model_dir)?;
    eprintln!("  vocab entries   = {}", vocab.len());

    let blank_id = 1024_i32;
    let max_tokens_per_step = 8_usize;

    let t0 = std::time::Instant::now();
    let tokens = transcribe_buffered(
        &mut sessions,
        &metadata,
        &mono,
        blank_id,
        max_tokens_per_step,
    )?;
    let dt = t0.elapsed();

    let text = detokenize(&vocab, &tokens);
    println!("\n=== transcript ===");
    println!("tokens ({}): {:?}", tokens.len(), tokens);
    println!("text: {text:?}");
    println!("elapsed: {dt:?}");

    Ok(())
}
