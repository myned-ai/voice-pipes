//! Off-line cache-aware streaming decoder for Nemotron-Speech-Streaming.
#![expect(
    clippy::needless_range_loop,
    clippy::manual_memcpy,
    reason = "spike example mirrors the reference NeMo decode loops index-for-index"
)]

//
// Part of the Nemotron FP32 spike work.
//
// This is an intentionally throwaway spike: it loads the three (or four)
// ONNX graphs produced by `scripts/export_nemotron_streaming.py`, drives
// them in a cache-aware streaming loop over a single WAV file, and prints
// the transcript plus per-chunk timing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use hound::WavReader;
use ort::session::Session;
use ort::value::{DynValue, Tensor};

#[derive(Parser, Debug)]
#[command(
    name = "streaming_decode",
    about = "Off-line cache-aware streaming decoder for Nemotron (experimental)."
)]
struct Args {
    #[arg(long)]
    model_dir: PathBuf,
    #[arg(long)]
    wav: Option<PathBuf>,
    #[arg(long)]
    mel: Option<PathBuf>,
    #[arg(long)]
    chunk: Option<usize>,
    #[arg(long)]
    introspect: bool,
    #[arg(long, default_value_t = false)]
    per_chunk_mel: bool,
    #[arg(long, default_value_t = false)]
    native_mel: bool,
    #[arg(long, default_value_t = 4)]
    threads: i16,
    #[arg(long, default_value_t = 1024)]
    blank_id: i32,
    #[arg(long, default_value_t = 8)]
    max_tokens_per_step: usize,
}

#[derive(Debug, serde::Deserialize)]
struct ExportMetadata {
    streaming_config: StreamingConfig,
    #[serde(default)]
    cache_init_shapes: HashMap<String, Vec<i64>>,
}

#[derive(Debug, serde::Deserialize, Clone, Copy)]
struct StreamingConfig {
    left: usize,
    chunk: usize,
    right: usize,
}

