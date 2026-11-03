//! Token-generation engine for [`LlmProcessor`] -- the blocking-pool
//! inference loops behind `process` / `process_stream`, plus their
//! logits and decoding helpers.

use std::sync::Arc;

use candle_transformers::generation::LogitsProcessor;
use tokenizers::Tokenizer;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::backend::{LlmBackend, LlmBackendError};
use super::chat_template::ChatTemplate;
use super::{LlmProcessor, TextFragment, TextProcessorError, split_first_sentence};

// ── Helpers ─────────────────────────────────────────────────

impl LlmProcessor {
    /// Resolve the EOS token ID using the template's priority list.
    ///
    /// Takes parts (not `&self`) so the blocking-pool generation closure can
    /// call it on its own `Arc` clones.
    fn resolve_eos_token(tokenizer: &Tokenizer, template: &dyn ChatTemplate) -> u32 {
        for tok_str in template.eos_tokens() {
            if let Some(id) = tokenizer.token_to_id(tok_str) {
                return id;
            }
        }
        0
    }

    /// Strip chat-template special tokens from decoded text.
    fn clean_output(template: &dyn ChatTemplate, text: &str) -> String {
        let mut out = text.to_string();
        for tok in template.strip_tokens() {
            out = out.replace(tok, "");
        }
        out
    }

    /// Feed prompt tokens into the backend and return logits for the last token.
    ///
    /// Always forwards the full token slice from position 0 in a single batch
    /// call, rebuilding the KV-cache from scratch. Feeding tokens one at a
    /// time at a non-zero `start_pos` is not an option with candle's causal
    /// mask (it is `[seq, seq]`, but attention over a warm cache needs
    /// `[seq, seq+cached]`), and the batch pass is far faster anyway
    /// (~1 s for a ~100-token prompt on CPU vs ~20 s sequentially).
    /// Feed the prompt after `reset_cache()` has restored the snapshot.
    /// A prompt that starts with the prefilled system-prompt tokens is fed
    /// from where they end, reusing their KV entries; any other (the system
    /// prompt was edited, or the tokenizer merged across the boundary) is
    /// fed from position 0, which replaces the cache.
    fn ingest_prompt(
        backend: &mut Box<dyn LlmBackend>,
        tokens: &[u32],
        prefilled: &[u32],
    ) -> Result<candle_core::Tensor, TextProcessorError> {
        let map_err = |e: LlmBackendError| TextProcessorError::new("llm", e.message);

        match tokens.strip_prefix(prefilled) {
            Some(rest) if !prefilled.is_empty() && !rest.is_empty() => {
                backend.forward(rest, prefilled.len()).map_err(map_err)
            }
            _ => backend.forward(tokens, 0).map_err(map_err),
        }
    }

    /// Apply repetition penalty to logits in-place.
    pub(super) fn apply_repeat_penalty(
        logits: &candle_core::Tensor,
        output_tokens: &[u32],
        penalty: f32,
    ) -> Result<candle_core::Tensor, TextProcessorError> {
        if penalty == 1.0 || output_tokens.is_empty() {
            return Ok(logits.clone());
        }

        let logits_vec: Vec<f32> = logits.to_vec1().map_err(|e| {
            TextProcessorError::new("llm", format!("repeat penalty logits read failed: {e}"))
        })?;

        let mut adjusted = logits_vec;
        for &tok in output_tokens {
            if let Some(logit) = adjusted.get_mut(tok as usize) {
                if *logit > 0.0 {
                    *logit /= penalty;
                } else {
                    *logit *= penalty;
                }
            }
        }

        candle_core::Tensor::from_vec(adjusted, logits.dims(), logits.device()).map_err(|e| {
            TextProcessorError::new("llm", format!("repeat penalty tensor creation failed: {e}"))
        })
    }
}

// ── Generation loops ────────────────────────────────────────

/// How long a streamed fragment send may wait on a full channel before the
/// turn is declared stalled and generation aborts. The consumer is the
/// orchestrator bridge feeding TTS, which drains far faster than realtime
/// playback; a channel that stays full this long means the consumer is
/// wedged, and waiting longer would hold the process-wide backend lock
/// hostage for every other turn.
const FRAGMENT_SEND_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

