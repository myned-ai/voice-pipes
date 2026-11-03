//! Conversation memory -- rolling context window for the LLM.

use parking_lot::RwLock;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::chat_template::{ChatTemplate, Message};
use voice_pipes_sdk::TraceEvent;
use voice_pipes_sdk::text::{TextFragment, TextProcessor, TextProcessorError};

/// Default rolling-window size, in user/assistant turn pairs.
/// Without a cap the prompt grows every turn until latency spikes or the
/// model's context overflows. Overridable per provider via the
/// `history_max_turns` config option.
pub(crate) const DEFAULT_HISTORY_MAX_TURNS: usize = 20;

/// Most content (bytes) the retained window may hold across all
/// entries. The turn cap bounds the count; this bounds the size, so a
/// few very long entries cannot grow the prompt past what the model
/// (or the prompt trace) can absorb.
pub(crate) const MAX_HISTORY_BYTES: usize = 256 * 1024;

/// What a prompt may take of the model's window: the window less the
/// room the reply may use, counted with the model's tokenizer.
pub(crate) struct TokenBudget {
    pub(crate) tokenizer: Arc<tokenizers::Tokenizer>,
    pub(crate) context: usize,
    /// The live `max_tokens` cell: the reply's room follows its edits.
    pub(crate) max_tokens: Arc<std::sync::atomic::AtomicI64>,
}

impl TokenBudget {
    /// Whether `prompt` leaves room for a full reply. A prompt that cannot
    /// be tokenized is let through: the processor reports that error.
    fn fits(&self, prompt: &str) -> bool {
        let Ok(tokens) = crate::chat_template::encode_prompt(&self.tokenizer, prompt) else {
            return true;
        };
        let reply = usize::try_from(self.max_tokens.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);
        tokens.len().saturating_add(reply) <= self.context
    }
}

/// Marker recorded in conversation history when the user interrupts the
/// assistant mid-response (or before any audio is played).
pub const INTERRUPTED_MARKER: &str = "[interrupted by user]";

/// A wrapper processor that maintains a conversation history.
/// It prepends previous conversation history and the system prompt to the user input
/// using the supplied chat template, and forwards the fully formatted prompt
/// to the wrapped inner processor.
pub struct ConversationMemoryWrapper {
    inner: Box<dyn TextProcessor>,
    history: RwLock<Vec<Message>>,
    /// True when the latest turn recorded an assistant message that has not
    /// yet been truncated. Prevents an early barge-in on a turn that produced
    /// no assistant tokens from clobbering a previous turn's assistant entry.
    last_turn_truncatable: std::sync::atomic::AtomicBool,
    template: Box<dyn ChatTemplate>,
    /// Held as an `ArcSwap` so that PATCH /debug/params updates to the
    /// `system_prompt` are picked up on the next turn (lock-free read).
    system_prompt: Arc<ArcSwap<String>>,
    audit_tx: Option<mpsc::Sender<TraceEvent>>,
    /// Rolling-window cap in turn pairs; oldest turns evict past it.
    history_max_turns: usize,
    /// Trace the full message list on every turn instead of its count
    /// and hash; the whole conversation would otherwise be written to
    /// the trace log once per turn.
    full_prompts: bool,
    /// The model's window, when known: the oldest turns are dropped until
    /// the prompt fits it.
    budget: Option<TokenBudget>,
}

/// What an `llm_prompt` trace event carries about the assembled prompt.
fn prompt_trace_payload(messages: &[Message], full: bool) -> serde_json::Value {
    if full {
        return serde_json::json!({ "messages": messages });
    }
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut bytes = 0usize;
    for m in messages {
        hasher.update(m.role.as_bytes());
        hasher.update(b"\0");
        hasher.update(m.content.as_bytes());
        hasher.update(b"\0");
        bytes += m.content.len();
    }
    let digest = hasher.finalize();
    let hash = digest.iter().take(16).fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });
    serde_json::json!({
        "messages_count": messages.len(),
        "prompt_bytes": bytes,
        "prompt_sha256": hash,
    })
}

impl std::fmt::Debug for ConversationMemoryWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationMemoryWrapper")
            .field("system_prompt", &*self.system_prompt.load_full())
            .finish_non_exhaustive()
    }
}

