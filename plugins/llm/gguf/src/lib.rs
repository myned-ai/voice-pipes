//! A local GGUF language model (candle), as a voice-pipes text-processor
//! plugin: model loading, inference, and chat templating. The binary
//! registers [`factory`] and [`wizard_choices`]; `[[text_processor]] type =
//! "llm"` selects it.
//!
//! This file holds the processor surface: `LlmConfig`, the `LlmProcessor`
//! struct with its live parameter cells, and the `TextProcessor` impl. The
//! heavy lifting lives in submodules: `generation` (blocking-pool token
//! loops), `factory` (TOML-driven construction), `backend` /
//! `candle_gguf` (forward pass + KV-cache), `chat_template`, `prefill`,
//! and `schema`.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

mod backend;
mod candle_gguf;
mod chat_template;
mod factory;
mod generation;
mod memory;
mod prefill;
mod schema;
mod wizard;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use parking_lot::Mutex;
use tokenizers::Tokenizer;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) use backend::LlmBackend;
pub(crate) use chat_template::ChatTemplate;
pub(crate) use factory::LlmFactory;
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::options::as_f32;
use voice_pipes_sdk::sentence::split_first_sentence;
use voice_pipes_sdk::text::{TextFragment, TextProcessor, TextProcessorError};
pub use wizard::wizard_choices;

/// The factory the binary registers.
pub fn factory() -> Box<dyn voice_pipes_sdk::TextProcessorFactory> {
    Box::new(LlmFactory)
}

/// Configuration for an LLM text processor.
#[derive(Debug, Clone)]
pub(crate) struct LlmConfig {
    /// Path to the GGUF model file.
    pub model_path: PathBuf,
    /// Path to a tokenizer.json file. If not set, looked up next to the GGUF.
    pub tokenizer_path: Option<PathBuf>,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Device to run on: `"cpu"`, `"cuda"`, `"metal"`.
    pub device: String,
    /// Chat template name, one of [`chat_template::NAMES`].
    pub chat_template: String,
    /// Pre-cache the system prompt in the KV-cache at startup, where the
    /// backend can continue a batch at an offset; otherwise left off with a
    /// warning.
    pub prefill_cache: bool,
    /// Repetition penalty applied to already-generated tokens (1.0 = off).
    pub repeat_penalty: f32,
    /// Nucleus sampling threshold (None = disabled).
    pub top_p: Option<f64>,
    /// RNG seed for sampling.
    pub seed: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/smollm2-135m-instruct.Q4_K_M.gguf"),
            tokenizer_path: None,
            system_prompt: "You are a helpful assistant. Respond briefly.".to_string(),
            max_tokens: 256,
            temperature: 0.7,
            device: "cpu".to_string(),
            chat_template: "chatml".to_string(),
            prefill_cache: false,
            repeat_penalty: 1.1,
            top_p: None,
            seed: 42,
        }
    }
}

/// A text processor that runs input through an LLM backend.
///
/// Composes an `LlmBackend` (model forward pass / KV-cache) with a
/// `ChatTemplate` (prompt formatting / EOS detection) and a tokenizer.
/// A loaded local model, ready to generate.
///
/// Holds no conversation state: history lives in the conversation-memory
/// wrapper around it. That separation is what makes this cheap to `Clone` --
/// every field is either an `Arc` over the loaded weights or a small value --
/// so a worker loads a model once and hands each session its own history
/// around the same weights.
///
/// Cloning shares the backend mutex, so concurrent sessions serialise their
/// generations rather than running in parallel. That is the right trade for a
/// local model: the alternative is a full copy of the weights per session,
/// which costs both the memory and a model load on the connect path.
#[derive(Clone)]
pub(crate) struct LlmProcessor {
    // Backend, tokenizer, and template are shared with the blocking-pool
    // closure that runs each generation (see `process_inner` /
    // `process_stream_inner`), hence the `Arc`s. The mutex is `parking_lot`
    // so a panic inside inference cannot poison it and take down every
    // subsequent turn.
    backend: Arc<Mutex<Box<dyn LlmBackend>>>,
    tokenizer: Arc<Tokenizer>,
    template: Arc<dyn ChatTemplate>,
    config: LlmConfig,
    /// Short display name derived from the GGUF filename stem (e.g.
    /// `"SmolLM2-135M-Instruct-Q4_K_M"`). Cached at construction so
    /// `name()` never allocates and the value is stable for the process
    /// lifetime.
    model_name: String,
    /// The system-prompt tokens already in the KV-cache snapshot (none if
    /// prefill is off). A prompt that starts with them is fed from where
    /// they end.
    prefilled: Arc<Vec<u32>>,
    /// Optional channel to emit trace events to the audit system.
    audit_tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
    // ── Runtime-mutable parameter cells ─────────────────────────
    // Initialised from `config` on construction. May be replaced by
    // `with_*_cell()` to share a cell with a worker-wide ParamRegistry,
    // so PATCH /debug/params updates take effect on the next turn.
    max_tokens_cell: Arc<AtomicI64>,
    /// f64 bits -- sampling temperature.
    temperature_cell: Arc<AtomicU64>,
    /// f64 bits -- nucleus sampling cutoff. NaN sentinel = disabled.
    top_p_cell: Arc<AtomicU64>,
    /// f64 bits -- repetition penalty.
    repeat_penalty_cell: Arc<AtomicU64>,
    /// System prompt -- currently only consumed at construction (prefill).
    /// Held as an `ArcSwap` so future re-prefill / per-turn injection paths
    /// can pick up updates without restarting the processor.
    system_prompt_cell: Arc<ArcSwap<String>>,
    text_chunking: voice_pipes_sdk::TextChunkingConfig,
    /// Component-base telemetry / health surface.
    core: Arc<ComponentCore>,
}

