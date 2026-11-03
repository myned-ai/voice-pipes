//! Piper model configuration -- voice metadata and synthesis params.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use voice_pipes_sdk::TtsError;

/// Top-level Piper model configuration (the `.json` sidecar file).
#[derive(Debug, Deserialize, Serialize)]
pub struct PiperModelConfig {
    pub audio: AudioConfig,
    pub inference: InferenceConfig,
    pub phoneme_id_map: HashMap<String, Vec<i64>>,
    #[serde(default)]
    pub num_speakers: u32,
    #[serde(default)]
    pub speaker_id_map: HashMap<String, i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AudioConfig {
    pub sample_rate: u32,
    #[serde(default = "default_channels")]
    pub channels: u16,
}

fn default_channels() -> u16 {
    1
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InferenceConfig {
    #[serde(default = "default_noise_scale")]
    pub noise_scale: f32,
    #[serde(default = "default_length_scale")]
    pub length_scale: f32,
    #[serde(default = "default_noise_w")]
    pub noise_w: f32,
}

fn default_noise_scale() -> f32 {
    0.667
}
fn default_length_scale() -> f32 {
    1.0
}
fn default_noise_w() -> f32 {
    0.8
}

impl PiperModelConfig {
    /// Load and parse the JSON config file next to the `.onnx` model.
    pub fn load(path: &Path) -> Result<Self, TtsError> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| TtsError::ConfigError(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&data, path)
    }

    /// Parse the config text read from `path`. A voice with no sample rate
    /// or no channels cannot be played, so it is refused here rather than
    /// producing frames no sink can interpret; so is a voice with more than
    /// one speaker, which would fail on its first synthesis.
    pub(crate) fn parse(data: &str, path: &Path) -> Result<Self, TtsError> {
        let config: Self = serde_json::from_str(data)
            .map_err(|e| TtsError::ConfigError(format!("invalid config JSON: {e}")))?;
        if config.audio.sample_rate == 0 || config.audio.channels == 0 {
            return Err(TtsError::ModelLoadFailed(format!(
                "{}: audio.sample_rate ({}) and audio.channels ({}) must both be above zero",
                path.display(),
                config.audio.sample_rate,
                config.audio.channels,
            )));
        }
        // A multi-speaker voice takes a speaker id as a fourth input, which
        // this engine does not pass; ONNX Runtime would refuse every run.
        if config.num_speakers > 1 {
            return Err(TtsError::ModelLoadFailed(format!(
                "{}: a voice with {} speakers needs a speaker id, which Piper here \
                 cannot select; use a single-speaker voice",
                path.display(),
                config.num_speakers,
            )));
        }
        Ok(config)
    }

    /// Map a phoneme character to its ID sequence (with inter-phoneme padding).
    /// Returns None if the phoneme is not in the map.
    pub fn phoneme_to_ids(&self, phoneme: &str) -> Option<&Vec<i64>> {
        self.phoneme_id_map.get(phoneme)
    }
}

#[cfg(test)]
#[path = "tests/model_config.rs"]
mod tests;
