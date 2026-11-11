//! Config-driven construction of the local LLM text processor.

use super::{LlmConfig, LlmProcessor, chat_template, schema};
use voice_pipes_sdk::options::as_f32;

// Default values for LLM config options omitted from the TOML.
const DEFAULT_SYSTEM_PROMPT: &str = "You are a helpful assistant. Respond briefly.";
const DEFAULT_MAX_TOKENS: u32 = 256;
const DEFAULT_TEMPERATURE: f32 = 0.7;
const DEFAULT_LLM_DEVICE: &str = "cpu";
const DEFAULT_CHAT_TEMPLATE: &str = "chatml";
const DEFAULT_PREFILL_CACHE: bool = false;
const DEFAULT_REPEAT_PENALTY: f32 = 1.1;
const DEFAULT_SEED: u64 = 42;
const DEFAULT_POOL_SIZE: usize = 1;

/// The settings the local LLM reads from its option table.
struct GgufOptions {
    model_path: std::path::PathBuf,
    tokenizer_path: Option<std::path::PathBuf>,
    system_prompt: String,
    max_tokens: u32,
    temperature: f32,
    device: String,
    chat_template: String,
    prefill_cache: bool,
    repeat_penalty: f32,
    top_p: Option<f64>,
    seed: u64,
    pool_size: usize,
    history_max_turns: Option<usize>,
    full_prompts: bool,
}

impl GgufOptions {
    fn read(
        cfg: &voice_pipes_sdk::TextProcessorConfig,
    ) -> Result<Self, Vec<voice_pipes_sdk::ConfigError>> {
        let mut o = cfg.options();
        let model = o.string("model");
        if model.is_none() {
            o.error("model", "LLM text processor requires a 'model' field");
        }
        let options = Self {
            model_path: model.unwrap_or_default().into(),
            // A GGUF carries weights but not a `tokenizers` JSON, so the
            // loader looks for one beside the model. Distributions that
            // ship the GGUF alone borrow a tokenizer from elsewhere.
            tokenizer_path: o.string("tokenizer_path").map(Into::into),
            system_prompt: o.string_or("system_prompt", DEFAULT_SYSTEM_PROMPT),
            max_tokens: o.u32_in("max_tokens", 1..=1_000_000, DEFAULT_MAX_TOKENS),
            temperature: as_f32(o.f64_in("temperature", 0.0..=2.0, f64::from(DEFAULT_TEMPERATURE))),
            device: o.one_of("device", &["cpu", "cuda", "metal"], DEFAULT_LLM_DEVICE),
            chat_template: o.one_of("chat_template", chat_template::NAMES, DEFAULT_CHAT_TEMPLATE),
            prefill_cache: o.bool("prefill_cache", DEFAULT_PREFILL_CACHE),
            repeat_penalty: as_f32(o.f64_in(
                "repeat_penalty",
                0.1..=10.0,
                f64::from(DEFAULT_REPEAT_PENALTY),
            )),
            top_p: o.optional_f64_in("top_p", 0.0..=1.0),
            seed: o.u64_in("seed", 0..=u64::MAX, DEFAULT_SEED),
            pool_size: o.usize_in("pool_size", 1..=32, DEFAULT_POOL_SIZE),
            history_max_turns: o
                .optional_u64_in("history_max_turns", 1..=10_000)
                .and_then(|n| usize::try_from(n).ok()),
            full_prompts: o.bool(voice_pipes_sdk::text::AUDIT_FULL_PROMPTS_OPTION, false),
        };
        o.finish()?;
        Ok(options)
    }
}

/// Factory for the local LLM (GGUF) text processor.
#[derive(Debug)]
pub struct LlmFactory;

impl voice_pipes_sdk::TextProcessorFactory for LlmFactory {
    fn name(&self) -> &'static str {
        "llm"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(
        &self,
        cfg: &voice_pipes_sdk::TextProcessorConfig,
    ) -> Vec<voice_pipes_sdk::ConfigError> {
        match GgufOptions::read(cfg) {
            Err(problems) => problems,
            Ok(options) if !options.model_path.exists() => vec![voice_pipes_sdk::ConfigError::new(
                "text_processor.model",
                format!("LLM model not found: {}", options.model_path.display()),
            )],
            Ok(_) => Vec::new(),
        }
    }

