//! Dump our log-mel spectrogram for a WAV file to a tiny binary format so a
//! Python reference (librosa with `NeMo` defaults) can compare bin-by-bin.
//!
//! File format (little-endian):
//!
//! ```text
//!   u32 n_mels
//!   u32 n_frames
//!   u32 sample_rate
//!   u32 n_fft
//!   u32 win_length
//!   u32 hop_length
//!   f32[n_mels * n_frames]  row-major: mel[m][t] then mel[m][t+1] ...
//! ```
//!
//! Build / usage:
//!
//! ```text
//! cargo build --release -p voice-pipes-stt-nemotron --example dump_mel_features
//! cargo run   --release -p voice-pipes-stt-nemotron --example dump_mel_features -- \
//!                       <input.wav> <output.melbin>
//! ```

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use voice_pipes_stt_nemotron::mel_native::log_mel_spectrogram;
use voice_pipes_stt_nemotron::streaming::{NemotronSessions, onnx_mel_features};

const SAMPLE_RATE: usize = 16_000;
const N_FFT: usize = 512;
const WIN_LENGTH: usize = 400;
const HOP_LENGTH: usize = 160;
const N_MELS: usize = 128;
const FMIN: f32 = 0.0;
const FMAX: f32 = 8_000.0;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 && args.len() != 4 {
        eprintln!("usage: dump_mel_features <input.wav> <output.melbin> [model_dir_for_mel_onnx]");
        std::process::exit(2);
    }
    let input_path = Path::new(&args[1]);
    let output_path = Path::new(&args[2]);
    let model_dir = args.get(3).map(Path::new);

    // Read WAV, convert to f32 mono at 16 kHz.
    let mut reader = hound::WavReader::open(input_path)?;
    let spec = reader.spec();
    eprintln!(
        "reading {}: sr={} channels={} bits={} format={:?}",
        input_path.display(),
        spec.sample_rate,
        spec.channels,
        spec.bits_per_sample,
        spec.sample_format
    );

    let raw_samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.unwrap() as f32 / 2_147_483_648.0)
            .collect(),
        (hound::SampleFormat::Float, 32) => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        other => return Err(format!("unsupported WAV format {other:?}").into()),
    };

    // Mix to mono if multichannel.
    let mono: Vec<f32> = if spec.channels == 1 {
        raw_samples
    } else {
        let ch = spec.channels as usize;
        raw_samples
            .chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };

    // Resample if needed (very simple linear resample — for parity tests use
    // a 16 kHz WAV directly). We just check and warn.
    if spec.sample_rate as usize != SAMPLE_RATE {
        return Err(format!(
            "expected {} Hz WAV, got {} Hz — resample first to keep parity test meaningful",
            SAMPLE_RATE, spec.sample_rate
        )
        .into());
    }

    // === A) Native Rust mel ===
    eprintln!("computing native mel: samples={}", mono.len());
    let mel = log_mel_spectrogram(
        &mono,
        SAMPLE_RATE,
        N_FFT,
        WIN_LENGTH,
        HOP_LENGTH,
        N_MELS,
        FMIN,
        FMAX,
    );
    let n_mels = mel.len() as u32;
    let n_frames = mel.first().map_or(0, Vec::len) as u32;
    eprintln!("native mel shape: [{n_mels}, {n_frames}]");

    // === B) ONNX mel (mel.onnx) — only if model_dir given ===
    if let Some(dir) = model_dir {
        eprintln!("loading sessions from {}", dir.display());
        let mut sessions = NemotronSessions::load(dir, 1)?;
        let mel_session = sessions
            .mel
            .as_mut()
            .ok_or("mel.onnx not found in model dir")?;
        let (flat, mb, nf, lens) = onnx_mel_features(mel_session, &mono)?;
        eprintln!("onnx mel shape: [{mb}, {nf}]  reported_lens={lens:?}");
        let onnx_path = output_path.with_extension("onnx.melbin");
        let f = File::create(&onnx_path)?;
        let mut w = BufWriter::new(f);
        w.write_all(&(mb as u32).to_le_bytes())?;
        w.write_all(&(nf as u32).to_le_bytes())?;
        w.write_all(&(SAMPLE_RATE as u32).to_le_bytes())?;
        w.write_all(&(N_FFT as u32).to_le_bytes())?;
        w.write_all(&(WIN_LENGTH as u32).to_le_bytes())?;
        w.write_all(&(HOP_LENGTH as u32).to_le_bytes())?;
        // flat is row-major [mb, nf]
        for v in &flat {
            w.write_all(&v.to_le_bytes())?;
        }
        w.flush()?;
        eprintln!("wrote {}", onnx_path.display());
    } else {
        eprintln!("(skipping onnx mel — no model_dir argument)");
    }

    let f = File::create(output_path)?;
    let mut w = BufWriter::new(f);
    w.write_all(&n_mels.to_le_bytes())?;
    w.write_all(&n_frames.to_le_bytes())?;
    w.write_all(&(SAMPLE_RATE as u32).to_le_bytes())?;
    w.write_all(&(N_FFT as u32).to_le_bytes())?;
    w.write_all(&(WIN_LENGTH as u32).to_le_bytes())?;
    w.write_all(&(HOP_LENGTH as u32).to_le_bytes())?;
    for row in &mel {
        for v in row {
            w.write_all(&v.to_le_bytes())?;
        }
    }
    w.flush()?;
    eprintln!("wrote {}", output_path.display());
    Ok(())
}