impl ConversationMemoryWrapper {
    /// Wrap a text processor with conversation history and chat templating.
    pub fn new(
        inner: Box<dyn TextProcessor>,
        template: Box<dyn ChatTemplate>,
        system_prompt: impl Into<String>,
        audit_tx: Option<mpsc::Sender<TraceEvent>>,
    ) -> Self {
        Self {
            inner,
            history: RwLock::new(Vec::new()),
            last_turn_truncatable: std::sync::atomic::AtomicBool::new(true),
            template,
            system_prompt: Arc::new(ArcSwap::from_pointee(system_prompt.into())),
            audit_tx,
            history_max_turns: DEFAULT_HISTORY_MAX_TURNS,
            full_prompts: false,
            budget: None,
        }
    }

    /// Keep every prompt within the model's window: the oldest turns are
    /// dropped until it fits, so a long session goes on answering instead of
    /// failing every turn once its history outgrows the window.
    #[must_use]
    pub(crate) fn with_token_budget(mut self, budget: TokenBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The prompt for the system prompt followed by `history`, with the
    /// oldest turns dropped from `history` until it fits the budget. The
    /// newest message is never dropped: a turn too long on its own is left
    /// for the model to refuse. Returns the messages sent and the prompt.
    fn fit_prompt(&self, history: &mut Vec<Message>) -> (Vec<Message>, String) {
        loop {
            let mut messages = Vec::with_capacity(history.len() + 1);
            messages.push(Message {
                role: "system".to_string(),
                content: (*self.system_prompt.load_full()).clone(),
            });
            messages.extend(history.iter().cloned());
            let prompt = self.template.format_messages(&messages);
            let fits = self.budget.as_ref().is_none_or(|b| b.fits(&prompt));
            if fits || !Self::drop_oldest_turn(history) {
                return (messages, prompt);
            }
            tracing::debug!(
                remaining = history.len(),
                "prompt past the model's window; dropped the oldest turn"
            );
        }
    }

    /// Drop the oldest message and any that would leave the history not
    /// starting with a user turn, never the newest. `false` when nothing
    /// can go.
    fn drop_oldest_turn(history: &mut Vec<Message>) -> bool {
        if history.len() <= 1 {
            return false;
        }
        history.remove(0);
        while history.len() > 1 && history.first().is_some_and(|m| m.role != "user") {
            history.remove(0);
        }
        true
    }

    /// Adopt an externally-owned `system_prompt` cell so updates to it are
    /// reflected on the next turn without restarting the processor.
    #[must_use]
    pub fn with_system_prompt_cell(mut self, cell: Arc<ArcSwap<String>>) -> Self {
        self.system_prompt = cell;
        self
    }

    /// Override the rolling-window cap (turn pairs). Values below 1 are
    /// clamped to 1 — a zero window would drop the turn being recorded.
    #[must_use]
    pub fn with_history_max_turns(mut self, max_turns: usize) -> Self {
        self.history_max_turns = max_turns.max(1);
        self
    }

    /// Trace the full prompt on every turn (default: count and hash).
    #[must_use]
    pub fn with_full_prompts(mut self, full: bool) -> Self {
        self.full_prompts = full;
        self
    }

    /// Helper to format the prompt. Includes the system prompt, history, and the new user message.
    fn format_and_append_user_message(&self, user_input: &str) -> String {
        self.last_turn_truncatable
            .store(false, std::sync::atomic::Ordering::Release);
        let mut history = self.history.write();
        // Append user's input to history
        history.push(Message {
            role: "user".to_string(),
            content: user_input.to_string(),
        });

        let (messages, prompt) = self.fit_prompt(&mut history);

        if let Some(tx) = &self.audit_tx {
            let event = TraceEvent::new(
                "llm:conversation-memory",
                "llm_prompt",
                prompt_trace_payload(&messages, self.full_prompts),
            );
            let _ = tx.try_send(event);
        }

        prompt
    }

    /// Roll back the trailing user message that was optimistically appended for
    /// a turn whose inner processor then failed (or produced nothing). Without
    /// this, a failed turn leaves a user message with no assistant reply, which
    /// permanently breaks the user/assistant alternation sent to the model on
    /// every subsequent turn. Only pops if the last entry is actually a user
    /// message, so it is a no-op once an assistant reply has been recorded.
    fn rollback_last_user_message(&self) {
        let mut history = self.history.write();
        if history.last().is_some_and(|m| m.role == "user") {
            history.pop();
            if let Some(tx) = &self.audit_tx {
                let event = TraceEvent::new(
                    "llm:conversation-memory",
                    "user_turn_rolled_back",
                    serde_json::json!({ "reason": "inner processor produced no assistant reply" }),
                );
                let _ = tx.try_send(event);
            }
        }
    }

    /// Evict the oldest turns so at most `history_max_turns` user/assistant
    /// pairs and at most [`MAX_HISTORY_BYTES`] of content remain. Called
    /// after every append, so a stream of oversized or unanswered
    /// entries cannot grow the window past either bound.
    fn evict_oldest_turns(history: &mut Vec<Message>, max_turns: usize) {
        let max_msgs = max_turns.saturating_mul(2);
        let mut excess = history.len().saturating_sub(max_msgs);
        let mut bytes: usize = history.iter().map(|m| m.content.len()).sum();
        for m in history.iter().skip(excess) {
            if bytes <= MAX_HISTORY_BYTES {
                break;
            }
            bytes -= m.content.len();
            excess += 1;
        }
        if excess == 0 {
            return;
        }
        history.drain(..excess);
        // Defensive: the retained window must start with a user message so
        // the alternation sent to the model stays intact. A `while` (not
        // `if`) because agent-initiated entries (assistant messages with
        // no preceding user turn, injected system context) mean several
        // non-user messages can be adjacent at the eviction boundary. A
        // window with no user turn at all (only injected context so far)
        // is kept as it is rather than emptied.
        if history.iter().any(|m| m.role == "user") {
            while history.first().is_some_and(|m| m.role != "user") {
                history.remove(0);
            }
        }
        tracing::debug!(
            evicted = excess,
            remaining = history.len(),
            "conversation history window full; evicted oldest turns"
        );
    }

    /// Append the assistant's response to the history.
    fn append_assistant_response(&self, assistant_response: &str) {
        let mut history = self.history.write();
        history.push(Message {
            role: "assistant".to_string(),
            content: assistant_response.to_string(),
        });
        Self::evict_oldest_turns(&mut history, self.history_max_turns);
        self.last_turn_truncatable
            .store(true, std::sync::atomic::Ordering::Release);

        if let Some(tx) = &self.audit_tx {
            let event = TraceEvent::new(
                "llm:conversation-memory",
                "llm_response",
                serde_json::json!({ "text": assistant_response }),
            );
            let _ = tx.try_send(event);
        }
    }

    /// Truncate the last assistant message in history to reflect only what the
    /// user actually heard before an interruption, and append an explicit
    /// interruption marker (`[interrupted by user]`) so the model knows its
    /// reply was cut short (or never heard at all when `played_text` is empty).
    ///
    /// - When `played_text` is non-empty and shorter than the spoken reply, the
    ///   last assistant entry is replaced with `"{played_text}... [interrupted by user]"`.
    /// - When `played_text` is empty (`0 ms` played / early barge-in), the last
    ///   assistant entry is replaced with `"[interrupted by user]"`.
    /// - When the last assistant entry looks like structured JSON (starts with
    ///   `{`), the JSON payload is kept intact (never sliced into invalid JSON)
    ///   and a `system` note is appended recording the interruption.
    ///
    /// Returns the original text if a modification was performed, `None` otherwise.
    pub fn truncate_assistant_message(&self, played_text: &str) -> Option<String> {
        if !self
            .last_turn_truncatable
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        let mut history = self.history.write();

        // Find the last assistant message (searching from the end).
        let last = history.iter_mut().rev().find(|m| m.role == "assistant")?;
        let original_text = last.content.clone();

        // Nothing to truncate if the user heard the entire response.
        if !played_text.is_empty() && played_text == original_text {
            return None;
        }

        // Already carries the interruption marker: idempotent no-op.
        if original_text.ends_with(INTERRUPTED_MARKER) {
            return None;
        }

        // Structured JSON response: keep the JSON payload intact so strict
        // parsers / grammar constraints are never fed broken JSON, and append
        // a system note so the model still knows the user interrupted.
        if original_text.trim_start().starts_with('{') {
            let note = if played_text.is_empty() {
                INTERRUPTED_MARKER.to_string()
            } else {
                format!("[interrupted by user after: {played_text}]")
            };
            history.push(Message {
                role: "system".to_string(),
                content: note,
            });
            Self::evict_oldest_turns(&mut history, self.history_max_turns);
            self.last_turn_truncatable
                .store(false, std::sync::atomic::Ordering::Release);
            drop(history);

            if let Some(tx) = &self.audit_tx {
                let event = TraceEvent::new(
                    "llm:conversation-memory",
                    "history_truncated",
                    serde_json::json!({
                        "original": original_text,
                        "retained": played_text,
                        "structured_json_preserved": true,
                    }),
                );
                let _ = tx.try_send(event);
            }

            tracing::info!(
                original_len = original_text.len(),
                retained_len = played_text.len(),
                "annotated structured JSON assistant turn as interrupted in conversation history"
            );

            return Some(original_text);
        }

        let retained = if played_text.is_empty() {
            INTERRUPTED_MARKER.to_string()
        } else {
            format!("{played_text}... {INTERRUPTED_MARKER}")
        };
        last.content = retained;
        self.last_turn_truncatable
            .store(false, std::sync::atomic::Ordering::Release);
        drop(history);

        if let Some(tx) = &self.audit_tx {
            let event = TraceEvent::new(
                "llm:conversation-memory",
                "history_truncated",
                serde_json::json!({
                    "original": original_text,
                    "retained": played_text,
                }),
            );
            let _ = tx.try_send(event);
        }

        tracing::info!(
            original_len = original_text.len(),
            retained_len = played_text.len(),
            "truncated assistant message in conversation history"
        );

        Some(original_text)
    }
}

/// Pass the inner processor's fragments on to `tx` and collect their text:
/// the reply as spoken, for the history. Fragments carry the whitespace
/// between them, so their concatenation is the reply as the model wrote
/// it. The task ends at the fragment that ends the response, or when the
/// inner processor drops its sender.
fn capture(
    tx: mpsc::Sender<TextFragment>,
) -> (mpsc::Sender<TextFragment>, tokio::task::JoinHandle<String>) {
    let (intercept_tx, mut intercept_rx) = mpsc::channel::<TextFragment>(32);
    let task = tokio::spawn(async move {
        let mut spoken = String::new();
        while let Some(frag) = intercept_rx.recv().await {
            spoken.push_str(&frag.text);
            let is_end = frag.is_end_of_response;
            let _ = tx.send(frag).await;
            if is_end {
                break;
            }
        }
        spoken
    });
    (intercept_tx, task)
}

#[async_trait]
impl TextProcessor for ConversationMemoryWrapper {
    fn name(&self) -> &str {
        // Delegate to the inner processor so that diagnostics and session
        // reports show the real model name (e.g. "SmolLM2-135M-Instruct")
        // rather than the wrapper label.
        self.inner.name()
    }

    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        let prompt = self.format_and_append_user_message(input);