    fn build(
        &self,
        cfg: &voice_pipes_sdk::TextProcessorConfig,
        ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::TextProcessor>, voice_pipes_sdk::BuildError> {
        let audit_tx = ctx.audit_tx.clone();
        let param_store = ctx.params;
        let text_chunking = &ctx.text_chunking;
        let GgufOptions {
            model_path,
            tokenizer_path,
            system_prompt,
            max_tokens,
            temperature,
            device,
            chat_template: chat_tmpl,
            prefill_cache,
            repeat_penalty,
            top_p,
            seed,
            pool_size,
            history_max_turns,
            full_prompts,
        } = GgufOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;

        let llm_config = LlmConfig {
            model_path,
            tokenizer_path,
            system_prompt: system_prompt.clone(),
            max_tokens,
            temperature,
            device,
            chat_template: chat_tmpl.clone(),
            prefill_cache,
            repeat_penalty,
            top_p,
            seed,
            pool_size,
        };

        // Load the weights once per worker, not once per session.
        //
        // Every session builds its own processors so conversation histories
        // stay separate; loading the weights there would re-read the whole
        // GGUF on each connect -- 44 s inside a CPU-bound container, during
        // which the session is not yet listening. The caller would speak,
        // nothing would be transcribed, and every log line would say the
        // worker is healthy. The
        // history lives in the wrapper below, so only that has to be per
        // session; the weights can be shared.
        let mut llm = load_shared(&llm_config, text_chunking)?.with_audit_tx(audit_tx.clone());

        // Adopt cells from the worker-wide ParamRegistry so PATCH
        // /debug/params updates take effect on the next turn.
        let mut shared_system_prompt: Option<std::sync::Arc<arc_swap::ArcSwap<String>>> = None;
        if let Some(store) = param_store {
            if let Some(cell) = store.int_cell("max_tokens") {
                llm = llm.with_max_tokens_cell(cell);
            }
            if let Some(cell) = store.float_cell("temperature") {
                llm = llm.with_temperature_cell(cell);
            }
            if let Some(cell) = store.float_cell("top_p") {
                llm = llm.with_top_p_cell(cell);
            }
            if let Some(cell) = store.float_cell("repeat_penalty") {
                llm = llm.with_repeat_penalty_cell(cell);
            }
            if let Some(cell) = store.string_cell("system_prompt") {
                llm = llm.with_system_prompt_cell(std::sync::Arc::clone(&cell));
                shared_system_prompt = Some(cell);
            }
        }

        let template_box = chat_template::from_name(&chat_tmpl);
        let budget = llm.token_budget();
        let mut wrapper = crate::memory::ConversationMemoryWrapper::new(
            Box::new(llm),
            template_box,
            system_prompt,
            audit_tx,
        )
        .with_token_budget(budget);
        if let Some(max_turns) = history_max_turns {
            wrapper = wrapper.with_history_max_turns(max_turns);
        }
        if let Some(cell) = shared_system_prompt {
            wrapper = wrapper.with_system_prompt_cell(cell);
        }
        if full_prompts {
            wrapper = wrapper.with_full_prompts(true);
        }
        Ok(Box::new(wrapper))
    }

    fn provider_schema(
        &self,
        cfg: &voice_pipes_sdk::TextProcessorConfig,
    ) -> voice_pipes_sdk::schema::ProviderSchema {
        schema::schema(cfg)
    }
}

// ── Shared model cache ──────────────────────────────────────

/// Loaded models, keyed by everything that affects the weights in memory.
///
/// Worker-wide and never evicted: a voice worker runs one or two pipelines
/// for its lifetime, so the set is tiny and bounded by the configuration
/// rather than by traffic.
static LOADED: std::sync::OnceLock<
    parking_lot::Mutex<std::collections::HashMap<String, LlmProcessor>>,
> = std::sync::OnceLock::new();

/// Identity of a loaded model.
///
/// Only the fields that change the weights or the prefilled KV cache belong
/// here. Sampling parameters are deliberately absent: they live in atomic
/// cells that `/debug/params` can retune at runtime, so two sessions that
/// differ only in temperature should share one model rather than load it
/// twice.
fn cache_key(cfg: &LlmConfig) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        cfg.model_path.display(),
        cfg.tokenizer_path
            .as_ref()
            .map_or_else(String::new, |p| p.display().to_string()),
        cfg.chat_template,
        cfg.device,
        cfg.pool_size.max(1),
        cfg.prefill_cache,
        // The prefill caches the system prompt's KV entries, so a different
        // prompt is a different loaded state even with identical weights.
        if cfg.prefill_cache {
            &cfg.system_prompt
        } else {
            ""
        },
    )
}

/// Return a handle to the model for `cfg`, loading it if this is the first
/// time the worker has seen one like it.
fn load_shared(
    cfg: &LlmConfig,
    text_chunking: &voice_pipes_sdk::TextChunkingConfig,
) -> Result<LlmProcessor, anyhow::Error> {
    let key = cache_key(cfg);
    let cache = LOADED.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

    if let Some(existing) = cache.lock().get(&key) {
        tracing::debug!(model = %cfg.model_path.display(), "reusing loaded LLM");
        return Ok(existing.clone());
    }

    // Loaded outside the lock: this is the slow path by definition, and
    // holding the map would stall every other pipeline's first session
    // behind it. Two sessions racing the same model both load, and the
    // second's copy is dropped below -- wasteful once at startup, against a
    // lock held for the length of a model load on every other key.
    let loaded = LlmProcessor::from_config(cfg.clone(), text_chunking.clone())?;

    let mut guard = cache.lock();
    let entry = guard.entry(key).or_insert(loaded);
    Ok(entry.clone())
}

#[cfg(test)]
#[path = "tests/factory.rs"]
mod cache_tests;
