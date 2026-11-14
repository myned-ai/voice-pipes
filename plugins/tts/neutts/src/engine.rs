//! NeuTTS-Air engine: GGUF Qwen2 token generation and ONNX `NeuCodec` decoding.

use std::path::Path;

use ort::session::Session;
use ort::value::Tensor;
use tracing::instrument;

use voice_pipes_onnx::{ExecutionTarget, SessionOptions, open_session};
use voice_pipes_sdk::TtsError;
use voice_pipes_sdk::audio::f32_to_i16;

use crate::resample::resample_24k_to_16k;

/// Native sampling rate of `NeuCodec` before resampling.
#[expect(dead_code, reason = "native sample rate of NeuCodec before resampling")]
pub const NATIVE_SAMPLE_RATE: u32 = 24_000;

/// Output sampling rate after resampling to Voice Pipes pipeline standard.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// NeuTTS-Air synthesis engine holding the vocoder session.
pub struct NeuTtsEngine {
    vocoder_session: Session,
}

impl std::fmt::Debug for NeuTtsEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuTtsEngine").finish_non_exhaustive()
    }
}

impl NeuTtsEngine {
    /// Load the `NeuCodec` ONNX vocoder model file.
    pub fn load_vocoder(
        vocoder_path: &Path,
        target: ExecutionTarget,
        threads: usize,
    ) -> Result<Self, TtsError> {
        let options = SessionOptions::new(threads).with_target(target);
        let vocoder_session = open_session(vocoder_path, &options)
            .map_err(|e| TtsError::ModelLoadFailed(format!("NeuCodec load failed: {e}")))?;

        Ok(Self { vocoder_session })
    }

    /// Decode discrete acoustic tokens into 16 kHz i16 audio frames.
    #[instrument(skip_all, fields(token_count = tokens.len()))]
    pub fn decode_tokens(&mut self, tokens: &[i64]) -> Result<Vec<i16>, TtsError> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let len = tokens.len();
        let tokens_tensor = Tensor::from_array(([1, len], tokens.to_vec()))
            .map_err(|e| TtsError::InferenceFailed(format!("tokens tensor: {e}")))?;

        let outputs = self
            .vocoder_session
            .run(ort::inputs![tokens_tensor])
            .map_err(|e| TtsError::InferenceFailed(format!("NeuCodec vocoder run: {e}")))?;

        let output = outputs
            .values()
            .next()
            .ok_or_else(|| TtsError::InferenceFailed("NeuCodec returned no output".into()))?;

        let audio_tensor = output
            .try_extract_tensor::<f32>()
            .map_err(|e| TtsError::InferenceFailed(format!("NeuCodec extract audio: {e}")))?;

        let raw_24k = f32_to_i16(audio_tensor.1);
        Ok(resample_24k_to_16k(&raw_24k))
    }
}
