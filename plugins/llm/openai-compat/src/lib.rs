//! OpenAI-compatible text processor -- works with `OpenAI`, llama.cpp, and any
//! server that implements the `/v1/chat/completions` endpoint.
//!
//! This file holds the processor surface: the [`OpenAiCompatProcessor`]
//! struct with its live parameter cells, the `TextProcessor` impl, and
//! history management. The heavy lifting lives in submodules: `request`
//! (body assembly + retrying HTTP), `streaming` (batch/SSE turn loops +
//! tool-call assembly), `factory` (TOML-driven construction), plus
//! `config`, `schema`, `sse`, and `types`.
//!
//! A plugin on the SDK and the net crate only: the binary registers
//! [`factory`] for `[[text_processor]] type = "openai-compat"` and
//! [`wizard_choices`] for the setup wizard's LLM menu.

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

mod config;
mod factory;
mod request;
mod schema;
mod sse;
mod streaming;
mod types;
mod wizard;

pub use config::OpenAiCompatConfig;
pub use wizard::wizard_choices;

/// The factory the binary registers for `type = "openai-compat"`.
pub fn factory() -> Box<dyn voice_pipes_sdk::TextProcessorFactory> {
    Box::new(OpenAiCompatFactory)
}

use arc_swap::ArcSwap;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use voice_pipes_net::reqwest::Client;

use self::types::{ChatCompletionRequest, ChatMessage};
pub(crate) use factory::OpenAiCompatFactory;
use voice_pipes_sdk::agent::{ClientToolSpec, SessionHandle};
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::sentence::split_first_sentence;
use voice_pipes_sdk::text::{TextFragment, TextProcessor, TextProcessorError};

/// Marker recorded in conversation history when the user interrupts the
/// assistant mid-response (or before any audio is played).
pub(crate) const INTERRUPTED_MARKER: &str = "[interrupted by user]";

const TOOL_CALL_PREFIX: &str = "[called client-side tool ";
const TOOL_CALL_NEWLINE_PREFIX: &str = "\n[called client-side tool ";

// ── Processor ───────────────────────────────────────────────

/// HTTP-based text processor targeting any OpenAI-compatible chat completions
/// endpoint.  Conversation history is accumulated in a `parking_lot::Mutex`
/// so that multi-turn context is sent to the LLM on every request.
///
/// ── Live-editable parameters (Layer 2 `ParamRegistry`) ────────────────────
/// `max_tokens`, `temperature`, `top_p`, `system_prompt`, and `stop` are
/// read through atomic cells on every request, so a `PATCH /debug/params`
/// (or the equivalent inline edit in the Live Pipeline UI) takes effect on
/// the very next turn -- no reconnect required. The factory adopts cells
/// from a worker-wide `AtomicParamStore` so the same updates flow through
/// the orchestrator's transparent proxy and the gRPC `UpdateParams` RPC.
///
/// `top_p`'s "disabled" state is encoded as a NaN sentinel (matching the
/// local-LLM convention), so an `f64::NAN` cell value means "do not send
/// `top_p` to the upstream". `stop` is stored as a single newline-separated
/// string; an empty string means "no stop sequences".
pub struct OpenAiCompatProcessor {
    client: Client,
    config: OpenAiCompatConfig,
    audit_tx: Option<mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
    /// Per-session conversation history (user + assistant turns).
    history: Mutex<Vec<ChatMessage>>,
    /// True when the latest turn recorded an assistant message that has not
    /// yet been truncated. Prevents an early barge-in on a turn that produced
    /// no assistant tokens from clobbering a previous turn's assistant entry.
    last_turn_truncatable: std::sync::atomic::AtomicBool,

    // Mutable runtime cells -- see struct doc for semantics.
    max_tokens_cell: Arc<AtomicI64>,
    temperature_cell: Arc<AtomicU64>,
    top_p_cell: Arc<AtomicU64>,
    system_prompt_cell: Arc<ArcSwap<String>>,
    /// Newline-separated stop sequences. Empty string == None.
    stop_cell: Arc<ArcSwap<String>>,
    text_chunking: voice_pipes_sdk::TextChunkingConfig,
    /// Component-base telemetry / health surface.
    core: Arc<ComponentCore>,
    /// Session agent handle (set via `bind_session`) — the out-of-turn
    /// channel client-tool calls are surfaced on.
    session_handle: Mutex<Option<SessionHandle>>,
    /// Client-side tools declared for this session (set via
    /// `configure_client_tools`). Advertised as `tools` on every
    /// streaming request; recognized calls are reported through the
    /// session handle — never executed here by the processor itself.
    client_tools: Mutex<Vec<ClientToolSpec>>,
}

