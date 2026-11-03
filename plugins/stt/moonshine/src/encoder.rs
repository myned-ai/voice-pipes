//! Moonshine encoder -- ONNX runtime session wrapping the encoder model.

use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use voice_pipes_sdk::SttError;

/// ONNX-backed encoder wrapper for Useful Sensors Moonshine STT.
#[derive(Debug)]
pub(crate) struct MoonshineEncoder {
    session: Session,
}

impl MoonshineEncoder {
    /// Load the encoder session from the given file path.
    pub(crate) fn new(model_path: &Path, threads: usize) -> Result<Self, SttError> {
        Ok(Self {
            session: crate::open_session(model_path, threads, "encoder")?,
        })
    }

    /// Run the audio waveform through the encoder.
    /// Input audio should be 16 kHz mono float32 samples.
    pub(crate) fn encode(&mut self, audio: &[f32]) -> Result<EncoderOutput, SttError> {
        let seq_len = audio.len();
        if seq_len == 0 {
            return Err(SttError::InvalidAudio(
                "empty audio buffer fed to encoder".into(),
            ));
        }

        // input_values shape: [1, seq_len]
        let input_values =
            Tensor::from_array(([1usize, seq_len], audio.to_vec())).map_err(|e| {
                SttError::InferenceFailed(format!("Moonshine encoder input_values tensor: {e}"))
            })?;

        let outputs = self
            .session
            .run(ort::inputs![
                "input_values" => input_values,
            ])
            .map_err(|e| SttError::InferenceFailed(format!("Moonshine encoder run: {e}")))?;

        let hidden = outputs.values().next().ok_or_else(|| {
            SttError::InferenceFailed("Moonshine encoder returned no output".into())
        })?;
        let (shape, data) = hidden.try_extract_tensor::<f32>().map_err(|e| {
            SttError::InferenceFailed(format!("Moonshine encoder extract hidden states: {e}"))
        })?;

        Ok(EncoderOutput {
            hidden_states: data.to_vec(),
            shape: shape.to_vec(),
        })
    }
}

/// Extracted encoder hidden states.
#[derive(Debug, Clone)]
pub(crate) struct EncoderOutput {
    pub(crate) hidden_states: Vec<f32>,
    pub(crate) shape: Vec<i64>,
}
