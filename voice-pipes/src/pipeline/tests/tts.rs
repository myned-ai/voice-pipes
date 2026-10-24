use super::*;

#[test]
fn test_compute_rtf() {
    assert_eq!(compute_rtf(1000.0, 2.0), 0.5); // 1s synthesis for 2s audio = 0.5 RTF
    assert_eq!(compute_rtf(2000.0, 1.0), 2.0); // 2s synthesis for 1s audio = 2.0 RTF
    assert_eq!(compute_rtf(1000.0, 0.0), 0.0); // 0s audio = 0.0 RTF
}

#[test]
fn test_fixed_chunk_samples() {
    let config = OutputChunkingConfig {
        enabled: true,
        frame_ms: 20,
    };
    // 20ms at 16000Hz = 320 samples
    assert_eq!(fixed_chunk_samples(Some(&config), 16000), Some(320));

    let config_disabled = OutputChunkingConfig {
        enabled: false,
        frame_ms: 20,
    };
    assert_eq!(fixed_chunk_samples(Some(&config_disabled), 16000), None);
    assert_eq!(fixed_chunk_samples(None, 16000), None);
}

// ── spawn_synthesis_task retry tests ─────────────────────

use crate::tts::TtsError;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU8, Ordering as AtomicOrdering};

/// Behaviour script for `FlakyMockTts`.
enum FlakyMode {
    /// Fail unconditionally on every call.
    AlwaysFail,
    /// Fail the first N calls then succeed by sending one frame.
    FailFirstN,
    /// Send one frame then fail on every call (tests no-retry-after-frame).
    FrameThenFail,
    /// Reject the request as the provider would an unknown voice.
    Rejected,
}

struct FlakyMockTts {
    remaining_failures: AtomicU8,
    calls: AtomicU8,
    sample_rate: u32,
    mode: FlakyMode,
}

impl FlakyMockTts {
    fn new(mode: FlakyMode, fail_first_n: u8) -> Self {
        Self {
            remaining_failures: AtomicU8::new(fail_first_n),
            calls: AtomicU8::new(0),
            sample_rate: 16_000,
            mode,
        }
    }
}

#[async_trait]
impl TtsProvider for FlakyMockTts {
    async fn synthesize(&self, _text: &str) -> Result<AudioFrame, TtsError> {
        unreachable!("retry tests never call batch synthesize")
    }

    async fn synthesize_stream(
        &self,
        _req: crate::tts::SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        if cancel.is_cancelled() {
            return Ok(());
        }
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        match self.mode {
            FlakyMode::AlwaysFail => Err(TtsError::InferenceFailed("always fails".into())),
            FlakyMode::FailFirstN => {
                let prev = self.remaining_failures.fetch_sub(1, AtomicOrdering::SeqCst);
                if prev > 0 {
                    Err(TtsError::InferenceFailed(format!(
                        "scripted failure, {prev} remaining"
                    )))
                } else {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    let _ = tx
                        .send(AudioFrame {
                            samples: vec![0i16; 160],
                            sample_rate: self.sample_rate,
                            channels: 1,
                        })
                        .await;
                    Ok(())
                }
            }
            FlakyMode::FrameThenFail => {
                let _ = tx
                    .send(AudioFrame {
                        samples: vec![0i16; 160],
                        sample_rate: self.sample_rate,
                        channels: 1,
                    })
                    .await;
                Err(TtsError::InferenceFailed("post-frame failure".into()))
            }
            FlakyMode::Rejected => Err(TtsError::from_http_status("Acme", 404, "voice not found")),
        }
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn name(&self) -> &'static str {
        "flaky-mock"
    }
}

#[tokio::test]
async fn spawn_synthesis_task_retries_until_success() {
    let tts: Arc<dyn TtsProvider> = Arc::new(FlakyMockTts::new(FlakyMode::FailFirstN, 2));
    let (handle, mut rx, words) = spawn_synthesis_task(
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        false,
        "hello".into(),
        None,
        3,
        tokio_util::sync::CancellationToken::new(),
    );
    assert!(words.is_none());

    let frame = rx.recv().await.expect("expected one audio frame");
    assert_eq!(frame.samples.len(), 160);

    let res = handle.await.expect("join ok");
    assert!(
        res.is_ok(),
        "expected Ok after successful retry, got {res:?}"
    );
}

#[tokio::test]
async fn spawn_synthesis_task_exhausts_retries_then_returns_err() {
    let tts: Arc<dyn TtsProvider> = Arc::new(FlakyMockTts::new(FlakyMode::AlwaysFail, 0));
    let start = std::time::Instant::now();
    let (handle, mut rx, _words) = spawn_synthesis_task(
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        false,
        "hello".into(),
        None,
        2,
        tokio_util::sync::CancellationToken::new(),
    );

    // No frames should be produced.
    assert!(rx.recv().await.is_none());

    let res = handle.await.expect("join ok");
    assert!(res.is_err(), "expected Err after exhausting retries");

    // Backoff is 100ms + 200ms = 300ms minimum across 2 retries.
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(250),
        "retries should have waited for backoff (elapsed {:?})",
        start.elapsed()
    );
}

