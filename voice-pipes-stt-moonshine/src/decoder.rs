//! Moonshine decoder -- ONNX runtime session wrapping the decoder model
//! and token decoding using the tokenizers crate.
//!
//! Two exports are supported. `decoder_model_merged.onnx` carries a
//! key/value cache: the first step runs without it and returns the
//! cross-attention cache for the whole utterance, every later step feeds one
//! token plus the caches, so a step costs the same however long the
//! transcript is. `decoder_model.onnx` has no cache and re-runs the whole
//! token prefix every step; it is kept for model directories that predate
//! the merged export. Both produce the same tokens.

use super::encoder::EncoderOutput;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::{DynValue, Tensor, TensorRef};
use std::borrow::Cow;
use std::path::Path;
use tokenizers::Tokenizer;
use voice_pipes_sdk::SttError;

const BOS_TOKEN_ID: i64 = 1;
const EOS_TOKEN_ID: i64 = 2;

/// One cache input of the merged export and the output that feeds it next.
#[derive(Debug)]
struct CacheSlot {
    /// `past_key_values.{layer}.{decoder|encoder}.{key|value}`.
    past: String,
    /// `present.{layer}.{decoder|encoder}.{key|value}`.
    present: String,
    /// Cross-attention cache: produced once by the first step, then reused.
    cross: bool,
}

/// How the decoder session is driven.
#[derive(Debug)]
enum DecodeMode {
    /// `decoder_model.onnx`: the whole token prefix every step.
    FullPrefix,
    /// `decoder_model_merged.onnx`: one token per step with the caches.
    Cached {
        slots: Vec<CacheSlot>,
        heads: usize,
        head_dim: usize,
    },
}

/// ONNX-backed decoder wrapper for Useful Sensors Moonshine STT.
#[derive(Debug)]
pub(crate) struct MoonshineDecoder {
    session: Session,
    tokenizer: Tokenizer,
    mode: DecodeMode,
}

/// Read the cache layout of a merged export from its inputs; `FullPrefix`
/// when the session has no `use_cache_branch` input.
fn decode_mode(session: &Session) -> Result<DecodeMode, SttError> {
    if !session
        .inputs()
        .iter()
        .any(|i| i.name() == "use_cache_branch")
    {
        return Ok(DecodeMode::FullPrefix);
    }
    let mut slots = Vec::new();
    let mut dims: Option<(usize, usize)> = None;
    for input in session.inputs() {
        let name = input.name();
        if !name.starts_with("past_key_values.") {
            continue;
        }
        // [batch, heads, sequence, head_dim]; heads and head_dim are fixed.
        if let ort::value::ValueType::Tensor { shape, .. } = input.dtype()
            && let (Some(&h), Some(&d)) = (shape.get(1), shape.get(3))
            && let (Ok(h), Ok(d)) = (usize::try_from(h), usize::try_from(d))
            && h > 0
            && d > 0
        {
            dims.get_or_insert((h, d));
        }
        slots.push(CacheSlot {
            past: name.to_string(),
            present: name.replacen("past_key_values", "present", 1),
            cross: name.contains(".encoder."),
        });
    }
    let (heads, head_dim) = dims.ok_or_else(|| {
        SttError::ModelLoadFailed("Moonshine merged decoder: no cache input shape".into())
    })?;
    if slots.is_empty() {
        return Err(SttError::ModelLoadFailed(
            "Moonshine merged decoder: no past_key_values inputs".into(),
        ));
    }
    Ok(DecodeMode::Cached {
        slots,
        heads,
        head_dim,
    })
}

