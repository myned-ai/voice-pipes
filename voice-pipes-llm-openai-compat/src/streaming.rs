//! Turn execution for the OpenAI-compatible processor -- the batch and
//! SSE-streaming loops behind `process` / `process_stream`, including
//! client-side tool-call assembly and history recording.

use std::fmt::Write;
use std::time::Instant;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::request::read_body_limited;
use super::sse::SseLine;
use super::types::{
    ChatCompletionChunk, ChatCompletionResponse, ChatMessage, ToolCallDelta, Usage,
};
use super::{OpenAiCompatProcessor, TextFragment, TextProcessorError, split_first_sentence, sse};

/// Highest `tool_calls[].index` accepted; the accumulator is sized by
/// it, so a server-chosen index must not size an allocation.
pub(super) const MAX_TOOL_CALL_INDEX: u32 = 64;
/// Longest argument JSON accepted for one tool call.
pub(super) const MAX_TOOL_ARGUMENTS_BYTES: usize = 64 * 1024;
/// Longest single SSE line; a stream that goes this far without a
/// newline is not an event stream and ends the turn.
pub(super) const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
/// Most response text retained for one turn; further content is
/// dropped and the turn ends with what was generated.
pub(super) const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Largest non-streaming response body parsed.
pub(super) const MAX_BATCH_BODY_BYTES: usize = 1024 * 1024;
/// Name and id fragments are short; anything longer is stream garbage.
const MAX_TOOL_NAME_BYTES: usize = 256;

/// How much of a server's in-stream error message is kept: enough to say
/// what went wrong, bounded because the server chooses its length.
const MAX_STREAM_ERROR_CHARS: usize = 300;

/// The turn's error for an error the server sent in the stream.
fn stream_error_message(e: &sse::StreamError) -> String {
    let message: String = e.message.chars().take(MAX_STREAM_ERROR_CHARS).collect();
    match &e.kind {
        Some(kind) => format!("upstream error mid-response ({kind}): {message}"),
        None => format!("upstream error mid-response: {message}"),
    }
}

/// One tool call being assembled from streamed `delta.tool_calls`
/// fragments (the provider sends `id`/`name` on the first fragment and
/// argument JSON as string pieces on the rest, correlated by `index`).
#[derive(Debug, Default, Clone)]
pub(super) struct PendingToolCall {
    pub(super) id: Option<String>,
    pub(super) name: String,
    pub(super) arguments: String,
    /// The arguments outgrew the cap; the call is discarded at the end
    /// of the turn instead of being surfaced truncated.
    pub(super) overflow: bool,
}

/// Fold one chunk's `delta.tool_calls` into the per-response
/// accumulator. Fragments without an `index` (non-conformant servers)
/// start a new call when they carry an id/name and otherwise continue
/// the last one. An index past the cap is ignored; arguments past
/// their cap mark the call as overflowed.
pub(super) fn accumulate_tool_calls(acc: &mut Vec<PendingToolCall>, deltas: &[ToolCallDelta]) {
    for delta in deltas {
        let idx = match delta.index {
            Some(i) if i > MAX_TOOL_CALL_INDEX => {
                tracing::warn!(index = i, "tool call index past the cap; fragment ignored");
                continue;
            }
            Some(i) => i as usize,
            None if delta.id.is_some()
                || delta.function.as_ref().is_some_and(|f| f.name.is_some()) =>
            {
                acc.len()
            }
            None => acc.len().saturating_sub(1),
        };
        if idx > MAX_TOOL_CALL_INDEX as usize {
            continue;
        }
        if acc.len() <= idx {
            acc.resize_with(idx + 1, Default::default);
        }
        let Some(slot) = acc.get_mut(idx) else {
            continue;
        };
        if let Some(id) = &delta.id
            && id.len() <= MAX_TOOL_NAME_BYTES
        {
            slot.id = Some(id.clone());
        }
        if let Some(f) = &delta.function {
            if let Some(name) = &f.name
                && slot.name.len() + name.len() <= MAX_TOOL_NAME_BYTES
            {
                slot.name.push_str(name);
            }
            if let Some(args) = &f.arguments
                && !slot.overflow
            {
                if slot.arguments.len() + args.len() > MAX_TOOL_ARGUMENTS_BYTES {
                    tracing::warn!(
                        index = idx,
                        "tool call arguments past the cap; call will be dropped"
                    );
                    slot.overflow = true;
                    slot.arguments.clear();
                } else {
                    slot.arguments.push_str(args);
                }
            }
        }
    }
}

