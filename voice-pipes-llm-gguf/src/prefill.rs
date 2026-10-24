//! System-prompt KV-cache prefill.
//!
//! When `prefill_cache` is enabled in [`LlmConfig`], the system prompt is
//! tokenized and forwarded through the backend at startup.  A KV-cache
//! snapshot is saved so every subsequent turn can restore the snapshot and
//! skip re-computing the system-prompt portion.
//!
//! This mirrors the strategy used by llama.cpp slot save/restore and
//! vLLM Automatic Prefix Caching (APC) -- the system prompt is the
//! highest-value prefix because it is shared by 100% of requests.

use tokenizers::Tokenizer;

use super::backend::LlmBackend;
use super::chat_template::ChatTemplate;
use voice_pipes_sdk::TextProcessorError;

/// Forward the system-prompt tokens through the backend and snapshot
/// the KV-cache.  Returns the tokens prefilled.
///
/// On each turn the caller restores the snapshot (`reset_cache()`) and,
/// when the prompt's tokens start with these, feeds only the rest,
/// starting at the position where these end.
pub fn prefill_system_prompt(
    backend: &mut dyn LlmBackend,
    tokenizer: &Tokenizer,
    template: &dyn ChatTemplate,
    system_prompt: &str,
) -> Result<Vec<u32>, TextProcessorError> {
    let prefix_text = template.system_prefix(system_prompt);

    let token_ids = crate::chat_template::encode_prompt(tokenizer, &prefix_text).map_err(|e| {
        TextProcessorError::new(
            "llm-prefill",
            format!("system prefix tokenization failed: {e}"),
        )
    })?;
    let tokens = token_ids.as_slice();
    if tokens.is_empty() {
        tracing::warn!("system prefix tokenized to zero tokens; skipping prefill");
        return Ok(Vec::new());
    }

    // Forward the full system prefix at position 0 (batch path).
    backend
        .forward(tokens, 0)
        .map_err(|e| TextProcessorError::new("llm-prefill", e.message))?;

    let len = tokens.len();
    backend.save_cache();

    tracing::info!(
        prefill_tokens = len,
        "system prompt KV-cache prefilled and snapshot saved"
    );

    Ok(token_ids)
}

#[cfg(test)]
#[path = "tests/prefill.rs"]
mod tests;
