//! LLM / text-processor types, traits, and errors.
//!
//! Implement [`TextProcessor`] and [`TextProcessorFactory`] here and register
//! your processor with `voice_pipes::text::register_text_processor_factory()`
//! before the engine serves. The engine's built-in processors
//! implement the same traits.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::{ConfigError, TextProcessorConfig};
use crate::schema::ProviderSchema;
use crate::tts::SynthesisHints;

/// Option key through which `[pipeline.audit] full_prompts` reaches a
/// history-keeping text processor's factory: `true` asks it to record the
/// whole prompt it sends, not only the new turn. The engine adds it; a
/// processor's own settings never need to list it
/// ([`TextProcessorConfig::unknown_options`] passes it).
pub const AUDIT_FULL_PROMPTS_OPTION: &str = "audit_full_prompts";

// ── Error ─────────────────────────────────────────────────────

/// Error returned by text-processor operations.
#[derive(Debug)]
#[non_exhaustive]
pub struct TextProcessorError {
    /// Short name of the processor that failed (for log correlation).
    pub processor: String,
    /// Human-readable failure message.
    pub message: String,
    /// Optional root cause.
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
    /// Whether retrying may succeed.
    pub kind: TextErrorKind,
}

/// Whether a [`TextProcessorError`] may pass on retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TextErrorKind {
    /// The same request fails the same way again (a refused request, a
    /// bad configuration, a local model error).
    Permanent,
    /// A network failure, a timeout, a stall or a server error: another
    /// attempt may succeed.
    Transient,
    /// The service asked the caller to slow down, for `retry_after` when
    /// it said.
    RateLimited {
        /// The service's `Retry-After`, when it sent one.
        retry_after: Option<std::time::Duration>,
    },
}

impl TextProcessorError {
    /// A permanent error with no source cause.
    pub fn new(processor: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            processor: processor.into(),
            message: message.into(),
            source: None,
            kind: TextErrorKind::Permanent,
        }
    }

    /// A permanent error with a source cause.
    pub fn with_source(
        processor: impl Into<String>,
        message: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            source: Some(source.into()),
            ..Self::new(processor, message)
        }
    }

    /// The same error, with `source` as its cause.
    #[must_use]
    pub fn caused_by(
        mut self,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The same error, with `source` (if any) as its cause.
    #[must_use]
    pub fn with_cause(mut self, source: Option<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        self.source = source;
        self
    }

    /// The same error, of `kind`.
    #[must_use]
    pub fn with_kind(mut self, kind: TextErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Whether retrying may succeed.
    pub fn is_transient(&self) -> bool {
        !matches!(self.kind, TextErrorKind::Permanent)
    }

    /// How long the service asked the caller to wait, if it did.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self.kind {
            TextErrorKind::RateLimited { retry_after } => retry_after,
            _ => None,
        }
    }
}

impl std::fmt::Display for TextProcessorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "text processor \"{}\": {}", self.processor, self.message)
    }
}

impl std::error::Error for TextProcessorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

// ── Fragment ──────────────────────────────────────────────────

/// A fragment of text emitted by a streaming text processor. Use
/// [`hints`](TextFragment::hints) to influence TTS behaviour per fragment.
///
/// `#[non_exhaustive]`: build one with [`new`](Self::new) and the `with_*`
/// setters, so a field added later does not break your crate.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TextFragment {
    /// The text content of this fragment.
    pub text: String,
    /// `true` on the last fragment of a complete response.
    pub is_end_of_response: bool,
    /// Optional per-fragment TTS parameter overrides.
    pub hints: Option<SynthesisHints>,
    /// Real token count reported by the LLM backend. Set on the final
    /// fragment; `None` on intermediate fragments.
    pub token_count: Option<u32>,
    /// Provider-measured time-to-first-token in milliseconds. Set on the
    /// first fragment that carries tokens; `None` on follow-up fragments and
    /// on providers that don't track TTFT.
    pub first_token_ms: Option<f64>,
}