impl OpenAiCompatProcessor {
    /// A turn that fails after part of the reply was already streamed
    /// (and possibly spoken) keeps that heard prefix in history, so the
    /// next turn's context matches what the user actually heard. A turn
    /// that produced nothing leaves history untouched.
    fn keep_heard_prefix(&self, input: &str, input_role: &str, heard: &str) {
        if heard.trim().is_empty() {
            return;
        }
        let mut history = self.history.lock();
        history.push(ChatMessage {
            role: input_role.into(),
            content: input.to_string(),
        });
        history.push(ChatMessage {
            role: "assistant".into(),
            content: heard.to_string(),
        });
        Self::evict_oldest_turns(&mut history, self.config.history_max_turns as usize);
        self.last_turn_truncatable
            .store(true, std::sync::atomic::Ordering::Release);
        tracing::info!(
            heard_len = heard.len(),
            "OpenAI-compat turn failed mid-stream; heard prefix kept in history"
        );
    }

    pub(super) async fn process_inner(&self, input: &str) -> Result<String, TextProcessorError> {
        self.last_turn_truncatable
            .store(false, std::sync::atomic::Ordering::Release);
        let t0 = Instant::now();
        let body = self.build_request(input, false);

        self.emit_audit(
            "llm_request",
            serde_json::json!({
                "model": &body.model,
                "stream": false,
                "messages_count": body.messages.len(),
            }),
        );

        let outcome = self.send_request(&body, &CancellationToken::new()).await?;
        let raw = read_body_limited(
            outcome.resp,
            MAX_BATCH_BODY_BYTES,
            self.config.read_timeout(),
            false,
        )
        .await?;
        let parsed: ChatCompletionResponse = serde_json::from_slice(&raw).map_err(|e| {
            // The serde message quotes the offending value, which can be
            // the model's reply; keep only where the parse failed.
            TextProcessorError::new(
                "openai-compat",
                format!(
                    "failed to parse response: {}",
                    voice_pipes_sdk::secrets::json_error(&e)
                ),
            )
        })?;

        let content = parsed
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

        tracing::info!(
            model = %self.config.model,
            elapsed_ms = format!("{elapsed_ms:.1}"),
            output_len = content.len(),
            "OpenAI-compat non-streaming complete"
        );

        self.emit_audit(
            "llm_detail",
            serde_json::json!({
                "model": &self.config.model,
                "stream": false,
                "elapsed_ms": (elapsed_ms * 10.0).round() / 10.0,
                "connect_ms": (outcome.connect_ms * 10.0).round() / 10.0,
                "retry_wait_ms": (outcome.retry_wait_ms * 10.0).round() / 10.0,
                "retries": outcome.retries,
                "usage": parsed.usage,
            }),
        );

        if content.is_empty() {
            // Nothing to speak and no assistant turn to record; the input
            // returned in its place would be spoken back as the answer.
            tracing::warn!("OpenAI-compat returned an empty response");
            Ok(String::new())
        } else {
            // Append this turn to conversation history
            let proc_id = std::ptr::from_ref(self) as usize;
            {
                let mut history = self.history.lock();
                history.push(ChatMessage {
                    role: "user".into(),
                    content: input.to_string(),
                });
                history.push(ChatMessage {
                    role: "assistant".into(),
                    content: content.clone(),
                });
                Self::evict_oldest_turns(&mut history, self.config.history_max_turns as usize);
                self.last_turn_truncatable
                    .store(true, std::sync::atomic::Ordering::Release);
                tracing::debug!(
                    proc_id,
                    history_len = history.len(),
                    "process: appended turn to conversation history"
                );
            };
            Ok(content)
        }
    }

