//! Integration tests for the OpenAI-compatible text processor.
//!
//! Uses a lightweight axum-based mock server to simulate an
//! `OpenAI` `/v1/chat/completions` endpoint, testing both
//! streaming (SSE) and non-streaming paths.

use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use voice_pipes_llm_openai_compat::OpenAiCompatConfig;
use voice_pipes_llm_openai_compat::OpenAiCompatProcessor;
use voice_pipes_sdk::TraceEvent;
use voice_pipes_sdk::text::{TextFragment, TextProcessor};

// ── Test timeout ────────────────────────────────────────────

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

// ── Helper: default test config pointing at mock server ─────

fn test_config(base_url: &str) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        base_url: base_url.to_string(),
        model: "mock-model".into(),
        api_key: voice_pipes_sdk::secrets::Secret::default(),
        system_prompt: "You are a test assistant.".into(),
        max_tokens: 64,
        temperature: Some(0.5),
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        stop: None,
        timeout_secs: 5,
        max_retries: 2,
        response_format: None,
        tool_choice: None,
        extra_body: None,
        history_max_turns: 20,
    }
}

// ═══════════════════════════════════════════════════════════════
//  Mock server infrastructure
// ═══════════════════════════════════════════════════════════════

/// Shared state for the mock server.
#[derive(Clone)]
struct MockState {
    /// Number of requests received (for retry tests).
    request_count: Arc<AtomicUsize>,
    /// The mock behavior to use.
    behavior: Arc<MockBehavior>,
    /// Captured request bodies (for inspecting messages sent by the processor).
    captured_bodies: Arc<std::sync::Mutex<Vec<Value>>>,
}

/// Configurable behavior for the mock server's `/v1/chat/completions`.
#[derive(Clone)]
enum MockBehavior {
    /// Return a non-streaming response with the given text.
    NonStreaming { text: String },
    /// Return an SSE stream of token chunks.
    Streaming { tokens: Vec<String> },
    /// Return an SSE stream with a configurable delay between chunks.
    StreamingWithDelay { tokens: Vec<String>, delay_ms: u64 },
    /// Return an error status on the first N requests, then succeed.
    FailThenSucceed {
        fail_status: u16,
        fail_count: usize,
        success_text: String,
    },
    /// Always return the given error status.
    AlwaysFail { status: u16, body: Value },
    /// Delay longer than the client timeout.
    SlowResponse { delay_ms: u64 },
    /// Return an SSE stream carrying one function tool call, split
    /// across chunks the way `OpenAI` streams them (id+name first, then
    /// argument fragments, then `finish_reason: "tool_calls"`).
    StreamingToolCall {
        call_id: String,
        name: String,
        arguments: String,
    },
}

/// Start the mock server and return its base URL (e.g. `http://127.0.0.1:PORT/v1`).
async fn start_mock(behavior: MockBehavior) -> (String, MockState) {
    let state = MockState {
        request_count: Arc::new(AtomicUsize::new(0)),
        behavior: Arc::new(behavior),
        captured_bodies: Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base_url = format!("http://127.0.0.1:{}/v1", addr.port());
    (base_url, state)
}

/// Handler for POST `/v1/chat/completions`.
async fn handle_chat_completions(
    State(state): State<MockState>,
    _headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let count = state.request_count.fetch_add(1, Ordering::SeqCst);
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    // Capture the request body for inspection.
    state.captured_bodies.lock().unwrap().push(body.clone());

    match state.behavior.as_ref() {
        MockBehavior::NonStreaming { text } => non_streaming_response(text),

        MockBehavior::Streaming { tokens } => {
            if stream {
                streaming_response(tokens, 0)
            } else {
                // Concatenate all tokens for non-streaming mode
                let full: String = tokens.iter().map(String::as_str).collect();
                non_streaming_response(&full)
            }
        }

        MockBehavior::StreamingWithDelay { tokens, delay_ms } => {
            if stream {
                streaming_response(tokens, *delay_ms)
            } else {
                let full: String = tokens.iter().map(String::as_str).collect();
                non_streaming_response(&full)
            }
        }

        MockBehavior::FailThenSucceed {
            fail_status,
            fail_count,
            success_text,
        } => {
            if count < *fail_count {
                error_response(*fail_status, &format!("Mock failure #{count}"))
            } else if stream {
                streaming_response(std::slice::from_ref(success_text), 0)
            } else {
                non_streaming_response(success_text)
            }
        }

        MockBehavior::AlwaysFail { status, body } => (
            StatusCode::from_u16(*status).unwrap(),
            axum::Json(body.clone()),
        )
            .into_response(),

        MockBehavior::SlowResponse { delay_ms } => {
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
            non_streaming_response("too slow")
        }

        MockBehavior::StreamingToolCall {
            call_id,
            name,
            arguments,
        } => streaming_tool_call_response(call_id, name, arguments),
    }
}

fn non_streaming_response(text: &str) -> Response {
    let body = json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });
    axum::Json(body).into_response()
}