impl std::fmt::Debug for OpenAiCompatProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatProcessor")
            .field("base_url", &self.config.base_url)
            .field("model", &self.config.model)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatProcessor {
    /// Create a new processor from parsed config.
    pub fn new(
        config: OpenAiCompatConfig,
        audit_tx: Option<mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
        text_chunking: voice_pipes_sdk::TextChunkingConfig,
    ) -> Result<Self, TextProcessorError> {
        // The same `Client` is reused for every turn within a session, so
        // keep-alive on the underlying TCP/TLS connection lets follow-up
        // turns skip the TLS handshake entirely. There is deliberately no
        // whole-request timeout: a long answer streams for as long as it
        // keeps arriving, and a stall fails at the read timeout, both in
        // the client and in the processor's own reads. The defaults below are
        // conservative -- chosen to outlast typical inter-turn pauses
        // without holding sockets indefinitely:
        //   * connect timeout (5s) and idle pool (120s, outlasting a
        //     60-90s "user reading" gap) come from voice-pipes-net, with
        //     its redirect and TLS policy
        //   * `pool_max_idle_per_host(4)`: small headroom for concurrent
        //     turns (e.g. parallel benchmarks against the same host)
        //   * `tcp_keepalive(60s)`: keeps NAT/firewall mappings alive when
        //     the connection sits idle between turns
        //   * `http2_keep_alive_*`: when the upstream negotiates h2 via
        //     ALPN (OpenAI does), PING frames every 30s prevent silent
        //     idle disconnection mid-conversation
        let client = voice_pipes_net::http_client(
            &voice_pipes_net::HttpOptions::streaming(config.read_timeout())
                .connect_timeout(Duration::from_secs(config::CONNECT_TIMEOUT_SECS)),
        )
        .map_err(|e| {
            TextProcessorError::new("openai-compat", format!("failed to build HTTP client: {e}"))
                .caused_by(e)
        })?;

        tracing::info!(
            base_url = %config.base_url,
            model = %config.model,
            max_tokens = config.max_tokens,
            "OpenAI-compat processor created"
        );

        let max_tokens_cell = Arc::new(AtomicI64::new(config.max_tokens as i64));
        // Default temperature to 1.0 (OpenAI's server default) when the
        // config has no explicit value; in practice the parser already fills
        // in `DEFAULT_TEMPERATURE`, so this is a defensive fallback.
        let temperature_cell =
            Arc::new(AtomicU64::new(config.temperature.unwrap_or(1.0).to_bits()));
        let top_p_cell = Arc::new(AtomicU64::new(config.top_p.unwrap_or(f64::NAN).to_bits()));
        let system_prompt_cell = Arc::new(ArcSwap::from_pointee(config.system_prompt.clone()));
        let stop_initial = config
            .stop
            .as_ref()
            .map(|v| v.join("\n"))
            .unwrap_or_default();
        let stop_cell = Arc::new(ArcSwap::from_pointee(stop_initial));

        Ok(Self {
            client,
            config,
            audit_tx,
            history: Mutex::new(Vec::new()),
            last_turn_truncatable: std::sync::atomic::AtomicBool::new(true),
            max_tokens_cell,
            temperature_cell,
            top_p_cell,
            system_prompt_cell,
            stop_cell,
            text_chunking,
            core: Arc::new(ComponentCore::new("text:openai", ComponentKind::Text)),
            session_handle: Mutex::new(None),
            client_tools: Mutex::new(Vec::new()),
        })
    }

    // ── Live-cell accessors (hot-path reads) ────────────────────

    /// Current `max_tokens`. Negative cell values clamp to zero.
    fn max_tokens(&self) -> u32 {
        u32::try_from(self.max_tokens_cell.load(Ordering::Relaxed).max(0)).unwrap_or(u32::MAX)
    }

    /// Current `temperature`.
    fn temperature(&self) -> f64 {
        f64::from_bits(self.temperature_cell.load(Ordering::Relaxed))
    }

    /// Current `top_p`. NaN sentinel means "do not send `top_p`".
    fn top_p(&self) -> Option<f64> {
        let v = f64::from_bits(self.top_p_cell.load(Ordering::Relaxed));
        if v.is_nan() { None } else { Some(v) }
    }

    /// Current system prompt (Arc-cloned, no allocation on hot path).
    fn system_prompt(&self) -> Arc<String> {
        self.system_prompt_cell.load_full()
    }

    /// Current stop sequences. Splits on `\n`, drops empty lines, caps at 4
    /// (`OpenAI`'s documented limit).
    fn stop(&self) -> Option<Vec<String>> {
        let raw = self.stop_cell.load_full();
        if raw.is_empty() {
            return None;
        }
        let v: Vec<String> = raw
            .split('\n')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(4)
            .map(String::from)
            .collect();
        if v.is_empty() { None } else { Some(v) }
    }

    // ── Cell setters (used by `OpenAiCompatFactory` to share cells with the
    //    worker-wide ParamRegistry) ──────────────────────────────────────

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
    pub fn with_system_prompt_cell(mut self, cell: Arc<ArcSwap<String>>) -> Self {
        self.system_prompt_cell = cell;
        self
    }

    #[must_use]
    pub fn with_stop_cell(mut self, cell: Arc<ArcSwap<String>>) -> Self {
        self.stop_cell = cell;
        self
    }
}

