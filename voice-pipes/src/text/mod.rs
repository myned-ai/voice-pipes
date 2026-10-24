//! Text processing chain -- LLM inference and prompt transforms.
//!
//! Re-exports the SDK's [`TextProcessor`] / [`TextProcessorFactory`] traits,
//! which built-in processors and plugin crates implement alike, and holds
//! the processor registry and the chain runners.
//!
//! # Plugin registration
//!
//! External provider crates depend only on `voice-pipes-sdk` and call
//! [`register_text_processor_factory`] before `voice_pipes::run()`:
//!
//! ```ignore
//! my_llm_plugin::register();   // calls register_text_processor_factory internally
//! voice_pipes::run().await;
//! ```

#[cfg(feature = "text-grpc")]
pub mod grpc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ── Traits and types ───────────────────────────────────────
// Built-in processors and plugin crates implement the same SDK traits, so a
// registered factory joins the built-ins as it is.

pub use voice_pipes_sdk::BuildContext;
pub use voice_pipes_sdk::text::{
    TextErrorKind, TextFragment, TextProcessor, TextProcessorError, TextProcessorFactory,
};

// ── Chain runner ────────────────────────────────────────────

/// Run text through a chain of processors in order.
/// Returns the original text unchanged if the chain is empty.
pub async fn run_chain(
    processors: &[Box<dyn TextProcessor>],
    input: &str,
) -> Result<String, TextProcessorError> {
    let mut text = input.to_string();
    for proc in processors {
        tracing::info!(
            processor = proc.name(),
            input_len = text.len(),
            "running text processor"
        );
        text = proc.process(&text).await?;
        tracing::info!(
            processor = proc.name(),
            output_len = text.len(),
            "text processor complete"
        );
    }
    Ok(text)
}

/// Run text through a chain and stream the final processor's output.
/// For chains with >1 processor, intermediate processors run in batch mode;
/// only the last processor streams.
/// The `cancel` token is forwarded to the streaming processor.
pub async fn run_chain_stream(
    processors: &[Box<dyn TextProcessor>],
    input: &str,
    tx: mpsc::Sender<TextFragment>,
    cancel: CancellationToken,
) -> Result<(), TextProcessorError> {
    if processors.is_empty() {
        let _ = tx
            .send(TextFragment::new(input.to_string()).with_is_end_of_response(true))
            .await;
        return Ok(());
    }

    // Run all but last in batch mode
    let mut text = input.to_string();
    for proc in &processors[..processors.len() - 1] {
        if cancel.is_cancelled() {
            return Ok(());
        }
        // Lengths only: what was said stays out of logs at every level.
        tracing::debug!(
            processor = proc.name(),
            chars = text.chars().count(),
            "text processor input (batch)"
        );
        text = proc.process(&text).await?;
        tracing::debug!(
            processor = proc.name(),
            chars = text.chars().count(),
            "text processor output (batch)"
        );
    }

    // Stream the last processor
    let last = processors.last().unwrap();
    tracing::debug!(
        processor = last.name(),
        chars = text.chars().count(),
        "text processor input (stream)"
    );
    last.process_stream(&text, tx, cancel).await
}

/// Context-mode variant of [`run_chain_stream`] for agent-initiated
/// `Generate` turns: `context` is injected background information (a
/// tool result, an external event), NOT user input, so the batch
/// pre-processors (translators, filters, ...) are skipped and the
/// final processor is driven via
/// [`TextProcessor::process_context_stream`].
pub async fn run_chain_context_stream(
    processors: &[Box<dyn TextProcessor>],
    context: &str,
    tx: mpsc::Sender<TextFragment>,
    cancel: CancellationToken,
) -> Result<(), TextProcessorError> {
    let Some(last) = processors.last() else {
        // No processors: speak the context verbatim (mirrors the
        // empty-chain passthrough above).
        let _ = tx
            .send(TextFragment::new(context.to_string()).with_is_end_of_response(true))
            .await;
        return Ok(());
    };
    tracing::debug!(
        processor = last.name(),
        "text processor input (context stream)"
    );
    last.process_context_stream(context, tx, cancel).await
}

// ─── Sentence boundary detection ────────────────────────────────────────

/// The streaming sentence splitter lives in the SDK so a plugin provider
/// chunks text exactly as a built-in one does.
pub use voice_pipes_sdk::sentence::split_first_sentence;

// ── Built-in processors ─────────────────────────────────────

/// Passthrough processor for testing -- returns input unchanged.
#[derive(Debug)]
pub struct EchoProcessor;

#[async_trait]
impl TextProcessor for EchoProcessor {
    fn name(&self) -> &'static str {
        "echo"
    }
    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        Ok(input.to_string())
    }
}

/// Look up a registered text processor factory by name or alias
/// (including external plugins).
pub fn lookup_text_processor_factory(
    name: &str,
) -> Option<std::sync::Arc<dyn TextProcessorFactory>> {
    TEXT_PROCESSOR_FACTORIES.lookup(name)
}

// ── Factory registry ──────────────────────────────────────────────────────

pub(crate) static TEXT_PROCESSOR_FACTORIES: crate::plugins::Registry<dyn TextProcessorFactory> =
    crate::plugins::Registry::new("text processor", builtin_text_processor_factories);

/// Register an external text-processor (LLM) factory before calling
/// `voice_pipes::run()`.
///
/// # Example
/// ```ignore
/// voice_pipes::text::register_text_processor_factory(my_llm_plugin::factory)?;
/// ```
///
/// Thread-safe.
///
/// # Errors
/// When the factory's name or one of its aliases already names a built-in
/// or an earlier plugin: one processor would shadow the other.
pub fn register_text_processor_factory(
    ctor: fn() -> Box<dyn TextProcessorFactory>,
) -> Result<(), crate::plugins::RegistrationError> {
    TEXT_PROCESSOR_FACTORIES.register(ctor)
}

/// Factory for the echo (passthrough) text processor.
#[derive(Debug)]
pub struct EchoFactory;

impl TextProcessorFactory for EchoFactory {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(
        &self,
        cfg: &crate::config::TextProcessorConfig,
    ) -> Vec<crate::config::ConfigError> {
        // Echo has no settings.
        cfg.options().into_errors()
    }

    fn build(
        &self,
        cfg: &crate::config::TextProcessorConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TextProcessor>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        Ok(Box::new(EchoProcessor))
    }

    fn provider_schema(
        &self,
        _cfg: &crate::config::TextProcessorConfig,
    ) -> crate::api::param_schema::ProviderSchema {
        // Echo has no configurable parameters — return an empty schema.
        crate::api::param_schema::ProviderSchema::default()
    }
}

/// The text-processor factories compiled into this build.
fn builtin_text_processor_factories() -> Vec<Box<dyn TextProcessorFactory>> {
    vec![
        Box::new(EchoFactory),
        #[cfg(feature = "text-grpc")]
        Box::new(grpc::GrpcTextProcessorFactory),
        #[cfg(feature = "text-grpc")]
        Box::new(crate::agent::grpc_link::AgentGrpcFactory),
    ]
}

/// Collect all text processor factories: compiled-in built-ins plus any
/// externally registered plugin factories.
pub fn text_processor_factories() -> Vec<std::sync::Arc<dyn TextProcessorFactory>> {
    TEXT_PROCESSOR_FACTORIES.all()
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/text.rs"]
mod tests;
