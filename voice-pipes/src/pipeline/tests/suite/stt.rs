use super::super::*;
use super::common::*;
use crate::audio::AudioFrame;
use crate::stt::SttProvider;
use crate::stt::{SttCapability, SttError, TranscriptionResult};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

/// When the old worker does not exit within `WORKER_JOIN_TIMEOUT`
/// the respawn path must abort it. Dropping the `JoinHandle` only detached
/// it, so every stuck respawn leaked a live provider stream (a paid
/// WebSocket in production) for the life of the process.
#[tokio::test(start_paused = true)]
async fn test_stt_task_aborts_stuck_worker_on_respawn() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, _text_rx) = mpsc::channel::<Utterance>(4);
    let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stt = Arc::new(HangingStreamStt {
        spawns: Arc::clone(&spawns),
        live: Arc::clone(&live),
    });
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let handle = tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    // Each chunk fails to reach the wedged worker and forces a respawn;
    // the join timeout and backoff elapse instantly under paused time.
    for _ in 0..3 {
        mic_tx
            .send(AudioChunk {
                samples: vec![0; 320],
                sample_rate: 16000,
                ..Default::default()
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }

    let spawned = spawns.load(std::sync::atomic::Ordering::SeqCst);
    let alive = live.load(std::sync::atomic::Ordering::SeqCst);
    assert!(spawned >= 3, "expected several respawns, got {spawned}");
    assert!(
        alive <= 1,
        "{alive} worker streams alive after {spawned} spawns: stuck workers were not aborted"
    );

    drop(mic_tx);
    let joined = tokio::time::timeout(std::time::Duration::from_secs(30), handle).await;
    assert!(
        joined.is_ok(),
        "stt_task did not exit after its input closed"
    );
    // Abort completion is observed by the awaiting teardown, but give the
    // runtime one more turn to run the dropped future's destructors.
    tokio::task::yield_now().await;
    assert_eq!(
        live.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "teardown must abort a worker that ignores cancel"
    );
}

/// The per-turn capture buffer is bounded: a turn that runs far past
/// the cap before its first final records at most the cap's worth of
/// (the newest) audio.
#[tokio::test]
async fn audit_buffer_for_one_turn_is_capped() {
    use crate::audit::test_support::{WRITE_TIMEOUT, wait_until};
    use crate::pipeline::stt::MAX_AUDIT_BUFFER_SECONDS;
    let dir = std::env::temp_dir().join(format!("vp_audit_cap_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(crate::audit::LocalFsAuditStore::new(dir.clone()));
    let sink = crate::audit::StreamingAuditSink::spawn(store, "cap-session".into(), 64);

    let rate = 16_000usize;
    let cap_samples = rate * MAX_AUDIT_BUFFER_SECONDS;
    // Finalise only after 1.5x the cap has streamed in.
    let stt = Arc::new(FinalizeAfterSamplesStt {
        finalize_after_samples: cap_samples + cap_samples / 2,
    });
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let handle = tokio::spawn(stt_task(
        mic_rx,
        text_tx,
        None,
        stt,
        td,
        Some(sink),
        None,
        0,
        None,
        None,
    ));

    let chunk_samples = rate; // one second per chunk
    for _ in 0..(MAX_AUDIT_BUFFER_SECONDS * 3 / 2 + 2) {
        mic_tx
            .send(AudioChunk {
                samples: vec![7; chunk_samples],
                sample_rate: rate as u32,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let utt = tokio::time::timeout(std::time::Duration::from_secs(10), text_rx.recv())
        .await
        .expect("turn commits")
        .expect("utterance");
    assert!(utt.text.contains("monologue"));
    drop(mic_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

    // Capture files are named by utterance id, and the first is FIRST_UTTERANCE_ID.
    let wav = dir.join("cap-session").join("001_mic_raw.wav");
    wait_until(WRITE_TIMEOUT, || wav.exists()).await;
    let bytes = std::fs::metadata(&wav).expect("mic capture written").len();
    let samples = bytes.saturating_sub(44) / 2;
    assert!(
        samples <= cap_samples as u64,
        "capture holds {samples} samples; cap is {cap_samples}"
    );
    assert!(samples >= (cap_samples / 2) as u64, "{samples}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A provider that fails on every stream must be respawned with backoff,
/// not in a hot loop (regression for the `AssemblyAI` 1008 reconnect hammer).
#[tokio::test]
async fn test_stt_task_worker_respawn_backs_off() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, _text_rx) = mpsc::channel::<Utterance>(4);

    let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stt = Arc::new(FailingStreamStt {
        spawns: Arc::clone(&spawns),
    });

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let handle = tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    // Backoff schedule after failures: 500ms, 1s, 2s, ... so ~1.3s of wall
    // time allows the initial spawn plus at most a few respawns. The old
    // zero-delay respawn produced hundreds in this window.
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    let count = spawns.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        count >= 2,
        "worker must be respawned after failure, got {count}"
    );
    assert!(
        count <= 5,
        "worker respawn must back off, got {count} spawns in 1.3s"
    );

    drop(mic_tx);
    handle.abort();
}

#[tokio::test]
async fn test_stt_task_end_to_end() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);

    let stt = Arc::new(MockStt {
        expected_rate: 16000,
        response: "hello world".into(),
    });

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    // Send an utterance (end-of-utterance chunk)
    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    let utterance = text_rx.recv().await.unwrap();
    assert_eq!(utterance.text, "hello world");
    assert_eq!(utterance.utterance_id, crate::transport::FIRST_UTTERANCE_ID);
}

/// STT provider that records the sample count of the final (`is_last`) chunk it
/// receives, and whose `needs_flush_silence` is configurable. Used to pin the
/// Mitigation: the ~500ms synthetic flush silence is appended *only* when
/// the provider opts in, so providers that opt out (local transducers that
/// hallucinate on a digital-zero cliff — Nemotron/Parakeet/Moonshine) never see
/// injected silence in their live stream.
struct FlushRecordingStt {
    needs_flush: bool,
    final_chunk_samples: Arc<std::sync::Mutex<Option<usize>>>,
}

#[async_trait]
impl SttProvider for FlushRecordingStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        unreachable!("streaming only")
    }

    async fn transcribe_stream(
        &self,
        _request: crate::stt::RecognitionRequest,
        mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        while let Some(chunk) = audio_rx.recv().await {
            if chunk.is_last {
                *self.final_chunk_samples.lock().unwrap() = Some(chunk.samples.len());
                let _ = tx
                    .send(TranscriptionResult::new("done", true).with_confidence(Some(0.99)))
                    .await;
            }
        }
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "flush-recording"
    }

    fn needs_flush_silence(&self) -> bool {
        self.needs_flush
    }

    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        std::collections::HashSet::from([SttCapability::Streaming])
    }
}

async fn final_chunk_len_for(needs_flush: bool) -> usize {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, _text_rx) = mpsc::channel::<Utterance>(4);
    let recorded = Arc::new(std::sync::Mutex::new(None));
    let stt = Arc::new(FlushRecordingStt {
        needs_flush,
        final_chunk_samples: Arc::clone(&recorded),
    });
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let handle = tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    // One final chunk carrying 320 real samples at the provider's rate.
    mic_tx
        .send(AudioChunk {
            samples: vec![100i16; 320],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();
    drop(mic_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;

    recorded.lock().unwrap().expect("final chunk was recorded")
}

/// A provider that opts out of the flush hack must receive its audio
/// verbatim — no 500ms of injected digital zeros that could bleed into the next
/// segment on a false endpoint (the cause of the transducer hallucinations
/// Nemotron/Parakeet documented).
#[tokio::test]
async fn test_stt_no_silence_injected_when_provider_opts_out() {
    assert_eq!(
        final_chunk_len_for(false).await,
        320,
        "opt-out provider must get its audio unmodified (no injected silence)"
    );
}

/// The flush is still applied for providers that request it (cloud streaming
/// providers that need trailing audio to flush their lookahead): 320 real
/// samples + 500ms of silence at 16 kHz (8000 samples).
#[tokio::test]
async fn test_stt_silence_injected_when_provider_opts_in() {
    assert_eq!(
        final_chunk_len_for(true).await,
        320 + 8000,
        "opt-in provider gets 500ms (8000 @ 16 kHz) of flush silence appended"
    );
}

#[tokio::test]
async fn test_stt_task_buffers_chunks() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);

    let stt = Arc::new(MockStt {
        expected_rate: 16000,
        response: "buffered".into(),
    });

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    // Send two non-final chunks then a final one
    for i in 0..3 {
        mic_tx
            .send(AudioChunk {
                samples: vec![1i16; 1000],
                sample_rate: 16000,
                channels: 1,
                is_end_of_utterance: i == 2,
                caption: None,
                utterance_id: 0,
                full_text: None,
                extras: None,
                last_speech_at: None,
                synth_duration: None,
            })
            .await
            .unwrap();
    }

    let utterance = text_rx.recv().await.unwrap();
    assert_eq!(utterance.text, "buffered");
    assert_eq!(utterance.utterance_id, crate::transport::FIRST_UTTERANCE_ID);
}

#[tokio::test]
async fn test_stt_task_skips_empty_transcription() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);

    let stt = Arc::new(MockStt {
        expected_rate: 16000,
        response: "   ".into(), // whitespace-only
    });

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 1000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    // Drop sender so recv will eventually return None
    drop(mic_tx);

    // Empty transcription should be skipped
    let result = tokio::time::timeout(std::time::Duration::from_millis(100), text_rx.recv()).await;

    assert!(result.is_err() || result.unwrap().is_none());
}

