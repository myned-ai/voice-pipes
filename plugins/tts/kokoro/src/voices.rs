//! Kokoro voice embedding loader.
//!
//! Reads an NPZ file (ZIP of `.npy` arrays) containing per-voice style
//! embeddings. Each entry is a 2D `f32` matrix of shape `[max_tokens, style_dim]`.
//! The style vector for inference is selected by indexing `matrix[num_tokens]`
//! (clamped to the matrix row count).

use std::collections::HashMap;
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use npyz::NpyFile;
use zip::ZipArchive;

use voice_pipes_sdk::TtsError;

/// In-memory cache of all loaded voice embeddings.
#[derive(Debug)]
pub struct KokoroVoices {
    /// `voice_name` -> `[max_tokens][style_dim]` row-major f32 matrix.
    voices: HashMap<String, VoiceMatrix>,
}

#[derive(Debug)]
struct VoiceMatrix {
    /// Flat row-major data.
    data: Vec<f32>,
    /// Number of rows (`max_tokens` dimension).
    rows: usize,
    /// Number of columns (style embedding dimension).
    cols: usize,
}

impl KokoroVoices {
    /// Load all voice embeddings from an NPZ file.
    pub fn load(path: &Path) -> Result<Self, TtsError> {
        let bytes = std::fs::read(path).map_err(|e| {
            TtsError::ModelLoadFailed(format!("cannot open voices file {}: {e}", path.display()))
        })?;
        Self::load_from_reader(Cursor::new(bytes))
    }

    /// Load from any seekable reader (useful for testing).
    fn load_from_reader<R: Read + Seek>(reader: R) -> Result<Self, TtsError> {
        let mut archive = ZipArchive::new(reader)
            .map_err(|e| TtsError::ModelLoadFailed(format!("invalid NPZ archive: {e}")))?;

        let mut voices = HashMap::new();

        for i in 0..archive.len() {
            let entry = archive.by_index(i).map_err(|e| {
                TtsError::ModelLoadFailed(format!("cannot read NPZ entry {i}: {e}"))
            })?;

            let name = entry.name().to_string();
            // Each entry is "voice_name.npy"
            let voice_name = name.strip_suffix(".npy").unwrap_or(&name).to_string();

            let npy = NpyFile::new(entry).map_err(|e| {
                TtsError::ModelLoadFailed(format!("invalid NPY entry \"{name}\": {e}"))
            })?;

            let shape = npy.shape().to_vec();
            // Accept 2D [rows, cols] or 3D [rows, 1, cols] (squeeze middle dim).
            let (rows, cols) = match shape.as_slice() {
                [r, c] | [r, 1, c] => (
                    usize::try_from(*r).unwrap_or(usize::MAX),
                    usize::try_from(*c).unwrap_or(usize::MAX),
                ),
                _ => {
                    return Err(TtsError::ModelLoadFailed(format!(
                        "voice \"{voice_name}\" has unsupported shape {shape:?} \
                         (expected [rows, cols] or [rows, 1, cols])"
                    )));
                }
            };

            let data: Vec<f32> = npy.into_vec().map_err(|e| {
                TtsError::ModelLoadFailed(format!(
                    "failed to read voice \"{voice_name}\" data: {e}"
                ))
            })?;

            if rows == 0 || cols == 0 {
                return Err(TtsError::ModelLoadFailed(format!(
                    "voice \"{voice_name}\" is empty ({rows}x{cols})"
                )));
            }
            if rows.checked_mul(cols) != Some(data.len()) {
                return Err(TtsError::ModelLoadFailed(format!(
                    "voice \"{voice_name}\" data length {} != {rows}x{cols}",
                    data.len()
                )));
            }

            voices.insert(voice_name, VoiceMatrix { data, rows, cols });
        }

        if voices.is_empty() {
            return Err(TtsError::ModelLoadFailed(
                "NPZ archive contains no voice entries".into(),
            ));
        }

        tracing::info!(voice_count = voices.len(), "loaded Kokoro voice embeddings");
        Ok(Self { voices })
    }

    /// Get the style embedding vector for a given voice and token count.
    ///
    /// Indexes `voice_matrix[min(num_tokens, max_rows - 1)]` to select the
    /// positional style row. Returns a borrowed slice of length `style_dim`.
    pub fn get_style(&self, voice: &str, num_tokens: usize) -> Result<&[f32], TtsError> {
        let matrix = self.voices.get(voice).ok_or_else(|| {
            TtsError::ConfigError(format!(
                "unknown Kokoro voice \"{voice}\"; available: {:?}",
                self.list_voices()
            ))
        })?;

        // Every loaded matrix is non-empty and holds rows x cols values.
        let row = num_tokens.min(matrix.rows.saturating_sub(1));
        let start = row.saturating_mul(matrix.cols);
        matrix
            .data
            .get(start..start.saturating_add(matrix.cols))
            .ok_or_else(|| TtsError::InferenceFailed(format!("voice \"{voice}\" has no row {row}")))
    }

    /// List all available voice names (sorted).
    pub fn list_voices(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.voices.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Style embedding dimension (columns). Returns 0 if empty.
    pub fn style_dim(&self) -> usize {
        self.voices.values().next().map_or(0, |m| m.cols)
    }
}

#[cfg(test)]
#[path = "tests/voices.rs"]
mod tests;

