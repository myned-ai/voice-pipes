//! Piper ONNX inference engine -- runs the VITS model.

use ort::session::Session;
use ort::value::Tensor;
use tracing::instrument;

use voice_pipes_sdk::TtsError;
use voice_pipes_sdk::audio::f32_to_i16;

use super::model_config::PiperModelConfig;

/// Loads an ONNX Piper model and runs inference to produce PCM f32 audio.
pub struct PiperEngine {
    session: Session,
}

impl std::fmt::Debug for PiperEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiperEngine").finish_non_exhaustive()
    }
}

impl PiperEngine {
    /// Load the `.onnx` model file through the shared opener: `threads`
    /// threads per operator, on the device `VP_ORT_EP` names (the CPU unless
    /// it asks for CUDA in a CUDA build).
    pub fn load(model_path: &std::path::Path, threads: usize) -> Result<Self, TtsError> {
        let options = voice_pipes_onnx::SessionOptions::new(threads)
            .with_target(voice_pipes_onnx::ExecutionTarget::from_env());
        let session = voice_pipes_onnx::open_session(model_path, &options)
            .map_err(|e| TtsError::ModelLoadFailed(e.to_string()))?;
        Ok(Self { session })
    }

    /// Run inference: phoneme IDs -> PCM i16 samples.
    #[instrument(skip_all, fields(phoneme_count = phoneme_ids.len()))]
    pub fn infer(
        &mut self,
        phoneme_ids: &[i64],
        config: &PiperModelConfig,
    ) -> Result<Vec<i16>, TtsError> {
        let len = phoneme_ids.len();

        // Use (shape, vec) tuple form -- avoids ndarray version conflicts with ort
        let input_ids_value = Tensor::from_array(([1, len], phoneme_ids.to_vec()))
            .map_err(|e| TtsError::InferenceFailed(format!("tensor input_ids: {e}")))?;
        let input_lengths_value = Tensor::from_array(([1usize], vec![len as i64]))
            .map_err(|e| TtsError::InferenceFailed(format!("tensor input_lengths: {e}")))?;
        let scales_value = Tensor::from_array((
            [3usize],
            vec![
                config.inference.noise_scale,
                config.inference.length_scale,
                config.inference.noise_w,
            ],
        ))
        .map_err(|e| TtsError::InferenceFailed(format!("tensor scales: {e}")))?;

        let outputs = self
            .session
            .run(ort::inputs![
                input_ids_value,
                input_lengths_value,
                scales_value
            ])
            .map_err(|e| TtsError::InferenceFailed(format!("ort run: {e}")))?;

        // Output shape: [1, 1, samples] -- squeeze to 1D f32, then convert to i16
        let audio_value = outputs
            .values()
            .next()
            .ok_or_else(|| TtsError::InferenceFailed("the voice returned no output".into()))?;
        let audio_tensor = audio_value
            .try_extract_tensor::<f32>()
            .map_err(|e| TtsError::InferenceFailed(format!("extract output: {e}")))?;

        Ok(f32_to_i16(audio_tensor.1))
    }
}