#[tokio::test]
async fn test_stt_task_flush_trick() {
    // Verify that the stt_task emits 500ms of zeros when it receives an end_of_utterance chunk.

    struct MockSttFlushVerifier;

    #[async_trait]
    impl SttProvider for MockSttFlushVerifier {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            unimplemented!()
        }

        async fn transcribe_stream(
            &self,
            _request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            _result_tx: mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            let mut received_last = false;
            let mut flush_size = 0;
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    received_last = true;
                    flush_size = chunk.samples.len();
                }
            }
            assert!(received_last, "Should receive a chunk with is_last=true");
            assert!(
                flush_size >= 8000,
                "Should have received at least 8000 samples of padding for 16kHz"
            );
            Ok(())
        }

        fn expected_sample_rate(&self) -> u32 {
            16000
        }

        fn name(&self) -> &'static str {
            "mock-stt-flush"
        }

        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, _text_rx) = mpsc::channel::<Utterance>(4);
    let stt: Arc<dyn SttProvider> = Arc::new(MockSttFlushVerifier {});
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));

    let stt_handle = tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    // Send a normal chunk followed by an end chunk.
    mic_tx
        .send(AudioChunk {
            samples: vec![1i16; 160],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: false,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    mic_tx
        .send(AudioChunk {
            samples: vec![1i16; 160], // Actual content
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    drop(mic_tx);

    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), stt_handle)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_stt_task_discards_short_utterance() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);

    // Mock STT returns a single letter -- typical noise artifact.
    let stt: Arc<dyn SttProvider> = Arc::new(MockStt {
        expected_rate: 16000,
        response: "L".into(),
    });

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    // min_utterance_chars = 2 -> "L" (1 char) should be discarded.
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 2, None, None,
    ));

    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    // Drop the sender so the stt_task will eventually exit.
    drop(mic_tx);

    // The short utterance should be discarded -- channel should close
    // without delivering anything.
    let result = tokio::time::timeout(std::time::Duration::from_millis(500), text_rx.recv()).await;

    if let Ok(Some(u)) = result {
        panic!("short utterance should be discarded, got: {:?}", u.text)
    }
}