impl LlmProcessor {
    /// Snapshot the current `max_tokens` budget.
    #[inline]
    pub fn max_tokens(&self) -> u32 {
        u32::try_from(self.max_tokens_cell.load(Ordering::Relaxed).max(0)).unwrap_or(u32::MAX)
    }

    /// Snapshot the current sampling temperature.
    #[inline]
    pub fn temperature(&self) -> f64 {
        f64::from_bits(self.temperature_cell.load(Ordering::Relaxed))
    }

    /// Snapshot the current top-p, or `None` if disabled (NaN sentinel).
    #[inline]
    pub fn top_p(&self) -> Option<f64> {
        let v = f64::from_bits(self.top_p_cell.load(Ordering::Relaxed));
        if v.is_nan() { None } else { Some(v) }
    }

    /// Snapshot the current repetition penalty.
    #[inline]
    pub fn repeat_penalty(&self) -> f32 {
        as_f32(f64::from_bits(
            self.repeat_penalty_cell.load(Ordering::Relaxed),
        ))
    }

    /// Adopt an externally-owned cell for `max_tokens`.
    #[must_use]
    pub fn with_max_tokens_cell(mut self, cell: Arc<AtomicI64>) -> Self {
        self.max_tokens_cell = cell;
        self
    }

    #[must_use]
    pub fn with_temperature_cell(mut self, cell: Arc<AtomicU64>) -> Self {
        self.temperature_cell = cell;
        self
    }

    #[must_use]
    pub fn with_top_p_cell(mut self, cell: Arc<AtomicU64>) -> Self {
        self.top_p_cell = cell;
        self
    }

    #[must_use]
    pub fn with_repeat_penalty_cell(mut self, cell: Arc<AtomicU64>) -> Self {
        self.repeat_penalty_cell = cell;
        self
    }

    #[must_use]
    pub fn with_system_prompt_cell(mut self, cell: Arc<ArcSwap<String>>) -> Self {
        self.system_prompt_cell = cell;
        self
    }
}

impl std::fmt::Debug for LlmProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmProcessor")
            .field("config", &self.config)
            .field("prefilled_tokens", &self.prefilled.len())
            .finish_non_exhaustive()
    }
}

impl LlmProcessor {
    /// What a prompt may take of this model's window, for the history
    /// wrapper to trim to.
    pub(crate) fn token_budget(&self) -> memory::TokenBudget {
        memory::TokenBudget {
            tokenizer: Arc::clone(&self.tokenizer),
            context: self.backend.lock().context_tokens(),
            max_tokens: Arc::clone(&self.max_tokens_cell),
        }
    }