impl LlmProcessor {
    /// Cancel-aware, bounded fragment send for the blocking generation loop.
    /// Returns `false` when generation must stop: the consumer is gone
    /// (channel closed), the turn was cancelled while waiting, or the
    /// consumer stayed stalled past [`FRAGMENT_SEND_STALL_TIMEOUT`]. A plain
    /// blocking send would ignore the closed-channel signal (an aborted turn
    /// would keep generating to `max_tokens` while holding the backend lock)
    /// and could park this thread inside the lock indefinitely behind a
    /// stalled consumer, with cancellation unable to reach it.
    fn send_fragment_or_stop(
        tx: &mpsc::Sender<TextFragment>,
        cancel: &CancellationToken,
        mut fragment: TextFragment,
    ) -> bool {
        use tokio::sync::mpsc::error::TrySendError;
        let deadline = std::time::Instant::now() + FRAGMENT_SEND_STALL_TIMEOUT;
        loop {
            match tx.try_send(fragment) {
                Ok(()) => return true,
                Err(TrySendError::Closed(_)) => {
                    tracing::info!("fragment consumer dropped; stopping generation");
                    return false;
                }
                Err(TrySendError::Full(f)) => {
                    if cancel.is_cancelled() {
                        tracing::info!(
                            "cancelled while fragment channel full; stopping generation"
                        );
                        return false;
                    }
                    if std::time::Instant::now() >= deadline {
                        tracing::warn!(
                            timeout_s = FRAGMENT_SEND_STALL_TIMEOUT.as_secs(),
                            "fragment consumer stalled; aborting generation to release the backend"
                        );
                        return false;
                    }
                    fragment = f;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    }

    pub(super) async fn process_inner(&self, input: &str) -> Result<String, TextProcessorError> {
        // Candle forward passes are CPU-bound (~1 s per 100 prompt tokens on
        // CPU), so the whole generation runs on the blocking pool — an inline
        // loop would stall this tokio worker for seconds and starve audio
        // streaming for every session. The backend lock is held for the full
        // turn so a concurrent turn cannot interleave its `reset_cache` with
        // our per-token `forward` calls and corrupt the KV-cache.
        let backend = Arc::clone(&self.backend);
        let tokenizer = Arc::clone(&self.tokenizer);
        let template = Arc::clone(&self.template);
        let prefilled = Arc::clone(&self.prefilled);
        let input = input.to_string();
        let max_tokens = self.max_tokens();
        let temperature = self.temperature();
        let top_p = self.top_p();
        let repeat_penalty = self.repeat_penalty();
        let seed = self.config.seed;
        let audit_tx = self.audit_tx.clone();
        let device = self.config.device.clone();
        let prefill_cache = self.config.prefill_cache;

        tokio::task::spawn_blocking(move || {
            // Since Memory processor wraps the prompt with ChatTemplate,
            // `input` is already fully formatted.
            // We just tokenize it.
            let t_tokenize = std::time::Instant::now();
            let prompt_tokens = crate::chat_template::encode_prompt(&tokenizer, &input)
                .map_err(|e| TextProcessorError::new("llm", format!("tokenization failed: {e}")))?;
            let tokenize_ms = t_tokenize.elapsed().as_secs_f64() * 1000.0;

            let eos_token = Self::resolve_eos_token(&tokenizer, template.as_ref());

            let mut logits_processor = LogitsProcessor::new(seed, Some(temperature), top_p);

            let t_lock = std::time::Instant::now();
            let mut backend = backend.lock();
            let _lock_wait_ms = t_lock.elapsed().as_secs_f64() * 1000.0;

            // Reset KV-cache to system-prompt snapshot (or clear).
            let t_cache = std::time::Instant::now();
            backend.reset_cache();
            let cache_reset_ms = t_cache.elapsed().as_secs_f64() * 1000.0;

            let mut output_tokens: Vec<u32> = Vec::new();

            // Ingest prompt tokens, get logits for last prompt token.
            let t_ingest = std::time::Instant::now();
            let first_logits = Self::ingest_prompt(&mut backend, &prompt_tokens, &prefilled)?;
            let prompt_ingest_ms = t_ingest.elapsed().as_secs_f64() * 1000.0;
            let _pos = prompt_tokens.len();

            // Auto-regressive generation.
            let t_generate = std::time::Instant::now();
            let first_logits =
                Self::apply_repeat_penalty(&first_logits, &output_tokens, repeat_penalty)?;
            let mut next_token = logits_processor
                .sample(&first_logits)
                .map_err(|e| TextProcessorError::new("llm", format!("sampling failed: {e}")))?;

            for pos in (prompt_tokens.len()..).take(max_tokens as usize) {
                if next_token == eos_token {
                    break;
                }
                output_tokens.push(next_token);

                let logits = backend
                    .forward(&[next_token], pos)
                    .map_err(|e| TextProcessorError::new("llm", e.message))?;

                let logits = Self::apply_repeat_penalty(&logits, &output_tokens, repeat_penalty)?;

                next_token = logits_processor
                    .sample(&logits)
                    .map_err(|e| TextProcessorError::new("llm", format!("sampling failed: {e}")))?;
            }

            let generate_ms = t_generate.elapsed().as_secs_f64() * 1000.0;
            let gen_tokens = output_tokens.len();
            let tokens_per_sec = if gen_tokens >= 2 && generate_ms > 0.0 {
                (gen_tokens - 1) as f64 / (generate_ms / 1000.0)
            } else {
                0.0
            };

            let result = tokenizer.decode(&output_tokens, true).map_err(|e| {
                TextProcessorError::new("llm", format!("detokenization failed: {e}"))
            })?;

            let result = Self::clean_output(template.as_ref(), &result)
                .trim()
                .to_string();

            // `input` is the templated prompt, never something to speak: an
            // empty generation is an empty reply.
            if result.is_empty() {
                tracing::warn!("LLM produced no output");
            }

            if let Some(tx) = &audit_tx {
                let timing_payload = build_llm_detail_payload(
                    cache_reset_ms,
                    prompt_ingest_ms,
                    prompt_tokens.len(),
                    generate_ms,
                    gen_tokens,
                    None,
                    None,
                    tokens_per_sec,
                    &device,
                    prefill_cache,
                    tokenize_ms,
                );
                let _ = tx.try_send(voice_pipes_sdk::TraceEvent::new(
                    "llm:local",
                    "llm_detail",
                    timing_payload,
                ));
            }

            Ok(result)
        })
        .await
        .map_err(|e| TextProcessorError::new("llm", format!("inference task failed: {e}")))?
    }

    pub(super) async fn process_stream_inner(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        // As in `process_inner`, generation is CPU-bound candle work and runs
        // on the blocking pool. The backend lock is held for the full turn so
        // a concurrent turn cannot interleave cache ops with ours.
        let backend = Arc::clone(&self.backend);
        let tokenizer = Arc::clone(&self.tokenizer);
        let template = Arc::clone(&self.template);
        let prefilled = Arc::clone(&self.prefilled);
        let input = input.to_string();
        let max_tokens = self.max_tokens();
        let temperature = self.temperature();
        let top_p = self.top_p();
        let repeat_penalty = self.repeat_penalty();
        let seed = self.config.seed;
        let text_chunking = self.text_chunking.clone();
        let audit_tx = self.audit_tx.clone();
        let device = self.config.device.clone();
        let prefill_cache = self.config.prefill_cache;
        // Time to first token runs from here, as the OpenAI-compatible
        // processor's does from its request: waiting for the backend and
        // ingesting the prompt (seconds on a CPU, growing with the history)
        // are part of it. Counted from generation alone it read ~0 ms, and
        // the report charged the wait to TTS.
        let t_request = std::time::Instant::now();

        tokio::task::spawn_blocking(move || -> Result<(), TextProcessorError> {
            use std::time::Instant;

            // Build prompt tokens
            let t_tokenize = Instant::now();
            let prompt_tokens = crate::chat_template::encode_prompt(&tokenizer, &input)
                .map_err(|e| TextProcessorError::new("llm", format!("tokenization failed: {e}")))?;
            let tokenize_ms = t_tokenize.elapsed().as_secs_f64() * 1000.0;
            let ingest_count = prompt_tokens.len();

            let eos_token = Self::resolve_eos_token(&tokenizer, template.as_ref());

            let mut logits_processor = LogitsProcessor::new(seed, Some(temperature), top_p);

            // ── Lock backend & Reset KV-cache ────────
            let t_lock = Instant::now();
            let mut backend = backend.lock();
            let _lock_wait_ms = t_lock.elapsed().as_secs_f64() * 1000.0;

            let t_cache = Instant::now();
            backend.reset_cache();
            let cache_reset_ms = t_cache.elapsed().as_secs_f64() * 1000.0;

            let mut output_tokens: Vec<u32> = Vec::new();
            let mut sentence_buffer = String::new();
            // Incremental streaming decoder: feed one token id per step; it
            // returns only the newly-completed text and does the byte-level
            // UTF-8 holdback natively. O(1) amortised per token — re-decoding
            // the whole `output_tokens` list every step would be O(n²) over
            // the reply length, all inside the backend lock.
            let mut decode_stream = tokenizer.decode_stream(true);

            // ── Ingest prompt tokens ───────────────────
            let t_ingest = Instant::now();
            let first_logits = Self::ingest_prompt(&mut backend, &prompt_tokens, &prefilled)?;
            let prompt_ingest_ms = t_ingest.elapsed().as_secs_f64() * 1000.0;
            let _pos = prompt_tokens.len();

            tracing::info!(
                cache_reset_ms = format!("{cache_reset_ms:.1}"),
                prompt_ingest_ms = format!("{prompt_ingest_ms:.1}"),
                prompt_tokens = ingest_count,
                ms_per_prompt_token = format!(
                    "{:.1}",
                    if ingest_count > 0 {
                        prompt_ingest_ms / ingest_count as f64
                    } else {
                        0.0
                    }
                ),
                "LLM prompt ingested"
            );

            // ── Auto-regressive generation with streaming ──
            let t_generate = Instant::now();
            let first_logits =
                Self::apply_repeat_penalty(&first_logits, &output_tokens, repeat_penalty)?;
            let mut next_token =
                logits_processor
                    .sample(&first_logits)
                    .map_err(|e| TextProcessorError::new("llm", format!("sampling failed: {e}")))?;

            let mut first_token_ms: Option<f64> = None;
            // Same value as `first_token_ms`, but consumed (`take`) on the first
            // emitted `TextFragment` so the orchestrator's bridge can use the
            // provider-internal TTFT instead of wall-clock from
            // `process_utterance` entry. See `pipeline::orchestrate` bridge loop.
            let mut pending_first_token_ms: Option<f64> = None;
            let mut first_sentence_chars: Option<usize> = None;
            // Set when a fragment send says the consumer is gone/stalled:
            // stop generating instead of grinding to `max_tokens` while
            // holding the backend lock.
            let mut consumer_stopped = false;

            for (pos, i) in (prompt_tokens.len()..).zip(0..max_tokens) {
                if cancel.is_cancelled() {
                    tracing::info!("LLM generation cancelled at token {i}");
                    break;
                }

                if next_token == eos_token {
                    break;
                }
                output_tokens.push(next_token);

                if first_token_ms.is_none() {
                    let t = t_request.elapsed().as_secs_f64() * 1000.0;
                    first_token_ms = Some(t);
                    pending_first_token_ms = Some(t);
                }

                // Feed this token to the incremental decoder. It returns the
                // newly-completed text, or nothing while a multi-byte character
                // is still spanning tokens (byte-level BPE tokens are not
                // individually valid UTF-8) — so the prefix is never re-decoded.
                if let Some(piece) = decode_stream.step(next_token).map_err(|e| {
                    TextProcessorError::new("llm", format!("token decode failed: {e}"))
                })? {
                    let token_text = Self::clean_output(template.as_ref(), &piece);
                    sentence_buffer.push_str(&token_text);
                }

                // Emit complete sentences as they form.
                while let Some((sentence, rest)) = split_first_sentence(&sentence_buffer, text_chunking.min_soft_split_chars, text_chunking.min_soft_split_words, false) {
                    if !sentence.trim().is_empty() {
                        if first_sentence_chars.is_none() {
                            first_sentence_chars = Some(sentence.trim_end().len());
                        }
                        tracing::debug!(
                            fragment_chars = sentence.chars().count(),
                            "LLM streaming fragment"
                        );
                        if !Self::send_fragment_or_stop(
                            &tx,
                            &cancel,
                            TextFragment::new(sentence.to_string())
                                .with_first_token_ms(pending_first_token_ms.take()),
                        ) {
                            consumer_stopped = true;
                            break;
                        }
                    }
                    sentence_buffer = rest.to_string();
                }
                if consumer_stopped {
                    break;
                }

                let logits =
                    backend
                        .forward(&[next_token], pos)
                        .map_err(|e| TextProcessorError::new("llm", e.message))?;

                let logits =
                    Self::apply_repeat_penalty(&logits, &output_tokens, repeat_penalty)?;

                next_token = logits_processor
                    .sample(&logits)
                    .map_err(|e| TextProcessorError::new("llm", format!("sampling failed: {e}")))?;
            }

            // No final re-decode needed: `decode_stream` emitted every complete
            // piece as it formed. Any bytes still held back are a genuinely
            // incomplete trailing UTF-8 sequence and are correctly dropped
            // (decoding them would only append a U+FFFD).

            let generate_ms = t_generate.elapsed().as_secs_f64() * 1000.0;
            let gen_tokens = output_tokens.len();
            let tokens_per_sec = if gen_tokens >= 2 && generate_ms > 0.0 {
                (gen_tokens - 1) as f64 / (generate_ms / 1000.0)
            } else {
                0.0
            };

            tracing::info!(
                generate_ms = format!("{generate_ms:.1}"),
                output_tokens = gen_tokens,
                first_token_ms = format!("{:.1}", first_token_ms.unwrap_or(0.0)),
                tokens_per_sec = format!("{tokens_per_sec:.1}"),
                cache_reset_ms = format!("{cache_reset_ms:.1}"),
                prompt_ingest_ms = format!("{prompt_ingest_ms:.1}"),
                prompt_tokens = ingest_count,
                total_ms = format!("{:.1}", tokenize_ms + cache_reset_ms + prompt_ingest_ms + generate_ms),
                "LLM generation complete"
            );

            // Flush remaining buffer.
            let final_text = sentence_buffer.trim().to_string();
            if first_sentence_chars.is_none() && !final_text.is_empty() {
                first_sentence_chars = Some(final_text.len());
            }

            // Emit timing as a trace event for the audit system.
            if let Some(tx) = &audit_tx {
                let timing_payload = build_llm_detail_payload(
                    cache_reset_ms,
                    prompt_ingest_ms,
                    ingest_count,
                    generate_ms,
                    gen_tokens,
                    first_token_ms,
                    first_sentence_chars,
                    tokens_per_sec,
                    &device,
                    prefill_cache,
                    tokenize_ms,
                );
                let _ = tx.try_send(voice_pipes_sdk::TraceEvent::new(
                    "llm:local",
                    "llm_detail",
                    timing_payload,
                ));
            }

            // Flush remaining buffer. Skipped when the consumer already went
            // away/stalled — the send would fail again and only delay release
            // of the backend lock.
            if !consumer_stopped {
                tracing::debug!(
                    fragment_chars = final_text.chars().count(),
                    is_final = true,
                    "LLM streaming fragment"
                );
                let _ = Self::send_fragment_or_stop(
                    &tx,
                    &cancel,
                    TextFragment::new(final_text)
                        .with_is_end_of_response(true)
                        .with_token_count(Some(u32::try_from(output_tokens.len()).unwrap_or(u32::MAX)))
                        // If the entire response fit before any sentence boundary,
                        // forward the captured TTFT on the final flush so it's not lost.
                        .with_first_token_ms(pending_first_token_ms.take()),
                );
            }

            Ok(())
        })
        .await
        .map_err(|e| TextProcessorError::new("llm", format!("inference task failed: {e}")))?
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "direct assembly of the flat llm_detail audit JSON payload"
)]
pub(crate) fn build_llm_detail_payload(
    cache_reset_ms: f64,
    prompt_ingest_ms: f64,
    prompt_tokens: usize,
    generate_ms: f64,
    output_tokens: usize,
    first_token_ms: Option<f64>,
    first_sentence_chars: Option<usize>,
    tokens_per_sec: f64,
    device: &str,
    prefill_cache: bool,
    tokenize_ms: f64,
) -> serde_json::Value {
    let total_ms = tokenize_ms + cache_reset_ms + prompt_ingest_ms + generate_ms;
    serde_json::json!({
        "tokenize_ms": (tokenize_ms * 10.0).round() / 10.0,
        "cache_reset_ms": (cache_reset_ms * 10.0).round() / 10.0,
        "prompt_ingest_ms": (prompt_ingest_ms * 10.0).round() / 10.0,
        "prompt_tokens": prompt_tokens,
        "generate_ms": (generate_ms * 10.0).round() / 10.0,
        "output_tokens": output_tokens,
        "first_token_ms": first_token_ms.map(|ms| (ms * 10.0).round() / 10.0),
        "first_sentence_chars": first_sentence_chars,
        "tokens_per_sec": (tokens_per_sec * 10.0).round() / 10.0,
        "total_ms": (total_ms * 10.0).round() / 10.0,
        "device": device,
        "prefill_cache": prefill_cache,
    })
}

#[cfg(test)]
mod tests {
    use super::build_llm_detail_payload;

    #[test]
    fn llm_detail_first_token_ms_is_null_when_absent() {
        let payload = build_llm_detail_payload(
            0.2,
            145.6,
            24,
            1.1,
            0,
            None,
            None,
            0.0,
            "cpu",
            false,
            0.0,
        );
        assert!(
            payload["first_token_ms"].is_null(),
            "missing first_token_ms must serialize as null, got {}",
            payload["first_token_ms"]
        );
    }
}