fn load_wav_mono_16k(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            let scale = 1.0_f32 / (1_i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    let mono = if spec.channels == 1 {
        raw
    } else {
        let ch = spec.channels as usize;
        raw.chunks(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    if spec.sample_rate == 16_000 {
        Ok(mono)
    } else {
        eprintln!(
            "[wav] resampling {} Hz -> 16000 Hz (windowed-sinc, 64 taps)",
            spec.sample_rate
        );
        Ok(resample_sinc(&mono, spec.sample_rate, 16_000))
    }
}

fn resample_sinc(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == dst_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = dst_rate as f64 / src_rate as f64;
    let cutoff = 0.5_f64 * ratio.min(1.0);
    let half_taps = 32_isize;
    let n_out = ((input.len() as f64) * ratio).ceil() as usize;
    let mut out = Vec::with_capacity(n_out);
    let pi = std::f64::consts::PI;
    let two_cutoff = 2.0 * cutoff;
    for n in 0..n_out {
        let center = n as f64 / ratio;
        let center_floor = center.floor() as isize;
        let i0 = center_floor - half_taps + 1;
        let i1 = center_floor + half_taps;
        let mut acc = 0.0_f64;
        for i in i0..=i1 {
            if i < 0 || (i as usize) >= input.len() {
                continue;
            }
            let t = center - i as f64;
            let pi_t = pi * t;
            let sinc = if t.abs() < 1e-12 {
                two_cutoff
            } else {
                (two_cutoff * pi_t).sin() / pi_t
            };
            let win = 0.5 + 0.5 * (pi * t / half_taps as f64).cos();
            acc += (sinc * win) * input[i as usize] as f64;
        }
        out.push(acc as f32);
    }
    out
}

fn open_session(path: &Path, threads: i16) -> Result<Session> {
    let threads = usize::try_from(threads.max(1)).unwrap_or(1);
    voice_pipes_onnx::open_session(path, &voice_pipes_onnx::SessionOptions::new(threads))
        .map_err(|e| anyhow!("{e}"))
}

fn dump_io(label: &str, session: &Session) {
    println!("[{label}] inputs:");
    for input in session.inputs() {
        println!("  {} :: {:?}", input.name(), input.dtype());
    }
    println!("[{label}] outputs:");
    for output in session.outputs() {
        println!("  {} :: {:?}", output.name(), output.dtype());
    }
}
#[derive(Debug)]
struct CacheTensors {
    state_f32: HashMap<String, (Vec<i64>, Vec<f32>)>,
    state_i64: HashMap<String, (Vec<i64>, Vec<i64>)>,
}

impl CacheTensors {
    fn new(encoder: &Session, init_shapes: &HashMap<String, Vec<i64>>) -> Result<Self> {
        let mut state_f32 = HashMap::new();
        let mut state_i64 = HashMap::new();
        for input in encoder.inputs() {
            let name = input.name().to_string();
            if !name.starts_with("cache_") {
                continue;
            }
            let (graph_shape, ty) = match input.dtype() {
                ort::value::ValueType::Tensor { shape, ty, .. } => (shape.clone(), *ty),
                _ => bail!("cache input '{name}' is not a tensor"),
            };
            let shape: Vec<i64> = match init_shapes.get(&name) {
                Some(s) => s.clone(),
                None => graph_shape
                    .iter()
                    .map(|&d| if d <= 0 { 1 } else { d })
                    .collect(),
            };
            let len: i64 = shape.iter().product();
            match ty {
                ort::value::TensorElementType::Float32 => {
                    state_f32.insert(name, (shape, vec![0.0_f32; len as usize]));
                }
                ort::value::TensorElementType::Int64 => {
                    state_i64.insert(name, (shape, vec![0_i64; len as usize]));
                }
                other => bail!("unsupported cache dtype {other:?} for '{name}'"),
            }
        }
        Ok(Self {
            state_f32,
            state_i64,
        })
    }
    fn snapshot_shapes(&self) -> Vec<(String, Vec<i64>)> {
        let mut v: Vec<_> = self
            .state_f32
            .iter()
            .map(|(k, (s, _))| (k.clone(), s.clone()))
            .collect();
        for (k, (s, _)) in &self.state_i64 {
            v.push((k.clone(), s.clone()));
        }
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

fn run_mel(session: &mut Session, waveform: &[f32]) -> Result<(Vec<f32>, usize, usize, Vec<i64>)> {
    let n = waveform.len();
    let wav_t = Tensor::from_array(([1usize, n], waveform.to_vec()))?;
    let len_t = Tensor::from_array(([1usize], vec![n as i64]))?;
    let outputs = session.run(ort::inputs![wav_t, len_t])?;
    let (feat_shape, feat_data) = outputs[0].try_extract_tensor::<f32>()?;
    let (_, lens_data) = outputs[1].try_extract_tensor::<i64>()?;
    let mel_bins = feat_shape.get(1).copied().unwrap_or(128) as usize;
    let time_steps = feat_shape.get(2).copied().unwrap_or(0) as usize;
    Ok((feat_data.to_vec(), mel_bins, time_steps, lens_data.to_vec()))
}

struct EncoderChunkOut {
    embeddings: Vec<f32>,
    embed_shape: Vec<i64>,
    encoded_lengths: Vec<i64>,
}

fn run_encoder_chunk(
    encoder: &mut Session,
    mel_features: &[f32],
    mel_bins: usize,
    time_steps: usize,
    feat_lens: &[i64],
    cache: &mut CacheTensors,
) -> Result<EncoderChunkOut> {
    let signal = Tensor::from_array(([1usize, mel_bins, time_steps], mel_features.to_vec()))?;
    let length = Tensor::from_array(([1usize], feat_lens.to_vec()))?;
    let mut named: Vec<(String, DynValue)> = Vec::new();
    let mut signal_used = false;
    let mut length_used = false;
    let input_specs: Vec<(String, ort::value::ValueType)> = encoder
        .inputs()
        .iter()
        .map(|i| (i.name().to_string(), i.dtype().clone()))
        .collect();
    for (name, _dtype) in input_specs {
        match name.as_str() {
            "audio_signal" if !signal_used => {
                named.push((name, signal.clone().into_dyn()));
                signal_used = true;
            }
            "length" if !length_used => {
                named.push((name, length.clone().into_dyn()));
                length_used = true;
            }
            n if n.starts_with("cache_") => {
                if let Some((shape, data)) = cache.state_f32.get(n) {
                    let t = Tensor::from_array((shape.as_slice(), data.clone()))?;
                    named.push((name, t.into_dyn()));
                } else if let Some((shape, data)) = cache.state_i64.get(n) {
                    let t = Tensor::from_array((shape.as_slice(), data.clone()))?;
                    named.push((name, t.into_dyn()));
                } else {
                    bail!("encoder requests cache input '{n}' not in state");
                }
            }
            _ => bail!("unexpected encoder input name '{name}'"),
        }
    }
    if !signal_used || !length_used {
        bail!("encoder did not request both 'audio_signal' and 'length' inputs");
    }
    let output_names: Vec<String> = encoder
        .outputs()
        .iter()
        .map(|o| o.name().to_string())
        .collect();
    let outputs = encoder.run(named)?;
    let mut embeddings: Option<(Vec<i64>, Vec<f32>)> = None;
    let mut encoded_lengths: Option<Vec<i64>> = None;
    let mut new_cache_f32: HashMap<String, (Vec<i64>, Vec<f32>)> = HashMap::new();
    let mut new_cache_i64: HashMap<String, (Vec<i64>, Vec<i64>)> = HashMap::new();
    for (i, name) in output_names.iter().enumerate() {
        match name.as_str() {
            "outputs" | "encoder_outputs" => {
                let (shape, data) = outputs[i].try_extract_tensor::<f32>()?;
                embeddings = Some((shape.to_vec(), data.to_vec()));
            }
            "encoded_lengths" => {
                let (_, data) = outputs[i].try_extract_tensor::<i64>()?;
                encoded_lengths = Some(data.to_vec());
            }
            n if n.starts_with("cache_") && n.ends_with("_next_len") => {
                let key = n.trim_end_matches("_next_len").to_string() + "_len";
                let (shape, data) = outputs[i].try_extract_tensor::<i64>()?;
                new_cache_i64.insert(key, (shape.to_vec(), data.to_vec()));
            }
            n if n.starts_with("cache_") && n.ends_with("_next") => {
                let key = n.trim_end_matches("_next").to_string();
                let (shape, data) = outputs[i].try_extract_tensor::<f32>()?;
                new_cache_f32.insert(key, (shape.to_vec(), data.to_vec()));
            }
            _ => {
                eprintln!("[encoder] note: ignoring unknown output '{name}'");
            }
        }
    }
    let (embed_shape, embed_data) =
        embeddings.ok_or_else(|| anyhow!("encoder produced no 'outputs' tensor"))?;
    let enc_lens = encoded_lengths.unwrap_or_else(|| vec![0]);
    for (k, v) in new_cache_f32 {
        cache.state_f32.insert(k, v);
    }
    for (k, v) in new_cache_i64 {
        cache.state_i64.insert(k, v);
    }
    Ok(EncoderChunkOut {
        embeddings: embed_data,
        embed_shape,
        encoded_lengths: enc_lens,
    })
}

fn load_vocab(model_dir: &Path) -> HashMap<u32, String> {
    let vocab_path = model_dir.join("tokenizer.vocab");
    if !vocab_path.exists() {
        eprintln!(
            "[tok] note: {} not found -- falling back to numeric token IDs in transcript",
            vocab_path.display()
        );
        return HashMap::new();
    }
    let mut map = HashMap::new();
    let raw = std::fs::read_to_string(&vocab_path).unwrap_or_default();
    for (idx, line) in raw.lines().enumerate() {
        let piece = line.split('\t').next().unwrap_or(line).to_string();
        map.insert(idx as u32, piece);
    }
    map
}

fn detokenize(vocab: &HashMap<u32, String>, ids: &[u32]) -> String {
    if vocab.is_empty() {
        return ids
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
    }
    ids.iter()
        .filter_map(|i| vocab.get(i))
        .cloned()
        .collect::<String>()
        .replace('\u{2581}', " ")
        .trim()
        .to_string()
}

struct DecoderState {
    inputs: Vec<(String, Vec<i64>, Vec<f32>)>,
    output_names: Vec<String>,
    prev_token: i32,
}

impl DecoderState {
    fn new(decoder: &Session, blank_id: i32) -> Result<Self> {
        fn is_state_shape(shape: &[i64]) -> bool {
            shape.len() == 3 && shape[0] > 0 && shape[0] <= 8
        }
        let mut inputs = Vec::new();
        for input in decoder.inputs() {
            let name = input.name().to_string();
            let (shape, ty) = match input.dtype() {
                ort::value::ValueType::Tensor { shape, ty, .. } => (shape.clone(), *ty),
                _ => continue,
            };
            if ty != ort::value::TensorElementType::Float32 || !is_state_shape(&shape) {
                continue;
            }
            let shape: Vec<i64> = shape.iter().map(|&d| if d <= 0 { 1 } else { d }).collect();
            let len: i64 = shape.iter().product();
            inputs.push((name, shape, vec![0.0_f32; len as usize]));
        }
        let mut output_names = Vec::new();
        for output in decoder.outputs() {
            let name = output.name().to_string();
            if name == "outputs" {
                continue;
            }
            let (shape, ty) = match output.dtype() {
                ort::value::ValueType::Tensor { shape, ty, .. } => (shape.clone(), *ty),
                _ => continue,
            };
            if ty != ort::value::TensorElementType::Float32 || !is_state_shape(&shape) {
                continue;
            }
            output_names.push(name);
        }
        if inputs.len() != output_names.len() {
            bail!(
                "decoder state I/O mismatch: {} state inputs vs {} state outputs",
                inputs.len(),
                output_names.len(),
            );
        }
        Ok(Self {
            inputs,
            output_names,
            prev_token: blank_id,
        })
    }
    fn shapes(&self) -> Vec<(String, Vec<i64>)> {
        self.inputs
            .iter()
            .map(|(n, s, _)| (n.clone(), s.clone()))
            .collect()
    }
}

fn argmax(xs: &[f32]) -> usize {
    let mut best_i = 0_usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &v) in xs.iter().enumerate() {
        if v > best_v {
            best_v = v;
            best_i = i;
        }
    }
    best_i
}

fn decode_chunk_split(
    decoder: &mut Session,
    joint: &mut Session,
    enc_out: &EncoderChunkOut,
    state: &mut DecoderState,
    blank_id: i32,
    max_tokens_per_step: usize,
) -> Result<Vec<u32>> {
    let mut emitted: Vec<u32> = Vec::new();
    let t_prime = *enc_out.encoded_lengths.first().unwrap_or(&0) as usize;
    if t_prime == 0 || enc_out.embed_shape.len() < 3 {
        return Ok(emitted);
    }
    let d1 = enc_out.embed_shape[1] as usize;
    let d2 = enc_out.embed_shape[2] as usize;
    let (time_axis_first, embed_dim) = if d1 == t_prime {
        (true, d2)
    } else {
        (false, d1)
    };
    let dec_output_names: Vec<String> = decoder
        .outputs()
        .iter()
        .map(|o| o.name().to_string())
        .collect();
    let joint_output_names: Vec<String> = joint
        .outputs()
        .iter()
        .map(|o| o.name().to_string())
        .collect();
    let pred_idx = dec_output_names
        .iter()
        .position(|n| n == "outputs")
        .ok_or_else(|| anyhow!("decoder graph has no 'outputs' tensor"))?;
    let joint_logits_idx = joint_output_names
        .iter()
        .position(|n| n == "outputs")
        .ok_or_else(|| anyhow!("joint graph has no 'outputs' tensor"))?;
    let state_out_idx: Vec<usize> = state
        .output_names
        .iter()
        .map(|on| {
            dec_output_names
                .iter()
                .position(|n| n == on)
                .ok_or_else(|| anyhow!("decoder lost state output '{on}'"))
        })
        .collect::<Result<_>>()?;
    for t in 0..t_prime {
        let frame: Vec<f32> = if time_axis_first {
            enc_out.embeddings[t * embed_dim..(t + 1) * embed_dim].to_vec()
        } else {
            (0..embed_dim)
                .map(|d| enc_out.embeddings[d * t_prime + t])
                .collect()
        };
        let mut tokens_at_t = 0_usize;
        loop {
            let tgt = Tensor::from_array(([1usize, 1usize], vec![state.prev_token]))?;
            let tgt_len = Tensor::from_array(([1usize], vec![1_i32]))?;
            let mut dec_inputs: Vec<(String, DynValue)> = vec![
                ("targets".to_string(), tgt.into_dyn()),
                ("target_length".to_string(), tgt_len.into_dyn()),
            ];
            for (name, shape, data) in &state.inputs {
                let st = Tensor::from_array((shape.as_slice(), data.clone()))?;
                dec_inputs.push((name.clone(), st.into_dyn()));
            }
            let dec_outputs = decoder.run(dec_inputs)?;
            let (pred_shape, pred_data) = dec_outputs[pred_idx].try_extract_tensor::<f32>()?;
            let d_dec = pred_shape
                .get(1)
                .copied()
                .ok_or_else(|| anyhow!("decoder output missing channel dim"))?
                as usize;
            let pred_owned: Vec<f32> = pred_data.to_vec();
            let mut candidate: Vec<Vec<f32>> = Vec::with_capacity(state_out_idx.len());
            for &oi in &state_out_idx {
                let (_, d) = dec_outputs[oi].try_extract_tensor::<f32>()?;
                candidate.push(d.to_vec());
            }
            let enc_t = Tensor::from_array(([1usize, embed_dim, 1usize], frame.clone()))?;
            let dec_t = Tensor::from_array(([1usize, d_dec, 1usize], pred_owned))?;
            let joint_outputs = joint.run(ort::inputs![
                "encoder_outputs" => enc_t,
                "decoder_outputs" => dec_t,
            ])?;
            let (_, logits) = joint_outputs[joint_logits_idx].try_extract_tensor::<f32>()?;
            let token = argmax(logits) as i32;
            if token == blank_id {
                break;
            }
            emitted.push(token as u32);
            state.prev_token = token;
            for ((_, _, slot), new_data) in state.inputs.iter_mut().zip(candidate) {
                *slot = new_data;
            }
            tokens_at_t += 1;
            if tokens_at_t >= max_tokens_per_step {
                break;
            }
        }
    }
    Ok(emitted)
}

const SAMPLE_RATE: usize = 16_000;
const N_FFT: usize = 512;
const WIN_LENGTH: usize = 400;
const HOP_LENGTH: usize = 160;
const MEL_BINS: usize = 128;
const SUBSAMPLING_FACTOR: usize = 8;

fn native_mel_features(waveform: &[f32]) -> (Vec<f32>, usize, usize, Vec<i64>) {
    // The raw log-mel the model was trained on: no normalisation, no padding.
    let mel = voice_pipes_stt_nemotron::mel_native::log_mel_spectrogram_opts(
        waveform,
        SAMPLE_RATE,
        N_FFT,
        WIN_LENGTH,
        HOP_LENGTH,
        MEL_BINS,
        0.0,
        SAMPLE_RATE as f32 / 2.0,
        false,
        false,
    );
    let n_mels = mel.len();
    let n_frames = if n_mels > 0 { mel[0].len() } else { 0 };
    // Layout [1, n_mels, n_frames] flattened row-major: mel[m][t]
    let mut flat = Vec::with_capacity(n_mels * n_frames);
    for m in 0..n_mels {
        flat.extend_from_slice(&mel[m]);
    }
    (flat, n_mels, n_frames, vec![n_frames as i64])
}

fn slice_mel_chunk(
    mel: &[f32],
    mel_bins: usize,
    total_frames: usize,
    start: usize,
    len: usize,
) -> (Vec<f32>, usize, Vec<i64>) {
    let end = (start + len).min(total_frames);
    let actual = end.saturating_sub(start);
    let mut out = vec![0.0_f32; mel_bins * len];
    for m in 0..mel_bins {
        let src_off = m * total_frames + start;
        let dst_off = m * len;
        for i in 0..actual {
            out[dst_off + i] = mel[src_off + i];
        }
    }
    (out, len, vec![actual as i64])
}

fn main() -> Result<()> {
    let args = Args::parse();

    if !args.model_dir.is_dir() {
        bail!("model_dir does not exist: {}", args.model_dir.display());
    }
    let metadata_path = args.model_dir.join("export_metadata.json");
    let metadata: ExportMetadata = if metadata_path.exists() {
        let raw = std::fs::read_to_string(&metadata_path)
            .with_context(|| format!("reading {}", metadata_path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", metadata_path.display()))?
    } else {
        bail!(
            "missing {} -- regenerate via export_nemotron_streaming.py",
            metadata_path.display()
        );
    };
    let cfg = metadata.streaming_config;
    let chunk_frames = args.chunk.unwrap_or(cfg.chunk * SUBSAMPLING_FACTOR);
    let right_ctx_frames = cfg.right * SUBSAMPLING_FACTOR;
    println!(
        "[cfg] streaming left={}, chunk={}, right={} (using chunk_frames={}, right_ctx_frames={})",
        cfg.left, cfg.chunk, cfg.right, chunk_frames, right_ctx_frames
    );

    let encoder_path = args.model_dir.join("encoder.onnx");
    let mel_path = args.model_dir.join("mel.onnx");
    let decoder_path = args.model_dir.join("decoder.onnx");
    let joint_path = args.model_dir.join("joint.onnx");
    let combined_path = args.model_dir.join("decoder_joint.onnx");

    let mut encoder = open_session(&encoder_path, args.threads)?;
    let mut mel_session: Option<Session> = if args.native_mel {
        None
    } else if mel_path.exists() {
        Some(open_session(&mel_path, args.threads)?)
    } else {
        bail!(
            "mel.onnx not found at {} and --native-mel not set",
            mel_path.display()
        );
    };
    let split_layout = decoder_path.exists() && joint_path.exists();
    if !split_layout {
        if combined_path.exists() {
            bail!("combined decoder_joint.onnx path is not implemented in this spike");
        }
        bail!(
            "no decoder.onnx + joint.onnx (or decoder_joint.onnx) found in {}",
            args.model_dir.display()
        );
    }
    let mut decoder = open_session(&decoder_path, args.threads)?;
    let mut joint = open_session(&joint_path, args.threads)?;

    if args.introspect {
        dump_io("encoder", &encoder);
        if let Some(s) = mel_session.as_ref() {
            dump_io("mel", s);
        } else {
            println!("[mel] (native Rust mel pipeline -- no ONNX session)");
        }
        dump_io("decoder", &decoder);
        dump_io("joint", &joint);
        return Ok(());
    }

    let wav_path = args
        .wav
        .as_ref()
        .ok_or_else(|| anyhow!("--wav <PATH> is required unless --introspect is set"))?;
    let waveform = load_wav_mono_16k(wav_path)?;
    println!(
        "[wav] {} samples at 16kHz (~{:.2}s)",
        waveform.len(),
        waveform.len() as f32 / SAMPLE_RATE as f32
    );

    let mut cache = CacheTensors::new(&encoder, &metadata.cache_init_shapes)?;
    println!(
        "[cache] initialised {} state tensor(s)",
        cache.snapshot_shapes().len()
    );

    let vocab = load_vocab(&args.model_dir);
    let mut decoder_state = DecoderState::new(&decoder, args.blank_id)?;
    println!(
        "[decoder] {} state tensor(s) tracked",
        decoder_state.shapes().len()
    );

    let t_start = Instant::now();
    let mut all_tokens: Vec<u32> = Vec::new();

    if args.per_chunk_mel {
        // Per-chunk mel: feed `chunk_frames + right_ctx_frames` waveform-equivalent samples each step.
        // We instead compute mel up to current waveform position each chunk to keep it simple.
        let samples_per_chunk = chunk_frames * HOP_LENGTH;
        let right_samples = right_ctx_frames * HOP_LENGTH;
        let mut sample_pos = 0_usize;
        let mut chunk_idx = 0_usize;
        while sample_pos < waveform.len() {
            let end = (sample_pos + samples_per_chunk + right_samples).min(waveform.len());
            let segment = &waveform[sample_pos..end];
            let (feat_data, mel_bins, time_steps, feat_lens) = if args.native_mel {
                native_mel_features(segment)
            } else {
                let sess = mel_session
                    .as_mut()
                    .ok_or_else(|| anyhow!("mel session unexpectedly None"))?;
                run_mel(sess, segment)?
            };
            let chunk_t0 = Instant::now();
            let enc_out = run_encoder_chunk(
                &mut encoder,
                &feat_data,
                mel_bins,
                time_steps,
                &feat_lens,
                &mut cache,
            )?;
            let tokens = decode_chunk_split(
                &mut decoder,
                &mut joint,
                &enc_out,
                &mut decoder_state,
                args.blank_id,
                args.max_tokens_per_step,
            )?;
            let elapsed = chunk_t0.elapsed();
            println!(
                "[chunk {chunk_idx}] samples={}..{} feats={} tokens={} took {:.1}ms",
                sample_pos,
                end,
                time_steps,
                tokens.len(),
                elapsed.as_secs_f32() * 1000.0
            );
            all_tokens.extend_from_slice(&tokens);
            sample_pos += samples_per_chunk;
            chunk_idx += 1;
        }
    } else {
        // Whole-mel path: compute mel once, then slice in chunks of chunk_frames + right_ctx_frames.
        let (mel_data, mel_bins, total_frames, _) = if args.native_mel {
            native_mel_features(&waveform)
        } else {
            let sess = mel_session
                .as_mut()
                .ok_or_else(|| anyhow!("mel session unexpectedly None"))?;
            run_mel(sess, &waveform)?
        };
        println!("[mel] computed full mel: {mel_bins} bins x {total_frames} frames");
        let window_frames = chunk_frames + right_ctx_frames;
        let mut frame_pos = 0_usize;
        let mut chunk_idx = 0_usize;
        while frame_pos < total_frames {
            let (feat_data, time_steps, feat_lens) =
                slice_mel_chunk(&mel_data, mel_bins, total_frames, frame_pos, window_frames);
            let chunk_t0 = Instant::now();
            let enc_out = run_encoder_chunk(
                &mut encoder,
                &feat_data,
                mel_bins,
                time_steps,
                &feat_lens,
                &mut cache,
            )?;
            let tokens = decode_chunk_split(
                &mut decoder,
                &mut joint,
                &enc_out,
                &mut decoder_state,
                args.blank_id,
                args.max_tokens_per_step,
            )?;
            let elapsed = chunk_t0.elapsed();
            println!(
                "[chunk {chunk_idx}] frames={}..{} valid_len={} tokens={} took {:.1}ms",
                frame_pos,
                frame_pos + window_frames,
                feat_lens[0],
                tokens.len(),
                elapsed.as_secs_f32() * 1000.0
            );
            all_tokens.extend_from_slice(&tokens);
            frame_pos += chunk_frames;
            chunk_idx += 1;
        }
    }

    let total_elapsed = t_start.elapsed();
    let audio_secs = waveform.len() as f32 / SAMPLE_RATE as f32;
    let rtf = if audio_secs > 0.0 {
        total_elapsed.as_secs_f32() / audio_secs
    } else {
        0.0
    };

    println!("--- results ---");
    println!("tokens emitted: {}", all_tokens.len());
    println!(
        "wall time: {:.2}s for {:.2}s audio (RTF = {:.3}, {} real-time)",
        total_elapsed.as_secs_f32(),
        audio_secs,
        rtf,
        if rtf <= 1.0 { "<=" } else { ">" }
    );
    let transcript = detokenize(&vocab, &all_tokens);
    println!("transcript: {transcript}");
    Ok(())
}
