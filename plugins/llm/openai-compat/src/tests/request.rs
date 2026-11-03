//! HTTP-level behaviour against a scripted in-process server: caps on
//! what the upstream may send, the idle read timeout, retry back-off
//! and cancellation, and what reaches the client-facing error.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::config::OpenAiCompatConfig;
use super::super::streaming::{MAX_RESPONSE_BYTES, MAX_SSE_LINE_BYTES};
use super::super::{OpenAiCompatProcessor, TextFragment};
use voice_pipes_sdk::TextProcessor;

/// One scripted HTTP response: a status line, then body parts written
/// after their delays, then `hold` before the socket closes. The same
/// script answers every connection.
#[derive(Clone)]
struct Script {
    status: u16,
    parts: Vec<(Duration, Vec<u8>)>,
    hold: Duration,
}

impl Script {
    fn ok(parts: Vec<(Duration, Vec<u8>)>) -> Self {
        Self {
            status: 200,
            parts,
            hold: Duration::ZERO,
        }
    }
}

fn sse_content(text: &str) -> Vec<u8> {
    format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"content":text},"finish_reason":null}]})
    )
    .into_bytes()
}

fn sse_finish() -> Vec<u8> {
    format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}})
    )
    .into_bytes()
}

/// Serve `script` on a fresh loopback port; returns the base URL and
/// the number of requests received so far.
async fn serve(script: Script) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            hits_srv.fetch_add(1, Ordering::SeqCst);
            let script = script.clone();
            tokio::spawn(async move {
                if voice_pipes_net::testing::read_request(&mut sock)
                    .await
                    .is_none()
                {
                    return;
                }
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                    script.status
                );
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for (delay, part) in &script.parts {
                    tokio::time::sleep(*delay).await;
                    if sock.write_all(part).await.is_err() {
                        return;
                    }
                    let _ = sock.flush().await;
                }
                tokio::time::sleep(script.hold).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/v1"), hits)
}

fn processor(base_url: &str, timeout_secs: u64, max_retries: u32) -> OpenAiCompatProcessor {
    let cfg = OpenAiCompatConfig {
        base_url: base_url.into(),
        model: "test-model".into(),
        api_key: voice_pipes_sdk::secrets::Secret::default(),
        system_prompt: "test".into(),
        max_tokens: 64,
        temperature: Some(0.5),
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        stop: None,
        timeout_secs,
        max_retries,
        response_format: None,
        tool_choice: None,
        extra_body: None,
        history_max_turns: 20,
    };
    OpenAiCompatProcessor::new(cfg, None, voice_pipes_sdk::TextChunkingConfig::default()).unwrap()
}

/// Run one streaming turn; returns the result and every fragment text.
async fn stream_turn(
    proc: &OpenAiCompatProcessor,
    cancel: CancellationToken,
) -> (Result<(), super::TextProcessorError>, Vec<String>) {
    let (tx, mut rx) = mpsc::channel::<TextFragment>(256);
    let collector = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(f) = rx.recv().await {
            out.push(f.text);
        }
        out
    });
    let res = proc.process_stream("hello", tx, cancel).await;
    (res, collector.await.unwrap())
}

#[tokio::test]
async fn a_stream_without_an_event_boundary_past_the_cap_ends_the_turn() {
    let blob = vec![b'x'; MAX_SSE_LINE_BYTES + 512 * 1024];
    let (url, _) = serve(Script::ok(vec![(Duration::ZERO, blob)])).await;
    let proc = processor(&url, 5, 0);
    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    let err = res.expect_err("an endless line must end the turn");
    assert!(err.message.contains("event boundary"), "{}", err.message);
}