        tracing::info!(
            processor = self.name(),
            "formatted conversation history prompt for LLM"
        );

        match self.inner.process(&prompt).await {
            Ok(response) if response.trim().is_empty() => {
                // Nothing will be spoken, so there is no assistant turn;
                // drop the user message as the streaming path does.
                self.rollback_last_user_message();
                Ok(response)
            }
            Ok(response) => {
                self.append_assistant_response(&response);
                Ok(response)
            }
            Err(e) => {
                // The turn failed after we optimistically recorded the user
                // message; drop it so history alternation stays intact.
                self.rollback_last_user_message();
                Err(e)
            }
        }
    }

    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let prompt = self.format_and_append_user_message(input);

        tracing::debug!(
            processor = self.name(),
            prompt_chars = prompt.chars().count(),
            "formatted conversation prompt for LLM stream"
        );

        let (intercept_tx, text_capture) = capture(tx);

        let stream_result = self
            .inner
            .process_stream(&prompt, intercept_tx, cancel)
            .await;

        // Collect whatever the inner processor streamed before finishing or
        // failing (a barge-in/cancel typically yields a non-empty heard prefix).
        let assistant_text = text_capture.await.ok().filter(|t| !t.is_empty());

        match assistant_text {
            Some(text) => {
                tracing::debug!(
                    processor = self.name(),
                    output_chars = text.chars().count(),
                    "LLM full response"
                );
                // Preserve the heard prefix as the assistant turn even if the
                // stream ultimately errored -- it keeps history consistent with
                // what the user actually heard.
                self.append_assistant_response(&text);
            }
            None => {
                // Nothing was produced (immediate failure or empty stream);
                // roll back the optimistic user message to avoid a dangling turn.
                self.rollback_last_user_message();
            }
        }