    pub(super) async fn process_stream_inner(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
        // "user" for normal turns; "system" for agent `Generate` turns
        // (injected context, not something the user said).
        input_role: &str,
    ) -> Result<(), TextProcessorError> {
        use futures_util::StreamExt;

        self.last_turn_truncatable
            .store(false, std::sync::atomic::Ordering::Release);
        let t0 = Instant::now();
        let body = self.build_request_as(input, true, input_role);
        self.emit_audit(
            "llm_request",
            serde_json::json!({
                "model": &body.model,
                "stream": true,
                "messages_count": body.messages.len(),
            }),
        );

        let outcome = match self.send_request(&body, &cancel).await {
            Ok(outcome) => outcome,
            Err(_) if cancel.is_cancelled() => {
                // Barge-in before the model answered: end the turn
                // quietly, the same way a cancelled stream does.
                tracing::info!("OpenAI-compat request cancelled before a response");
                let _ = tx
                    .send(TextFragment::new(String::new()).with_is_end_of_response(true))
                    .await;
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        // `send_request` measures `connect_ms` on the winning HTTP attempt
        // (excluding any prior failed attempts and retry back-off sleeps,
        // which are reported separately in `retry_wait_ms` / `retries`).
        let connect_ms = outcome.connect_ms;
        let retry_wait_ms = outcome.retry_wait_ms;
        let retries = outcome.retries;
        let resp = outcome.resp;

        let mut turn = StreamTurn::new(t0);
        let mut sse_lines = sse::SseReader::new(MAX_SSE_LINE_BYTES);
        let read_timeout = self.config.read_timeout();
        let mut stream = resp.bytes_stream();
        loop {
            let next = tokio::select! {
                () = cancel.cancelled() => {
                    tracing::info!("OpenAI-compat stream cancelled by user");
                    break;
                }
                next = tokio::time::timeout(read_timeout, stream.next()) => next,
            };
            let read = match next {
                Ok(Some(Ok(bytes))) => turn.on_bytes(self, &mut sse_lines, &bytes, &tx).await,
                Ok(None) => break,
                // The client bounds each read too; its timeout is the same
                // stall as this one.
                Err(_) => Err(stalled(read_timeout)),
                Ok(Some(Err(e))) if e.is_timeout() => Err(stalled(read_timeout).caused_by(e)),
                Ok(Some(Err(e))) => Err(TextProcessorError::new(
                    "openai-compat",
                    format!("stream read error: {e}"),
                )
                .caused_by(e)
                .with_kind(voice_pipes_sdk::text::TextErrorKind::Transient)),
            };
            match read {
                Ok(false) => {}
                Ok(true) => break,
                Err(e) => {
                    self.keep_heard_prefix(input, input_role, &turn.full_response);
                    return Err(e);
                }
            }
        }

        self.finish_stream(
            input,
            input_role,
            turn,
            &tx,
            cancel.is_cancelled(),
            connect_ms,
            retry_wait_ms,
            retries,
        )
        .await;
        Ok(())
    }

    /// The turn's read is over: report it, send the rest of the text with
    /// the end of the response, surface the tool calls and record the turn.
    #[expect(
        clippy::too_many_arguments,
        reason = "internal stream finalizer carrying request timing and turn state"
    )]
    async fn finish_stream(
        &self,
        input: &str,
        input_role: &str,
        mut turn: StreamTurn,
        tx: &mpsc::Sender<TextFragment>,
        cancelled: bool,
        connect_ms: f64,
        retry_wait_ms: f64,
        retries: u32,
    ) {
        let elapsed_ms = turn.t0.elapsed().as_secs_f64() * 1000.0;
        // Fully-assembled client tool calls (fragments with no name are
        // stream garbage, not calls; overflowed arguments are not a call
        // either). A cancelled turn (barge-in) drops its calls: the user cut
        // the assistant off mid-decision.
        let tool_calls: Vec<PendingToolCall> = if cancelled {
            Vec::new()
        } else {
            std::mem::take(&mut turn.tool_calls)
                .into_iter()
                .filter(|c| !c.name.is_empty() && !c.overflow)
                .collect()
        };
        tracing::info!(
            model = %self.config.model,
            elapsed_ms = format!("{elapsed_ms:.1}"),
            first_token_ms = format!("{:.1}", turn.first_token_ms.unwrap_or(0.0)),
            output_len = turn.full_response.len(),
            tool_calls = tool_calls.len(),
            "OpenAI-compat streaming complete"
        );

        let final_text = turn.sentence_buffer.trim().to_string();
        if turn.first_sentence_chars.is_none() && !final_text.is_empty() {
            turn.first_sentence_chars = Some(final_text.len());
        }
        self.emit_audit(
            "llm_detail",
            serde_json::json!({
                "model": &self.config.model,
                "stream": true,
                "elapsed_ms": (elapsed_ms * 10.0).round() / 10.0,
                "connect_ms": (connect_ms * 10.0).round() / 10.0,
                "retry_wait_ms": (retry_wait_ms * 10.0).round() / 10.0,
                "retries": retries,
                "first_token_ms": turn.first_token_ms.map(|v| (v * 10.0).round() / 10.0),
                "first_sentence_chars": turn.first_sentence_chars,
                "tool_calls": tool_calls.len(),
                "usage": turn.usage,
            }),
        );

        // Record the turn in conversation history before awaiting channel sends
        // so that a cancelled downstream consumer cannot abort the task before
        // the partial assistant entry is available for `truncate_history`.
        self.record_streamed_turn(input, input_role, turn.full_response, &tool_calls);

        if !final_text.is_empty() {
            tracing::debug!(
                fragment_chars = final_text.chars().count(),
                is_final = true,
                "OpenAI-compat streaming fragment"
            );
        }
        let _ = tx
            .send(
                TextFragment::new(final_text)
                    .with_is_end_of_response(true)
                    .with_token_count(turn.usage.as_ref().map(|u| u.completion_tokens))
                    // A response that never reached a sentence boundary has
                    // its first tokens here: the TTFT goes with them.
                    .with_first_token_ms(turn.pending_first_token_ms.take()),
            )
            .await;

        self.surface_tool_calls(&tool_calls).await;
    }

