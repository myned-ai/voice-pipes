//! Parakeet TDT decoder -- token-and-duration transducer.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use tracing::debug;

use voice_pipes_sdk::{SttError, WordInfo};

use voice_pipes_onnx::argmax;

use super::encoder::EncoderOutput;

/// `NeMo` preprocessor hop size: 10 ms per mel frame.
const MEL_FRAME_SHIFT_MS: f64 = 10.0;
/// `FastConformer` encoder temporal downsampling factor.
const ENCODER_DOWNSAMPLE: f64 = 8.0;
/// Duration of one encoder frame in seconds.
const ENCODER_FRAME_SECS: f64 = MEL_FRAME_SHIFT_MS * ENCODER_DOWNSAMPLE / 1000.0;

/// A decoded token with its encoder frame index (for timestamp estimation).
#[derive(Debug, Clone)]
pub struct DecodedToken {
    pub token_id: u32,
    pub frame_index: usize,
}

/// Internal RNN decoder state (opaque to callers).
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_field_names,
    reason = "the model's input_states_1 and input_states_2, named as it names them"
)]
pub struct DecoderState {
    pub states_1: Vec<f32>,
    pub states_1_shape: Vec<usize>,
    pub states_2: Vec<f32>,
    pub states_2_shape: Vec<usize>,
}

/// TDT decoder + joint network for Parakeet TDT.
///
/// Performs greedy frame-by-frame decoding over encoder embeddings.
/// TDT predicts both token and duration -- how many encoder frames to skip --
/// giving it a speed advantage over standard RNN-T.
pub struct TdtDecoder {
    session: Session,
    vocab: HashMap<u32, String>,
    blank_idx: usize,
    vocab_size: usize,
    /// Max tokens to emit per encoder frame before forced advance.
    max_tokens_per_step: usize,
}

impl std::fmt::Debug for TdtDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TdtDecoder")
            .field("vocab_size", &self.vocab_size)
            .field("blank_idx", &self.blank_idx)
            .field("max_tokens_per_step", &self.max_tokens_per_step)
            .finish_non_exhaustive()
    }
}

impl TdtDecoder {
    /// Load the decoder+joint ONNX model and BPE vocabulary.
    pub fn new(model_path: &Path, vocab_path: &Path, threads: usize) -> Result<Self, SttError> {
        let session = crate::open_session(model_path, threads)?;

        let vocab = load_vocab(vocab_path)?;
        let vocab_size = vocab.len();

        let blank_idx = vocab
            .iter()
            .find(|(_, v)| v.as_str() == "<blk>")
            .map_or(vocab_size, |(k, _)| *k as usize); // blank is often the last token

        debug!(vocab_size, blank_idx, "TDT decoder loaded");

        Ok(Self {
            session,
            vocab,
            blank_idx,
            vocab_size,
            max_tokens_per_step: 10,
        })
    }

    /// Zero-initialized decoder state, in the shapes of the 0.6B model.
    pub fn initial_state() -> DecoderState {
        // Inspect ONNX input shapes for state dimensions
        // Use fixed initial state shapes for the 0.6B model.
        // The TDT decoder's LSTM states are [layers, batch, hidden_dim].
        // Parakeet-TDT has a 2-layer LSTM decoder with hidden_dim=640.
        let (s1_shape, s2_shape) = (vec![2usize, 1, 640], vec![2usize, 1, 640]);

        let s1_len: usize = s1_shape.iter().product();
        let s2_len: usize = s2_shape.iter().product();

        DecoderState {
            states_1: vec![0.0f32; s1_len],
            states_1_shape: s1_shape,
            states_2: vec![0.0f32; s2_len],
            states_2_shape: s2_shape,
        }
    }