fn streaming_response(tokens: &[String], delay_ms: u64) -> Response {
    let mut sse_body = String::new();
    for (i, token) in tokens.iter().enumerate() {
        let finish = if i == tokens.len() - 1 {
            r#""stop""#
        } else {
            "null"
        };
        let chunk = format!(
            r#"{{"id":"chatcmpl-mock","choices":[{{"delta":{{"content":"{token}"}},"finish_reason":{finish}}}]}}"#,
        );
        let _ = write!(sse_body, "data: {chunk}\n\n");
    }
    // Final usage chunk + [DONE]
    sse_body.push_str("data: {\"id\":\"chatcmpl-mock\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n");
    sse_body.push_str("data: [DONE]\n\n");

    if delay_ms == 0 {
        Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from(sse_body))
            .unwrap()
    } else {
        // For delayed streaming, we need to send chunks with pauses.
        // Build an async stream of bytes.
        let tokens_owned: Vec<String> = tokens.to_vec();
        let stream = async_stream::stream! {
            for (i, token) in tokens_owned.iter().enumerate() {
                if delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                let finish = if i == tokens_owned.len() - 1 { r#""stop""# } else { "null" };
                let chunk = format!(
                    r#"{{"id":"chatcmpl-mock","choices":[{{"delta":{{"content":"{token}"}},"finish_reason":{finish}}}]}}"#,
                );
                let line = format!("data: {chunk}\n\n");
                yield Ok::<_, std::convert::Infallible>(line);
            }
            yield Ok("data: {\"id\":\"chatcmpl-mock\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n".to_string());
            yield Ok("data: [DONE]\n\n".to_string());
        };
        Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
    }
}

/// SSE stream for a tool-call response: the call is split across
/// chunks exactly as `OpenAI` streams it — `id` + `name` + empty
/// `arguments` first, argument JSON in two fragments, then a
/// prose-free `finish_reason: "tool_calls"`.
fn streaming_tool_call_response(call_id: &str, name: &str, arguments: &str) -> Response {
    let mid = arguments.len() / 2;
    // Split on a char boundary near the middle.
    let mut split = mid;
    while !arguments.is_char_boundary(split) {
        split += 1;
    }
    let (args_a, args_b) = arguments.split_at(split);
    let mut sse_body = String::new();
    let head = json!({
        "id": "chatcmpl-mock",
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [{
                "index": 0, "id": call_id, "type": "function",
                "function": { "name": name, "arguments": "" }
            }]},
            "finish_reason": null
        }]
    });
    let _ = write!(sse_body, "data: {head}\n\n");
    for fragment in [args_a, args_b] {
        let chunk = json!({
            "id": "chatcmpl-mock",
            "choices": [{
                "index": 0,
                "delta": { "tool_calls": [{
                    "index": 0,
                    "function": { "arguments": fragment }
                }]},
                "finish_reason": null
            }]
        });
        let _ = write!(sse_body, "data: {chunk}\n\n");
    }
    sse_body.push_str(
        "data: {\"id\":\"chatcmpl-mock\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    );
    sse_body.push_str("data: {\"id\":\"chatcmpl-mock\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n");
    sse_body.push_str("data: [DONE]\n\n");
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(Body::from(sse_body))
        .unwrap()
}