/// Index of the largest logit in the last position of `[1, seq, vocab]`.
fn argmax_last(shape: &[i64], logits: &[f32]) -> Result<i64, SttError> {
    // Model outputs are untrusted shapes; a 2-D logits tensor from a
    // re-exported model must be an error, not a panic.
    let vocab_size = shape
        .get(2)
        .and_then(|&v| usize::try_from(v).ok())
        .filter(|&v| v > 0)
        .ok_or_else(|| {
            SttError::InferenceFailed(format!("Moonshine decoder logits shape {shape:?}"))
        })?;
    let seq = shape
        .get(1)
        .and_then(|&s| usize::try_from(s).ok())
        .unwrap_or(0);
    let last = logits
        .get(seq.saturating_sub(1) * vocab_size..seq * vocab_size)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| {
            SttError::InferenceFailed("Moonshine decoder logits shorter than shape".into())
        })?;
    let token = voice_pipes_onnx::argmax(last).ok_or_else(|| {
        SttError::InferenceFailed("Moonshine decoder logits hold no usable value".into())
    })?;
    i64::try_from(token)
        .map_err(|e| SttError::InferenceFailed(format!("Moonshine decoder vocabulary: {e}")))
}

/// The logits of a decoder run; both exports name the output `logits`.
fn logits<'s>(outputs: &'s SessionOutputs<'_>) -> Result<(&'s [i64], &'s [f32]), SttError> {
    let value = outputs
        .get("logits")
        .ok_or_else(|| SttError::InferenceFailed("Moonshine decoder returned no logits".into()))?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| SttError::InferenceFailed(format!("Moonshine decoder extract logits: {e}")))?;
    Ok((shape, data))
}

/// A token id as the tokenizer takes it.
fn token(id: i64) -> Result<u32, SttError> {
    u32::try_from(id).map_err(|e| {
        SttError::InferenceFailed(format!("Moonshine decoder token {id} out of range: {e}"))
    })
}

impl MoonshineDecoder {
    /// Load the decoder session and tokenizer from the model directory.
    pub(crate) fn new(
        model_path: &Path,
        tokenizer_path: &Path,
        threads: usize,
    ) -> Result<Self, SttError> {
        let session = crate::open_session(model_path, threads, "decoder")?;

        let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| {
            SttError::ModelLoadFailed(format!(
                "failed to load tokenizer from {}: {e}",
                tokenizer_path.display()
            ))
        })?;

        let mode = decode_mode(&session)?;
        tracing::info!(
            model = %model_path.display(),
            cached = matches!(mode, DecodeMode::Cached { .. }),
            "Moonshine decoder loaded"
        );