/// A too-short utterance thrown away by the silence timer (the detector was
/// not yet satisfied when the result came in) is recorded once in the
/// capture, as on the path that decides at the result.
#[tokio::test]
async fn a_short_utterance_dropped_by_the_silence_timer_is_recorded_once() {
    use crate::audit::test_support::{WRITE_TIMEOUT, wait_until};
    let dir = std::env::temp_dir().join(format!("vp_drop_once_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(crate::audit::LocalFsAuditStore::new(dir.clone()));
    let sink = crate::audit::StreamingAuditSink::spawn(store, "drop-session".into(), 64);

    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, _text_rx) = mpsc::channel::<Utterance>(4);
    /// Answers "L" once, at the end of the utterance, and holds the stream
    /// open so the stage's silence timer is what decides.
    struct OneNoiseStt;
    #[async_trait]
    impl SttProvider for OneNoiseStt {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            unreachable!("streaming only")
        }
        async fn transcribe_stream(
            &self,
            _request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    break;
                }
            }
            let _ = tx.send(TranscriptionResult::new("L", true)).await;
            while audio_rx.recv().await.is_some() {}
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "one-noise"
        }
        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }
    let stt: Arc<dyn SttProvider> = Arc::new(OneNoiseStt);
    // Not complete when the result arrives; complete 300 ms later, on the
    // timer.
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(300));
    let handle = tokio::spawn(stt_task(
        mic_rx,
        text_tx,
        None,
        stt,
        td,
        Some(sink),
        None,
        2,
        None,
        None,
    ));
    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let events = dir.join("drop-session").join("pipeline_events.jsonl");
    wait_until(WRITE_TIMEOUT, || events.exists()).await;
    // Give a second, duplicate record time to land if there is one.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    drop(mic_tx);
    handle.abort();
    let text = std::fs::read_to_string(&events).expect("events written");
    let drops = text.lines().filter(|l| l.contains("too_short")).count();
    assert_eq!(drops, 1, "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_stt_task_passes_long_enough_utterance() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);

    let stt: Arc<dyn SttProvider> = Arc::new(MockStt {
        expected_rate: 16000,
        response: "Hello world".into(),
    });

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    // min_utterance_chars = 2 -> "Hello world" (11 chars) passes.
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 2, None, None,
    ));

    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    let utterance = text_rx.recv().await.unwrap();
    assert_eq!(utterance.text, "Hello world");
}

// ── STT timing regression tests ─────────────────────────────────────────────
//
// Regression guard for the bug where `utterance_audio_start` was not reset
// after a short-utterance discard, causing the next utterance's `stt_latency`
// to measure from the *previous* utterance's first audio chunk -- producing
// wildly inflated values (observed: 23 s for a 2-second phrase).

/// A mock STT that returns different responses on successive calls, simulating
/// a session where one utterance is noise/echo and the next is real speech.
struct MultiMockStt {
    expected_rate: u32,
    responses: Vec<String>,
    call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl SttProvider for MultiMockStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        unimplemented!("streaming only")
    }

    async fn transcribe_stream(
        &self,
        _request: crate::stt::RecognitionRequest,
        mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        // The stt_task creates one stream per session and sends all utterances
        // through it.  Emit one final result per end-of-utterance (is_last) chunk.
        while let Some(chunk) = audio_rx.recv().await {
            if chunk.is_last {
                let idx = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let text = self.responses.get(idx).cloned().unwrap_or_default();
                let _ = tx
                    .send(TranscriptionResult::new(text, true).with_confidence(Some(0.99)))
                    .await;
            }
        }
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        self.expected_rate
    }
    fn name(&self) -> &'static str {
        "multi-mock-stt"
    }
    fn capabilities(&self) -> std::collections::HashSet<crate::stt::SttCapability> {
        std::collections::HashSet::new()
    }
}

