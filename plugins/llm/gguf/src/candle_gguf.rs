//! GGUF model loader -- memory-mapped weight access via candle.

use std::path::Path;

use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::models::{quantized_gemma3, quantized_llama};

use super::backend::{LlmBackend, LlmBackendError};

/// Quantized model weights, dispatched on the GGUF `general.architecture`.
///
/// candle exposes a distinct `ModelWeights` type per architecture family
/// (each reads its own metadata-key prefix, e.g. `llama.*` vs `gemma3.*`),
/// so we wrap them in an enum and delegate the small surface we use.
#[derive(Debug, Clone)]
enum Weights {
    /// Llama-family GGUF (Llama 2/3, `SmolLM2`, etc. — `general.architecture = "llama"`).
    Llama(quantized_llama::ModelWeights),
    /// Gemma 3 GGUF (`general.architecture = "gemma3"`).
    Gemma3(quantized_gemma3::ModelWeights),
}

impl Weights {
    fn forward(&mut self, x: &Tensor, index_pos: usize) -> candle_core::Result<Tensor> {
        match self {
            Self::Llama(m) => m.forward(x, index_pos),
            Self::Gemma3(m) => m.forward(x, index_pos),
        }
    }
}

/// Candle-based GGUF model backend.
///
/// Loads a quantized GGUF file and runs inference via candle's per-architecture
/// `ModelWeights`, selected from the file's `general.architecture` metadata.
///
/// Supports CPU, CUDA, and Metal devices.  KV-cache snapshots
/// are implemented by cloning the `ModelWeights` struct (the
/// quantized weight tensors are reference-counted, so only the
/// KV-cache tensors inside each layer are actually duplicated).
pub struct CandleGgufBackend {
    model: Weights,
    device: Device,
    /// Snapshot of model state after system-prompt prefill.
    snapshot: Option<Weights>,
    /// Positions a sequence can hold: the model's trained window, capped at
    /// what candle's implementation of the architecture precomputes.
    context_tokens: usize,
}

impl std::fmt::Debug for CandleGgufBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandleGgufBackend")
            .field("device", &self.device)
            .field("has_snapshot", &self.snapshot.is_some())
            .finish_non_exhaustive()
    }
}

impl CandleGgufBackend {
    /// Load a GGUF model from disk.
    pub fn load(model_path: &Path, device: Device) -> Result<Self, LlmBackendError> {
        let mut file = std::fs::File::open(model_path).map_err(|e| LlmBackendError {
            message: format!("failed to open model {}: {e}", model_path.display()),
            source: None,
        })?;

        Self::from_reader(&mut file, device).map_err(|e| LlmBackendError {
            message: format!("{}: {}", model_path.display(), e.message),
            source: e.source,
        })
    }

    /// Load a GGUF model from any seekable source.
    fn from_reader<R: std::io::Read + std::io::Seek>(
        file: &mut R,
        device: Device,
    ) -> Result<Self, LlmBackendError> {
        let gguf = gguf_file::Content::read(file).map_err(|e| LlmBackendError {
            message: format!("failed to read GGUF: {e}"),
            source: None,
        })?;

        // Dispatch on the model architecture declared in the GGUF header so we
        // pick the matching candle loader (each reads a different metadata-key
        // prefix). Unknown architectures fall back to the Llama loader, which is
        // also the right choice for Llama-derived models (SmolLM2, etc.).
        let arch = gguf
            .metadata
            .get("general.architecture")
            .and_then(|v| v.to_string().ok())
            .map_or("llama", String::as_str)
            .to_owned();

        let trained = gguf
            .metadata
            .get(&format!("{arch}.context_length"))
            .and_then(|v| v.to_u32().ok())
            .map_or(usize::MAX, |n| n as usize);
        let supported = if arch == "gemma3" {
            quantized_gemma3::MAX_SEQ_LEN
        } else {
            quantized_llama::MAX_SEQ_LEN
        };
        let context_tokens = trained.min(supported);

        let model = match arch.as_str() {
            "gemma3" => {
                quantized_gemma3::ModelWeights::from_gguf(gguf, file, &device).map(Weights::Gemma3)
            }
            _ => quantized_llama::ModelWeights::from_gguf(gguf, file, &device).map(Weights::Llama),
        }
        .map_err(|e| LlmBackendError {
            message: format!("failed to load model weights (architecture={arch:?}): {e}"),
            source: None,
        })?;

        tracing::info!(architecture = %arch, context_tokens, "GGUF model loaded (candle backend)");

        // Save an initial snapshot of the pristine state so that
        // `reset_cache()` can always restore -- even when prefill is off.
        let snapshot = Some(model.clone());

        Ok(Self {
            model,
            device,
            snapshot,
            context_tokens,
        })
    }
}

impl LlmBackend for CandleGgufBackend {
    // Llama can (candle 0.11 builds its attention mask for a batch at an
    // offset; 0.9 built it for position 0 only, and the batch failed to
    // broadcast). Gemma 3 cannot: its mask for an offset joins an `f32`
    // block to a `u32` one, which fails, and would mark the prefix as hidden
    // if it did not. A test runs both on a tiny model and holds this answer
    // to what they do, and a "yes" to the same logits as one token at a time.
    fn continues_batches(&self) -> bool {
        matches!(self.model, Weights::Llama(_))
    }

    fn context_tokens(&self) -> usize {
        self.context_tokens
    }

    fn forward(&mut self, tokens: &[u32], pos: usize) -> Result<Tensor, LlmBackendError> {
        let input = Tensor::new(tokens, &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| LlmBackendError {
                message: format!("tensor creation failed: {e}"),
                source: None,
            })?;

        let logits = self
            .model
            .forward(&input, pos)
            .map_err(|e| LlmBackendError {
                message: format!("forward pass failed: {e}"),
                source: None,
            })?;

        // logits shape: [batch, seq_len, vocab] or [batch, vocab] -- squeeze
        // to [seq_len, vocab] or [vocab], then take the last token's logits.
        let logits = logits.squeeze(0).map_err(|e| LlmBackendError {
            message: format!("squeeze failed: {e}"),
            source: None,
        })?;

        let last_logits = if logits.dims().len() == 2 {
            let seq_len = logits.dim(0).unwrap_or(1);
            logits.get(seq_len - 1).map_err(|e| LlmBackendError {
                message: format!("logits indexing failed: {e}"),
                source: None,
            })?
        } else {
            logits
        };

        Ok(last_logits)
    }

    fn save_cache(&mut self) -> usize {
        // Clone the entire ModelWeights.  The quantized weight tensors are
        // reference-counted under the hood, so the actual data copy is limited
        // to the per-layer KV-cache tensors.
        self.snapshot = Some(self.model.clone());
        tracing::debug!("KV-cache snapshot saved");
        // We don't track the exact token position inside ModelWeights (it
        // uses internal KV-cache concatenation).  The caller tracks it.
        0 // position tracking is the caller's responsibility
    }

    fn reset_cache(&mut self) {
        if let Some(ref snapshot) = self.snapshot {
            self.model = snapshot.clone();
            tracing::debug!("KV-cache restored from snapshot");
        } else {
            // No snapshot -- ModelWeights doesn't expose a cache-clear method.
            // CandleGgufBackend always has a snapshot (pristine or prefilled), so
            // reaching here means a custom backend forgot to save.  Log at debug
            // level since it's not user-actionable.
            tracing::debug!("reset_cache called without a snapshot; KV-cache may be stale");
        }
    }
}

#[cfg(test)]
#[path = "tests/candle_gguf.rs"]
mod tests;