/// Surface the operator's *requested* LLM device from `LLAMA_GPU_LAYERS`.
///
/// We can't confirm a llama-server sidecar's actual GPU offload over HTTP — its
/// `/props` is device-blind — so this
/// reports the *requested* offload as an explicitly UNCONFIRMED device
/// (`source = "env-requested"`, rendered as an amber `cuda?` badge).
///
/// Self-gating on env presence: when `LLAMA_GPU_LAYERS` is unset (a real cloud
/// OpenAI-compatible endpoint, not a local llama-server), returns `None` and no
/// device is shown. `ngl > 0` ⇒ cuda requested; `0` ⇒ cpu requested.
fn requested_llm_device_from(ngl: Option<&str>) -> Option<voice_pipes_sdk::component::DeviceInfo> {
    let n: i64 = ngl?.trim().parse().ok()?;
    let kind = if n > 0 { "cuda" } else { "cpu" };
    Some(voice_pipes_sdk::component::DeviceInfo::requested(
        kind,
        format!("ngl={n}"),
    ))
}

#[async_trait]
impl TextProcessor for OpenAiCompatProcessor {
    fn name(&self) -> &'static str {
        "openai-compat"
    }

    async fn device_info(&self) -> Option<voice_pipes_sdk::component::DeviceInfo> {
        let ngl = std::env::var("LLAMA_GPU_LAYERS").ok();
        requested_llm_device_from(ngl.as_deref())
    }

    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        let start = Instant::now();
        let res = self.process_inner(input).await;
        match &res {
            Ok(_) => self.core.record_ok(start.elapsed()),
            Err(e) => self.core.record_error(e),
        }
        res
    }

    /// Cloud LLM warm-up: send a 1-token throwaway completion to
    /// establish the TLS / HTTP-2 connection and trigger any
    /// provider-side model load before the first real user turn.
    /// On gpt-4o-mini this typically eliminates 2–4 s from the
    /// cold-start TTFT.
    ///
    /// Bypasses `process_inner` so it doesn't touch conversation
    /// history or emit user-visible audit events.
    async fn warm_up(&self) -> Result<(), TextProcessorError> {
        let body = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: ".".into(),
            }],
            stream: false,
            temperature: Some(0.0),
            top_p: None,
            max_tokens: Some(1),
            max_completion_tokens: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            user: None,
            response_format: None,
            stream_options: None,
            tools: None,
            tool_choice: None,
            extra: serde_json::Map::new(),
        };
        // Single attempt, no retries — warm-up is best-effort and
        // mustn't delay session readiness if the provider is sluggish.
        let url = self.config.completions_url();
        let read_timeout = self.config.read_timeout();
        let mut req = self.client.post(&url).json(&body);
        if !self.config.api_key.is_empty() {
            req = req.bearer_auth(self.config.api_key.expose());
        }
        match tokio::time::timeout(read_timeout, req.send()).await {
            Ok(Ok(resp)) => {
                // Drain (a bounded amount of) the body so the connection
                // can be returned to the pool warm. Discard any error
                // response — we just need the round-trip to have happened.
                let _ = request::read_body_limited(resp, 64 * 1024, read_timeout, true).await;
                Ok(())
            }
            Ok(Err(e)) => Err(TextProcessorError::new(
                "openai-compat",
                format!("warm-up request failed: {e}"),
            )
            .caused_by(e)),
            Err(_) => Err(TextProcessorError::new(
                "openai-compat",
                format!(
                    "warm-up request got no response within {}s",
                    read_timeout.as_secs()
                ),
            )),
        }
    }

    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let start = Instant::now();
        let token = cancel.clone();
        let res = self.process_stream_inner(input, tx, cancel, "user").await;
        self.record_turn(&res, start, &token);
        res
    }

    async fn process_context_stream(
        &self,
        context: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        // Agent `Generate` turn: context goes to the model (and into
        // history) as a `system` message, never as a synthetic user turn.
        let start = Instant::now();
        let token = cancel.clone();
        let res = self
            .process_stream_inner(context, tx, cancel, "system")
            .await;
        self.record_turn(&res, start, &token);
        res
    }

    fn bind_session(&self, handle: SessionHandle) {
        *self.session_handle.lock() = Some(handle);
    }

    fn configure_client_tools(&self, tools: Vec<ClientToolSpec>) {
        tracing::info!(
            tools = tools.len(),
            "openai-compat: client-side tools declared for this session"
        );
        *self.client_tools.lock() = tools;
    }

    async fn append_assistant_message(&self, text: &str) {
        // Agent `Say` turn spoken via the TTS-only path: record it so
        // history stays complete and barge-in truncation can find it.
        let mut history = self.history.lock();
        history.push(ChatMessage {
            role: "assistant".into(),
            content: text.to_string(),
        });
        Self::evict_oldest_turns(&mut history, self.config.history_max_turns as usize);
        self.last_turn_truncatable.store(true, Ordering::Release);
    }

    async fn inject_context(&self, text: &str) {
        let mut history = self.history.lock();
        history.push(ChatMessage {
            role: "system".into(),
            content: text.to_string(),
        });
        Self::evict_oldest_turns(&mut history, self.config.history_max_turns as usize);
    }

    fn snapshot_history(&self) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        let history = self.history.lock().clone();
        Some(Box::new(history))
    }

    fn restore_history(&self, snapshot: &(dyn std::any::Any + Send + Sync)) {
        if let Some(history) = snapshot.downcast_ref::<Vec<ChatMessage>>() {
            (*self.history.lock()).clone_from(history);
        }
    }

    /// Truncate the last assistant message in history to reflect only what the
    /// user actually heard before a barge-in, and append an explicit
    /// interruption marker (`[interrupted by user]`) so the model knows its
    /// reply was cut short (or never heard at all when `played_text` is empty).
    ///
    /// - When `played_text` is non-empty and shorter than the spoken reply, the
    ///   spoken portion is replaced with `"{played_text}... [interrupted by user]"`.
    /// - When `played_text` is empty (`0 ms` played / early barge-in), the
    ///   spoken portion is replaced with `"[interrupted by user]"`.
    /// - Any trailing `[called client-side tool ...]` markers on the assistant
    ///   entry are preserved intact.
    /// - When the assistant response is structured JSON (starts with `{`), the
    ///   JSON payload is kept intact (never sliced into invalid JSON) and a
    ///   `system` note is appended recording the interruption.
    ///
    /// Mirrors `ConversationMemoryWrapper` so the cloud and local LLM paths
    /// behave identically. Returns the original spoken text if a modification
    /// was performed, `None` otherwise.
    fn truncate_history(&self, played_text: &str) -> Option<String> {
        if !self.last_turn_truncatable.load(Ordering::Acquire) {
            return None;
        }
        let mut history = self.history.lock();

        // Find the last assistant message (searching from the end).
        let last = history.iter_mut().rev().find(|m| m.role == "assistant")?;
        let original_full = last.content.clone();

        // Pure tool-call turn with no spoken prose: nothing spoken to truncate.
        if original_full.starts_with(TOOL_CALL_PREFIX) {
            return None;
        }

        // Separate spoken prose from any trailing `[called client-side tool ...]` markers.
        let (spoken_original, tool_suffix) =
            match original_full.split_once(TOOL_CALL_NEWLINE_PREFIX) {
                Some((spoken, rest)) => (
                    spoken.to_string(),
                    format!("{TOOL_CALL_NEWLINE_PREFIX}{rest}"),
                ),
                None => (original_full, String::new()),
            };

        // Nothing to truncate if the user heard the entire spoken response.
        if !played_text.is_empty() && played_text == spoken_original {
            return None;
        }

        // Already carries the interruption marker: idempotent no-op.
        if spoken_original.ends_with(INTERRUPTED_MARKER) {
            return None;
        }

        // Structured JSON response: keep the JSON payload intact so strict
        // parsers / grammar constraints are never fed broken JSON, and append
        // a system note so the model still knows the user interrupted.
        if spoken_original.trim_start().starts_with('{') {
            let note = if played_text.is_empty() {
                INTERRUPTED_MARKER.to_string()
            } else {
                format!("[interrupted by user after: {played_text}]")
            };
            history.push(ChatMessage {
                role: "system".into(),
                content: note,
            });
            Self::evict_oldest_turns(&mut history, self.config.history_max_turns as usize);
            self.last_turn_truncatable.store(false, Ordering::Release);
            drop(history);

            self.emit_audit(
                "history_truncated",
                serde_json::json!({
                    "original": spoken_original,
                    "retained": played_text,
                    "structured_json_preserved": true,
                }),
            );

            tracing::info!(
                original_len = spoken_original.len(),
                retained_len = played_text.len(),
                "annotated structured JSON assistant turn as interrupted in conversation history"
            );

            return Some(spoken_original);
        }

        let retained_spoken = if played_text.is_empty() {
            INTERRUPTED_MARKER.to_string()
        } else {
            format!("{played_text}... {INTERRUPTED_MARKER}")
        };
        last.content = format!("{retained_spoken}{tool_suffix}");
        self.last_turn_truncatable.store(false, Ordering::Release);
        drop(history);

        self.emit_audit(
            "history_truncated",
            serde_json::json!({
                "original": spoken_original,
                "retained": played_text,
            }),
        );

        tracing::info!(
            original_len = spoken_original.len(),
            retained_len = played_text.len(),
            "truncated assistant message in conversation history"
        );

        Some(spoken_original)
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(std::sync::Arc::clone(
            &self.core,
        ))))
    }
}