#[tokio::test]
async fn the_turn_ends_at_finish_reason_without_waiting_for_done() {
    // The server reports finish + usage, then holds the socket open
    // without ever sending `[DONE]`. The read timeout is 1 s; ending at
    // the finish chunk means the turn completes well before that.
    let script = Script {
        status: 200,
        parts: vec![
            (Duration::ZERO, sse_content("Hello there. ")),
            (Duration::ZERO, sse_finish()),
        ],
        hold: Duration::from_secs(4),
    };
    let (url, _) = serve(script).await;
    let proc = processor(&url, 1, 0);
    let t0 = Instant::now();
    let (res, frags) = stream_turn(&proc, CancellationToken::new()).await;
    res.expect("turn completes");
    assert!(
        t0.elapsed() < Duration::from_millis(900),
        "waited {:?} for a stream that had already finished",
        t0.elapsed()
    );
    assert!(frags.concat().contains("Hello there."), "{frags:?}");
}

#[tokio::test]
async fn response_text_past_the_cap_ends_the_turn_with_what_was_generated() {
    let piece = "y".repeat(4096);
    let mut parts = Vec::new();
    for _ in 0..(MAX_RESPONSE_BYTES / 4096 + 8) {
        parts.push((Duration::ZERO, sse_content(&piece)));
    }
    let (url, _) = serve(Script::ok(parts)).await;
    let proc = processor(&url, 5, 0);
    let (res, frags) = stream_turn(&proc, CancellationToken::new()).await;
    res.expect("capped turn still completes");
    let total: usize = frags.iter().map(String::len).sum();
    assert!(total <= MAX_RESPONSE_BYTES, "retained {total} bytes");
    assert!(total > 0);
}

#[tokio::test]
async fn upstream_error_bodies_stay_out_of_the_client_error() {
    let body = br#"{"error":{"message":"invalid api key sk-super-secret-value","type":"auth"}}"#;
    let script = Script {
        status: 401,
        parts: vec![(Duration::ZERO, body.to_vec())],
        hold: Duration::ZERO,
    };
    let (url, hits) = serve(script).await;
    let proc = processor(&url, 5, 3);
    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    let err = res.expect_err("401 is an error");
    assert!(err.message.contains("401"), "{}", err.message);
    assert!(!err.message.contains("sk-super-secret"), "{}", err.message);
    assert!(!err.message.contains("invalid api key"), "{}", err.message);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "4xx is not retried");
}

#[tokio::test]
async fn a_slow_but_live_stream_survives_the_read_timeout_and_a_stall_does_not() {
    // Four parts 400 ms apart: 1.6 s total, more than the 1 s read
    // timeout, but never more than 1 s between reads.
    let step = Duration::from_millis(400);
    let script = Script {
        status: 200,
        parts: vec![
            (step, sse_content("One. ")),
            (step, sse_content("Two. ")),
            (step, sse_content("Three. ")),
            (step, sse_finish()),
        ],
        hold: Duration::ZERO,
    };
    let (url, _) = serve(script).await;
    let proc = processor(&url, 1, 0);
    let (res, frags) = stream_turn(&proc, CancellationToken::new()).await;
    res.expect("steady stream survives");
    assert!(frags.concat().contains("Three."), "{frags:?}");

    // One part, then silence past the read timeout.
    let script = Script {
        status: 200,
        parts: vec![
            (Duration::ZERO, sse_content("Start. ")),
            (Duration::from_millis(2500), sse_finish()),
        ],
        hold: Duration::ZERO,
    };
    let (url, _) = serve(script).await;
    let proc = processor(&url, 1, 0);
    let t0 = Instant::now();
    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    let err = res.expect_err("stalled stream is cut");
    assert!(err.message.contains("no data"), "{}", err.message);
    assert!(
        t0.elapsed() < Duration::from_millis(2200),
        "{:?}",
        t0.elapsed()
    );
}