impl TextFragment {
    /// A fragment carrying `text`, not the end of the response, with no
    /// hints or counts.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    with_setters! {
        with_is_end_of_response => is_end_of_response: bool,
        with_hints => hints: Option<SynthesisHints>,
        with_token_count => token_count: Option<u32>,
        with_first_token_ms => first_token_ms: Option<f64>,
    }
}

// ── Provider trait ────────────────────────────────────────────

/// The plugin-facing text-processor (LLM) interface.
///
/// Implement this trait in your plugin crate, then expose a
/// [`TextProcessorFactory`] and register it with
/// `voice_pipes::text::register_text_processor_factory()`.
///
/// # Component health
/// Override [`as_component`](TextProcessor::as_component) to register this
/// processor in the health dashboard, the auto-degrade loop, and the LLM
/// error-retry policy. Embed an `Arc<ComponentCore>` and return a `CoreComponent`:
///
/// ```ignore
/// fn as_component(&self) -> Option<std::sync::Arc<dyn voice_pipes_sdk::Component>> {
///     Some(std::sync::Arc::new(
///         voice_pipes_sdk::component::CoreComponent::new(self.core.clone())
///             .with_error_policy(voice_pipes_sdk::component::ErrorPolicy::RetryWithBackoff { max: 3 })))
/// }
/// ```
#[async_trait]
pub trait TextProcessor: Send + Sync {
    /// Human-readable name for logging and metrics.
    fn name(&self) -> &str;

    /// Transform input text, returning processed text (batch mode).
    async fn process(&self, input: &str) -> Result<String, TextProcessorError>;

    /// Optional session-start warm-up. Default: no-op.
    ///
    /// Cloud LLM processors should send a tiny throwaway completion here to
    /// amortise TLS, HTTP/2, and provider-side routing cost off the first
    /// user-perceived latency. Errors are logged but do not block the session.
    async fn warm_up(&self) -> Result<(), TextProcessorError> {
        Ok(())
    }

    /// Streaming variant: sends text fragments via the channel.
    ///
    /// Default implementation wraps batch [`process`](TextProcessor::process)
    /// and sends one fragment. The `cancel` token allows early termination
    /// (e.g. on user interruption / barge-in).
    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let result = self.process(input).await?;
        let _ = tx
            .send(TextFragment {
                text: result,
                is_end_of_response: true,
                hints: None,
                token_count: None,
                first_token_ms: None,
            })
            .await;
        Ok(())
    }

    /// Streaming generation from injected **context** (a tool result, an
    /// external event) rather than a user message, for an agent-initiated
    /// turn: treat `context` as background information, generate the spoken
    /// reply from it, and record history without a synthetic user turn.
    ///
    /// Default: [`process_stream`](TextProcessor::process_stream) — correct
    /// output, but a history-keeping processor would record the context as
    /// if the user said it, so such a processor overrides this.
    async fn process_context_stream(
        &self,
        context: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        self.process_stream(context, tx, cancel).await
    }

    /// Identity of the model actually serving this processor, resolved once
    /// at session setup (a server's `/v1/models`, say). Default: `None`, and
    /// the session reports the configured model name.
    async fn model_identity(&self) -> Option<crate::component::ModelIdentity> {
        None
    }

    /// Compute device this processor runs on, recorded for the session
    /// report. Default: `None` (not reported).
    async fn device_info(&self) -> Option<crate::component::DeviceInfo> {
        None
    }

    /// The live session this processor serves, given once when the session
    /// is built. Keep it to report client-side tool calls. Default: ignored.
    fn bind_session(&self, _session: crate::agent::SessionHandle) {}

    /// The client-side tools this session allows, given after
    /// [`bind_session`](TextProcessor::bind_session). A function-calling
    /// processor advertises them to its model and reports recognised calls
    /// through the session handle — never executes them. Default: ignored,
    /// and the model never calls a client tool.
    fn configure_client_tools(&self, _tools: Vec<crate::agent::ClientToolSpec>) {}

    /// Record an assistant message produced outside this processor (an
    /// agent's spoken announcement) so conversation history stays complete.
    /// Default: no-op; only history-keeping processors need it.
    async fn append_assistant_message(&self, _text: &str) {}

    /// Record background context that was not spoken (a silent tool result,
    /// a rich client message kept in history) so the model can refer to it
    /// later. Default: no-op.
    async fn inject_context(&self, _text: &str) {}

    /// Truncate the last assistant response in conversation history to reflect
    /// only the text the user actually heard (called on barge-in or cancel).
    ///
    /// When `played_text` is empty (`""`, e.g. `0 ms` of audio played or early
    /// barge-in before TTS), history-keeping implementations replace the
    /// unheard assistant turn with `[interrupted by user]`. When `played_text`
    /// is a non-empty strict prefix of the generated turn, implementations
    /// retain `{played_text}... [interrupted by user]` so the LLM knows both
    /// what the user heard and that the turn was cut short. Structured JSON
    /// outputs are kept syntactically valid and annotated with a trailing
    /// system interruption note.
    ///
    /// Default: no-op — only processors that maintain conversation history
    /// need to override this. Returns the original text if truncation was
    /// performed.
    fn truncate_history(&self, _played_text: &str) -> Option<String> {
        None
    }

    /// Snapshot the current stateful history (if any) to prevent speculative
    /// execution from corrupting permanent logs.
    ///
    /// Default: `None` — stateless processors don't need this.
    fn snapshot_history(&self) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        None
    }

    /// Restore a previously taken stateful history snapshot.
    ///
    /// Default: no-op.
    fn restore_history(&self, _snapshot: &(dyn std::any::Any + Send + Sync)) {}

    /// Optional [`Component`](crate::component::Component) adapter for health
    /// registration, auto-degrade, and LLM retry policy.
    ///
    /// Return `Some(Arc::new(CoreComponent::new(self.core.clone())))` to opt
    /// in. Default: `None` (no health registration, default error policy).
    fn as_component(&self) -> Option<std::sync::Arc<dyn crate::component::Component>> {
        None
    }
}

