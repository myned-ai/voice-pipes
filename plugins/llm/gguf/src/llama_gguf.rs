//! GGUF model loader -- inference and KV-cache management via `llama-cpp-2`.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use candle_core::Tensor;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::token::LlamaToken;

use super::backend::{LlmBackend, LlmBackendError};

static BACKEND: OnceLock<Arc<LlamaBackend>> = OnceLock::new();
static BACKEND_MUTEX: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
static MODELS: OnceLock<parking_lot::Mutex<HashMap<String, &'static LlamaModel>>> = OnceLock::new();

/// Safely initialize and retrieve the shared `llama.cpp` backend.
pub(crate) fn get_or_init_backend() -> Result<Arc<LlamaBackend>, LlmBackendError> {
    if let Some(backend) = BACKEND.get() {
        return Ok(Arc::clone(backend));
    }
    let _guard = BACKEND_MUTEX.lock();
    if let Some(backend) = BACKEND.get() {
        return Ok(Arc::clone(backend));
    }
    let mut backend = LlamaBackend::init().map_err(|e| LlmBackendError {
        message: format!("LlamaBackend init failed: {e}"),
        source: None,
    })?;
    backend.void_logs();
    let arc = Arc::new(backend);
    let _ = BACKEND.set(Arc::clone(&arc));
    Ok(arc)
}

/// Retrieve or load a cached model for the process lifetime.
fn get_or_load_model(
    backend: &LlamaBackend,
    path: &Path,
    device: &str,
) -> Result<&'static LlamaModel, LlmBackendError> {
    let key = format!("{}|{device}", path.display());
    let cache = MODELS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    if let Some(&model) = cache.lock().get(&key) {
        return Ok(model);
    }
    let mut guard = cache.lock();
    if let Some(&model) = guard.get(&key) {
        return Ok(model);
    }

    let n_gpu_layers = if device == "cuda" { 999 } else { 0 };
    let model_params = LlamaModelParams::default().with_n_gpu_layers(n_gpu_layers);
    let model =
        LlamaModel::load_from_file(backend, path, &model_params).map_err(|e| LlmBackendError {
            message: format!("failed to load GGUF model from {}: {e}", path.display()),
            source: None,
        })?;
    let leaked: &'static LlamaModel = Box::leak(Box::new(model));
    guard.insert(key, leaked);
    Ok(leaked)
}

/// `llama.cpp`-based GGUF model backend.
///
/// Executes multi-threaded CPU inference (AVX2/AVX-512) or GPU-offloaded forward
/// passes and provides O(1) KV-cache truncation and restoration.
pub struct LlamaGgufBackend {
    context: LlamaContext<'static>,
    context_tokens: usize,
    saved_pos: Option<usize>,
    current_pos: usize,
}

impl std::fmt::Debug for LlamaGgufBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlamaGgufBackend")
            .field("context_tokens", &self.context_tokens)
            .field("saved_pos", &self.saved_pos)
            .field("current_pos", &self.current_pos)
            .finish_non_exhaustive()
    }
}

impl LlamaGgufBackend {
    /// Load a GGUF model from disk using `llama-cpp-2`.
    pub fn load(path: &Path, device: &str) -> Result<Self, LlmBackendError> {
        if !path.exists() {
            return Err(LlmBackendError {
                message: format!(
                    "failed to load GGUF model: file does not exist: {}",
                    path.display()
                ),
                source: None,
            });
        }

        let backend = get_or_init_backend()?;
        let model = get_or_load_model(backend.as_ref(), path, device)?;

        let trained_ctx = usize::try_from(model.n_ctx_train()).unwrap_or(2048);
        let context_tokens = trained_ctx.clamp(64, 4096);

        let threads_i32 = i32::try_from(
            std::thread::available_parallelism()
                .map_or(4, std::num::NonZero::get)
                .min(8),
        )
        .unwrap_or(4);

        let n_ctx_u32 = u32::try_from(context_tokens).map_err(|e| LlmBackendError {
            message: format!("context_tokens overflow: {e}"),
            source: None,
        })?;
        let non_zero_ctx = NonZeroU32::new(n_ctx_u32).ok_or_else(|| LlmBackendError {
            message: "context_tokens must be non-zero".into(),
            source: None,
        })?;

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(non_zero_ctx))
            .with_n_batch(2048)
            .with_n_ubatch(512)
            .with_n_threads(threads_i32)
            .with_n_threads_batch(threads_i32);

        let context = model
            .new_context(backend.as_ref(), ctx_params)
            .map_err(|e| LlmBackendError {
                message: format!("create context failed: {e}"),
                source: None,
            })?;

        tracing::info!(
            model = %path.display(),
            device,
            context_tokens,
            threads = threads_i32,
            "GGUF model loaded (llama.cpp backend)"
        );

        Ok(Self {
            context,
            context_tokens,
            saved_pos: None,
            current_pos: 0,
        })
    }
}

impl LlmBackend for LlamaGgufBackend {
    fn continues_batches(&self) -> bool {
        true
    }

    fn context_tokens(&self) -> usize {
        self.context_tokens
    }

    fn forward(&mut self, tokens: &[u32], pos: usize) -> Result<Tensor, LlmBackendError> {
        if tokens.is_empty() {
            return Err(LlmBackendError {
                message: "cannot forward empty tokens".into(),
                source: None,
            });
        }

        let n_tokens = tokens.len();
        let mut batch = LlamaBatch::new(n_tokens.max(1), 1);
        let last_idx = n_tokens.saturating_sub(1);

        for (i, &tok) in tokens.iter().enumerate() {
            let is_last = i == last_idx;
            let tok_i32 = i32::try_from(tok).map_err(|e| LlmBackendError {
                message: format!("token ID {tok} exceeds i32: {e}"),
                source: None,
            })?;
            let pos_i32 = i32::try_from(pos.saturating_add(i)).map_err(|e| LlmBackendError {
                message: format!("token position {} exceeds i32: {e}", pos + i),
                source: None,
            })?;
            batch
                .add(LlamaToken(tok_i32), pos_i32, &[0], is_last)
                .map_err(|e| LlmBackendError {
                    message: format!("batch add failed: {e}"),
                    source: None,
                })?;
        }

        self.context
            .decode(&mut batch)
            .map_err(|e| LlmBackendError {
                message: format!("decode failed: {e}"),
                source: None,
            })?;

        self.current_pos = pos.saturating_add(n_tokens);

        let logits_slice = self.context.get_logits();
        Tensor::new(logits_slice, &candle_core::Device::Cpu).map_err(|e| LlmBackendError {
            message: format!("tensor creation failed: {e}"),
            source: None,
        })
    }

    fn save_cache(&mut self) -> usize {
        self.saved_pos = Some(self.current_pos);
        tracing::debug!(saved_pos = self.current_pos, "KV-cache snapshot saved");
        self.current_pos
    }

    fn reset_cache(&mut self) {
        if let Some(saved_pos) = self.saved_pos {
            let p0 = u32::try_from(saved_pos).ok();
            let _ = self.context.clear_kv_cache_seq(None, p0, None);
            self.current_pos = saved_pos;
            tracing::debug!(saved_pos, "KV-cache restored from snapshot");
        } else {
            self.context.clear_kv_cache();
            self.current_pos = 0;
            tracing::debug!("KV-cache cleared (no snapshot)");
        }
    }
}

#[cfg(test)]
#[path = "tests/llama_gguf.rs"]
mod tests;