    /// Construct from an already-built backend and tokenizer.
    /// Prefer [`LlmProcessor::from_config`] for the standard GGUF path.
    pub fn new(
        mut backend: Box<dyn LlmBackend>,
        tokenizer: Tokenizer,
        template: Box<dyn ChatTemplate>,
        config: LlmConfig,
        text_chunking: voice_pipes_sdk::TextChunkingConfig,
    ) -> Result<Self, TextProcessorError> {
        let prefill = config.prefill_cache && backend.continues_batches();
        if config.prefill_cache && !prefill {
            tracing::warn!(
                "prefill_cache is not supported for this model architecture; the system prompt is computed with each turn"
            );
        }
        let prefilled = if prefill {
            prefill::prefill_system_prompt(
                backend.as_mut(),
                &tokenizer,
                template.as_ref(),
                &config.system_prompt,
            )?
        } else {
            Vec::new()
        };

        Ok(Self {
            backend: Arc::new(Mutex::new(backend)),
            tokenizer: Arc::new(tokenizer),
            template: Arc::from(template),
            prefilled: Arc::new(prefilled),
            model_name: config
                .model_path
                .file_stem()
                .map_or_else(|| "llm".to_string(), |s| s.to_string_lossy().into_owned()),
            audit_tx: None,
            max_tokens_cell: Arc::new(AtomicI64::new(config.max_tokens as i64)),
            temperature_cell: Arc::new(AtomicU64::new((config.temperature as f64).to_bits())),
            top_p_cell: Arc::new(AtomicU64::new(config.top_p.unwrap_or(f64::NAN).to_bits())),
            repeat_penalty_cell: Arc::new(AtomicU64::new((config.repeat_penalty as f64).to_bits())),
            system_prompt_cell: Arc::new(ArcSwap::from_pointee(config.system_prompt.clone())),
            config,
            text_chunking,
            core: Arc::new(ComponentCore::new("text:llm", ComponentKind::Text)),
        })
    }

    /// Attach an audit trace channel for emitting LLM timing events.
    #[must_use]
    pub fn with_audit_tx(
        mut self,
        tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
    ) -> Self {
        self.audit_tx = tx;
        self
    }

    /// Standard constructor: loads a GGUF model, tokenizer, and chat template
    /// from config paths.
    pub fn from_config(
        config: LlmConfig,
        text_chunking: voice_pipes_sdk::TextChunkingConfig,
    ) -> Result<Self, TextProcessorError> {
        let device = Self::resolve_device(&config.device)?;

        let backend = candle_gguf::CandleGgufBackend::load(&config.model_path, device)
            .map_err(|e| TextProcessorError::new("llm", e.message))?;

        // Load tokenizer -- look next to the GGUF or at explicit path
        let tokenizer_path = config.tokenizer_path.clone().unwrap_or_else(|| {
            config
                .model_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("tokenizer.json")
        });

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| TextProcessorError::new("llm", format!(
                "failed to load tokenizer from {}: {e}. A GGUF file carries weights but not a tokenizer; put a tokenizer.json beside the model, or point `tokenizer_path` at one from the same model family.",
                tokenizer_path.display()
            )))?;

        let template = chat_template::from_name(&config.chat_template);

        tracing::info!(
            model = %config.model_path.display(),
            device = %config.device,
            chat_template = %config.chat_template,
            prefill_cache = config.prefill_cache,
            "LLM loaded (quantized GGUF)"
        );

        Self::new(
            Box::new(backend),
            tokenizer,
            template,
            config,
            text_chunking,
        )
    }

    /// Resolve a candle `Device` from a config string.
    fn resolve_device(name: &str) -> Result<candle_core::Device, TextProcessorError> {
        match name {
            "cpu" => Ok(candle_core::Device::Cpu),
            "cuda" => candle_core::Device::new_cuda(0).map_err(|e| {
                TextProcessorError::new("llm", format!("CUDA device init failed: {e}"))
            }),
            "metal" => candle_core::Device::new_metal(0).map_err(|e| {
                TextProcessorError::new("llm", format!("Metal device init failed: {e}"))
            }),
            other => Err(TextProcessorError::new(
                "llm",
                format!("unsupported device: \"{other}\" (use cpu, cuda, or metal)"),
            )),
        }
    }
}

#[async_trait]
impl TextProcessor for LlmProcessor {
    fn name(&self) -> &str {
        &self.model_name
    }

    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        let start = std::time::Instant::now();
        let res = self.process_inner(input).await;
        match &res {
            Ok(_) => self.core.record_ok(start.elapsed()),
            Err(e) => self.core.record_error(e),
        }
        res
    }

    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let start = std::time::Instant::now();
        let res = self.process_stream_inner(input, tx, cancel).await;
        match &res {
            Ok(()) => self.core.record_ok(start.elapsed()),
            Err(e) => self.core.record_error(e),
        }
        res
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(std::sync::Arc::clone(
            &self.core,
        ))))
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/llm.rs"]
mod tests;