fn error_response(status: u16, message: &str) -> Response {
    let body = json!({
        "error": {
            "message": message,
            "type": "server_error",
            "code": Value::Null
        }
    });
    (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
}

// ═══════════════════════════════════════════════════════════════
//  Helper: collect all fragments from a streaming call
// ═══════════════════════════════════════════════════════════════

async fn collect_fragments(
    proc: &OpenAiCompatProcessor,
    input: &str,
    cancel: CancellationToken,
) -> Result<Vec<TextFragment>, voice_pipes_sdk::text::TextProcessorError> {
    let (tx, mut rx) = mpsc::channel::<TextFragment>(64);
    proc.process_stream(input, tx, cancel).await?;
    let mut fragments = Vec::new();
    while let Ok(frag) = rx.try_recv() {
        fragments.push(frag);
    }
    Ok(fragments)
}

// ═══════════════════════════════════════════════════════════════
//  1. Non-streaming round-trip
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_process_non_streaming() {
    let (base_url, _state) = start_mock(MockBehavior::NonStreaming {
        text: "Hello from the mock!".into(),
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    let result = timeout(TEST_TIMEOUT, proc.process("Hi there"))
        .await
        .expect("timed out")
        .expect("process failed");

    assert_eq!(result, "Hello from the mock!");
}

/// A reply with no content is an empty reply, not the prompt: the prompt
/// passed through would be spoken back to the user as the answer.
#[tokio::test]
async fn an_empty_reply_is_empty_not_the_prompt() {
    let (base_url, _state) = start_mock(MockBehavior::NonStreaming {
        text: String::new(),
    })
    .await;
    let proc = OpenAiCompatProcessor::new(
        test_config(&base_url),
        None,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();
    let result = timeout(TEST_TIMEOUT, proc.process("Hi there"))
        .await
        .expect("timed out")
        .expect("process failed");
    assert_eq!(result, "");
}

// ═══════════════════════════════════════════════════════════════
//  2. Streaming basic flow
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_process_stream_basic() {
    let (base_url, _state) = start_mock(MockBehavior::Streaming {
        tokens: vec![
            "Hello".into(),
            " world".into(),
            "!".into(),
            " How".into(),
            " are".into(),
            " you".into(),
            "?".into(),
        ],
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();
    let cancel = CancellationToken::new();

    let fragments = timeout(TEST_TIMEOUT, collect_fragments(&proc, "Hi", cancel))
        .await
        .expect("timed out")
        .expect("stream failed");

    // There should be at least one fragment
    assert!(!fragments.is_empty(), "expected at least one fragment");

    // The last fragment must have is_end_of_response = true
    assert!(
        fragments.last().unwrap().is_end_of_response,
        "last fragment should be end-of-response"
    );

    // Concatenated text should contain the full response
    let full: String = fragments.iter().map(|f| f.text.clone()).collect();
    assert!(
        full.contains("Hello world"),
        "full text should contain 'Hello world', got: {full}"
    );
}

// ═══════════════════════════════════════════════════════════════
//  3. Streaming cancellation
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_process_stream_cancellation() {
    // Slow stream with 200ms between chunks — gives time to cancel
    let (base_url, _state) = start_mock(MockBehavior::StreamingWithDelay {
        tokens: (0..20).map(|i| format!("word{i} ")).collect(),
        delay_ms: 200,
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();
    let cancel = CancellationToken::new();

    let (tx, mut rx) = mpsc::channel::<TextFragment>(64);
    let cancel_clone = cancel.clone();

    // Spawn the stream processor
    let handle = tokio::spawn(async move { proc.process_stream("Hi", tx, cancel_clone).await });

    // Wait briefly then cancel
    tokio::time::sleep(Duration::from_millis(400)).await;
    cancel.cancel();

    let result = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("timed out")
        .expect("join failed");

    // Should complete without error (cancellation is graceful)
    assert!(result.is_ok(), "cancellation should not produce an error");

    // Should have received fewer fragments than the full 20 tokens would produce
    let mut count = 0;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    // We can't assert an exact count, but we should have fewer than a full stream
    // would produce. With 20 tokens at 200ms each, 400ms should yield ~2 chunks.
    assert!(
        count < 15,
        "expected fewer fragments after cancellation, got {count}"
    );
}

// ═══════════════════════════════════════════════════════════════
//  4. Sentence boundary streaming
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_process_stream_sentence_boundaries() {
    // Tokens that form two sentences: "Hello world. How are you?"
    let (base_url, _state) = start_mock(MockBehavior::Streaming {
        tokens: vec![
            "Hello".into(),
            " world".into(),
            ".".into(),
            " How".into(),
            " are".into(),
            " you".into(),
            "?".into(),
        ],
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();
    let cancel = CancellationToken::new();

    let fragments = timeout(TEST_TIMEOUT, collect_fragments(&proc, "Hi", cancel))
        .await
        .expect("timed out")
        .expect("stream failed");

    // With sentence boundary detection, we expect at least 2 fragments:
    // "Hello world." and "How are you?" (plus possibly the final flush)
    assert!(
        fragments.len() >= 2,
        "expected >=2 sentence fragments, got {}: {:?}",
        fragments.len(),
        fragments.iter().map(|f| &f.text).collect::<Vec<_>>()
    );

    // Only the last fragment should have is_end_of_response
    for (i, frag) in fragments.iter().enumerate() {
        if i < fragments.len() - 1 {
            assert!(
                !frag.is_end_of_response,
                "intermediate fragment {i} should not be end-of-response"
            );
        }
    }
    assert!(fragments.last().unwrap().is_end_of_response);
}

// ═══════════════════════════════════════════════════════════════
//  5. Retry on 429
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_retry_on_429() {
    let (base_url, state) = start_mock(MockBehavior::FailThenSucceed {
        fail_status: 429,
        fail_count: 1,
        success_text: "Recovered!".into(),
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    let result = timeout(TEST_TIMEOUT, proc.process("test"))
        .await
        .expect("timed out")
        .expect("should succeed after retry");

    assert_eq!(result, "Recovered!");
    // Should have made 2 requests: 1 failed + 1 success
    assert_eq!(state.request_count.load(Ordering::SeqCst), 2);
}

// ═══════════════════════════════════════════════════════════════
//  6. Retry on 500
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_retry_on_500() {
    let (base_url, state) = start_mock(MockBehavior::FailThenSucceed {
        fail_status: 500,
        fail_count: 2,
        success_text: "Server recovered!".into(),
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    let result = timeout(TEST_TIMEOUT, proc.process("test"))
        .await
        .expect("timed out")
        .expect("should succeed after retries");

    assert_eq!(result, "Server recovered!");
    // 2 failures + 1 success = 3 requests
    assert_eq!(state.request_count.load(Ordering::SeqCst), 3);
}

// ═══════════════════════════════════════════════════════════════
//  7. Retry exhausted
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_retry_exhausted() {
    let (base_url, state) = start_mock(MockBehavior::FailThenSucceed {
        fail_status: 429,
        fail_count: 100, // more failures than retries
        success_text: "never reached".into(),
    })
    .await;

    let mut cfg = test_config(&base_url);
    cfg.max_retries = 1; // Only 1 retry → 2 total attempts

    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    let result = timeout(TEST_TIMEOUT, proc.process("test"))
        .await
        .expect("timed out");

    assert!(result.is_err(), "should fail after exhausting retries");
    let err = result.unwrap_err();
    // The error names how many attempts were made and why the last one
    // failed, so an operator can tell "retried and gave up" from "failed
    // once" without the upstream body.
    assert!(
        err.message.contains("2 attempts"),
        "error should say how many attempts were made: {}",
        err.message
    );
    assert!(
        err.message.contains("429") && err.message.contains("rate limited"),
        "error should name the status that exhausted the retries: {}",
        err.message
    );
    // 1 initial + 1 retry = 2 total
    assert_eq!(state.request_count.load(Ordering::SeqCst), 2);
}

// ═══════════════════════════════════════════════════════════════
//  8. Request timeout
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_request_timeout() {
    let (base_url, _state) = start_mock(MockBehavior::SlowResponse {
        delay_ms: 10_000, // 10 second delay
    })
    .await;

    let mut cfg = test_config(&base_url);
    cfg.timeout_secs = 1; // 1 second timeout
    cfg.max_retries = 0; // No retries

    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    let result = timeout(TEST_TIMEOUT, proc.process("test"))
        .await
        .expect("test timed out (outer)");

    assert!(result.is_err(), "should fail with timeout");
}

// ═══════════════════════════════════════════════════════════════
//  9. API error response
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_api_error_response() {
    let (base_url, _state) = start_mock(MockBehavior::AlwaysFail {
        status: 400,
        body: json!({
            "error": {
                "message": "Invalid model: nonexistent-model",
                "type": "invalid_request_error",
                "code": "model_not_found"
            }
        }),
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    let result = timeout(TEST_TIMEOUT, proc.process("test"))
        .await
        .expect("timed out");

    assert!(result.is_err());
    let err = result.unwrap_err();

    // The client-facing error states the status and its class, and nothing
    // else. The upstream body may quote the prompt, the API key or provider
    // internals, so it is log-only -- leaking it here is the regression.
    assert!(
        err.message.contains("400") && err.message.contains("client error"),
        "error should name the status and its class, got: {}",
        err.message
    );
    for leaked in [
        "Invalid model",
        "nonexistent-model",
        "invalid_request_error",
        "model_not_found",
    ] {
        assert!(
            !err.message.contains(leaked),
            "upstream body must not reach the client, but {leaked:?} did: {}",
            err.message
        );
    }
}

// ═══════════════════════════════════════════════════════════════
//  10. Concurrent requests
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_concurrent_requests() {
    let (base_url, state) = start_mock(MockBehavior::Streaming {
        tokens: vec!["OK".into(), ".".into()],
    })
    .await;

    let cfg = test_config(&base_url);
    let proc = Arc::new(
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap(),
    );

    let mut handles = Vec::new();
    for i in 0..10 {
        let p = Arc::clone(&proc);
        handles.push(tokio::spawn(async move {
            let cancel = CancellationToken::new();
            let (tx, mut rx) = mpsc::channel::<TextFragment>(32);
            p.process_stream(&format!("request {i}"), tx, cancel)
                .await
                .expect("concurrent stream failed");
            let mut text = String::new();
            while let Ok(frag) = rx.try_recv() {
                text.push_str(&frag.text);
            }
            text
        }));
    }

    let results = timeout(TEST_TIMEOUT, async {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.unwrap());
        }
        out
    })
    .await
    .expect("timed out");

    assert_eq!(results.len(), 10);
    for result in &results {
        assert!(
            result.contains("OK"),
            "each response should contain 'OK', got: {result}"
        );
    }
    // All 10 requests should have hit the server
    assert_eq!(state.request_count.load(Ordering::SeqCst), 10);
}

// ═══════════════════════════════════════════════════════════════
//  12. Audit events emitted
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_audit_events_emitted() {
    let (base_url, _state) = start_mock(MockBehavior::NonStreaming {
        text: "Audited response".into(),
    })
    .await;

    let cfg = test_config(&base_url);
    let (audit_tx, mut audit_rx) = mpsc::channel::<TraceEvent>(64);
    let proc = OpenAiCompatProcessor::new(
        cfg,
        Some(audit_tx),
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let _result = timeout(TEST_TIMEOUT, proc.process("test audit"))
        .await
        .expect("timed out")
        .expect("process failed");

    // Collect audit events
    let mut events = Vec::new();
    while let Ok(ev) = audit_rx.try_recv() {
        events.push(ev);
    }

    // Expect at least: llm_request + llm_detail
    let event_types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(
        event_types.contains(&"llm_request"),
        "should emit llm_request event, got: {event_types:?}"
    );
    assert!(
        event_types.contains(&"llm_detail"),
        "should emit llm_detail event, got: {event_types:?}"
    );

    // Verify llm_request payload
    let req_event = events
        .iter()
        .find(|e| e.event_type == "llm_request")
        .unwrap();
    assert_eq!(req_event.payload["model"], "mock-model");
    assert_eq!(req_event.payload["stream"], false);

    // Verify llm_detail payload has elapsed_ms
    let timing_event = events
        .iter()
        .find(|e| e.event_type == "llm_detail")
        .unwrap();
    assert!(
        timing_event.payload.get("elapsed_ms").is_some(),
        "timing event should have elapsed_ms"
    );
    assert!(
        timing_event.payload.get("usage").is_some(),
        "timing event should have usage stats"
    );
}

#[tokio::test]
async fn test_audit_events_emitted_streaming() {
    let (base_url, _state) = start_mock(MockBehavior::Streaming {
        tokens: vec!["Hello".into(), ".".into()],
    })
    .await;

    let cfg = test_config(&base_url);
    let (audit_tx, mut audit_rx) = mpsc::channel::<TraceEvent>(64);
    let proc = OpenAiCompatProcessor::new(
        cfg,
        Some(audit_tx),
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let cancel = CancellationToken::new();
    let _fragments = timeout(TEST_TIMEOUT, collect_fragments(&proc, "test", cancel))
        .await
        .expect("timed out")
        .expect("stream failed");

    let mut events = Vec::new();
    while let Ok(ev) = audit_rx.try_recv() {
        events.push(ev);
    }

    let event_types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(
        event_types.contains(&"llm_request"),
        "streaming should emit llm_request, got: {event_types:?}"
    );
    assert!(
        event_types.contains(&"llm_detail"),
        "streaming should emit llm_detail, got: {event_types:?}"
    );

    // Streaming timing should include first_token_ms
    let timing = events
        .iter()
        .find(|e| e.event_type == "llm_detail")
        .unwrap();
    assert_eq!(timing.payload["stream"], true);
}

// ═══════════════════════════════════════════════════════════════
//  13. Multi-turn conversation history via process_stream
// ═══════════════════════════════════════════════════════════════

/// Verifies that calling `process_stream` multiple times on the same
/// `OpenAiCompatProcessor` accumulates conversation history.
#[tokio::test]
async fn test_process_stream_multi_turn_history() {
    let (base_url, state) = start_mock(MockBehavior::Streaming {
        tokens: vec!["I am fine.".into()],
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    // Turn 1
    let cancel1 = CancellationToken::new();
    let _frags1 = timeout(TEST_TIMEOUT, collect_fragments(&proc, "Hello!", cancel1))
        .await
        .expect("timed out")
        .expect("turn 1 failed");

    // Turn 2
    let cancel2 = CancellationToken::new();
    let _frags2 = timeout(TEST_TIMEOUT, collect_fragments(&proc, "Weather?", cancel2))
        .await
        .expect("timed out")
        .expect("turn 2 failed");

    // Turn 3
    let cancel3 = CancellationToken::new();
    let _frags3 = timeout(TEST_TIMEOUT, collect_fragments(&proc, "Joke?", cancel3))
        .await
        .expect("timed out")
        .expect("turn 3 failed");

    assert_eq!(state.request_count.load(Ordering::SeqCst), 3);

    // messages_count should grow: 2 → 4 → 6
    let bodies = state.captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "should have captured 3 request bodies");

    let msgs1 = bodies[0]["messages"].as_array().expect("messages array");
    let msgs2 = bodies[1]["messages"].as_array().expect("messages array");
    let msgs3 = bodies[2]["messages"].as_array().expect("messages array");

    assert_eq!(
        msgs1.len(),
        2,
        "turn 1: expected 2 messages, got {}",
        msgs1.len()
    );
    assert_eq!(
        msgs2.len(),
        4,
        "turn 2: expected 4, got {}; msgs: {:#?}",
        msgs2.len(),
        msgs2
    );
    assert_eq!(
        msgs3.len(),
        6,
        "turn 3: expected 6, got {}; msgs: {:#?}",
        msgs3.len(),
        msgs3
    );
}

// ═══════════════════════════════════════════════════════════════
//  14. Multi-turn conversation history via process (batch)
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_process_multi_turn_history() {
    let (base_url, state) = start_mock(MockBehavior::NonStreaming {
        text: "I am fine.".into(),
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    for (i, input) in ["Hello!", "Weather?", "Joke?"].iter().enumerate() {
        let result = timeout(TEST_TIMEOUT, proc.process(input))
            .await
            .expect("timed out")
            .unwrap_or_else(|e| panic!("turn {} failed: {}", i + 1, e));
        assert_eq!(result, "I am fine.");
    }

    assert_eq!(state.request_count.load(Ordering::SeqCst), 3);

    let bodies = state.captured_bodies.lock().unwrap();
    let msgs1 = bodies[0]["messages"].as_array().unwrap();
    let msgs2 = bodies[1]["messages"].as_array().unwrap();
    let msgs3 = bodies[2]["messages"].as_array().unwrap();

    assert_eq!(
        msgs1.len(),
        2,
        "batch turn 1: expected 2, got {}",
        msgs1.len()
    );
    assert_eq!(
        msgs2.len(),
        4,
        "batch turn 2: expected 4, got {}",
        msgs2.len()
    );
    assert_eq!(
        msgs3.len(),
        6,
        "batch turn 3: expected 6, got {}",
        msgs3.len()
    );
}

// ═══════════════════════════════════════════════════════════════
//  15. Multi-turn via run_chain_stream (simulates pipeline path)
// ═══════════════════════════════════════════════════════════════

/// Simulates the pipeline's usage: processors behind Arc<Vec<Box<dyn TextProcessor>>>,
/// the last (here the only) one streamed each turn, as the engine's chain
/// runner does -- verifies history accumulates.
#[tokio::test]
async fn test_run_chain_stream_multi_turn_history() {
    let (base_url, state) = start_mock(MockBehavior::Streaming {
        tokens: vec!["Response.".into()],
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    // Wrap in Arc<Vec<Box<dyn TextProcessor>>> — exact pipeline type
    let processors: std::sync::Arc<Vec<Box<dyn TextProcessor>>> =
        std::sync::Arc::new(vec![Box::new(proc)]);

    for (i, input) in ["Hello!", "Weather?", "Joke?"].iter().enumerate() {
        let procs = std::sync::Arc::clone(&processors);
        let (tx, mut rx) = mpsc::channel::<voice_pipes_sdk::text::TextFragment>(64);
        let cancel = CancellationToken::new();
        timeout(TEST_TIMEOUT, procs[0].process_stream(input, tx, cancel))
            .await
            .expect("timed out")
            .unwrap_or_else(|e| panic!("turn {} failed: {}", i + 1, e));
        // Drain fragments
        while rx.try_recv().is_ok() {}
    }

    assert_eq!(state.request_count.load(Ordering::SeqCst), 3);

    let bodies = state.captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "should have 3 request bodies");
    let msgs1 = bodies[0]["messages"].as_array().unwrap();
    let msgs2 = bodies[1]["messages"].as_array().unwrap();
    let msgs3 = bodies[2]["messages"].as_array().unwrap();

    assert_eq!(
        msgs1.len(),
        2,
        "chain turn 1: expected 2, got {}",
        msgs1.len()
    );
    assert_eq!(
        msgs2.len(),
        4,
        "chain turn 2: expected 4, got {}",
        msgs2.len()
    );
    assert_eq!(
        msgs3.len(),
        6,
        "chain turn 3: expected 6, got {}",
        msgs3.len()
    );
}

// ═══════════════════════════════════════════════════════════════
//  16. Multi-turn via tokio::spawn (simulates fire-and-forget runner path)
// ═══════════════════════════════════════════════════════════════

/// Simulates the runner's fire-and-forget pattern: each utterance spawns a
/// tokio task that streams the processor.  Verifies history still accumulates
/// across spawned tasks sharing the same Arc<Vec<Box<dyn TextProcessor>>>.
#[tokio::test]
async fn test_spawn_multi_turn_history() {
    let (base_url, state) = start_mock(MockBehavior::Streaming {
        tokens: vec!["Response.".into()],
    })
    .await;

    let cfg = test_config(&base_url);
    let proc =
        OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
            .unwrap();

    let processors: std::sync::Arc<Vec<Box<dyn TextProcessor>>> =
        std::sync::Arc::new(vec![Box::new(proc)]);

    for (i, input) in ["Hello!", "Weather?", "Joke?"].iter().enumerate() {
        let procs = std::sync::Arc::clone(&processors);
        let input_owned = input.to_string();
        // Spawn and AWAIT — like the runner, which spawns fire-and-forget
        // but we await to ensure sequential execution (mimicking the
        // 4-8 second gap observed in production captures).
        let handle = tokio::spawn(async move {
            let (tx, mut rx) = mpsc::channel::<voice_pipes_sdk::text::TextFragment>(64);
            let cancel = CancellationToken::new();
            procs[0]
                .process_stream(&input_owned, tx, cancel)
                .await
                .unwrap_or_else(|e| panic!("turn {} failed: {}", i + 1, e));
            // Drain fragments
            while rx.try_recv().is_ok() {}
        });
        timeout(TEST_TIMEOUT, handle)
            .await
            .expect("timed out")
            .expect("task panicked");
    }

    assert_eq!(state.request_count.load(Ordering::SeqCst), 3);

    let bodies = state.captured_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "should have 3 request bodies");
    let msgs1 = bodies[0]["messages"].as_array().unwrap();
    let msgs2 = bodies[1]["messages"].as_array().unwrap();
    let msgs3 = bodies[2]["messages"].as_array().unwrap();

    assert_eq!(
        msgs1.len(),
        2,
        "spawn turn 1: expected 2, got {}",
        msgs1.len()
    );
    assert_eq!(
        msgs2.len(),
        4,
        "spawn turn 2: expected 4, got {}",
        msgs2.len()
    );
    assert_eq!(
        msgs3.len(),
        6,
        "spawn turn 3: expected 6, got {}",
        msgs3.len()
    );
}

// ═══════════════════════════════════════════════════════════════
// 17. Client-side tools
// ═══════════════════════════════════════════════════════════════

mod client_tools {
    use super::*;
    use voice_pipes_sdk::agent::{
        ClientToolCall, ClientToolSpec, SessionEnded, SessionHandle, SessionSink,
    };

    /// Records the client-tool calls a processor reports, as the engine's
    /// agent channel would receive them.
    #[derive(Debug, Default)]
    struct Recorder(std::sync::Mutex<Vec<ClientToolCall>>);

    #[async_trait::async_trait]
    impl SessionSink for Recorder {
        async fn client_tool_call(&self, call: ClientToolCall) -> Result<(), SessionEnded> {
            self.0.lock().unwrap().push(call);
            Ok(())
        }
    }

    fn navigate_tool() -> ClientToolSpec {
        ClientToolSpec::new(
            "navigate",
            "Navigate the UI to a named screen.",
            json!({
                "type": "object",
                "properties": { "screen": { "type": "string" } },
                "required": ["screen"]
            }),
        )
    }

    /// The full loop: declared tools ride the request, a streamed
    /// `tool_calls` response surfaces as a client-tool call on the
    /// bound session handle, the turn ends prose-free, and the
    /// next turn's history carries the tool-call marker.
    #[tokio::test]
    async fn streaming_tool_call_surfaces_client_tool_event() {
        let (base_url, state) = start_mock(MockBehavior::StreamingToolCall {
            call_id: "call_nav_1".into(),
            name: "navigate".into(),
            arguments: r#"{"screen":"settings"}"#.into(),
        })
        .await;

        let cfg = test_config(&base_url);
        let proc =
            OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
                .unwrap();
        let recorder = Arc::new(Recorder::default());
        proc.bind_session(SessionHandle::new(Arc::<Recorder>::clone(&recorder), "s-1"));
        proc.configure_client_tools(vec![navigate_tool()]);

        let cancel = CancellationToken::new();
        let fragments = timeout(
            TEST_TIMEOUT,
            collect_fragments(&proc, "open the settings screen", cancel),
        )
        .await
        .expect("timed out")
        .expect("stream failed");

        // A tool-call turn carries no prose — nothing to speak.
        assert!(
            fragments.iter().all(|f| f.text.trim().is_empty()),
            "tool-call turn must be prose-free, got: {:?}",
            fragments.iter().map(|f| &f.text).collect::<Vec<_>>()
        );

        // The call surfaced on the session with the provider's id and fully
        // reassembled arguments.
        let calls = std::mem::take(&mut *recorder.0.lock().unwrap());
        assert_eq!(calls.len(), 1, "one client tool call: {calls:?}");
        assert_eq!(calls[0].call_id, "call_nav_1");
        assert_eq!(calls[0].name, "navigate");
        assert_eq!(calls[0].arguments["screen"], "settings");

        // The request advertised the declared tools.
        {
            let bodies = state.captured_bodies.lock().unwrap();
            let tools = bodies[0]["tools"].as_array().expect("tools in request");
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0]["type"], "function");
            assert_eq!(tools[0]["function"]["name"], "navigate");
            assert_eq!(tools[0]["function"]["parameters"]["required"][0], "screen");
        };

        // Turn 2: history must carry the user turn AND the tool-call
        // marker so the model remembers what it invoked.
        let cancel2 = CancellationToken::new();
        let _ = timeout(
            TEST_TIMEOUT,
            collect_fragments(&proc, "did you do it?", cancel2),
        )
        .await
        .expect("timed out")
        .expect("turn 2 failed");
        let bodies = state.captured_bodies.lock().unwrap();
        let msgs2 = bodies[1]["messages"].as_array().unwrap();
        let all_text: Vec<&str> = msgs2.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            all_text
                .iter()
                .any(|t| t.contains("open the settings screen")),
            "user turn missing from history: {all_text:?}"
        );
        assert!(
            all_text
                .iter()
                .any(|t| t.contains("[called client-side tool navigate(")),
            "tool-call marker missing from history: {all_text:?}"
        );
    }

    /// Without declared client tools, requests must not carry a `tools`
    /// field at all (token cost + provider strictness).
    #[tokio::test]
    async fn no_tools_field_when_none_declared() {
        let (base_url, state) = start_mock(MockBehavior::Streaming {
            tokens: vec!["Hi.".into()],
        })
        .await;
        let cfg = test_config(&base_url);
        let proc =
            OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
                .unwrap();
        let cancel = CancellationToken::new();
        let _ = timeout(TEST_TIMEOUT, collect_fragments(&proc, "hello", cancel))
            .await
            .expect("timed out")
            .expect("stream failed");
        let bodies = state.captured_bodies.lock().unwrap();
        assert!(
            bodies[0].get("tools").is_none(),
            "tools must be absent when none are declared"
        );
        assert!(bodies[0].get("tool_choice").is_none());
    }

    /// `tool_choice` config passes through when tools are declared.
    #[tokio::test]
    async fn tool_choice_passthrough() {
        let (base_url, state) = start_mock(MockBehavior::Streaming {
            tokens: vec!["Hi.".into()],
        })
        .await;
        let mut cfg = test_config(&base_url);
        cfg.tool_choice = Some("auto".into());
        let proc =
            OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default())
                .unwrap();
        proc.configure_client_tools(vec![navigate_tool()]);
        let cancel = CancellationToken::new();
        let _ = timeout(TEST_TIMEOUT, collect_fragments(&proc, "hello", cancel))
            .await
            .expect("timed out")
            .expect("stream failed");
        let bodies = state.captured_bodies.lock().unwrap();
        assert_eq!(bodies[0]["tool_choice"], "auto");
    }
}
