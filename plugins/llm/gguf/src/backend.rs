//! LLM backend trait and runtime selection.

use std::fmt;

use candle_core::Tensor;

// ── Error ───────────────────────────────────────────────────

/// Errors from the low-level LLM backend (model forward pass / cache ops).
#[derive(Debug)]
pub struct LlmBackendError {
    pub message: String,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for LlmBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "llm backend: {}", self.message)
    }
}

impl std::error::Error for LlmBackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e.as_ref() as _)
    }
}

// ── Trait ───────────────────────────────────────────────────

/// Low-level model backend -- does forward passes and KV-cache management.
///
/// Implementations must be `Send` (moved between tasks) but NOT `Sync`;
/// the processor wraps them in `Mutex`.
pub trait LlmBackend: Send {
    /// Run a forward pass.
    ///
    /// `tokens` is the full prompt (when `pos == 0`), a single continuation
    /// token, or, when [`continues_batches`](Self::continues_batches), the
    /// rest of a prompt after a prefilled prefix.  Returns logits for the
    /// **last** token, shape `[vocab_size]`.
    fn forward(&mut self, tokens: &[u32], pos: usize) -> Result<Tensor, LlmBackendError>;

    /// Whether [`forward`](Self::forward) takes several tokens at a position
    /// other than 0, which a prefilled system prompt needs: the turn's own
    /// tokens are fed after the prefix. Without it the prefix cannot be
    /// reused and prefill is left off.
    fn continues_batches(&self) -> bool {
        false
    }

    /// The most positions a sequence can hold: prompt plus reply. Unbounded
    /// unless the backend knows its model's window.
    fn context_tokens(&self) -> usize {
        usize::MAX
    }

    /// Snapshot the current KV-cache state (e.g. after processing the system
    /// prompt).  Returns the number of tokens whose KV entries are now saved.
    fn save_cache(&mut self) -> usize;

    /// Restore the KV-cache to the last snapshot saved by [`save_cache`], or
    /// fully clear it if no snapshot exists.
    fn reset_cache(&mut self);
}

#[cfg(test)]
#[path = "tests/backend.rs"]
pub(crate) mod tests;