    /// Surface client tool calls out-of-band: the runner validates them
    /// against the session allowlist, registers each call, and sends the
    /// `trigger_action` command to the client. The turn itself has already
    /// ended prose-free — there is deliberately no in-turn tool loop.
    async fn surface_tool_calls(&self, tool_calls: &[PendingToolCall]) {
        if tool_calls.is_empty() {
            return;
        }
        let handle = self.session_handle.lock().clone();
        let Some(handle) = handle else {
            tracing::warn!(
                calls = tool_calls.len(),
                "OpenAI-compat tool calls with no bound session -- dropped"
            );
            return;
        };
        for call in tool_calls {
            let call_id = call
                .id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let arguments = if call.arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .unwrap_or_else(|_not_json| serde_json::Value::String(call.arguments.clone()))
            };
            tracing::info!(
                tool = %call.name,
                call_id = %call_id,
                "OpenAI-compat surfacing client tool call"
            );
            let _ = handle
                .client_tool_call(voice_pipes_sdk::agent::ClientToolCall::new(
                    &call_id, &call.name, arguments,
                ))
                .await;
        }
    }

    /// Append the turn to conversation history. Tool calls are recorded as
    /// a readable marker so the model remembers what it invoked on the next
    /// turn (there is no native tool-role history here — see
    /// `enforce_role_alternation` for why the wire shape stays plain
    /// user/assistant text).
    fn record_streamed_turn(
        &self,
        input: &str,
        input_role: &str,
        response: String,
        tool_calls: &[PendingToolCall],
    ) {
        let mut assistant_entry = response;
        for call in tool_calls {
            if !assistant_entry.is_empty() {
                assistant_entry.push('\n');
            }
            let _ = write!(
                assistant_entry,
                "[called client-side tool {}({})]",
                call.name, call.arguments
            );
        }
        let proc_id = std::ptr::from_ref(self) as usize;
        if assistant_entry.is_empty() {
            tracing::warn!(
                proc_id,
                "process_stream: empty response and no tool calls, skipping history append"
            );
            return;
        }
        let mut history = self.history.lock();
        history.push(ChatMessage {
            role: input_role.into(),
            content: input.to_string(),
        });
        history.push(ChatMessage {
            role: "assistant".into(),
            content: assistant_entry,
        });
        Self::evict_oldest_turns(&mut history, self.config.history_max_turns as usize);
        self.last_turn_truncatable
            .store(true, std::sync::atomic::Ordering::Release);
        tracing::debug!(
            proc_id,
            history_len = history.len(),
            "process_stream: appended turn to conversation history"
        );
    }
}

/// The turn's error for a stream that went quiet.
fn stalled(read_timeout: std::time::Duration) -> TextProcessorError {
    TextProcessorError::new(
        "openai-compat",
        format!(
            "no data from upstream for {}s mid-response",
            read_timeout.as_secs()
        ),
    )
    .with_kind(voice_pipes_sdk::text::TextErrorKind::Transient)
}

/// What one streamed turn has produced so far.
struct StreamTurn {
    t0: Instant,
    sentence_buffer: String,
    full_response: String,
    /// Client-side tool calls streamed by the model, assembled across
    /// chunks. A tool-call response typically carries no prose
    /// (`finish_reason: "tool_calls"`), so the turn ends silently and the
    /// calls are surfaced out-of-band.
    tool_calls: Vec<PendingToolCall>,
    usage: Option<Usage>,
    first_token_ms: Option<f64>,
    /// Same value as `first_token_ms`, consumed by the first fragment sent,
    /// so the orchestrator's bridge uses the provider's own TTFT rather than
    /// wall-clock from its entry.
    pending_first_token_ms: Option<f64>,
    first_sentence_chars: Option<usize>,
    /// Set once a choice reports `finish_reason` or the response cap is
    /// hit. The chunk after that may still carry usage; anything else after
    /// it ends the read, so a server that keeps streaming past its own end
    /// (or never sends `[DONE]`) cannot hold the turn open.
    finished: bool,
}

