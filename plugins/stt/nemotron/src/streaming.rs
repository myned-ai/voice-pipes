//! Cache-aware streaming engine for Nemotron-Speech-Streaming.
//!
//! Ports the off-line decoder from `examples/nemotron_streaming_decode.rs`
//! into a reusable engine with per-utterance state. Uses native Rust mel
//! (no `mel.onnx` session needed).

use std::collections::HashMap;
use std::path::Path;

use ort::session::Session;
use ort::value::{DynValue, Tensor};

use voice_pipes_onnx::{ExecutionTarget, SessionOptions};
use voice_pipes_sdk::stt::SttError;

use crate::mel_native;

pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = 512;
pub const WIN_LENGTH: usize = 400;
pub const HOP_LENGTH: usize = 160;
pub const MEL_BINS: usize = 128;
pub const SUBSAMPLING_FACTOR: usize = 8;
/// Audio kept before the next window's first frame: its window and its
/// pre-emphasis reach `N_FFT / 2 + 1` samples back.
const LEFT_CONTEXT: usize = N_FFT / 2 + 1;

#[derive(Debug, serde::Deserialize)]
pub struct ExportMetadata {
    pub streaming_config: StreamingConfig,
    #[serde(default)]
    pub cache_init_shapes: HashMap<String, Vec<i64>>,
}

#[derive(Debug, serde::Deserialize, Clone, Copy)]
pub struct StreamingConfig {
    pub left: usize,
    pub chunk: usize,
    pub right: usize,
}

pub fn load_metadata(model_dir: &Path) -> Result<ExportMetadata, SttError> {
    let metadata_path = model_dir.join("export_metadata.json");
    let raw = std::fs::read_to_string(&metadata_path).map_err(|e| {
        SttError::ModelLoadFailed(format!("reading {}: {e}", metadata_path.display()))
    })?;
    parse_metadata(&raw)
        .map_err(|e| SttError::ModelLoadFailed(format!("{}: {e}", metadata_path.display())))
}

/// The most subsampled frames a window side may hold: about ten minutes,
/// far past any export, and small enough that the frame and sample counts
/// computed from it cannot overflow.
const MAX_CONFIG_FRAMES: usize = 8_000;

/// Parse and check an export's metadata. A zero chunk would make every
/// window advance by nothing, so the decoder would loop for ever.
fn parse_metadata(raw: &str) -> Result<ExportMetadata, String> {
    let metadata: ExportMetadata = serde_json::from_str(raw)
        .map_err(|e| format!("parsing: {}", voice_pipes_sdk::secrets::json_error(&e)))?;
    let StreamingConfig { left, chunk, right } = metadata.streaming_config;
    if chunk == 0 {
        return Err("streaming_config.chunk is 0; a window must advance".into());
    }
    for (name, frames) in [("left", left), ("chunk", chunk), ("right", right)] {
        if frames > MAX_CONFIG_FRAMES {
            return Err(format!(
                "streaming_config.{name} is {frames} frames, more than {MAX_CONFIG_FRAMES}"
            ));
        }
    }
    Ok(metadata)
}

/// The session thread count for the `threads` option: the number given,
/// and 0 (or a negative number) for ONNX Runtime's own choice, as in every
/// other local model.
pub(crate) fn session_threads(threads: i16) -> usize {
    usize::try_from(threads).unwrap_or(0)
}

/// Open one of the model's graphs on `threads` threads (see
/// `session_threads`), where `VP_ORT_EP` says (CUDA when the build has
/// `ort-cuda`).
pub fn open_session(path: &Path, threads: i16) -> Result<Session, SttError> {
    let options =
        SessionOptions::new(session_threads(threads)).with_target(ExecutionTarget::from_env());
    voice_pipes_onnx::open_session(path, &options)
        .map_err(|e| SttError::ModelLoadFailed(e.to_string()))
}

/// How many elements a state tensor of `shape` holds. The shape comes from
/// the model's metadata or graph, so a negative dimension or a product past
/// what fits in memory is a broken export, refused rather than wrapped into
/// a wrong allocation.
fn element_count(name: &str, shape: &[i64]) -> Result<usize, SttError> {
    shape
        .iter()
        .try_fold(1usize, |acc, &d| {
            usize::try_from(d).ok().and_then(|d| acc.checked_mul(d))
        })
        .ok_or_else(|| {
            SttError::ModelLoadFailed(format!("state '{name}' has an impossible shape {shape:?}"))
        })
}

#[derive(Debug, Clone, Default)]
pub struct CacheTensors {
    pub state_f32: HashMap<String, (Vec<i64>, Vec<f32>)>,
    pub state_i64: HashMap<String, (Vec<i64>, Vec<i64>)>,
}

