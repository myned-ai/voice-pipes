//! Mel-spectrogram and feature preprocessing for Parakeet.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;

use voice_pipes_sdk::SttError;

/// Output from the mel spectrogram preprocessor.
#[derive(Debug)]
pub struct MelOutput {
    /// Log-mel features, flattened from shape [1, 128, T].
    pub features: Vec<f32>,
    /// Number of mel bins (128 for Parakeet v2).
    pub mel_bins: usize,
    /// Number of time frames.
    pub time_steps: usize,
    /// Feature lengths per batch element.
    pub features_lens: Vec<i64>,
}

/// ONNX-based log-mel spectrogram preprocessor (nemo128.onnx).
///
/// Converts raw f32 PCM waveform (16kHz) to 128-dim log-mel features
/// with pre-emphasis, STFT, mel filterbank, log, and per-utterance normalization.
pub struct MelPreprocessor {
    session: Session,
}

impl std::fmt::Debug for MelPreprocessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MelPreprocessor").finish_non_exhaustive()
    }
}

impl MelPreprocessor {
    /// Load the preprocessor ONNX model.
    pub fn new(model_path: &Path) -> Result<Self, SttError> {
        let session = crate::open_session(model_path)?;
        Ok(Self { session })
    }

    /// Process waveform -> log-mel features.
    ///
    /// Input: f32 PCM samples at 16kHz.
    /// Output: [1, 128, T] log-mel features.
    pub fn process(&mut self, waveform: &[f32]) -> Result<MelOutput, SttError> {
        if waveform.is_empty() {
            return Err(SttError::InvalidAudio("empty waveform".into()));
        }

        let num_samples = waveform.len();

        let waveform_tensor = Tensor::from_array(([1usize, num_samples], waveform.to_vec()))
            .map_err(|e| {
                SttError::InferenceFailed(format!("mel preprocessor waveform tensor: {e}"))
            })?;

        let length_tensor =
            Tensor::from_array(([1usize], vec![num_samples as i64])).map_err(|e| {
                SttError::InferenceFailed(format!("mel preprocessor length tensor: {e}"))
            })?;

        let outputs = self
            .session
            .run(ort::inputs![waveform_tensor, length_tensor])
            .map_err(|e| SttError::InferenceFailed(format!("mel preprocessor run: {e}")))?;

        // Output 0: features [1, 128, T]
        let features_tensor_value = voice_pipes_onnx::nth_output(&outputs, 0, "mel preprocessor")
            .map_err(crate::model_output)?;
        let features_tensor = features_tensor_value
            .try_extract_tensor::<f32>()
            .map_err(|e| SttError::InferenceFailed(format!("mel extract features: {e}")))?;
        let features_shape = &features_tensor.0;
        let features_data: Vec<f32> = features_tensor.1.to_vec();

        // Output 1: features_lens [1]
        let lens_tensor_value = voice_pipes_onnx::nth_output(&outputs, 1, "mel preprocessor")
            .map_err(crate::model_output)?;
        let lens_tensor = lens_tensor_value
            .try_extract_tensor::<i64>()
            .map_err(|e| SttError::InferenceFailed(format!("mel extract lens: {e}")))?;
        let features_lens: Vec<i64> = lens_tensor.1.to_vec();

        let (mel_bins, time_steps) = match (features_shape.get(1), features_shape.get(2)) {
            (Some(&bins), Some(&steps)) => (
                voice_pipes_onnx::dim(bins, "a mel bin count").map_err(crate::model_output)?,
                voice_pipes_onnx::dim(steps, "a feature length").map_err(crate::model_output)?,
            ),
            _ => (128, features_data.len() / 128),
        };
        if mel_bins == 0 {
            return Err(SttError::InferenceFailed(
                "the mel preprocessor reported no mel bins".into(),
            ));
        }

        Ok(MelOutput {
            features: features_data,
            mel_bins,
            time_steps,
            features_lens,
        })
    }
}