/// A request the provider rejects fails the same way every time: it is
/// tried once and its error returned at once, without the backoff.
#[tokio::test]
async fn spawn_synthesis_task_does_not_retry_a_rejected_request() {
    let flaky = Arc::new(FlakyMockTts::new(FlakyMode::Rejected, 0));
    let tts: Arc<dyn TtsProvider> = Arc::<FlakyMockTts>::clone(&flaky);
    let start = std::time::Instant::now();
    let (handle, mut rx, _words) = spawn_synthesis_task(
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        false,
        "hello".into(),
        None,
        3,
        tokio_util::sync::CancellationToken::new(),
    );
    assert!(rx.recv().await.is_none());
    let res = handle.await.expect("join ok");
    assert!(matches!(res, Err(TtsError::ConfigError(_))), "{res:?}");
    assert_eq!(flaky.calls.load(AtomicOrdering::SeqCst), 1);
    assert!(
        start.elapsed() < std::time::Duration::from_millis(100),
        "no backoff before a permanent error ({:?})",
        start.elapsed()
    );
}

#[tokio::test]
async fn spawn_synthesis_task_no_retry_after_frame_forwarded() {
    let tts: Arc<dyn TtsProvider> = Arc::new(FlakyMockTts::new(FlakyMode::FrameThenFail, 0));
    let (handle, mut rx, _words) = spawn_synthesis_task(
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        false,
        "hello".into(),
        None,
        3,
        tokio_util::sync::CancellationToken::new(),
    );

    // First frame should arrive.
    let frame = rx.recv().await.expect("expected first audio frame");
    assert_eq!(frame.samples.len(), 160);

    // Channel should close (no retry => no second frame).
    assert!(rx.recv().await.is_none());

    let res = handle.await.expect("join ok");
    assert!(
        res.is_err(),
        "expected Err - retry must not run after audio was forwarded"
    );
}

#[cfg(test)]
mod tts_session_tests {
    use super::*;
    use crate::tts::{SynthesisHints, TtsSession};
    use tokio::sync::Mutex;

    struct TestSessionImpl {
        audio_pushed: Arc<Mutex<Vec<i16>>>,
        committed: Arc<Mutex<bool>>,
        cancelled: Arc<Mutex<bool>>,
    }

    #[async_trait]
    impl TtsSession for TestSessionImpl {
        async fn push_user_audio(&mut self, frame: &AudioFrame) -> Result<(), TtsError> {
            let mut guard = self.audio_pushed.lock().await;
            guard.extend_from_slice(&frame.samples);
            Ok(())
        }

        async fn commit_user_turn(&mut self) -> Result<(), TtsError> {
            let mut guard = self.committed.lock().await;
            *guard = true;
            Ok(())
        }

        async fn synthesize_turn(
            &mut self,
            text: &str,
            _hints: Option<&SynthesisHints>,
            tx: mpsc::Sender<AudioFrame>,
        ) -> Result<(), TtsError> {
            let is_comm = { *self.committed.lock().await };
            let count = if is_comm { text.len() * 10 } else { 0 };
            let frame = AudioFrame {
                samples: vec![7i16; count],
                sample_rate: 16000,
                channels: 1,
            };
            let _ = tx.send(frame).await;
            Ok(())
        }

        async fn cancel(&mut self) -> Result<(), TtsError> {
            let mut guard = self.cancelled.lock().await;
            *guard = true;
            Ok(())
        }

        async fn close(self: Box<Self>) -> Result<(), TtsError> {
            Ok(())
        }
    }

    struct MockTtsLocal {
        sample_rate: u32,
    }

    #[async_trait]
    impl TtsProvider for MockTtsLocal {
        async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
            let num_samples = text.len() * 100;
            Ok(AudioFrame {
                samples: vec![0i16; num_samples],
                sample_rate: self.sample_rate,
                channels: 1,
            })
        }
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn name(&self) -> &'static str {
            "mock-tts-local"
        }
    }

    #[tokio::test]
    async fn test_tts_session_lifecycle_flow_and_synthesis() {
        let audio_pushed = Arc::new(Mutex::new(Vec::new()));
        let committed = Arc::new(Mutex::new(false));
        let cancelled = Arc::new(Mutex::new(false));

        let session_impl = TestSessionImpl {
            audio_pushed: Arc::clone(&audio_pushed),
            committed: Arc::clone(&committed),
            cancelled: Arc::clone(&cancelled),
        };

        let tts_session: Arc<tokio::sync::Mutex<Option<Box<dyn TtsSession>>>> =
            Arc::new(tokio::sync::Mutex::new(Some(Box::new(session_impl))));

        // 1. Check push_user_audio behavior
        {
            let mut s_lock = tts_session.lock().await;
            let s = s_lock.as_mut().unwrap();
            s.push_user_audio(&AudioFrame {
                samples: vec![123; 100],
                sample_rate: 16000,
                channels: 1,
            })
            .await
            .unwrap();
        };
        assert_eq!(audio_pushed.lock().await.len(), 100);

        // 2. Check commit_user_turn boundary
        {
            let mut s_lock = tts_session.lock().await;
            let s = s_lock.as_mut().unwrap();
            s.commit_user_turn().await.unwrap();
        };
        assert!(*committed.lock().await);

        // 3. Dispatch synthesis via spawn_synthesis_task
        let dummy_tts = Arc::new(MockTtsLocal { sample_rate: 16000 });
        let (handle, mut frx, _wrx) = spawn_synthesis_task(
            dummy_tts,
            Arc::clone(&tts_session),
            true,
            "hello-session".to_string(),
            None,
            0,
            tokio_util::sync::CancellationToken::new(),
        );

        let result = handle.await.unwrap();
        assert!(result.is_ok());

        let frame = frx.recv().await.unwrap();
        assert_eq!(frame.samples.len(), 13 * 10);
        assert_eq!(frame.samples[0], 7);

        // 4. Validate cancel behavior
        {
            let mut s_lock = tts_session.lock().await;
            let s = s_lock.as_mut().unwrap();
            s.cancel().await.unwrap();
        };
        assert!(*cancelled.lock().await);
    }
}