/// A stream that dies after part of the reply was streamed keeps the
/// heard prefix in history; a stream that dies before any content
/// leaves history untouched.
#[tokio::test]
async fn mid_stream_failure_keeps_the_heard_prefix_in_history() {
    let script = Script {
        status: 200,
        parts: vec![
            (Duration::ZERO, sse_content("Once upon a time. ")),
            (Duration::from_millis(2500), sse_finish()),
        ],
        hold: Duration::ZERO,
    };
    let (url, _) = serve(script).await;
    let proc = processor(&url, 1, 0);
    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    res.expect_err("stalled stream fails the turn");
    {
        let history = proc.history.lock();
        assert_eq!(history.len(), 2, "{history:?}");
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[1].role, "assistant");
        assert_eq!(history[1].content, "Once upon a time. ");
    };

    let script = Script {
        status: 200,
        parts: vec![(Duration::from_millis(2500), sse_content("late"))],
        hold: Duration::ZERO,
    };
    let (url, _) = serve(script).await;
    let proc = processor(&url, 1, 0);
    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    res.expect_err("stalled stream fails the turn");
    assert!(
        proc.history.lock().is_empty(),
        "nothing heard, nothing kept"
    );
}

#[tokio::test]
async fn cancel_during_rate_limit_backoff_returns_promptly() {
    let script = Script {
        status: 429,
        parts: vec![(
            Duration::ZERO,
            b"{\"error\":{\"message\":\"slow down\"}}".to_vec(),
        )],
        hold: Duration::ZERO,
    };
    let (url, hits) = serve(script).await;
    // Five retries would mean 500 ms + 1 s + 2 s + 4 s + 8 s of back-off.
    let proc = processor(&url, 5, 5);
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        canceller.cancel();
    });
    let t0 = Instant::now();
    let (res, frags) = stream_turn(&proc, cancel).await;
    res.expect("a cancelled turn ends quietly");
    assert!(
        t0.elapsed() < Duration::from_millis(600),
        "cancel did not interrupt the back-off: {:?}",
        t0.elapsed()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "no retry after cancel");
    assert!(frags.last().is_some_and(String::is_empty), "{frags:?}");
}

/// A stream cancelled part-way (a barge-in) ends quietly and is not
/// counted as a successful turn or its latency, on either entry point.
#[tokio::test]
async fn a_cancelled_stream_is_not_recorded_as_a_success() {
    let script = Script::ok(vec![
        (Duration::ZERO, sse_content("Hello there. ")),
        (Duration::from_millis(300), sse_content("More words. ")),
        (Duration::ZERO, sse_finish()),
    ]);
    let (url, _) = serve(script).await;
    let proc = processor(&url, 5, 0);
    for context in [false, true] {
        let (tx, mut rx) = mpsc::channel::<TextFragment>(256);
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        let reader = tokio::spawn(async move {
            let first = rx.recv().await;
            canceller.cancel();
            while rx.recv().await.is_some() {}
            first.is_some()
        });
        let res = if context {
            proc.process_context_stream("hello", tx, cancel).await
        } else {
            proc.process_stream("hello", tx, cancel).await
        };
        res.expect("a cancelled turn ends quietly");
        assert!(reader.await.unwrap(), "text arrived before the cancel");
    }
    let telemetry = proc.core.telemetry();
    assert_eq!(telemetry.processed_total, 0);
    assert_eq!(telemetry.errors_total, 0);
}

#[tokio::test]
async fn transient_failures_retry_with_backoff_and_then_give_up() {
    let script = Script {
        status: 503,
        parts: vec![],
        hold: Duration::ZERO,
    };
    let (url, hits) = serve(script).await;
    let proc = processor(&url, 5, 1);
    let t0 = Instant::now();
    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    let err = res.expect_err("exhausted retries fail the turn");
    assert!(err.message.contains("503"), "{}", err.message);
    assert_eq!(hits.load(Ordering::SeqCst), 2, "one retry");
    // The 500 ms first backoff, jittered down to no less than half.
    assert!(
        t0.elapsed() >= Duration::from_millis(240),
        "{:?}",
        t0.elapsed()
    );
}

