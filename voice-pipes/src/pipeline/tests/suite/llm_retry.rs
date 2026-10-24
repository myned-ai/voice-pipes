use crate::text::TextProcessorError;
use crate::text::{TextFragment, TextProcessor};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Test-only text processor that fails the first `fail_count` calls,
/// then succeeds with a single fragment.  Optionally emits one
/// fragment **before** failing on the first attempt to exercise the
/// "token already seen" gate.
struct FlakyMockLlm {
    attempts: Arc<AtomicU32>,
    fail_count: u32,
    emit_token_before_fail: bool,
    failure: crate::text::TextErrorKind,
}

impl FlakyMockLlm {
    fn new(fail_count: u32) -> Self {
        Self {
            attempts: Arc::default(),
            fail_count,
            emit_token_before_fail: false,
            failure: crate::text::TextErrorKind::Transient,
        }
    }

    fn with_token_before_fail(fail_count: u32) -> Self {
        Self {
            emit_token_before_fail: true,
            ..Self::new(fail_count)
        }
    }

    fn failing_with(fail_count: u32, failure: crate::text::TextErrorKind) -> Self {
        Self {
            failure,
            ..Self::new(fail_count)
        }
    }
}

#[async_trait::async_trait]
impl TextProcessor for FlakyMockLlm {
    fn name(&self) -> &'static str {
        "flaky-mock-llm"
    }

    async fn process(&self, _input: &str) -> Result<String, TextProcessorError> {
        unreachable!("retry path uses process_stream only")
    }

    async fn process_stream(
        &self,
        _input: &str,
        tx: mpsc::Sender<TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < self.fail_count {
            if self.emit_token_before_fail {
                let _ = tx.send(TextFragment::new("leaked-token")).await;
            }
            return Err(TextProcessorError::new(
                "flaky-mock-llm",
                format!("synthetic failure #{}", attempt + 1),
            )
            .with_kind(self.failure));
        }
        let _ = tx
            .send(
                TextFragment::new("ok")
                    .with_is_end_of_response(true)
                    .with_token_count(Some(1)),
            )
            .await;
        Ok(())
    }
}

fn make_processors(p: FlakyMockLlm) -> Arc<Vec<Box<dyn TextProcessor>>> {
    Arc::new(vec![Box::new(p) as Box<dyn TextProcessor>])
}

#[tokio::test]
async fn run_llm_chain_with_retry_succeeds_after_transient_failures() {
    let procs = make_processors(FlakyMockLlm::new(2));
    let (tx, mut rx) = mpsc::channel::<TextFragment>(16);
    let cancel = CancellationToken::new();

    let result = super::super::orchestrate::run_llm_chain_with_retry(
        procs,
        "hi".into(),
        tx,
        cancel,
        3,
        false,
    )
    .await;
    assert!(
        result.is_ok(),
        "expected success after retries, got {result:?}"
    );

    // Exactly one fragment should reach the caller (from the
    // successful third attempt -- the first two failed before
    // emitting anything).
    let frag = rx.recv().await.expect("expected one fragment");
    assert_eq!(frag.text, "ok");
    assert!(rx.recv().await.is_none(), "no further fragments expected");
}

#[tokio::test]
async fn run_llm_chain_with_retry_exhausts_then_returns_err() {
    // 5 failures available, only 2 retries permitted (3 attempts total).
    let procs = make_processors(FlakyMockLlm::new(5));
    let (tx, mut rx) = mpsc::channel::<TextFragment>(16);
    let cancel = CancellationToken::new();

    let result = super::super::orchestrate::run_llm_chain_with_retry(
        procs,
        "hi".into(),
        tx,
        cancel,
        2,
        false,
    )
    .await;
    let err = result.expect_err("expected failure after retries exhausted");
    assert!(err.message.contains("synthetic failure"));
    assert!(
        rx.recv().await.is_none(),
        "no fragments should reach the caller"
    );
}

#[tokio::test]
async fn run_llm_chain_with_retry_no_retry_after_token_forwarded() {
    // First attempt emits a token then fails; retry must NOT fire
    // because the client already received text.  Even though
    // max_retries=3, we get exactly one error back.
    let procs = make_processors(FlakyMockLlm::with_token_before_fail(1));
    let (tx, mut rx) = mpsc::channel::<TextFragment>(16);
    let cancel = CancellationToken::new();

    let result = super::super::orchestrate::run_llm_chain_with_retry(
        procs,
        "hi".into(),
        tx,
        cancel,
        3,
        false,
    )
    .await;
    assert!(
        result.is_err(),
        "expected error to propagate; retry skipped"
    );

    // The leaked token must have been forwarded once and only once.
    let frag = rx.recv().await.expect("expected leaked token");
    assert_eq!(frag.text, "leaked-token");
    assert!(rx.recv().await.is_none(), "no further fragments expected");
}

/// A permanent failure (a refused request, a bad configuration) fails the
/// same way again: it is returned at once, not retried.
#[tokio::test]
async fn a_permanent_failure_is_not_retried() {
    let llm = FlakyMockLlm::failing_with(5, crate::text::TextErrorKind::Permanent);
    let attempts = Arc::clone(&llm.attempts);
    let (tx, _rx) = mpsc::channel::<TextFragment>(16);
    let result = super::super::orchestrate::run_llm_chain_with_retry(
        make_processors(llm),
        "hi".into(),
        tx,
        CancellationToken::new(),
        3,
        false,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "one attempt, no retries"
    );
}

/// A rate limit is waited out for as long as the service asked, before
/// the retry.
#[tokio::test(start_paused = true)]
async fn a_rate_limit_waits_the_time_the_service_asked() {
    let llm = FlakyMockLlm::failing_with(
        1,
        crate::text::TextErrorKind::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(4)),
        },
    );
    let (tx, _rx) = mpsc::channel::<TextFragment>(16);
    let started = tokio::time::Instant::now();
    let result = super::super::orchestrate::run_llm_chain_with_retry(
        make_processors(llm),
        "hi".into(),
        tx,
        CancellationToken::new(),
        3,
        false,
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}

/// A rate limit longer than a turn can wait fails the turn at once: a
/// `Retry-After: 3600` must not hold the reply for an hour.
#[tokio::test(start_paused = true)]
async fn a_rate_limit_longer_than_a_turn_can_wait_fails_the_turn() {
    let llm = FlakyMockLlm::failing_with(
        1,
        crate::text::TextErrorKind::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(3600)),
        },
    );
    let attempts = Arc::clone(&llm.attempts);
    let (tx, _rx) = mpsc::channel::<TextFragment>(16);
    let started = tokio::time::Instant::now();
    let result = super::super::orchestrate::run_llm_chain_with_retry(
        make_processors(llm),
        "hi".into(),
        tx,
        CancellationToken::new(),
        3,
        false,
    )
    .await;
    let err = result.expect_err("the turn fails");
    assert!(
        matches!(err.kind, crate::text::TextErrorKind::RateLimited { .. }),
        "{err:?}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "not retried");
    assert_eq!(started.elapsed(), std::time::Duration::ZERO);
}
