//! Kokoro ONNX inference engine.
//!
//! Loads the Kokoro-82M ONNX model and runs inference to produce 24 kHz
//! PCM i16 audio from phoneme token IDs and a style embedding vector.

use ort::session::Session;
use ort::value::Tensor;
use tracing::instrument;

use voice_pipes_sdk::TtsError;
use voice_pipes_sdk::audio::f32_to_i16;

/// Output sample rate of the Kokoro model.
pub const SAMPLE_RATE: u32 = 24_000;

/// Kokoro ONNX inference session.
pub struct KokoroEngine {
    session: Session,
}

impl std::fmt::Debug for KokoroEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KokoroEngine").finish_non_exhaustive()
    }
}

impl KokoroEngine {
    /// Load the Kokoro `.onnx` model file through the shared opener, on
    /// `target`.
    pub fn load(
        model_path: &std::path::Path,
        target: voice_pipes_onnx::ExecutionTarget,
    ) -> Result<Self, TtsError> {
        use ort::session::builder::GraphOptimizationLevel;
        use voice_pipes_onnx::{SessionOptions, open_session};

        let options = SessionOptions::new(4).with_target(target);
        // The graph is optimised once and saved beside the model; later
        // loads take the saved graph as it is.
        let optimized_path = model_path.with_extension("optimized.onnx");
        let session = if optimized_path.exists() {
            open_session(
                &optimized_path,
                &options.with_optimization(GraphOptimizationLevel::Disable),
            )
        } else {
            open_session(
                model_path,
                &options
                    .with_optimization(GraphOptimizationLevel::Level3)
                    .with_optimized_model_path(&optimized_path),
            )
        }
        .map_err(|e| TtsError::ModelLoadFailed(e.to_string()))?;

        Ok(Self { session })
    }

    /// Run inference: token IDs + style vector + speed -> PCM i16 samples.
    ///
    /// # Tensor layout
    /// - `input_ids`: `[1, N+2]` i64 -- token IDs padded with a leading and
    ///   trailing zero.
    /// - `style`: `[D]` f32 -- style embedding vector from the voice matrix.
    /// - `speed`: `[1]` f32 -- synthesis speed multiplier.
    #[instrument(skip_all, fields(token_count = tokens.len(), speed))]
    pub fn infer(
        &mut self,
        tokens: &[i64],
        style: &[f32],
        speed: f32,
    ) -> Result<Vec<i16>, TtsError> {
        // Build input_ids: [0, ...tokens..., 0]
        let padded_len = tokens.len() + 2;
        let mut input_ids = Vec::with_capacity(padded_len);
        input_ids.push(0i64);
        input_ids.extend_from_slice(tokens);
        input_ids.push(0i64);

        let input_ids_tensor = Tensor::from_array(([1, padded_len], input_ids))
            .map_err(|e| TtsError::InferenceFailed(format!("tensor input_ids: {e}")))?;

        let style_dim = style.len();
        let style_tensor = Tensor::from_array(([1, style_dim], style.to_vec()))
            .map_err(|e| TtsError::InferenceFailed(format!("tensor style: {e}")))?;

        let speed_tensor = Tensor::from_array(([1usize], vec![speed]))
            .map_err(|e| TtsError::InferenceFailed(format!("tensor speed: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![input_ids_tensor, style_tensor, speed_tensor])
            .map_err(|e| TtsError::InferenceFailed(format!("ort run: {e}")))?;

        // Output is f32 audio -- squeeze to 1D and convert to i16
        let output = outputs
            .values()
            .next()
            .ok_or_else(|| TtsError::InferenceFailed("the model returned no output".into()))?;
        let audio_tensor = output
            .try_extract_tensor::<f32>()
            .map_err(|e| TtsError::InferenceFailed(format!("extract output: {e}")))?;

        Ok(f32_to_i16(audio_tensor.1))
    }
}
