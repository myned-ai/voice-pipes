//! Request construction and HTTP transport for the OpenAI-compatible
//! processor -- body assembly (including role-alternation repair for
//! strict chat templates), retrying POST, and audit-event emission.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::types::{
    ApiErrorResponse, ChatCompletionRequest, ChatMessage, FunctionDef, ResponseFormat,
    StreamOptions, ToolDef,
};
use super::{OpenAiCompatProcessor, TextProcessorError};

/// First back-off delay; doubles per attempt.
const RETRY_BASE_BACKOFF: Duration = Duration::from_millis(500);
/// Longest single back-off delay.
const RETRY_MAX_BACKOFF: Duration = Duration::from_secs(8);
/// How much of an upstream error body is read for the server-side log.
const ERROR_BODY_LOG_BYTES: usize = 256;

/// The wait before retry `attempt`: the jittered backoff, or longer when
/// the server asked for longer.
fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let backoff = voice_pipes_sdk::reconnect::backoff_with_jitter(
        RETRY_BASE_BACKOFF,
        RETRY_MAX_BACKOFF,
        attempt,
    );
    retry_after.map_or(backoff, |wait| wait.min(RETRY_MAX_BACKOFF).max(backoff))
}

/// Log a rejected response by its status and the kind and length of its
/// body. The body itself is not logged: a vendor's error message can quote
/// the request, which carries the conversation. The caller sees only the
/// status summary.
async fn log_rejection(
    resp: voice_pipes_net::reqwest::Response,
    attempt: u32,
    read_timeout: Duration,
) {
    let status = resp.status();
    let body = read_body_limited(resp, ERROR_BODY_LOG_BYTES, read_timeout, true).await;
    let (body_kind, body_bytes) = match &body {
        Ok(bytes) => (rejection_body_kind(bytes), bytes.len()),
        Err(_) => ("unreadable", 0),
    };
    tracing::warn!(
        status = status.as_u16(),
        attempt,
        body_kind,
        body_bytes,
        "OpenAI-compat request rejected by upstream"
    );
}

/// What a rejection's body (its first [`ERROR_BODY_LOG_BYTES`]) is, without
/// repeating any of it.
pub(super) fn rejection_body_kind(bytes: &[u8]) -> &'static str {
    if bytes.is_empty() {
        "empty"
    } else if serde_json::from_slice::<ApiErrorResponse>(bytes).is_ok() {
        "api error"
    } else if serde_json::from_slice::<serde_json::Value>(bytes).is_ok() {
        "other JSON"
    } else {
        "not JSON"
    }
}