impl StreamTurn {
    fn new(t0: Instant) -> Self {
        Self {
            t0,
            sentence_buffer: String::new(),
            full_response: String::new(),
            tool_calls: Vec::new(),
            usage: None,
            first_token_ms: None,
            pending_first_token_ms: None,
            first_sentence_chars: None,
            finished: false,
        }
    }

    /// The SSE lines in `bytes`. `Ok(true)` ends the read.
    async fn on_bytes(
        &mut self,
        processor: &OpenAiCompatProcessor,
        sse_lines: &mut sse::SseReader,
        bytes: &[u8],
        tx: &mpsc::Sender<TextFragment>,
    ) -> Result<bool, TextProcessorError> {
        let lines = sse_lines.push(bytes).map_err(|too_long| {
            TextProcessorError::new(
                "openai-compat",
                format!(
                    "upstream sent more than {} bytes without an event boundary",
                    too_long.limit
                ),
            )
        })?;
        for line in lines {
            match sse::parse_sse_line(&line) {
                Ok(SseLine::Chunk(chunk)) => {
                    if self.on_chunk(processor, chunk, tx).await {
                        return Ok(true);
                    }
                }
                Ok(SseLine::Done) => return Ok(true),
                Ok(SseLine::Error(e)) => {
                    return Err(TextProcessorError::new(
                        "openai-compat",
                        stream_error_message(&e),
                    ));
                }
                Ok(SseLine::Skip) => {}
                Err(e) => tracing::warn!(error = %e, "SSE parse error, skipping line"),
            }
        }
        Ok(false)
    }

    /// One streamed chunk: its usage, tool-call fragments and text, with
    /// each completed sentence sent on for speech. `true` ends the read.
    async fn on_chunk(
        &mut self,
        processor: &OpenAiCompatProcessor,
        mut chunk: ChatCompletionChunk,
        tx: &mpsc::Sender<TextFragment>,
    ) -> bool {
        if chunk.usage.is_some() {
            self.usage = chunk.usage.take();
        }
        if self.finished {
            return true;
        }
        for choice in &chunk.choices {
            if choice.finish_reason.is_some() {
                self.finished = true;
            }
            if let Some(deltas) = &choice.delta.tool_calls {
                accumulate_tool_calls(&mut self.tool_calls, deltas);
            }
            if let Some(content) = &choice.delta.content {
                self.on_content(processor, content, tx).await;
            }
        }
        // Finish plus usage: nothing left worth waiting for. Finish alone:
        // allow one more chunk for a trailing usage record.
        self.finished && self.usage.is_some()
    }

    async fn on_content(
        &mut self,
        processor: &OpenAiCompatProcessor,
        content: &str,
        tx: &mpsc::Sender<TextFragment>,
    ) {
        if self.full_response.len() + content.len() > MAX_RESPONSE_BYTES {
            tracing::warn!(
                retained = self.full_response.len(),
                "OpenAI-compat response past the size cap; ending the turn"
            );
            self.finished = true;
            return;
        }
        // L3: Stamp TTFT only on non-empty content so an initial role-only
        // chunk (`delta: { role: "assistant", content: "" }`) does not stamp
        // TTFT before any token has been generated.
        if !content.is_empty() && self.first_token_ms.is_none() {
            let t = self.t0.elapsed().as_secs_f64() * 1000.0;
            self.first_token_ms = Some(t);
            self.pending_first_token_ms = Some(t);
        }
        self.sentence_buffer.push_str(content);
        self.full_response.push_str(content);
        let chunking = &processor.text_chunking;
        while let Some((sentence, rest)) = split_first_sentence(
            &self.sentence_buffer,
            chunking.min_soft_split_chars,
            chunking.min_soft_split_words,
            false,
        ) {
            if !sentence.trim().is_empty() {
                if self.first_sentence_chars.is_none() {
                    self.first_sentence_chars = Some(sentence.trim_end().len());
                }
                tracing::debug!(
                    fragment_chars = sentence.chars().count(),
                    "OpenAI-compat streaming fragment"
                );
                let _ = tx
                    .send(
                        TextFragment::new(sentence.to_string())
                            .with_first_token_ms(self.pending_first_token_ms.take()),
                    )
                    .await;
            }
            self.sentence_buffer = rest.to_string();
        }
    }
}