/// After a short utterance is discarded, `utterance_audio_start` must be
/// reset so the immediately following real utterance gets a fresh (small)
/// `stt_latency`.  Without the fix the second utterance's latency includes
/// the full elapsed time since the first chunk of the first (discarded)
/// utterance arrived.
#[tokio::test]
async fn test_stt_latency_not_inflated_after_discard() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(16);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);

    // First call -> short noise ("L", 1 char); second call -> real speech.
    let stt: Arc<dyn SttProvider> = Arc::new(MultiMockStt {
        expected_rate: 16000,
        responses: vec!["L".into(), "Hello world".into()],
        call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    // min_utterance_chars = 2 -> "L" discarded, "Hello world" passes.
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 2, None, None,
    ));

    // First utterance: short noise.
    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    // Wait for the discard to be processed before sending the next utterance.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Second utterance: real speech, sent shortly after.
    let before = std::time::Instant::now();
    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    let utt = tokio::time::timeout(std::time::Duration::from_secs(2), text_rx.recv())
        .await
        .expect("should receive utterance")
        .expect("channel should not be closed");

    assert_eq!(utt.text, "Hello world", "second utterance text");

    // The measured latency must reflect only the second utterance's audio, not
    // the cumulative time from the first chunk of the first utterance.
    let max_expected = before.elapsed() + std::time::Duration::from_millis(500);
    let latency = utt
        .stt_latency
        .expect("stt_latency should be Some after real utterance");
    assert!(
        latency <= max_expected,
        "stt_latency {latency:?} should be <= {max_expected:?} -- \
         if this fails the utterance_audio_start was not reset after the discard",
    );
}

/// An EOU sentinel arriving before any audio chunks must not set
/// `utterance_audio_start`, which would poison the following utterance's
/// latency measurement.  (Regression for removed edge-case in `stt_task`.)
#[tokio::test]
async fn test_stt_latency_not_inflated_after_empty_eou() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(16);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);

    let stt: Arc<dyn SttProvider> = Arc::new(MultiMockStt {
        expected_rate: 16000,
        // First stream sees no audio so returns empty; second returns real text.
        responses: vec![String::new(), "Hello world".into()],
        call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));

    // Send an EOU with NO prior audio chunks.
    mic_tx
        .send(AudioChunk {
            samples: vec![],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    // Allow the sentinel to be processed.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Now send a real utterance and record when its first audio arrives.
    let before = std::time::Instant::now();
    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .unwrap();

    let utt = tokio::time::timeout(std::time::Duration::from_secs(2), text_rx.recv())
        .await
        .expect("should receive utterance")
        .expect("channel should not be closed");

    assert_eq!(utt.text, "Hello world");

    let max_expected = before.elapsed() + std::time::Duration::from_millis(500);
    let latency = utt.stt_latency.expect("stt_latency should be Some");
    assert!(
        latency <= max_expected,
        "stt_latency {latency:?} should be <= {max_expected:?} -- \
         empty EOU sentinel must not set utterance_audio_start",
    );
}

#[tokio::test]
async fn test_stt_watchdog_timer_correctness() {
    tokio::time::pause();
    let pending_final_since = Some(tokio::time::Instant::now());
    let mut watchdog_fired = false;
    let mut silence_timer_ticks = 0;

    // Buggy implementation simulation
    for _ in 0..50 {
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_millis(500)), if pending_final_since.is_some() => {
                watchdog_fired = true;
            }
            () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                silence_timer_ticks += 1;
            }
        }
    }
    assert!(
        !watchdog_fired,
        "Bug confirmed: inline watchdog sleep never fires when silence timer ticks"
    );
    assert_eq!(silence_timer_ticks, 50);

    // Fixed implementation simulation
    let mut watchdog_fired_fixed = false;
    let mut silence_timer_ticks_fixed = 0;
    let watchdog_delay = tokio::time::sleep(std::time::Duration::from_millis(500));
    tokio::pin!(watchdog_delay);

    for _ in 0..50 {
        tokio::select! {
            () = &mut watchdog_delay, if pending_final_since.is_some() => {
                watchdog_fired_fixed = true;
            }
            () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                silence_timer_ticks_fixed += 1;
            }
        }
    }
    assert!(
        watchdog_fired_fixed,
        "Fixed: pinned watchdog sleep fires correctly"
    );
    assert_eq!(silence_timer_ticks_fixed, 4); // Stops after watchdog fires at 500ms
}