// ── Factory trait ─────────────────────────────────────────────

/// Registration metadata + construction for one text processor type.
///
/// Implement this on a unit struct in your plugin crate, then call
/// `voice_pipes::text::register_text_processor_factory(|| Box::new(MyFactory))`.
pub trait TextProcessorFactory: Send + Sync {
    /// Name as it appears in TOML `type = "..."`.
    fn name(&self) -> &str;

    /// Additional names that resolve to this factory (aliases).
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Whether `n` is this factory's name or one of its aliases.
    fn matches(&self, n: &str) -> bool {
        self.name() == n || self.aliases().contains(&n)
    }

    /// Local processors don't require an API key or network access.
    fn is_local(&self) -> bool {
        false
    }

    /// The environment variable the host reads this provider's API key
    /// from when the config sets none. The host resolves the key and hands
    /// it over in the config's `api_key`; the provider never reads the
    /// environment for it. `None` for a provider without a key.
    fn default_api_key_env(&self) -> Option<&'static str> {
        None
    }

    /// Validate processor-specific config. Return one [`ConfigError`] per
    /// invalid field.
    fn validate(&self, cfg: &TextProcessorConfig) -> Vec<ConfigError>;

    /// Construct the provider from its config. `ctx` carries what the
    /// framework supplies: an audit channel, this component's live
    /// parameters, text chunking; see [`BuildContext`](crate::build::BuildContext).
    fn build(
        &self,
        cfg: &TextProcessorConfig,
        ctx: &crate::build::BuildContext<'_>,
    ) -> Result<Box<dyn TextProcessor>, crate::BuildError>;

    /// Provider-owned parameter schema for the graph editor.
    ///
    /// Return the complete descriptor list plus matching current values.
    /// The default implementation returns a minimal fallback so unmigrated
    /// providers still display their name.
    fn provider_schema(&self, cfg: &TextProcessorConfig) -> ProviderSchema {
        let mut s =
            ProviderSchema::fallback(self.name(), &format!("{} text processor", self.name()));
        s.push(
            crate::schema::ParamDescriptor::static_str(
                "type",
                "Processor type",
                "The LLM framework or service used for text generation.",
            ),
            cfg.processor_type.clone(),
        );
        s
    }
}