impl CacheTensors {
    pub fn new(
        encoder: &Session,
        init_shapes: &HashMap<String, Vec<i64>>,
    ) -> Result<Self, SttError> {
        let mut state_f32 = HashMap::new();
        let mut state_i64 = HashMap::new();
        for input in encoder.inputs() {
            let name = input.name().to_string();
            if !name.starts_with("cache_") {
                continue;
            }
            let (graph_shape, ty) = match input.dtype() {
                ort::value::ValueType::Tensor { shape, ty, .. } => (shape.clone(), *ty),
                _ => {
                    return Err(SttError::ModelLoadFailed(format!(
                        "cache input '{name}' is not a tensor"
                    )));
                }
            };
            let shape: Vec<i64> = match init_shapes.get(&name) {
                Some(s) => s.clone(),
                None => graph_shape
                    .iter()
                    .map(|&d| if d <= 0 { 1 } else { d })
                    .collect(),
            };
            let len = element_count(&name, &shape)?;
            match ty {
                ort::value::TensorElementType::Float32 => {
                    state_f32.insert(name, (shape, vec![0.0_f32; len]));
                }
                ort::value::TensorElementType::Int64 => {
                    state_i64.insert(name, (shape, vec![0_i64; len]));
                }
                other => {
                    return Err(SttError::ModelLoadFailed(format!(
                        "unsupported cache dtype {other:?} for '{name}'"
                    )));
                }
            }
        }
        Ok(Self {
            state_f32,
            state_i64,
        })
    }
}

#[derive(Debug)]
pub struct EncoderChunkOut {
    pub embeddings: Vec<f32>,
    pub embed_shape: Vec<i64>,
    pub encoded_lengths: Vec<i64>,
}

fn ort_err<E: std::fmt::Display>(stage: &str) -> impl Fn(E) -> SttError + '_ {
    move |e| SttError::InferenceFailed(format!("{stage}: {e}"))
}

pub fn run_encoder_chunk(
    encoder: &mut Session,
    mel_features: &[f32],
    mel_bins: usize,
    time_steps: usize,
    feat_lens: &[i64],
    cache: &mut CacheTensors,
) -> Result<EncoderChunkOut, SttError> {
    let signal = Tensor::from_array(([1usize, mel_bins, time_steps], mel_features.to_vec()))
        .map_err(ort_err("signal tensor"))?;
    let length =
        Tensor::from_array(([1usize], feat_lens.to_vec())).map_err(ort_err("length tensor"))?;
    let mut named: Vec<(String, DynValue)> = Vec::new();
    let mut signal_used = false;
    let mut length_used = false;
    let input_specs: Vec<(String, ort::value::ValueType)> = encoder
        .inputs()
        .iter()
        .map(|i| (i.name().to_string(), i.dtype().clone()))
        .collect();
    // Cache tensors are moved into the run, not copied: the encoder hands
    // back a replacement for each one, so the copy would only be dropped.
    // Every moved name is checked against the outputs below; a cache the
    // graph did not return is an error rather than a silently emptied
    // state.
    let mut moved: Vec<String> = Vec::new();
    for (name, _dtype) in input_specs {
        if name.starts_with("cache_") {
            if let Some((shape, data)) = cache.state_f32.get_mut(&name) {
                let t = Tensor::from_array((shape.as_slice(), std::mem::take(data)))
                    .map_err(ort_err("cache f32 tensor"))?;
                named.push((name.clone(), t.into_dyn()));
                moved.push(name);
                continue;
            }
            if let Some((shape, data)) = cache.state_i64.get_mut(&name) {
                let t = Tensor::from_array((shape.as_slice(), std::mem::take(data)))
                    .map_err(ort_err("cache i64 tensor"))?;
                named.push((name.clone(), t.into_dyn()));
                moved.push(name);
            }
        } else if !signal_used {
            named.push((name, signal.clone().into_dyn()));
            signal_used = true;
        } else if !length_used {
            named.push((name, length.clone().into_dyn()));
            length_used = true;
        }
    }
    let output_names: Vec<String> = encoder
        .outputs()
        .iter()
        .map(|o| o.name().to_string())
        .collect();
    let outputs = encoder.run(named).map_err(ort_err("encoder run"))?;
    let mut embeddings: Option<(Vec<i64>, Vec<f32>)> = None;
    let mut encoded_lengths: Option<Vec<i64>> = None;
    let mut new_cache_f32: HashMap<String, (Vec<i64>, Vec<f32>)> = HashMap::new();
    let mut new_cache_i64: HashMap<String, (Vec<i64>, Vec<i64>)> = HashMap::new();
    for (name, value) in output_names.iter().zip(outputs.values()) {
        match name.as_str() {
            "outputs" => {
                let (shape, data) = value
                    .try_extract_tensor::<f32>()
                    .map_err(ort_err("extract embeddings"))?;
                embeddings = Some((shape.to_vec(), data.to_vec()));
            }
            "encoded_lengths" => {
                let (_, data) = value
                    .try_extract_tensor::<i64>()
                    .map_err(ort_err("extract enc lens"))?;
                encoded_lengths = Some(data.to_vec());
            }
            n if n.starts_with("cache_") && n.ends_with("_next_len") => {
                let key = n.trim_end_matches("_next_len").to_string() + "_len";
                let (shape, data) = value
                    .try_extract_tensor::<i64>()
                    .map_err(ort_err("extract cache len"))?;
                new_cache_i64.insert(key, (shape.to_vec(), data.to_vec()));
            }
            n if n.starts_with("cache_") && n.ends_with("_next") => {
                let key = n.trim_end_matches("_next").to_string();
                let (shape, data) = value
                    .try_extract_tensor::<f32>()
                    .map_err(ort_err("extract cache f32"))?;
                new_cache_f32.insert(key, (shape.to_vec(), data.to_vec()));
            }
            _ => {}
        }
    }
    let (embed_shape, embed_data) = embeddings
        .ok_or_else(|| SttError::InferenceFailed("encoder produced no 'outputs' tensor".into()))?;
    let enc_lens = encoded_lengths.unwrap_or_else(|| vec![0]);
    for name in &moved {
        if !new_cache_f32.contains_key(name) && !new_cache_i64.contains_key(name) {
            return Err(SttError::InferenceFailed(format!(
                "encoder returned no updated state for cache input '{name}'"
            )));
        }
    }
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

#[derive(Debug, Clone)]
pub struct DecoderState {
    pub inputs: Vec<(String, Vec<i64>, Vec<f32>)>,
    pub output_names: Vec<String>,
    pub prev_token: i32,
}

impl DecoderState {
    pub fn new(decoder: &Session, blank_id: i32) -> Result<Self, SttError> {
        fn is_state_shape(shape: &[i64]) -> bool {
            matches!(shape, [layers, _, _] if (1..=8).contains(layers))
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
            let len = element_count(&name, &shape)?;
            inputs.push((name, shape, vec![0.0_f32; len]));
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
            return Err(SttError::ModelLoadFailed(format!(
                "decoder state I/O mismatch: {} inputs vs {} outputs",
                inputs.len(),
                output_names.len(),
            )));
        }
        Ok(Self {
            inputs,
            output_names,
            prev_token: blank_id,
        })
    }
}

use voice_pipes_onnx::argmax;

/// How the encoder's `[1, a, b]` output lays out its frames. Exports differ
/// on whether time comes first, and a zero-padded final chunk reports fewer
/// valid frames than the tensor is wide, so the valid length is neither the
/// time axis's size nor the row stride.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameLayout {
    time_first: bool,
    /// Values per frame.
    embed_dim: usize,
    /// The time axis's full size: the row stride when channels come first.
    width: usize,
}

