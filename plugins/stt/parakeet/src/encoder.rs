//! Parakeet encoder -- `FastConformer` feature extraction.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

use voice_pipes_sdk::SttError;

/// Output from the `FastConformer` encoder.
#[derive(Debug)]
pub struct EncoderOutput {
    /// Encoder embeddings, flattened from shape [1, T', D].
    pub embeddings: Vec<f32>,
    /// Embedding dimension (D).
    pub embed_dim: usize,
    /// Encoded lengths per batch element.
    pub encoded_lengths: Vec<i64>,
}

/// ONNX-based `FastConformer` encoder for Parakeet TDT.
///
/// Takes 128-dim log-mel features from `MelPreprocessor` and produces
/// encoder embeddings (8x temporally downsampled).
pub struct ParakeetEncoder {
    session: Session,
}

impl std::fmt::Debug for ParakeetEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParakeetEncoder").finish_non_exhaustive()
    }
}

impl ParakeetEncoder {
    /// Load the encoder ONNX model.
    ///
    /// **Quantization guard (do not change without re-running WER eval):**
    /// We rely on the encoder model file being mixed-precision INT8
    /// (matmul operands quantized; activations / accumulators in
    /// floating point). Per the Microsoft on-device streaming ASR paper
    /// (arXiv:2604.14493v2  5.4 / Table 6), folding `MatMul` and `Conv`
    /// into the integer-only `MatMulInteger` / `ConvInteger` ONNX
    /// counterparts degrades WER from ~8.20 -> ~10.14 on Librispeech.
    /// Therefore:
    ///   * Do **not** add ORT graph-optimization passes that perform
    ///     this folding (e.g. an aggressive `Level3` optimization with
    ///     integer-arithmetic transforms enabled).
    ///   * Do **not** swap to a model file that was re-exported with an
    ///     "integer-only" quantization preset.
    ///     Any change here must be paired with a streaming-WER + BSF
    ///     regression run.
    pub fn new(model_path: &Path, threads: usize) -> Result<Self, SttError> {
        let session = crate::open_session(model_path, threads)?;
        Ok(Self { session })
    }

    /// Encode mel features -> encoder embeddings.
    ///
    /// Input:  `audio_signal` [1, `mel_bins`, T], length [1]
    /// Output: embeddings [1, T', D] (8x downsampled), `encoded_lengths` [1]
    pub fn encode(
        &mut self,
        features: &[f32],
        mel_bins: usize,
        time_steps: usize,
        features_lens: &[i64],
    ) -> Result<EncoderOutput, SttError> {
        // audio_signal: [1, mel_bins, T]
        let signal_tensor = Tensor::from_array(([1usize, mel_bins, time_steps], features.to_vec()))
            .map_err(|e| SttError::InferenceFailed(format!("encoder signal tensor: {e}")))?;

        // length: [1]
        let length_tensor = Tensor::from_array(([1usize], features_lens.to_vec()))
            .map_err(|e| SttError::InferenceFailed(format!("encoder length tensor: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![signal_tensor, length_tensor])
            .map_err(|e| SttError::InferenceFailed(format!("encoder run: {e}")))?;

        // Output 0: encoder embeddings -- may be [1, D, T'] or [1, T', D]
        let embed_tensor_value =
            voice_pipes_onnx::nth_output(&outputs, 0, "encoder").map_err(crate::model_output)?;
        let embed_tensor = embed_tensor_value
            .try_extract_tensor::<f32>()
            .map_err(|e| SttError::InferenceFailed(format!("encoder extract embeddings: {e}")))?;
        let embed_shape = &embed_tensor.0;
        let embed_data: Vec<f32> = embed_tensor.1.to_vec();

        // Output 1: encoded_lengths [1]
        let lens_tensor_value =
            voice_pipes_onnx::nth_output(&outputs, 1, "encoder").map_err(crate::model_output)?;
        let lens_tensor = lens_tensor_value
            .try_extract_tensor::<i64>()
            .map_err(|e| SttError::InferenceFailed(format!("encoder extract lens: {e}")))?;
        let encoded_lengths: Vec<i64> = lens_tensor.1.to_vec();

        // Determine dimensions -- onnx-asr reference transposes to [B, T', D]
        // Shape from ONNX may be [1, D, T'] -- we need [1, T', D] for decoding
        let (Some(&d1), Some(&d2)) = (embed_shape.get(1), embed_shape.get(2)) else {
            return Err(SttError::InferenceFailed(
                "unexpected encoder output shape".into(),
            ));
        };
        let (d1, d2) = (
            voice_pipes_onnx::dim(d1, "an embedding axis").map_err(crate::model_output)?,
            voice_pipes_onnx::dim(d2, "an embedding axis").map_err(crate::model_output)?,
        );
        // The encoded_lengths tells us T' -- use it to figure out axis order.
        let expected_t = voice_pipes_onnx::dim(
            encoded_lengths.first().copied().ok_or_else(|| {
                SttError::InferenceFailed("encoder returned no encoded_lengths".into())
            })?,
            "an encoded length",
        )
        .map_err(crate::model_output)?;
        // [1, T', D] as it is, or [1, D, T'] to transpose.
        let channel_first = d1 != expected_t;
        let (time_steps_out, embed_dim) = if channel_first { (d2, d1) } else { (d1, d2) };
        if Some(embed_data.len()) != time_steps_out.checked_mul(embed_dim) {
            return Err(SttError::InferenceFailed(format!(
                "encoder returned {} values for a {time_steps_out} x {embed_dim} output",
                embed_data.len()
            )));
        }

        // Ensure data is in [1, T', D] layout
        let embeddings = if channel_first {
            // Transpose [1, D, T'] -> [1, T', D]; the length was checked, so
            // every index is in range.
            let (d, t) = (embed_dim, time_steps_out);
            (0..t)
                .flat_map(|ti| (0..d).map(move |di| di * t + ti))
                .map(|i| embed_data.get(i).copied().unwrap_or_default())
                .collect()
        } else {
            embed_data
        };

        Ok(EncoderOutput {
            embeddings,
            embed_dim,
            encoded_lengths,
        })
    }
}