/// A streaming provider that answers the first audio chunk with a scripted
/// list of finals (text, segment index) and then stays open until its input
/// closes.
struct SegmentedStt {
    finals: Vec<(&'static str, u32)>,
    cumulative: bool,
}

#[async_trait]
impl SttProvider for SegmentedStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Err(SttError::NetworkError("batch not used".into()))
    }

    async fn transcribe_stream(
        &self,
        _request: crate::stt::RecognitionRequest,
        mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        if audio_rx.recv().await.is_none() {
            return Ok(());
        }
        for (text, segment_index) in &self.finals {
            let _ = tx
                .send(
                    TranscriptionResult::new((*text).to_string(), true)
                        .with_confidence(Some(0.9))
                        .with_segment_index(*segment_index),
                )
                .await;
        }
        while audio_rx.recv().await.is_some() {}
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "mock-segmented-stt"
    }

    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        let mut caps = std::collections::HashSet::new();
        if self.cumulative {
            caps.insert(SttCapability::CumulativeFinals);
        }
        caps
    }
}

async fn committed_text(stt: SegmentedStt) -> String {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);
    // Long enough for every scripted final to arrive before the detector
    // completes on the silence timer.
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(300));
    let handle = tokio::spawn(stt_task(
        mic_rx,
        text_tx,
        None,
        Arc::new(stt),
        td,
        None,
        None,
        0,
        None,
        None,
    ));
    mic_tx
        .send(AudioChunk {
            samples: vec![1000; 320],
            sample_rate: 16000,
            ..Default::default()
        })
        .await
        .unwrap();
    let utterance = tokio::time::timeout(std::time::Duration::from_secs(5), text_rx.recv())
        .await
        .expect("utterance timed out")
        .expect("utterance channel closed");
    drop(mic_tx);
    handle.abort();
    utterance.text
}

/// Two finals for two speech segments before the turn detector completes
/// commit as one utterance, not as the last segment alone.
#[tokio::test]
async fn stt_task_appends_finals_of_different_segments() {
    let text = committed_text(SegmentedStt {
        finals: vec![("I want to book a flight", 0), ("to Zagreb", 1)],
        cumulative: false,
    })
    .await;
    assert_eq!(text, "I want to book a flight to Zagreb");
}

/// A provider that re-sends the growing segment under the same segment
/// index keeps the latest version of that segment, and a new segment is
/// still appended.
#[tokio::test]
async fn stt_task_keeps_the_latest_cumulative_final() {
    let text = committed_text(SegmentedStt {
        finals: vec![("I want", 0), ("I want to book", 0), ("to Zagreb", 1)],
        cumulative: true,
    })
    .await;
    assert_eq!(text, "I want to book to Zagreb");
}

#[test]
fn merge_final_text_rules() {
    fn result(text: &str, segment_index: u32) -> TranscriptionResult {
        TranscriptionResult::new(text, true).with_segment_index(segment_index)
    }
    let merge = super::super::stt::merge_final_text;

    // Same segment, no capability, unrelated text: appended.
    let mut acc = String::new();
    merge(&mut acc, None, &result("one", 0), false);
    merge(&mut acc, Some(0), &result("two", 0), false);
    assert_eq!(acc, "one two");

    // A final that literally extends the accumulated text supersedes it
    // even without the capability.
    let mut acc = String::from("one two");
    merge(&mut acc, Some(0), &result("one two three", 0), false);
    assert_eq!(acc, "one two three");

    // With the capability, the same segment supersedes; a new one appends.
    let mut acc = String::from("hello");
    merge(&mut acc, Some(3), &result("goodbye", 3), true);
    assert_eq!(acc, "goodbye");
    merge(&mut acc, Some(3), &result("again", 4), true);
    assert_eq!(acc, "goodbye again");

    // Empty finals change nothing.
    merge(&mut acc, Some(4), &result("   ", 5), true);
    assert_eq!(acc, "goodbye again");
}

/// What the capture records as the turn's transcript is the text the turn
/// commits, whatever shape the recogniser delivers it in.
///
/// Recorded from the last final alone, a recogniser that finalises in pieces
/// left the capture holding the last piece while the model was sent all of
/// them: a session showed "Sirix Rifts is all about..." for a turn whose
/// prompt began "Some could have got hold of that part...". For a recogniser
/// that sends one final per turn the two are the same string, so nothing
/// about its captures changes.
#[test]
fn the_recorded_transcript_is_what_the_turn_commits() {
    fn result(text: &str, segment_index: u32) -> TranscriptionResult {
        TranscriptionResult::new(text, true).with_segment_index(segment_index)
    }
    let merge = super::super::stt::merge_final_text;

    // One final per turn (Parakeet): the recorded text is that final.
    let mut acc = String::new();
    merge(&mut acc, None, &result("Hello, how are you?", 0), false);
    assert_eq!(acc.trim(), "Hello, how are you?");

    // Several finals per turn (Moonshine, cloud recognisers): the recorded
    // text is all of them, which is also what the model receives.
    let mut acc = String::new();
    merge(
        &mut acc,
        None,
        &result("Some could have got hold of that part", 0),
        false,
    );
    merge(
        &mut acc,
        Some(0),
        &result("Sirix Rifts is all about as your prime suspect.", 1),
        false,
    );
    assert_eq!(
        acc.trim(),
        "Some could have got hold of that part Sirix Rifts is all about as your prime suspect."
    );
}