/// The regression: an error frame mid-stream was logged as a parse
/// failure with its payload and skipped, and the turn ended as a success.
/// It ends the turn with the vendor's error, keeping what was already sent.
#[tokio::test]
async fn an_error_frame_mid_stream_fails_the_turn() {
    let error = format!(
        "data: {}\n\n",
        serde_json::json!({"error":{"message":"The server had an error","type":"server_error"}})
    )
    .into_bytes();
    let (url, _) = serve(Script::ok(vec![
        (Duration::ZERO, sse_content("One. ")),
        (Duration::ZERO, sse_content("Two. ")),
        (Duration::ZERO, error),
    ]))
    .await;
    let proc = processor(&url, 5, 0);
    let (res, frags) = stream_turn(&proc, CancellationToken::new()).await;
    let err = res.expect_err("a vendor error fails the turn");
    assert!(
        err.message.contains("The server had an error"),
        "{}",
        err.message
    );
    assert!(frags.concat().contains("One."), "{frags:?}");
}

/// A rejection is logged by the kind of its body, never its words: the
/// vendor's message can quote the conversation.
#[test]
fn a_rejection_body_is_described_by_its_kind() {
    let api = br#"{"error":{"message":"bad input: tell me about my diagnosis","type":"invalid_request_error"}}"#;
    assert_eq!(super::rejection_body_kind(api), "api error");
    assert_eq!(
        super::rejection_body_kind(br#"{"detail":"x"}"#),
        "other JSON"
    );
    assert_eq!(super::rejection_body_kind(b"<html>502</html>"), "not JSON");
    assert_eq!(super::rejection_body_kind(b""), "empty");
}

// ── Retry-After ─────────────────────────────────────────────

/// Answer the first request with 429 and `Retry-After: <retry_after>`,
/// every later one with a short streamed reply; returns the base URL and
/// the number of requests received so far.
async fn serve_rate_limited_once(retry_after: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let n = hits_srv.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if voice_pipes_net::testing::read_request(&mut sock)
                    .await
                    .is_none()
                {
                    return;
                }
                let response = if n == 0 {
                    let body = b"{\"error\":{\"message\":\"slow down\"}}";
                    let mut r = format!(
                        "HTTP/1.1 429 X\r\nRetry-After: {retry_after}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(body);
                    r
                } else {
                    let mut r = b"HTTP/1.1 200 X\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n".to_vec();
                    r.extend(sse_content("Fine. "));
                    r.extend(sse_finish());
                    r
                };
                let _ = sock.write_all(&response).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/v1"), hits)
}

/// A wait longer than the back-off cap is not shortened and retried: the
/// call ends at once, carrying what the server asked for.
#[tokio::test]
async fn a_retry_after_past_the_cap_ends_the_call_at_once() {
    let (url, hits) = serve_rate_limited_once("3600").await;
    let proc = processor(&url, 5, 3);
    let t0 = Instant::now();
    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    let err = res.expect_err("rate limited for an hour");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one attempt");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "it waited: {:?}",
        t0.elapsed()
    );
    assert_eq!(
        err.kind,
        voice_pipes_sdk::text::TextErrorKind::RateLimited {
            retry_after: Some(Duration::from_secs(3600))
        }
    );
}

/// A wait inside the cap is honoured, and the retry then succeeds.
#[tokio::test]
async fn a_short_retry_after_is_waited_and_retried() {
    let (url, hits) = serve_rate_limited_once("2").await;
    let proc = processor(&url, 5, 3);
    let t0 = Instant::now();
    let (res, frags) = stream_turn(&proc, CancellationToken::new()).await;
    res.expect("the retry succeeds");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(
        t0.elapsed() >= Duration::from_secs(2),
        "the server's wait was cut short: {:?}",
        t0.elapsed()
    );
    assert!(frags.concat().contains("Fine."), "{frags:?}");
}

#[tokio::test]
async fn empty_role_delta_does_not_stamp_first_token_ms() {
    let role_only = format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]})
    )
    .into_bytes();
    let script = Script::ok(vec![
        (Duration::ZERO, role_only),
        (Duration::from_millis(160), sse_content("Hello there. ")),
        (Duration::ZERO, sse_finish()),
    ]);
    let (url, _) = serve(script).await;
    let (audit_tx, mut audit_rx) = mpsc::channel(16);
    let cfg = OpenAiCompatConfig {
        base_url: url,
        model: "test-model".into(),
        api_key: voice_pipes_sdk::secrets::Secret::default(),
        system_prompt: "test".into(),
        max_tokens: 64,
        temperature: Some(0.5),
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        stop: None,
        timeout_secs: 5,
        max_retries: 0,
        response_format: None,
        tool_choice: None,
        extra_body: None,
        history_max_turns: 20,
    };
    let proc = OpenAiCompatProcessor::new(
        cfg,
        Some(audit_tx),
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (tx, mut rx) = mpsc::channel::<TextFragment>(16);
    proc.process_stream("hello", tx, CancellationToken::new())
        .await
        .expect("stream succeeds");

    let first_frag = rx.recv().await.expect("first fragment");
    let frag_ttft = first_frag
        .first_token_ms
        .expect("first fragment carries first_token_ms");
    assert!(
        frag_ttft >= 120.0,
        "empty role delta must not stamp first_token_ms early: got {frag_ttft:.1} ms"
    );

    let mut detail_ttft = None;
    while let Ok(ev) = audit_rx.try_recv() {
        if ev.event_type == "llm_detail" {
            detail_ttft = ev.payload["first_token_ms"].as_f64();
        }
    }
    let detail_ttft = detail_ttft.expect("llm_detail first_token_ms");
    assert!(
        detail_ttft >= 120.0,
        "llm_detail first_token_ms must reflect first non-empty content delta: got {detail_ttft:.1} ms"
    );
}