impl FrameLayout {
    /// The layout of `shape` holding `valid` frames: the time axis is the one
    /// equal to `valid`, or, on a padded chunk, the smaller one that can
    /// hold it (an embedding is far wider than a chunk is long). `None` for
    /// no frames or a shape that cannot hold them.
    pub(crate) fn of(shape: &[i64], valid: usize) -> Option<Self> {
        let [_, a, b] = shape else { return None };
        let (a, b) = (usize::try_from(*a).ok()?, usize::try_from(*b).ok()?);
        if valid == 0 {
            return None;
        }
        let time_first = match (a == valid, b == valid) {
            (true, false) => true,
            (false, true) => false,
            // Both or neither equal: the time axis is the smaller that fits.
            _ => (a <= b && a >= valid) || b < valid,
        };
        let (width, embed_dim) = if time_first { (a, b) } else { (b, a) };
        (width >= valid).then_some(Self {
            time_first,
            embed_dim,
            width,
        })
    }

    /// Frame `t` of `embeddings`, or `None` when the tensor holds fewer
    /// values than its shape says.
    pub(crate) fn frame(self, embeddings: &[f32], t: usize) -> Option<Vec<f32>> {
        if self.time_first {
            embeddings
                .get(t * self.embed_dim..(t + 1) * self.embed_dim)
                .map(<[f32]>::to_vec)
        } else {
            (0..self.embed_dim)
                .map(|d| embeddings.get(d * self.width + t).copied())
                .collect()
        }
    }
}

/// Where the decoder's prediction, the joint's logits and each decoder
/// state output sit among their sessions' outputs.
struct OutputIndices {
    pred: usize,
    logits: usize,
    states: Vec<usize>,
}