/// Every session kind numbers its first spoken utterance the same, and says
/// so in the handshake.
///
/// Cascade sessions counted from 0 and speech-to-speech ones from 1. A
/// client predicting the id of the turn about to start -- which it must, to
/// file `speech_end` before the server has announced the turn -- assumed 1,
/// so on every cascade session the first turn's speech reports landed on the
/// second turn. The health report then measured the second turn's response
/// time from the first turn's end of speech: 8.3 s reported for a turn that
/// took 1.8 s.
#[test]
fn the_first_utterance_id_is_one_value_and_the_handshake_states_it() {
    assert_eq!(crate::transport::FIRST_UTTERANCE_ID, 1);

    // A `session_ready` from a server that predates the field must still
    // deserialize, and to the same base.
    let legacy = serde_json::json!({
        "audio_format": "pcm_i16_le",
        "input_sample_rate": 16000,
        "output_sample_rate": 48000,
        "channels": 1
    });
    let cfg: crate::transport::NegotiatedConfig =
        serde_json::from_value(legacy).expect("legacy session_ready parses");
    assert_eq!(cfg.first_utterance_id, crate::transport::FIRST_UTTERANCE_ID);
    assert_eq!(
        cfg.text_utterance_id_base,
        crate::transport::TEXT_UTTERANCE_ID_BASE
    );

    // And a current one carries it on the wire, where a client can read it.
    let wire = serde_json::to_value(&cfg).unwrap();
    assert_eq!(wire["first_utterance_id"], 1);
    assert_eq!(wire["text_utterance_id_base"], 10_000);
}