impl OpenAiCompatProcessor {
    /// A streamed turn's outcome on the component: its latency when it
    /// succeeds, the error when it fails, and nothing when `cancel` fired. A
    /// stream cut short by a barge-in returns `Ok` with part of the reply,
    /// and counting it would add a success and a latency sample for a turn
    /// that never finished.
    fn record_turn(
        &self,
        res: &Result<(), TextProcessorError>,
        start: Instant,
        cancel: &CancellationToken,
    ) {
        if cancel.is_cancelled() {
            return;
        }
        match res {
            Ok(()) => self.core.record_ok(start.elapsed()),
            Err(e) => self.core.record_error(e),
        }
    }

    /// Evict the oldest turns so at most `max_turns` user/assistant pairs
    /// remain. Called once per completed turn, after the assistant
    /// reply is recorded, so eviction always removes whole pairs. Mirrors
    /// `ConversationMemoryWrapper` so the cloud and local paths behave
    /// identically.
    fn evict_oldest_turns(history: &mut Vec<ChatMessage>, max_turns: usize) {
        let max_msgs = max_turns.saturating_mul(2);
        if history.len() <= max_msgs {
            return;
        }
        let excess = history.len() - max_msgs;
        history.drain(..excess);
        // Defensive: the retained window must start with a user message so
        // the alternation sent to the model stays intact. A `while` (not
        // `if`) because agent-initiated entries (assistant messages with no
        // preceding user turn, injected system context) mean several
        // non-user messages can be adjacent at the eviction boundary.
        while history.first().is_some_and(|m| m.role != "user") {
            history.remove(0);
        }
        tracing::debug!(
            evicted = excess,
            remaining = history.len(),
            "conversation history window full; evicted oldest turns"
        );
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/openai_compat.rs"]
mod tests;