    /// Run one decoder step: given encoder output at frame `t`, previous token,
    /// and decoder state -> produce logits and new state.
    fn step(
        &mut self,
        encoder_out: &EncoderOutput,
        frame: usize,
        prev_token: i64,
        state: DecoderState,
    ) -> Result<(Vec<f32>, DecoderState), SttError> {
        let d = encoder_out.embed_dim;

        // encoder_outputs: [1, D, 1] -- single frame, transposed
        let frame_data: Vec<f32> = encoder_out
            .embeddings
            .get(frame * d..(frame + 1) * d)
            .ok_or_else(|| {
                SttError::InferenceFailed(format!("encoder frame {frame} is past its output"))
            })?
            .to_vec();
        let enc_tensor = Tensor::from_array(([1usize, d, 1usize], frame_data))
            .map_err(|e| SttError::InferenceFailed(format!("decoder enc tensor: {e}")))?;

        // targets: [[prev_token]] -- model expects i32
        let prev_token = i32::try_from(prev_token).map_err(|_too_large| {
            SttError::InferenceFailed(format!("token {prev_token} is past the model's range"))
        })?;
        let targets_tensor = Tensor::from_array(([1usize, 1usize], vec![prev_token]))
            .map_err(|e| SttError::InferenceFailed(format!("decoder targets tensor: {e}")))?;

        // target_length: [1] -- model expects i32
        let target_len_tensor = Tensor::from_array(([1usize], vec![1i32]))
            .map_err(|e| SttError::InferenceFailed(format!("decoder target_len tensor: {e}")))?;

        // input_states_1, input_states_2
        let s1_tensor = Tensor::from_array((state.states_1_shape.as_slice(), state.states_1))
            .map_err(|e| SttError::InferenceFailed(format!("decoder s1 tensor: {e}")))?;
        let s2_tensor = Tensor::from_array((state.states_2_shape.as_slice(), state.states_2))
            .map_err(|e| SttError::InferenceFailed(format!("decoder s2 tensor: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![
                enc_tensor,
                targets_tensor,
                target_len_tensor,
                s1_tensor,
                s2_tensor
            ])
            .map_err(|e| SttError::InferenceFailed(format!("decoder run: {e}")))?;

        // Output 0: logits [vocab_size + num_durations]
        let logits_tensor_value =
            voice_pipes_onnx::nth_output(&outputs, 0, "decoder").map_err(crate::model_output)?;
        let logits_tensor = logits_tensor_value
            .try_extract_tensor::<f32>()
            .map_err(|e| SttError::InferenceFailed(format!("decoder extract logits: {e}")))?;
        let logits: Vec<f32> = logits_tensor.1.to_vec();

        // Output 1: prednet_lengths (i32, skip)

        // Output 2: output_states_1
        let os1_value =
            voice_pipes_onnx::nth_output(&outputs, 2, "decoder").map_err(crate::model_output)?;
        let os1 = os1_value
            .try_extract_tensor::<f32>()
            .map_err(|e| SttError::InferenceFailed(format!("decoder extract s1: {e}")))?;
        let os1_shape = os1
            .0
            .iter()
            .map(|&d| voice_pipes_onnx::dim(d, "a state axis"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(crate::model_output)?;
        let os1_data: Vec<f32> = os1.1.to_vec();

        // Output 3: output_states_2
        let os2_value =
            voice_pipes_onnx::nth_output(&outputs, 3, "decoder").map_err(crate::model_output)?;
        let os2 = os2_value
            .try_extract_tensor::<f32>()
            .map_err(|e| SttError::InferenceFailed(format!("decoder extract s2: {e}")))?;
        let os2_shape = os2
            .0
            .iter()
            .map(|&d| voice_pipes_onnx::dim(d, "a state axis"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(crate::model_output)?;
        let os2_data: Vec<f32> = os2.1.to_vec();

        let new_state = DecoderState {
            states_1: os1_data,
            states_1_shape: os1_shape,
            states_2: os2_data,
            states_2_shape: os2_shape,
        };

        Ok((logits, new_state))
    }

    /// Greedy TDT decode over the full encoder output.
    ///
    /// Returns decoded tokens with frame indices (for timestamp estimation).
    pub fn decode(&mut self, encoder_out: &EncoderOutput) -> Result<Vec<DecodedToken>, SttError> {
        // Model outputs are untrusted shapes (users drop in
        // re-exported models); never index or slice past what is there.
        let reported = encoder_out
            .encoded_lengths
            .first()
            .copied()
            .ok_or_else(|| {
                SttError::InferenceFailed("encoder returned no encoded_lengths".into())
            })?;
        let available = encoder_out
            .embeddings
            .len()
            .checked_div(encoder_out.embed_dim)
            .unwrap_or(0);
        let t_max = usize::try_from(reported.max(0)).unwrap_or(0).min(available);

        let params = GreedyParams {
            vocab_size: self.vocab_size,
            blank_idx: self.blank_idx,
            max_tokens_per_step: self.max_tokens_per_step,
        };
        let initial = Self::initial_state();
        let mut stepper = OnnxStep {
            decoder: self,
            encoder_out,
        };
        let tokens = greedy_decode(&mut stepper, initial, t_max, &params)?;

        debug!(
            token_count = tokens.len(),
            frames = t_max,
            "TDT decode complete"
        );
        Ok(tokens)
    }

    /// Detokenize with per-word timestamps derived from encoder frame indices.
    ///
    /// Groups BPE sub-tokens into words (split on `▁` boundaries) and computes
    /// start/end times using the known encoder frame duration.
    pub fn detokenize_with_timestamps(
        &self,
        tokens: &[DecodedToken],
        total_audio_secs: f64,
    ) -> (String, Vec<WordInfo>) {
        detokenize_with_timestamps_impl(&self.vocab, tokens, total_audio_secs)
    }
}

/// One decoder+joint evaluation: the network behind the greedy walk.
///
/// Abstracted from the ONNX session so the walk itself can be exercised
/// with scripted logits.
pub(crate) trait Step {
    /// Logits for encoder frame `frame` given the previous token and the
    /// decoder state, plus the state after consuming `prev_token`.
    fn step(
        &mut self,
        frame: usize,
        prev_token: i64,
        state: DecoderState,
    ) -> Result<(Vec<f32>, DecoderState), SttError>;
}

/// The real decoder+joint session over one encoder output.
struct OnnxStep<'a> {
    decoder: &'a mut TdtDecoder,
    encoder_out: &'a EncoderOutput,
}

impl Step for OnnxStep<'_> {
    fn step(
        &mut self,
        frame: usize,
        prev_token: i64,
        state: DecoderState,
    ) -> Result<(Vec<f32>, DecoderState), SttError> {
        self.decoder
            .step(self.encoder_out, frame, prev_token, state)
    }
}

/// Shape of the joint output and the loop guards for [`greedy_decode`].
pub(crate) struct GreedyParams {
    /// Logits `[..vocab_size]` are token scores; the rest are duration
    /// scores (index = number of encoder frames to advance).
    pub vocab_size: usize,
    pub blank_idx: usize,
    /// Tokens emitted on one frame before the walk is forced forward.
    pub max_tokens_per_step: usize,
}

/// Greedy token-and-duration walk over `t_max` encoder frames.
///
/// Every step predicts a token and a duration. The duration says how many
/// frames the walk advances after this step, for blank and non-blank
/// alike: a token with duration 3 means "this token covers three frames",
/// so the next step looks at frame `t + 3`. Only a duration of zero keeps
/// the walk on the same frame, and then only for a non-blank (a blank
/// with zero duration must still move, or nothing ever would). A frame
/// that keeps emitting zero-duration tokens is left after
/// `max_tokens_per_step` of them.
pub(crate) fn greedy_decode<S: Step>(
    stepper: &mut S,
    initial: DecoderState,
    t_max: usize,
    params: &GreedyParams,
) -> Result<Vec<DecodedToken>, SttError> {
    let mut state = initial;
    let mut tokens: Vec<DecodedToken> = Vec::new();
    let mut prev_token: i64 = params.blank_idx as i64;
    let mut t: usize = 0;
    let mut emitted_at_frame = 0usize;

    while t < t_max {
        // Cloned, not moved: on a blank the walk keeps this state, so it must
        // survive the call. Two LSTM tensors, ~10 KB, beside an ONNX run of
        // the decoder and joint on the same frame.
        let (logits, new_state) = stepper.step(t, prev_token, state.clone())?;
        if logits.len() < params.vocab_size {
            return Err(SttError::InferenceFailed(format!(
                "joint returned {} logits, expected at least {}",
                logits.len(),
                params.vocab_size
            )));
        }

        // The length was checked above, so the split is inside the logits.
        let (token_logits, duration_logits) = logits.split_at(params.vocab_size);
        let token = argmax(token_logits).ok_or_else(|| {
            SttError::InferenceFailed("joint returned no usable token logit".into())
        })?;
        let duration = if duration_logits.is_empty() {
            // No duration head: plain transducer, one frame per blank.
            0
        } else {
            argmax(duration_logits).ok_or_else(|| {
                SttError::InferenceFailed("joint returned no usable duration logit".into())
            })?
        };

        if token == params.blank_idx {
            // A blank says nothing, so the prediction network must not move:
            // `new_state` is the state after consuming `prev_token` *again*,
            // and adopting it here feeds the last token in once more on every
            // frame of silence. A second of lead-in before speech leaves the
            // state dozens of steps from where it belongs, and the joint then
            // predicts blanks through the first words while it recovers --
            // which is how "Hello, how are you?" arrived as "o, how are you?".
            // NeMo's greedy TDT likewise keeps `hidden` on a blank and takes
            // `hidden_prime` only on an emission.
            t += duration.max(1);
            emitted_at_frame = 0;
            continue;
        }
        state = new_state;

        let token_id = u32::try_from(token).map_err(|_too_large| {
            SttError::InferenceFailed(format!("token {token} is past the vocabulary's range"))
        })?;
        tokens.push(DecodedToken {
            token_id,
            frame_index: t,
        });
        prev_token = i64::from(token_id);

        if duration > 0 {
            t += duration;
            emitted_at_frame = 0;
        } else {
            emitted_at_frame += 1;
            if emitted_at_frame >= params.max_tokens_per_step {
                t += 1;
                emitted_at_frame = 0;
            }
        }
    }
    Ok(tokens)
}

/// Detokenize BPE tokens into a text string plus per-word `WordInfo` timestamps.
///
/// Tokens whose vocabulary piece starts with `▁` (U+2581) mark the beginning of
/// a new word.  We group consecutive sub-tokens into words and derive timing from
/// the first sub-token's `frame_index`.
fn detokenize_with_timestamps_impl(
    vocab: &HashMap<u32, String>,
    tokens: &[DecodedToken],
    total_audio_secs: f64,
) -> (String, Vec<WordInfo>) {
    use std::time::Duration;

    if tokens.is_empty() {
        return (String::new(), Vec::new());
    }

    // Collect (piece_text, frame_index) for each token, skipping unknowns.
    let pieces: Vec<(&str, usize)> = tokens
        .iter()
        .filter_map(|tok| {
            vocab
                .get(&tok.token_id)
                .map(|p| (p.as_str(), tok.frame_index))
        })
        .collect();

    if pieces.is_empty() {
        return (String::new(), Vec::new());
    }

    // Group sub-tokens into words.  A piece starting with ▁ begins a new word.
    struct WordAcc {
        text: String,
        start_frame: usize,
        last_frame: usize,
    }
    let mut words: Vec<WordAcc> = Vec::new();

    for (piece, frame) in &pieces {
        let is_boundary = piece.starts_with('\u{2581}');
        let clean = piece.replace('\u{2581}', "");
        if clean.is_empty() {
            continue;
        }
        if is_boundary || words.is_empty() {
            words.push(WordAcc {
                text: clean,
                start_frame: *frame,
                last_frame: *frame,
            });
        } else if let Some(last) = words.last_mut() {
            last.text.push_str(&clean);
            last.last_frame = *frame;
        }
    }

    let full_text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let word_infos: Vec<WordInfo> = words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let start_secs = w.start_frame as f64 * ENCODER_FRAME_SECS;
            // End = next word's start, or total audio duration for the last word.
            let end_secs = words.get(i + 1).map_or(total_audio_secs, |next| {
                next.start_frame as f64 * ENCODER_FRAME_SECS
            });
            WordInfo::new(
                w.text.clone(),
                Duration::from_secs_f64(start_secs),
                Duration::from_secs_f64(end_secs),
            )
        })
        .collect();

    (full_text, word_infos)
}

/// Load BPE vocabulary from vocab.txt.
///
/// Format: `token id` per line (space-separated). Last column is the integer ID.
/// Special tokens: `<blk>` (blank), `▁` (word boundary / space).
fn load_vocab(path: &Path) -> Result<HashMap<u32, String>, SttError> {
    let file =
        std::fs::File::open(path).map_err(|e| SttError::ConfigError(format!("vocab file: {e}")))?;
    let reader = std::io::BufReader::new(file);

    let mut vocab = HashMap::new();
    for (line_num, line) in reader.lines().enumerate() {
        let line =
            line.map_err(|e| SttError::ConfigError(format!("vocab read line {line_num}: {e}")))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Format: "token id" -- split on last space to get token and numeric ID
        if let Some(last_space) = line.rfind(' ') {
            let token = &line[..last_space];
            let id_str = &line[last_space + 1..];
            if let Ok(id) = id_str.parse::<u32>() {
                vocab.insert(id, token.to_string());
                continue;
            }
        }
        // Fallback: use line number as ID (single-column format)
        let id = u32::try_from(line_num).map_err(|_too_many| {
            SttError::ConfigError(format!("vocab has more than {} lines", u32::MAX))
        })?;
        vocab.insert(id, line.to_string());
    }

    if vocab.is_empty() {
        return Err(SttError::ConfigError("empty vocabulary".into()));
    }

    Ok(vocab)
}

#[cfg(test)]
#[path = "tests/decoder.rs"]
mod tests;