/// The regression: an utterance that transcribed to nothing was recorded as
/// dropped but its audio stayed in the capture buffer, so the next
/// committed turn's microphone capture began with it.
#[tokio::test]
async fn a_dropped_empty_utterance_is_not_in_the_next_turns_capture() {
    use crate::audit::test_support::{WRITE_TIMEOUT, wait_until};
    let dir = std::env::temp_dir().join(format!("vp_drop_empty_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(crate::audit::LocalFsAuditStore::new(dir.clone()));
    let sink = crate::audit::StreamingAuditSink::spawn(store, "empty-session".into(), 64);

    /// Finalises each utterance: the first as nothing, the second as words.
    struct EmptyThenWords;
    #[async_trait]
    impl SttProvider for EmptyThenWords {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            unreachable!("streaming only")
        }
        async fn transcribe_stream(
            &self,
            _request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            let mut ends = 0;
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    let text = if ends == 0 { "" } else { "hello there" };
                    let _ = tx.send(TranscriptionResult::new(text, true)).await;
                    ends += 1;
                }
            }
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "empty-then-words"
        }
        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let handle = tokio::spawn(stt_task(
        mic_rx,
        text_tx,
        None,
        Arc::new(EmptyThenWords),
        td,
        Some(sink),
        None,
        0,
        None,
        None,
    ));
    for samples in [16_000usize, 8_000] {
        mic_tx
            .send(AudioChunk {
                samples: vec![5; samples],
                sample_rate: 16_000,
                channels: 1,
                is_end_of_utterance: true,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let utt = tokio::time::timeout(std::time::Duration::from_secs(10), text_rx.recv())
        .await
        .expect("turn commits")
        .expect("utterance");
    assert!(utt.text.contains("hello there"));
    drop(mic_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

    let wav = dir.join("empty-session").join("001_mic_raw.wav");
    wait_until(WRITE_TIMEOUT, || wav.exists()).await;
    let samples = std::fs::metadata(&wav)
        .expect("capture written")
        .len()
        .saturating_sub(44)
        / 2;
    assert_eq!(
        samples, 8_000,
        "the capture holds only the committed utterance"
    );
    // The dropped utterance's audio is kept too, under the reason it made
    // no turn, so a session the recogniser heard nothing in can be heard.
    let dropped = dir
        .join("empty-session")
        .join("001_dropped_empty_transcript_mic_raw.wav");
    wait_until(WRITE_TIMEOUT, || dropped.exists()).await;
    let dropped_samples = std::fs::metadata(&dropped)
        .expect("dropped audio written")
        .len()
        .saturating_sub(44)
        / 2;
    assert_eq!(dropped_samples, 16_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A recogniser that takes the audio and never answers: the turn is given
/// up after the bound, its audio kept under `stt_timeout`, the recogniser
/// restarted, and the next utterance is heard by the new one.
#[tokio::test(start_paused = true)]
async fn a_recogniser_that_never_answers_is_given_up_and_its_audio_kept() {
    use crate::audit::test_support::{WRITE_TIMEOUT, wait_until};
    use std::sync::atomic::{AtomicU32, Ordering};
    let dir = std::env::temp_dir().join(format!("vp_stt_timeout_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(crate::audit::LocalFsAuditStore::new(dir.clone()));
    let sink = crate::audit::StreamingAuditSink::spawn(store, "timeout-session".into(), 64);

    /// The first worker reads and says nothing until cancelled; every later
    /// one answers each utterance.
    struct DeafThenHearing {
        starts: AtomicU32,
        slot_waits: std::sync::Mutex<Vec<Option<std::time::Duration>>>,
    }
    #[async_trait]
    impl SttProvider for DeafThenHearing {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            unreachable!("streaming only")
        }
        async fn transcribe_stream(
            &self,
            request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<TranscriptionResult>,
            cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            self.slot_waits.lock().unwrap().push(request.slot_wait);
            let deaf = self.starts.fetch_add(1, Ordering::SeqCst) == 0;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return Ok(()),
                    chunk = audio_rx.recv() => match chunk {
                        None => return Ok(()),
                        Some(chunk) if chunk.is_last && !deaf => {
                            let _ = tx.send(TranscriptionResult::new("heard you", true)).await;
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "deaf-then-hearing"
        }
        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let provider = Arc::new(DeafThenHearing {
        starts: AtomicU32::new(0),
        slot_waits: std::sync::Mutex::new(Vec::new()),
    });
    let handle = tokio::spawn(stt_task(
        mic_rx,
        text_tx,
        None,
        Arc::clone(&provider) as Arc<dyn SttProvider>,
        td,
        Some(sink),
        None,
        0,
        None,
        None,
    ));
    let utterance = |samples: usize| AudioChunk {
        samples: vec![5; samples],
        sample_rate: 16_000,
        channels: 1,
        is_end_of_utterance: true,
        ..Default::default()
    };
    let first_sent = tokio::time::Instant::now();
    mic_tx.send(utterance(16_000)).await.unwrap();
    // A caller who keeps talking does not re-arm the clock: a second
    // unanswered utterance eight seconds in leaves the give-up at fifteen
    // seconds from the first, not twenty-three.
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    mic_tx.send(utterance(4_000)).await.unwrap();

    // Nothing comes back; the give-up bound passes (paused time advances
    // when nothing else can run) and the audio is kept under its reason.
    let dropped = dir
        .join("timeout-session")
        .join("001_dropped_stt_timeout_mic_raw.wav");
    wait_until(WRITE_TIMEOUT, || dropped.exists()).await;
    assert!(dropped.exists(), "the unanswered utterance's audio is kept");
    let given_up_after = first_sent.elapsed();
    assert!(
        given_up_after < std::time::Duration::from_secs(20),
        "given up {given_up_after:?} after the first unanswered utterance"
    );

    // The restarted recogniser hears the next utterance.
    mic_tx.send(utterance(8_000)).await.unwrap();
    let utt = tokio::time::timeout(std::time::Duration::from_secs(60), text_rx.recv())
        .await
        .expect("the next turn commits")
        .expect("utterance");
    assert_eq!(utt.text, "heard you");
    // The first stream took the provider's own wait; the one that replaced
    // the given-up worker asked to wait for the slot that worker's stalled
    // run may still hold.
    assert_eq!(
        *provider.slot_waits.lock().unwrap(),
        vec![None, Some(std::time::Duration::from_secs(60))]
    );
    drop(mic_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A turn detector hears that the turn ended when the stage commits it, so
/// a detector keeping one turn's audio starts the next turn empty; the
/// layered detector passes it on to each layer.
#[tokio::test]
async fn the_turn_detector_hears_when_a_turn_ends() {
    use crate::stt::turn::TurnDetector;
    struct Counting(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait]
    impl TurnDetector for Counting {
        async fn evaluate(
            &self,
            _result: &TranscriptionResult,
            _silence: std::time::Duration,
        ) -> crate::stt::turn::TurnSignal {
            crate::stt::turn::TurnSignal::likely_complete()
        }
        fn turn_ended(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn name(&self) -> &'static str {
            "counting"
        }
    }
    let ended = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let td: Arc<dyn TurnDetector> =
        Arc::new(crate::stt::turn::LayeredTurnDetector::new(vec![Box::new(
            Counting(Arc::clone(&ended)),
        )]));
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);
    /// Answers each utterance once, when it ends, and keeps its stream open.
    struct OncePerUtterance;
    #[async_trait]
    impl SttProvider for OncePerUtterance {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            unreachable!("streaming only")
        }
        async fn transcribe_stream(
            &self,
            _request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    let _ = tx.send(TranscriptionResult::new("Hello world", true)).await;
                }
            }
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "once-per-utterance"
        }
        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }
    let stt: Arc<dyn SttProvider> = Arc::new(OncePerUtterance);
    tokio::spawn(stt_task(
        mic_rx, text_tx, None, stt, td, None, None, 0, None, None,
    ));
    mic_tx
        .send(AudioChunk {
            samples: vec![0i16; 16000],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), text_rx.recv())
        .await
        .expect("turn commits")
        .expect("utterance");
    assert_eq!(ended.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Speech that makes no turn is announced on the session's event channel,
/// where the runner tells the client, and recorded by the session's
/// recorder from there, not by the stage as well.
#[tokio::test]
async fn a_dropped_utterance_is_announced_on_the_event_channel_and_recorded_once() {
    use crate::pipeline::events::PipelineEvent;
    let dir = std::env::temp_dir().join(format!("vp_drop_announced_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(crate::audit::LocalFsAuditStore::new(dir.clone()));
    let sink = crate::audit::StreamingAuditSink::spawn(store, "announced".into(), 64);

    /// Finalises the first two utterances as nothing, the third as words.
    struct EmptyTwiceThenWords;
    #[async_trait]
    impl SttProvider for EmptyTwiceThenWords {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            unreachable!("streaming only")
        }
        async fn transcribe_stream(
            &self,
            _request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            let mut ends = 0;
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    let text = if ends < 2 { "" } else { "hello there" };
                    let _ = tx.send(TranscriptionResult::new(text, true)).await;
                    ends += 1;
                }
            }
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "empty-twice-then-words"
        }
        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let (events_tx, mut events_rx) = tokio::sync::broadcast::channel::<PipelineEvent>(16);
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let handle = tokio::spawn(stt_task(
        mic_rx,
        text_tx,
        None,
        Arc::new(EmptyTwiceThenWords),
        td,
        Some(sink),
        Some(events_tx),
        0,
        None,
        None,
    ));
    for samples in [16_000usize, 12_000, 8_000] {
        mic_tx
            .send(AudioChunk {
                samples: vec![5; samples],
                sample_rate: 16_000,
                channels: 1,
                is_end_of_utterance: true,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let utt = tokio::time::timeout(std::time::Duration::from_secs(10), text_rx.recv())
        .await
        .expect("turn commits")
        .expect("utterance");
    assert!(utt.text.contains("hello there"));

    let mut announced = Vec::new();
    while let Ok(Ok(ev)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), events_rx.recv()).await
    {
        if let PipelineEvent::UtteranceDropped {
            utterance_id,
            reason,
            ..
        } = ev
        {
            announced.push((utterance_id, reason));
        }
        if announced.len() == 2 {
            break;
        }
    }
    assert_eq!(
        announced,
        vec![
            (1, "empty_transcript".to_string()),
            (1, "empty_transcript".to_string())
        ],
        "both drops announced under the id the speech would have become"
    );
    drop(mic_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

    // Recording the announcement is the session recorder's job: the stage
    // must not also write it, or the capture holds it twice.
    let events = dir.join("announced").join("pipeline_events.jsonl");
    // Give a record time to land if there is one.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    if let Ok(text) = std::fs::read_to_string(&events) {
        assert!(
            !text.contains("utterance_dropped"),
            "the stage wrote the drop it also announced: {text}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The id stays until a turn commits, so two utterances dropped in a row
/// are dropped under one id; the second's recording must not overwrite
/// the first's.
#[tokio::test]
async fn two_utterances_dropped_under_one_id_keep_both_recordings() {
    use crate::audit::test_support::{WRITE_TIMEOUT, wait_until};
    let dir = std::env::temp_dir().join(format!("vp_drop_twice_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(crate::audit::LocalFsAuditStore::new(dir.clone()));
    let sink = crate::audit::StreamingAuditSink::spawn(store, "twice".into(), 64);

    /// Finalises the first two utterances as nothing, the third as words.
    struct EmptyTwiceThenWords;
    #[async_trait]
    impl SttProvider for EmptyTwiceThenWords {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            unreachable!("streaming only")
        }
        async fn transcribe_stream(
            &self,
            _request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            let mut ends = 0;
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    let text = if ends < 2 { "" } else { "hello there" };
                    let _ = tx.send(TranscriptionResult::new(text, true)).await;
                    ends += 1;
                }
            }
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "empty-twice-then-words"
        }
        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (text_tx, mut text_rx) = mpsc::channel::<Utterance>(4);
    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let handle = tokio::spawn(stt_task(
        mic_rx,
        text_tx,
        None,
        Arc::new(EmptyTwiceThenWords),
        td,
        Some(sink),
        None,
        0,
        None,
        None,
    ));
    for samples in [16_000usize, 12_000, 8_000] {
        mic_tx
            .send(AudioChunk {
                samples: vec![5; samples],
                sample_rate: 16_000,
                channels: 1,
                is_end_of_utterance: true,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let utt = tokio::time::timeout(std::time::Duration::from_secs(10), text_rx.recv())
        .await
        .expect("turn commits")
        .expect("utterance");
    assert!(utt.text.contains("hello there"));
    drop(mic_tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

    let session = dir.join("twice");
    let first = session.join("001_dropped_empty_transcript_mic_raw.wav");
    let second = session.join("001_dropped_empty_transcript_2_mic_raw.wav");
    wait_until(WRITE_TIMEOUT, || first.exists() && second.exists()).await;
    let samples_in =
        |p: &std::path::Path| std::fs::metadata(p).unwrap().len().saturating_sub(44) / 2;
    assert_eq!(
        samples_in(&first),
        16_000,
        "the first dropped utterance's audio"
    );
    assert_eq!(
        samples_in(&second),
        12_000,
        "the second's, not overwriting the first"
    );
    let committed = session.join("001_mic_raw.wav");
    wait_until(WRITE_TIMEOUT, || committed.exists()).await;
    assert_eq!(
        samples_in(&committed),
        8_000,
        "the turn's own recording is separate"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