/// A `Retry-After` given in seconds (the form rate limits use).
fn retry_after_header(headers: &voice_pipes_net::reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(voice_pipes_net::reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Short, body-free description of a rejected request for the
/// client-facing error: the status code and its class. The upstream
/// body (which may quote the request, the key, or provider internals)
/// only goes to the server log.
fn status_summary(status: voice_pipes_net::reqwest::StatusCode) -> String {
    let class = match status.as_u16() {
        429 => "rate limited",
        401 | 403 => "authentication rejected",
        s if (400..500).contains(&s) => "client error",
        s if (500..600).contains(&s) => "server error",
        _ => "unexpected status",
    };
    format!("upstream returned HTTP {} ({class})", status.as_u16())
}

/// Read a response body with a byte cap and the read timeout applied
/// between chunks. With `truncate` the prefix is returned once the cap
/// is reached; without it, exceeding the cap is an error.
pub(super) async fn read_body_limited(
    resp: voice_pipes_net::reqwest::Response,
    limit: usize,
    read_timeout: Duration,
    truncate: bool,
) -> Result<Vec<u8>, TextProcessorError> {
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut out = Vec::new();
    loop {
        let Ok(next) = tokio::time::timeout(read_timeout, stream.next()).await else {
            return Err(TextProcessorError::new(
                "openai-compat",
                format!(
                    "no data from upstream for {}s while reading the response body",
                    read_timeout.as_secs()
                ),
            )
            .with_kind(voice_pipes_sdk::text::TextErrorKind::Transient));
        };
        match next {
            Some(Ok(bytes)) => {
                if out.len() + bytes.len() > limit {
                    if truncate {
                        let room = limit.saturating_sub(out.len());
                        out.extend(bytes.iter().take(room));
                        return Ok(out);
                    }
                    return Err(TextProcessorError::new(
                        "openai-compat",
                        format!("response body exceeds {limit} bytes"),
                    ));
                }
                out.extend_from_slice(&bytes);
            }
            Some(Err(e)) => {
                // The client bounds each read too; its timeout is the same
                // stall as the one above.
                let message = if e.is_timeout() {
                    format!(
                        "no data from upstream for {}s while reading the response body",
                        read_timeout.as_secs()
                    )
                } else {
                    format!("response read error: {e}")
                };
                return Err(TextProcessorError::new("openai-compat", message)
                    .caused_by(e)
                    .with_kind(voice_pipes_sdk::text::TextErrorKind::Transient));
            }
            None => return Ok(out),
        }
    }
}

impl OpenAiCompatProcessor {
    /// Build the request body for chat completions.
    pub(super) fn build_request(&self, input: &str, stream: bool) -> ChatCompletionRequest {
        self.build_request_as(input, stream, "user")
    }

    /// Like [`build_request`](Self::build_request) but with an explicit
    /// role for the new input message. Agent `Generate` turns send the
    /// injected context as a `system` message so the model doesn't
    /// treat a tool result / external event as something the user said.
    pub(super) fn build_request_as(
        &self,
        input: &str,
        stream: bool,
        input_role: &str,
    ) -> ChatCompletionRequest {
        // Snapshot live cells once per request so all fields agree on a
        // single point-in-time value, even if a PATCH races with us.
        let system_prompt = self.system_prompt();
        let max_tokens = self.max_tokens();
        let temperature = self.temperature();
        let top_p = self.top_p();
        let stop = self.stop();

        // Always the structured shape: system + history + the new turn.
        // The input is content, whatever it contains; the chat endpoint
        // applies its own template, so template markers inside the text
        // are just text.
        let mut messages = vec![ChatMessage {
            role: "system".into(),
            content: (*system_prompt).clone(),
        }];
        let proc_id = std::ptr::from_ref(self) as usize;
        {
            let history = self.history.lock();
            tracing::debug!(
                proc_id,
                history_len = history.len(),
                "build_request: reading conversation history"
            );
            messages.extend(history.iter().cloned());
        };
        messages.push(ChatMessage {
            role: input_role.into(),
            content: input.to_string(),
        });
        Self::enforce_role_alternation(&mut messages);

        let response_format = self
            .config
            .response_format
            .as_ref()
            .map(|fmt| ResponseFormat {
                format_type: fmt.clone(),
            });

        let stream_options = stream.then_some(StreamOptions {
            include_usage: true,
        });

        // Client-side tools ride streaming requests only — the batch
        // `process()` path has no way to surface a call, so advertising
        // tools there could only produce an empty response.
        let tools = if stream {
            let declared = self.client_tools.lock();
            if declared.is_empty() {
                None
            } else {
                Some(
                    declared
                        .iter()
                        .map(|t| ToolDef {
                            tool_type: "function".into(),
                            function: FunctionDef {
                                name: t.name.clone(),
                                description: t.description.clone(),
                                parameters: t.parameters.clone(),
                            },
                        })
                        .collect(),
                )
            }
        } else {
            None
        };
        let tool_choice = if tools.is_some() {
            self.config
                .tool_choice
                .as_ref()
                .map(|c| serde_json::Value::String(c.clone()))
        } else {
            None
        };

        ChatCompletionRequest {
            model: self.config.model.clone(),
            messages,
            stream,
            temperature: Some(temperature),
            top_p,
            max_tokens: Some(max_tokens),
            max_completion_tokens: None,
            stop,
            presence_penalty: self.config.presence_penalty,
            frequency_penalty: self.config.frequency_penalty,
            seed: self.config.seed,
            user: None,
            response_format,
            stream_options,
            tools,
            tool_choice,
            extra: self.config.extra_body.clone().unwrap_or_default(),
        }
    }
}

pub(super) struct SendOutcome {
    pub(super) resp: voice_pipes_net::reqwest::Response,
    /// Time from the winning HTTP attempt's `send()` to response headers (ms).
    pub(super) connect_ms: f64,
    /// Total time spent in failed attempts and retry back-off sleeps before
    /// the winning attempt (ms).
    pub(super) retry_wait_ms: f64,
    /// Number of retry attempts performed before succeeding.
    pub(super) retries: u32,
}

impl OpenAiCompatProcessor {
    /// Send the request, returning the raw response once its headers
    /// arrive alongside per-attempt timing. Transient failures (5xx, 429,
    /// connect and header timeouts) are retried with capped exponential
    /// back-off; `cancel` ends the wait at once, including mid-back-off.
    /// The error returned to the caller never carries the upstream body.
    pub(super) async fn send_request(
        &self,
        body: &ChatCompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<SendOutcome, TextProcessorError> {
        let url = self.config.completions_url();
        let read_timeout = self.config.read_timeout();
        let max_retries = self
            .config
            .max_retries
            .min(super::config::MAX_RETRIES_LIMIT);
        let mut last_err = None;
        // What the server last asked the caller to wait, on a 429.
        let mut retry_after: Option<Duration> = None;
        let retry_clock = std::time::Instant::now();
        let mut retry_wait_ms = 0.0_f64;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = retry_delay(attempt, retry_after);
                tracing::warn!(
                    attempt,
                    delay_ms = delay.as_millis(),
                    "retrying OpenAI-compat request"
                );
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = cancel.cancelled() => return Err(Self::cancelled_error()),
                }
                retry_wait_ms = retry_clock.elapsed().as_secs_f64() * 1000.0;
            }

            let attempt_start = std::time::Instant::now();
            let mut req = self.client.post(&url).json(body);
            if !self.config.api_key.is_empty() {
                req = req.bearer_auth(self.config.api_key.expose());
            }

            let sent = tokio::select! {
                r = tokio::time::timeout(read_timeout, req.send()) => r,
                () = cancel.cancelled() => return Err(Self::cancelled_error()),
            };

            match sent {
                Err(_) => {
                    last_err = Some(format!(
                        "no response headers within {}s",
                        read_timeout.as_secs()
                    ));
                }
                Ok(Ok(resp)) => {
                    let status = resp.status();
                    if status.is_success() {
                        let connect_ms = attempt_start.elapsed().as_secs_f64() * 1000.0;
                        return Ok(SendOutcome {
                            resp,
                            connect_ms,
                            retry_wait_ms,
                            retries: attempt,
                        });
                    }
                    retry_after = retry_after_header(resp.headers());
                    log_rejection(resp, attempt, read_timeout).await;
                    let summary = status_summary(status);

                    if status.is_server_error() || status.as_u16() == 429 {
                        // A wait longer than any back-off here is the
                        // server's answer for now: retrying sooner would
                        // only be refused again, so the call ends with it
                        // and the caller decides when to come back.
                        if let Some(wait) = retry_after.filter(|w| *w > RETRY_MAX_BACKOFF) {
                            return Err(TextProcessorError::new("openai-compat", summary)
                                .with_kind(voice_pipes_sdk::text::TextErrorKind::RateLimited {
                                    retry_after: Some(wait),
                                }));
                        }
                        last_err = Some(summary);
                        continue;
                    }

                    return Err(TextProcessorError::new("openai-compat", summary));
                }
                Ok(Err(e)) => {
                    if e.is_timeout() || e.is_connect() {
                        last_err = Some(format!("network error: {e}"));
                        continue;
                    }
                    return Err(TextProcessorError::new(
                        "openai-compat",
                        format!("request failed: {e}"),
                    )
                    .caused_by(e));
                }
            }
        }

        // Every failure retried here is transient; the caller may try the
        // whole turn again, after what the server asked for if it did.
        let kind = match retry_after {
            Some(wait) => voice_pipes_sdk::text::TextErrorKind::RateLimited {
                retry_after: Some(wait),
            },
            None => voice_pipes_sdk::text::TextErrorKind::Transient,
        };
        Err(TextProcessorError::new(
            "openai-compat",
            format!(
                "all {} attempts failed: {}",
                max_retries + 1,
                last_err.unwrap_or_default()
            ),
        )
        .with_kind(kind))
    }

    /// The error a cancelled send resolves to; callers that own the
    /// token treat it as a quiet end of turn.
    pub(super) fn cancelled_error() -> TextProcessorError {
        TextProcessorError::new("openai-compat", "request cancelled")
    }

    /// Emit an audit trace event.
    pub(super) fn emit_audit(&self, event_type: &str, payload: serde_json::Value) {
        if let Some(tx) = &self.audit_tx {
            let _ = tx.try_send(voice_pipes_sdk::TraceEvent::new(
                "llm:openai-compat",
                event_type,
                payload,
            ));
        }
    }

    /// Normalize the outgoing message list for chat templates that hard-
    /// require user/assistant alternation starting with a user turn
    /// (Gemma's llama.cpp template `raise_exception`s an HTTP 400
    /// otherwise; OpenAI-style cloud endpoints don't care).
    ///
    /// Agent-initiated turns legitimately break alternation in two ways:
    /// consecutive assistant messages (idle re-engagement prompts the
    /// user never answered) and an assistant-first history (the agent
    /// spoke before the user did). Memory keeps the true structure;
    /// only the wire request is massaged.
    fn enforce_role_alternation(msgs: &mut Vec<ChatMessage>) {
        let first_chat = usize::from(msgs.first().is_some_and(|m| m.role == "system"));
        // Merge runs of same-role messages into one, newline-joined.
        let mut alternating: Vec<ChatMessage> = Vec::with_capacity(msgs.len());
        for (k, m) in std::mem::take(msgs).into_iter().enumerate() {
            match alternating.last_mut() {
                Some(prev) if k > first_chat && prev.role == m.role => {
                    prev.content.push('\n');
                    prev.content.push_str(&m.content);
                }
                _ => alternating.push(m),
            }
        }
        *msgs = alternating;
        // An assistant-first conversation needs a user turn in front of it.
        if msgs.get(first_chat).is_some_and(|m| m.role == "assistant") {
            msgs.insert(
                first_chat,
                ChatMessage {
                    role: "user".into(),
                    content: "(The user hasn't said anything yet.)".into(),
                },
            );
        }
    }
}

#[cfg(test)]
#[path = "tests/request.rs"]
mod tests;