        stream_result
    }

    async fn process_context_stream(
        &self,
        context: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        // Agent `Generate` turn: record the context as a system message
        // (NOT a synthetic user turn) and generate the assistant reply
        // from it. The context entry stays in history even if generation
        // fails — it remains true background knowledge either way.
        self.last_turn_truncatable
            .store(false, std::sync::atomic::Ordering::Release);
        let prompt = {
            let mut history = self.history.write();
            history.push(Message {
                role: "system".to_string(),
                content: context.to_string(),
            });
            // A context entry stays even when generation fails, so the
            // window is trimmed here rather than only once a reply
            // lands; otherwise repeated failing turns would grow it.
            Self::evict_oldest_turns(&mut history, self.history_max_turns);
            let (messages, prompt) = self.fit_prompt(&mut history);
            if let Some(tx) = &self.audit_tx {
                let mut payload = prompt_trace_payload(&messages, self.full_prompts);
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("context_turn".into(), serde_json::Value::Bool(true));
                }
                let event = TraceEvent::new("llm:conversation-memory", "llm_prompt", payload);
                let _ = tx.try_send(event);
            }
            prompt
        };

        let (intercept_tx, text_capture) = capture(tx);

        let stream_result = self
            .inner
            .process_stream(&prompt, intercept_tx, cancel)
            .await;
        if let Some(text) = text_capture.await.ok().filter(|t| !t.is_empty()) {
            self.append_assistant_response(&text);
        }
        stream_result
    }

    // The wrapper adds history and nothing else: every hook it does not
    // need for that reaches the processor it wraps, which may warm a
    // connection, report its model, or take the session's client tools.
    async fn warm_up(&self) -> Result<(), TextProcessorError> {
        self.inner.warm_up().await
    }

    async fn model_identity(&self) -> Option<voice_pipes_sdk::component::ModelIdentity> {
        self.inner.model_identity().await
    }

    async fn device_info(&self) -> Option<voice_pipes_sdk::component::DeviceInfo> {
        self.inner.device_info().await
    }

    fn bind_session(&self, session: voice_pipes_sdk::agent::SessionHandle) {
        self.inner.bind_session(session);
    }

    fn configure_client_tools(&self, tools: Vec<voice_pipes_sdk::agent::ClientToolSpec>) {
        self.inner.configure_client_tools(tools);
    }

    async fn append_assistant_message(&self, text: &str) {
        // Agent `Say` turn spoken via the TTS-only path: record it so
        // history stays complete (and barge-in truncation can find it).
        self.append_assistant_response(text);
    }

    async fn inject_context(&self, text: &str) {
        let mut history = self.history.write();
        history.push(Message {
            role: "system".to_string(),
            content: text.to_string(),
        });
        Self::evict_oldest_turns(&mut history, self.history_max_turns);
    }

    fn truncate_history(&self, played_text: &str) -> Option<String> {
        self.truncate_assistant_message(played_text)
    }

    fn snapshot_history(&self) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        let history = self.history.read().clone();
        Some(Box::new(history))
    }

    fn restore_history(&self, snapshot: &(dyn std::any::Any + Send + Sync)) {
        if let Some(history) = snapshot.downcast_ref::<Vec<Message>>() {
            (*self.history.write()).clone_from(history);
        }
    }

    fn as_component(&self) -> Option<std::sync::Arc<dyn voice_pipes_sdk::component::Component>> {
        self.inner.as_component()
    }
}

#[cfg(test)]
#[path = "tests/memory.rs"]
mod tests;