#[tokio::test]
async fn rate_limit_retry_wait_is_separated_from_connect_ms() {
    let (url, hits) = serve_rate_limited_once("1").await;
    let (audit_tx, mut audit_rx) = mpsc::channel(16);
    let cfg = OpenAiCompatConfig {
        base_url: url,
        model: "test-model".into(),
        api_key: voice_pipes_sdk::secrets::Secret::default(),
        system_prompt: "test".into(),
        max_tokens: 64,
        temperature: Some(0.5),
        top_p: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        stop: None,
        timeout_secs: 5,
        max_retries: 3,
        response_format: None,
        tool_choice: None,
        extra_body: None,
        history_max_turns: 20,
    };
    let proc = OpenAiCompatProcessor::new(
        cfg,
        Some(audit_tx),
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (res, _) = stream_turn(&proc, CancellationToken::new()).await;
    res.expect("retry succeeds");
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    let mut detail = None;
    while let Ok(ev) = audit_rx.try_recv() {
        if ev.event_type == "llm_detail" {
            detail = Some(ev.payload);
        }
    }
    let detail = detail.expect("llm_detail event");
    let connect_ms = detail["connect_ms"].as_f64().expect("connect_ms");
    let retry_wait_ms = detail["retry_wait_ms"].as_f64().expect("retry_wait_ms");
    let retries = detail["retries"].as_u64().expect("retries");
    assert_eq!(retries, 1);
    assert!(
        retry_wait_ms >= 900.0,
        "retry_wait_ms must capture the 429 retry sleep, got {retry_wait_ms:.1} ms"
    );
    assert!(
        connect_ms < 500.0,
        "connect_ms must measure the winning HTTP attempt, not retry backoff: got {connect_ms:.1} ms"
    );
}

#[tokio::test]
async fn metric_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::TextFragment>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        // First emit empty role frame
        tx.send(voice_pipes_sdk::TextFragment::new(""))
            .await
            .unwrap();

        let delay = std::time::Duration::from_millis(25);
        tokio::time::sleep(delay).await;

        let mut frag1 = voice_pipes_sdk::TextFragment::new("Hello");
        frag1.first_token_ms = Some(25.0);
        tx.send(frag1).await.unwrap();

        let mut frag2 = voice_pipes_sdk::TextFragment::new(" world");
        frag2.is_end_of_response = true;
        frag2.token_count = Some(2);
        tx.send(frag2).await.unwrap();
        Ok(())
    };

    let ttft = voice_pipes_sdk::testing::assert_llm_metrics_conformance(
        run,
        std::time::Duration::from_millis(25),
    )
    .await
    .unwrap();
    assert_eq!(ttft, 25.0);
}