fn output_indices(
    decoder: &Session,
    joint: &Session,
    state: &DecoderState,
) -> Result<OutputIndices, SttError> {
    let names = |session: &Session| -> Vec<String> {
        session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect()
    };
    let position = |names: &[String], want: &str| names.iter().position(|n| n == want);
    let decoder_names = names(decoder);
    let pred = position(&decoder_names, "outputs")
        .ok_or_else(|| SttError::ModelLoadFailed("decoder has no 'outputs' tensor".into()))?;
    let logits = position(&names(joint), "outputs")
        .ok_or_else(|| SttError::ModelLoadFailed("joint has no 'outputs' tensor".into()))?;
    let states = state
        .output_names
        .iter()
        .map(|on| {
            position(&decoder_names, on).ok_or_else(|| {
                SttError::ModelLoadFailed(format!("decoder lost state output '{on}'"))
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(OutputIndices {
        pred,
        logits,
        states,
    })
}

pub fn decode_chunk_split(
    decoder: &mut Session,
    joint: &mut Session,
    enc_out: &EncoderChunkOut,
    state: &mut DecoderState,
    blank_id: i32,
    max_tokens_per_step: usize,
) -> Result<Vec<u32>, SttError> {
    let mut emitted: Vec<u32> = Vec::new();
    let t_prime = usize::try_from(*enc_out.encoded_lengths.first().unwrap_or(&0)).unwrap_or(0);
    let Some(layout) = FrameLayout::of(&enc_out.embed_shape, t_prime) else {
        return Ok(emitted);
    };
    let OutputIndices {
        pred: pred_idx,
        logits: joint_logits_idx,
        states: state_out_idx,
    } = output_indices(decoder, joint, state)?;
    for t in 0..t_prime {
        let frame = layout.frame(&enc_out.embeddings, t).ok_or_else(|| {
            SttError::InferenceFailed("encoder output holds fewer values than its shape".into())
        })?;
        let mut tokens_at_t = 0_usize;
        loop {
            let tgt = Tensor::from_array(([1usize, 1usize], vec![state.prev_token]))
                .map_err(ort_err("decoder target tensor"))?;
            let tgt_len = Tensor::from_array(([1usize], vec![1_i32]))
                .map_err(ort_err("decoder target_length tensor"))?;
            let mut dec_inputs: Vec<(String, DynValue)> = vec![
                ("targets".to_string(), tgt.into_dyn()),
                ("target_length".to_string(), tgt_len.into_dyn()),
            ];
            for (name, shape, data) in &state.inputs {
                let st = Tensor::from_array((shape.as_slice(), data.clone()))
                    .map_err(ort_err("decoder state tensor"))?;
                dec_inputs.push((name.clone(), st.into_dyn()));
            }
            let dec_outputs = decoder.run(dec_inputs).map_err(ort_err("decoder run"))?;
            let pred = voice_pipes_onnx::nth_output(&dec_outputs, pred_idx, "decoder")
                .map_err(model_output)?;
            let (pred_shape, pred_data) = pred
                .try_extract_tensor::<f32>()
                .map_err(ort_err("extract pred"))?;
            let d_dec = voice_pipes_onnx::dim(
                pred_shape.get(1).copied().ok_or_else(|| {
                    SttError::InferenceFailed("decoder output missing channel dim".into())
                })?,
                "decoder channels",
            )
            .map_err(model_output)?;
            let pred_owned: Vec<f32> = pred_data.to_vec();
            let mut candidate: Vec<Vec<f32>> = Vec::with_capacity(state_out_idx.len());
            for &oi in &state_out_idx {
                let out = voice_pipes_onnx::nth_output(&dec_outputs, oi, "decoder state")
                    .map_err(model_output)?;
                let (_, d) = out
                    .try_extract_tensor::<f32>()
                    .map_err(ort_err("extract candidate state"))?;
                candidate.push(d.to_vec());
            }
            let enc_t = Tensor::from_array(([1usize, layout.embed_dim, 1usize], frame.clone()))
                .map_err(ort_err("joint enc tensor"))?;
            let dec_t = Tensor::from_array(([1usize, d_dec, 1usize], pred_owned))
                .map_err(ort_err("joint dec tensor"))?;
            let joint_outputs = joint
                .run(ort::inputs![
                    "encoder_outputs" => enc_t,
                    "decoder_outputs" => dec_t,
                ])
                .map_err(ort_err("joint run"))?;
            let out = voice_pipes_onnx::nth_output(&joint_outputs, joint_logits_idx, "joint")
                .map_err(model_output)?;
            let (_, logits) = out
                .try_extract_tensor::<f32>()
                .map_err(ort_err("extract logits"))?;
            let token = argmax(logits).ok_or_else(|| {
                SttError::InferenceFailed("the joint emitted no usable logit".into())
            })?;
            let token = i32::try_from(token).map_err(|_too_large| {
                SttError::InferenceFailed("the joint emitted a token id past i32".into())
            })?;
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

/// Compute native log-mel features in `[1, n_mels, n_frames]` flattened layout.
pub fn native_mel_features(waveform: &[f32]) -> (Vec<f32>, usize, usize, Vec<i64>) {
    let mel = mel_native::log_mel_spectrogram(
        waveform,
        SAMPLE_RATE,
        N_FFT,
        WIN_LENGTH,
        HOP_LENGTH,
        MEL_BINS,
        0.0,
        SAMPLE_RATE as f32 / 2.0,
    );
    let n_mels = mel.len();
    let n_frames = mel.first().map_or(0, Vec::len);
    let mut flat = Vec::with_capacity(n_mels * n_frames);
    for row in &mel {
        flat.extend_from_slice(row);
    }
    (flat, n_mels, n_frames, vec![n_frames as i64])
}

/// Slice a fixed-size window out of a precomputed `[n_mels, total_frames]`
/// flattened mel buffer. Pads the trailing window with zeros and reports
/// the actual valid length so the encoder ignores the padding.
pub fn slice_mel_chunk(
    mel: &[f32],
    mel_bins: usize,
    total_frames: usize,
    start: usize,
    len: usize,
) -> (Vec<f32>, usize, Vec<i64>) {
    let end = (start + len).min(total_frames);
    let actual = end.saturating_sub(start);
    let mut out = vec![0.0_f32; mel_bins * len];
    // `actual == 0` (window starts past the end) copies nothing; guard so the
    // per-row source offset is never formed as a slice bound in that case.
    if actual > 0 {
        for m in 0..mel_bins {
            let src_off = m * total_frames + start;
            let dst_off = m * len;
            if let (Some(dst), Some(src)) = (
                out.get_mut(dst_off..dst_off + actual),
                mel.get(src_off..src_off + actual),
            ) {
                dst.copy_from_slice(src);
            }
        }
    }
    (out, len, vec![actual as i64])
}

/// Owned, fully-loaded set of ONNX sessions for one Nemotron pipeline.
#[derive(Debug)]
pub struct NemotronSessions {
    pub encoder: Session,
    pub decoder: Session,
    pub joint: Session,
    /// Optional hand-built mel preprocessor (`mel.onnx`). When present this is
    /// the gold-standard NeMo-equivalent feature path; otherwise we fall back
    /// to the native Rust mel implementation.
    pub mel: Option<Session>,
}

impl NemotronSessions {
    pub fn load(model_dir: &Path, threads: i16) -> Result<Self, SttError> {
        let encoder_path = model_dir.join("encoder.onnx");
        let decoder_path = model_dir.join("decoder.onnx");
        let joint_path = model_dir.join("joint.onnx");
        let mel_path = model_dir.join("mel.onnx");
        if !decoder_path.exists() || !joint_path.exists() {
            return Err(SttError::ModelLoadFailed(format!(
                "expected split decoder.onnx + joint.onnx in {}",
                model_dir.display()
            )));
        }
        let mel = if mel_path.exists() {
            Some(open_session(&mel_path, threads)?)
        } else {
            None
        };
        Ok(Self {
            encoder: open_session(&encoder_path, threads)?,
            decoder: open_session(&decoder_path, threads)?,
            joint: open_session(&joint_path, threads)?,
            mel,
        })
    }
}

/// Run the mel preprocessor ONNX session and return
/// `(flat_mel[1, n_mels, n_frames], n_mels, n_frames, [n_frames])`.
pub fn onnx_mel_features(
    session: &mut Session,
    waveform: &[f32],
) -> Result<(Vec<f32>, usize, usize, Vec<i64>), SttError> {
    let n = waveform.len();
    let wav_t = Tensor::from_array(([1usize, n], waveform.to_vec()))
        .map_err(|e| SttError::InferenceFailed(format!("mel signal tensor: {e}")))?;
    let len_t = Tensor::from_array(([1usize], vec![n as i64]))
        .map_err(|e| SttError::InferenceFailed(format!("mel length tensor: {e}")))?;
    let outputs = session
        .run(ort::inputs![wav_t, len_t])
        .map_err(|e| SttError::InferenceFailed(format!("mel session run: {e}")))?;
    let feats =
        voice_pipes_onnx::nth_output(&outputs, 0, "mel preprocessor").map_err(model_output)?;
    let (feat_shape, feat_data) = feats
        .try_extract_tensor::<f32>()
        .map_err(|e| SttError::InferenceFailed(format!("mel features extract: {e}")))?;
    let lens =
        voice_pipes_onnx::nth_output(&outputs, 1, "mel preprocessor").map_err(model_output)?;
    let (_, lens_data) = lens
        .try_extract_tensor::<i64>()
        .map_err(|e| SttError::InferenceFailed(format!("mel feature_length extract: {e}")))?;
    let mel_bins = match feat_shape.get(1) {
        Some(&bins) => voice_pipes_onnx::dim(bins, "mel bins").map_err(model_output)?,
        None => MEL_BINS,
    };
    let n_frames = voice_pipes_onnx::dim(feat_shape.get(2).copied().unwrap_or(0), "mel frames")
        .map_err(model_output)?;
    Ok((feat_data.to_vec(), mel_bins, n_frames, lens_data.to_vec()))
}

/// Pick the mel preprocessing path.
///
/// **Default: raw native log-mel** matching `model_config.yaml`
/// (`normalize: NA`, `pad_to: 0`). The exported `mel.onnx` is bypassed
/// because it bakes in per-feature normalization that the trained encoder
/// was NOT trained with -- using it produces gibberish ("Reverse." for any
/// utterance).
///
/// Setting `NEMOTRON_NORMALIZED_MEL=1` opts back into the legacy
/// normalized+padded path (mel.onnx if available, else native with
/// normalization). Debug / back-compat only.
fn mel_features_for(
    sessions: &mut NemotronSessions,
    waveform: &[f32],
) -> Result<(Vec<f32>, usize, usize, Vec<i64>), SttError> {
    if mel_native::legacy_normalized_mel_enabled()
        && let Some(mel_session) = sessions.mel.as_mut()
    {
        onnx_mel_features(mel_session, waveform)
    } else {
        Ok(native_mel_features(waveform))
    }
}

/// Features for a stretch of the utterance: the frames centred on
/// `stretch[left]` onward, equal to what a whole-utterance pass computes
/// there (see [`mel_native::utterance_log_mel`]), at most `max_frames` of
/// them. The legacy normalised ONNX mel has no such form and computes the
/// stretch on its own, as it always has.
fn stretch_mel_features(
    sessions: &mut NemotronSessions,
    stretch: &[f32],
    left: usize,
    ends: bool,
    max_frames: Option<usize>,
) -> Result<(Vec<f32>, usize, usize, Vec<i64>), SttError> {
    if mel_native::legacy_normalized_mel_enabled() && sessions.mel.is_some() {
        return mel_features_for(sessions, stretch.split_at(left.min(stretch.len())).1);
    }
    let mel = mel_native::utterance_log_mel(
        stretch,
        left,
        ends,
        SAMPLE_RATE,
        N_FFT,
        WIN_LENGTH,
        HOP_LENGTH,
        MEL_BINS,
        0.0,
        SAMPLE_RATE as f32 / 2.0,
    );
    let available = mel.first().map_or(0, Vec::len);
    let frames = max_frames.map_or(available, |m| m.min(available));
    let mut flat = Vec::with_capacity(mel.len() * frames);
    for row in &mel {
        flat.extend(row.iter().take(frames));
    }
    Ok((flat, mel.len(), frames, vec![frames as i64]))
}

/// Run the full streaming engine over a buffered waveform in one shot.
///
/// Used for batch (`transcribe`) and final flush (`transcribe_stream`).
/// Mutates sessions and consumes/initialises a fresh cache each call.
pub fn transcribe_buffered(
    sessions: &mut NemotronSessions,
    metadata: &ExportMetadata,
    waveform: &[f32],
    blank_id: i32,
    max_tokens_per_step: usize,
) -> Result<Vec<u32>, SttError> {
    let cfg = metadata.streaming_config;
    // `cfg.chunk` and `cfg.right` are in **subsampled output frames**
    // (per the NeMo cache-aware export). Convert to mel input frames by
    // multiplying by SUBSAMPLING_FACTOR (8x for FastConformer).
    let chunk_frames = cfg.chunk * SUBSAMPLING_FACTOR;
    let right_ctx_frames = cfg.right * SUBSAMPLING_FACTOR;
    let window_frames = chunk_frames + right_ctx_frames;

    let mut cache = CacheTensors::new(&sessions.encoder, &metadata.cache_init_shapes)?;
    let mut decoder_state = DecoderState::new(&sessions.decoder, blank_id)?;
    let (mel_data, mel_bins, total_frames, _) = mel_features_for(sessions, waveform)?;
    if total_frames == 0 {
        return Ok(Vec::new());
    }

    let mut all_tokens: Vec<u32> = Vec::new();
    let mut frame_pos = 0_usize;
    while frame_pos < total_frames {
        let (feat_data, time_steps, feat_lens) =
            slice_mel_chunk(&mel_data, mel_bins, total_frames, frame_pos, window_frames);
        let enc_out = run_encoder_chunk(
            &mut sessions.encoder,
            &feat_data,
            mel_bins,
            time_steps,
            &feat_lens,
            &mut cache,
        )?;
        let tokens = decode_chunk_split(
            &mut sessions.decoder,
            &mut sessions.joint,
            &enc_out,
            &mut decoder_state,
            blank_id,
            max_tokens_per_step,
        )?;
        all_tokens.extend_from_slice(&tokens);
        frame_pos += chunk_frames;
    }
    Ok(all_tokens)
}

/// Per-utterance streaming engine: maintains cache + decoder state across
/// `drain_ready_chunks` calls without holding an ONNX `Session` slot while
/// waiting for incoming network audio.
pub struct StreamingEngine {
    initial_cache: CacheTensors,
    initial_decoder_state: DecoderState,
    pub cache: CacheTensors,
    pub decoder_state: DecoderState,
    pub chunk_frames: usize,
    pub right_ctx_frames: usize,
    pub blank_id: i32,
    pub max_tokens_per_step: usize,
    /// Unprocessed audio tail. The consumed prefix is drained after each
    /// `drain_ready_chunks` so this holds ~one window, not the whole
    /// utterance — the encoder's cross-chunk state lives in `cache`.
    sample_buffer: Vec<f32>,
    /// Read cursor into `sample_buffer`: the centre of the next window's
    /// first frame. `LEFT_CONTEXT` samples before it are kept.
    pub sample_pos: usize,
    /// Samples of this utterance drained from the front of `sample_buffer`,
    /// so `consumed + sample_pos` is the position in the utterance.
    consumed: usize,
    /// Monotonic count of chunks committed to the encoder. Used as the
    /// "did the encoder run" probe (independent of buffer draining).
    pub chunks_processed: usize,
}

impl std::fmt::Debug for StreamingEngine {
    /// Summarises the engine without dumping the audio tail or the encoder
    /// cache tensors (megabytes of floats).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingEngine")
            .field("chunk_frames", &self.chunk_frames)
            .field("right_ctx_frames", &self.right_ctx_frames)
            .field("blank_id", &self.blank_id)
            .field("max_tokens_per_step", &self.max_tokens_per_step)
            .field("buffered_samples", &self.sample_buffer.len())
            .field("sample_pos", &self.sample_pos)
            .field("chunks_processed", &self.chunks_processed)
            .finish_non_exhaustive()
    }
}

impl StreamingEngine {
    pub fn from_templates(
        initial_cache: CacheTensors,
        initial_decoder_state: DecoderState,
        metadata: &ExportMetadata,
        blank_id: i32,
        max_tokens_per_step: usize,
    ) -> Self {
        let cfg = metadata.streaming_config;
        let cache = initial_cache.clone();
        let decoder_state = initial_decoder_state.clone();
        Self {
            initial_cache,
            initial_decoder_state,
            cache,
            decoder_state,
            chunk_frames: cfg.chunk * SUBSAMPLING_FACTOR,
            right_ctx_frames: cfg.right * SUBSAMPLING_FACTOR,
            blank_id,
            max_tokens_per_step,
            sample_buffer: Vec::new(),
            sample_pos: 0,
            consumed: 0,
            chunks_processed: 0,
        }
    }

    pub fn new(
        sessions: &NemotronSessions,
        metadata: &ExportMetadata,
        blank_id: i32,
        max_tokens_per_step: usize,
    ) -> Result<Self, SttError> {
        let initial_cache = CacheTensors::new(&sessions.encoder, &metadata.cache_init_shapes)?;
        let initial_decoder_state = DecoderState::new(&sessions.decoder, blank_id)?;
        Ok(Self::from_templates(
            initial_cache,
            initial_decoder_state,
            metadata,
            blank_id,
            max_tokens_per_step,
        ))
    }

    /// Forget the current utterance: fresh encoder cache and decoder state,
    /// empty audio tail. Requires no ONNX session checkout.
    pub fn reset(&mut self) {
        self.cache.clone_from(&self.initial_cache);
        self.decoder_state.clone_from(&self.initial_decoder_state);
        self.sample_buffer.clear();
        self.sample_pos = 0;
        self.consumed = 0;
    }

    /// Append incoming audio samples to the buffer without checking out an
    /// ONNX session.
    pub fn push_samples(&mut self, samples: &[f32]) {
        self.sample_buffer.extend_from_slice(samples);
    }

    /// Number of buffered samples beyond `sample_pos` required to run one
    /// full streaming encoder chunk.
    fn needed_chunk_samples(&self) -> usize {
        let chunk_samples = self.chunk_frames * HOP_LENGTH;
        let right_samples = self.right_ctx_frames * HOP_LENGTH;
        chunk_samples + right_samples + WIN_LENGTH
    }

    /// Whether enough audio is buffered to execute at least one streaming
    /// encoder step.
    pub fn has_ready_chunk(&self) -> bool {
        self.sample_pos + self.needed_chunk_samples() <= self.sample_buffer.len()
    }

    /// Whether enough trailing audio remains to execute a final flush step.
    pub fn has_flushable_audio(&self) -> bool {
        self.sample_buffer.len().saturating_sub(self.sample_pos) >= WIN_LENGTH
    }

    /// The utterance's audio is spent: whatever arrives next starts a new
    /// one, from its own start.
    pub(crate) fn end_audio(&mut self) {
        self.sample_buffer.clear();
        self.sample_pos = 0;
        self.consumed = 0;
    }

    /// How much audio before `sample_pos` the next window reads: none at
    /// the utterance's start, [`LEFT_CONTEXT`] after it.
    fn left_context(&self) -> usize {
        if self.consumed + self.sample_pos == 0 {
            0
        } else {
            LEFT_CONTEXT
        }
    }

    /// Drain as many full chunks as possible using `sessions`. Returns any
    /// tokens emitted by those chunks.
    pub fn drain_ready_chunks(
        &mut self,
        sessions: &mut NemotronSessions,
    ) -> Result<Vec<u32>, SttError> {
        let chunk_samples = self.chunk_frames * HOP_LENGTH;
        let needed = self.needed_chunk_samples();
        let mut emitted = Vec::new();
        let window_frames = self.chunk_frames + self.right_ctx_frames;
        while self.sample_pos + needed <= self.sample_buffer.len() {
            let left = self.left_context();
            let stretch = buffered(&self.sample_buffer, self.sample_pos, left, Some(needed))?;
            let (feat, mel_bins, time_steps, feat_lens) =
                stretch_mel_features(sessions, stretch, left, false, Some(window_frames))?;
            let enc_out = run_encoder_chunk(
                &mut sessions.encoder,
                &feat,
                mel_bins,
                time_steps,
                &feat_lens,
                &mut self.cache,
            )?;
            let tokens = decode_chunk_split(
                &mut sessions.decoder,
                &mut sessions.joint,
                &enc_out,
                &mut self.decoder_state,
                self.blank_id,
                self.max_tokens_per_step,
            )?;
            emitted.extend_from_slice(&tokens);
            self.sample_pos += chunk_samples;
            self.chunks_processed += 1;
        }
        // Drop the fully-consumed prefix so the buffer holds only the
        // unprocessed tail instead of growing for the whole utterance. The next
        // read starts at `sample_pos`, so nothing before it is ever re-read.
        if self.sample_pos > LEFT_CONTEXT {
            let drop = self.sample_pos - LEFT_CONTEXT;
            self.sample_buffer.drain(..drop);
            self.consumed += drop;
            self.sample_pos = LEFT_CONTEXT;
        }
        Ok(emitted)
    }

    /// Append samples and drain as many full chunks as possible. Returns
    /// any tokens emitted by those chunks.
    pub fn feed_samples(
        &mut self,
        sessions: &mut NemotronSessions,
        samples: &[f32],
    ) -> Result<Vec<u32>, SttError> {
        self.push_samples(samples);
        self.drain_ready_chunks(sessions)
    }

    /// Flush the trailing audio at end-of-utterance. Drains all remaining
    /// frames in fixed-size mel chunks (zero-padded if needed) so the
    /// encoder sees the same shape it was traced for. This is what makes
    /// short utterances ("hi", "hello") transcribe at all -- the streaming
    /// `feed_samples` loop only fires when ~685 ms of audio is buffered;
    /// for anything shorter, all the work happens here.
    pub fn flush(&mut self, sessions: &mut NemotronSessions) -> Result<Vec<u32>, SttError> {
        if !self.has_flushable_audio() {
            self.end_audio();
            return Ok(Vec::new());
        }
        let left = self.left_context();
        let stretch = buffered(&self.sample_buffer, self.sample_pos, left, None)?;
        let (mel_data, mel_bins, total_frames, _) =
            stretch_mel_features(sessions, stretch, left, true, None)?;
        if total_frames == 0 {
            self.end_audio();
            return Ok(Vec::new());
        }
        let window_frames = self.chunk_frames + self.right_ctx_frames;
        let mut emitted: Vec<u32> = Vec::new();
        let mut frame_pos = 0_usize;
        while frame_pos < total_frames {
            let (feat_data, time_steps, feat_lens) =
                slice_mel_chunk(&mel_data, mel_bins, total_frames, frame_pos, window_frames);
            let enc_out = run_encoder_chunk(
                &mut sessions.encoder,
                &feat_data,
                mel_bins,
                time_steps,
                &feat_lens,
                &mut self.cache,
            )?;
            let tokens = decode_chunk_split(
                &mut sessions.decoder,
                &mut sessions.joint,
                &enc_out,
                &mut self.decoder_state,
                self.blank_id,
                self.max_tokens_per_step,
            )?;
            emitted.extend_from_slice(&tokens);
            frame_pos += self.chunk_frames;
        }
        self.end_audio();
        Ok(emitted)
    }
}

/// A model output the engine could not read (too few outputs, a negative
/// length), as an inference failure.
fn model_output(e: voice_pipes_onnx::ModelOutputError) -> SttError {
    SttError::InferenceFailed(e.to_string())
}

/// The buffered audio from `left` samples before `pos` to `len` samples
/// after it (or to the end): the stretch a window's features read.
fn buffered(
    buffer: &[f32],
    pos: usize,
    left: usize,
    len: Option<usize>,
) -> Result<&[f32], SttError> {
    let from = pos.checked_sub(left);
    let stretch = match (from, len) {
        (Some(from), Some(len)) => buffer.get(from..pos + len),
        (Some(from), None) => buffer.get(from..),
        (None, _) => None,
    };
    stretch.ok_or_else(|| SttError::InferenceFailed("the window's audio is not buffered".into()))
}

#[cfg(test)]
#[path = "tests/streaming.rs"]
mod tests;