        Ok(Self {
            session,
            tokenizer,
            mode,
        })
    }

    /// Whether this decoder runs with a key/value cache.
    pub(crate) fn is_cached(&self) -> bool {
        matches!(self.mode, DecodeMode::Cached { .. })
    }

    /// Autoregressively decode the next tokens given encoder hidden states.
    pub(crate) fn decode(
        &mut self,
        encoder_out: &EncoderOutput,
        max_new_tokens: usize,
    ) -> Result<Vec<u32>, SttError> {
        if self.is_cached() {
            self.decode_cached(encoder_out, max_new_tokens, true)
        } else {
            self.decode_full_prefix(encoder_out, max_new_tokens)
        }
    }

    /// Run a few decode steps whatever the model predicts, so every path
    /// the session has pays its first-run cost here: silence alone decodes
    /// straight to end-of-text and would leave the cached steps cold.
    pub(crate) fn warm_up(&mut self, encoder_out: &EncoderOutput) -> Result<(), SttError> {
        if self.is_cached() {
            self.decode_cached(encoder_out, 3, false).map(|_| ())
        } else {
            self.decode_full_prefix(encoder_out, 1).map(|_| ())
        }
    }

    fn decode_cached(
        &mut self,
        encoder_out: &EncoderOutput,
        max_new_tokens: usize,
        stop_at_eos: bool,
    ) -> Result<Vec<u32>, SttError> {
        let DecodeMode::Cached {
            slots,
            heads,
            head_dim,
        } = &self.mode
        else {
            return Err(SttError::InferenceFailed(
                "Moonshine decoder: the full-prefix export has no cache".into(),
            ));
        };
        let err = |what: &str| {
            let what = what.to_string();
            move |e: ort::Error| SttError::InferenceFailed(format!("Moonshine decoder {what}: {e}"))
        };

        // Step one runs without a cache: empty past tensors, sequence 0.
        // A zero-length tensor cannot be built from raw data; the runtime's
        // allocator makes one.
        let allocator = ort::memory::Allocator::default();
        let mut past: Vec<DynValue> = Vec::with_capacity(slots.len());
        for _ in slots {
            let empty = Tensor::<f32>::new(&allocator, [1usize, *heads, 0, *head_dim])
                .map_err(err("empty cache tensor"))?;
            past.push(empty.into_dyn());
        }

        let mut tokens: Vec<u32> = Vec::new();
        let mut next_input = BOS_TOKEN_ID;
        let mut use_cache = false;
        for _ in 0..max_new_tokens {
            let hidden = TensorRef::from_array_view((
                encoder_out.shape.as_slice(),
                encoder_out.hidden_states.as_slice(),
            ))
            .map_err(err("encoder_hidden_states tensor"))?;
            let ids = Tensor::from_array(([1usize, 1usize], vec![next_input]))
                .map_err(err("input_ids tensor"))?;
            let branch = Tensor::from_array(([1usize], vec![use_cache]))
                .map_err(err("use_cache_branch tensor"))?;

            let mut inputs: Vec<(Cow<'_, str>, SessionInputValue<'_>)> =
                Vec::with_capacity(slots.len() + 3);
            inputs.push(("input_ids".into(), ids.into()));
            inputs.push(("encoder_hidden_states".into(), hidden.into()));
            inputs.push(("use_cache_branch".into(), branch.into()));
            for (slot, value) in slots.iter().zip(&past) {
                inputs.push((slot.past.as_str().into(), value.view().into()));
            }

            let mut outputs = self.session.run(inputs).map_err(err("run"))?;
            let next = {
                let (shape, logits) = logits(&outputs)?;
                argmax_last(shape, logits)?
            };
            // The self-attention cache grows every step; the cross-attention
            // cache comes from the first step and is kept as it is.
            for (slot, value) in slots.iter().zip(past.iter_mut()) {
                if slot.cross && use_cache {
                    continue;
                }
                *value = outputs.remove(slot.present.as_str()).ok_or_else(|| {
                    SttError::InferenceFailed(format!(
                        "Moonshine decoder output {} missing",
                        slot.present
                    ))
                })?;
            }
            use_cache = true;

            if next == EOS_TOKEN_ID && stop_at_eos {
                break;
            }
            tokens.push(token(next)?);
            next_input = next;
        }
        Ok(tokens)
    }

    fn decode_full_prefix(
        &mut self,
        encoder_out: &EncoderOutput,
        max_new_tokens: usize,
    ) -> Result<Vec<u32>, SttError> {
        let mut tokens = vec![BOS_TOKEN_ID];

        let enc_hidden_states = Tensor::from_array((
            encoder_out.shape.as_slice(),
            encoder_out.hidden_states.clone(),
        ))
        .map_err(|e| {
            SttError::InferenceFailed(format!(
                "Moonshine decoder encoder_hidden_states tensor: {e}"
            ))
        })?;

        for _ in 0..max_new_tokens {
            let k = tokens.len();
            let input_ids_tensor =
                Tensor::from_array(([1usize, k], tokens.clone())).map_err(|e| {
                    SttError::InferenceFailed(format!("Moonshine decoder input_ids tensor: {e}"))
                })?;

            // Run decoder model
            let outputs = self
                .session
                .run(ort::inputs![
                    "input_ids" => input_ids_tensor,
                    "encoder_hidden_states" => enc_hidden_states.clone(),
                ])
                .map_err(|e| SttError::InferenceFailed(format!("Moonshine decoder run: {e}")))?;

            // Logits: shape [batch_size (1), seq_len (k), vocab_size]
            let (shape, logits_data) = logits(&outputs)?;
            let next_token = argmax_last(shape, logits_data)?;
            if next_token == EOS_TOKEN_ID {
                break;
            }
            tokens.push(next_token);
        }

        // Return tokens excluding the initial BOS token
        tokens.iter().skip(1).map(|&t| token(t)).collect()
    }

    /// Detokenize the predicted token IDs back to a text string.
    pub(crate) fn detokenize(&self, tokens: &[u32]) -> Result<String, SttError> {
        let text = self
            .tokenizer
            .decode(tokens, true)
            .map_err(|e| SttError::InferenceFailed(format!("Moonshine detokenize failed: {e}")))?;
        Ok(text)
    }
}
