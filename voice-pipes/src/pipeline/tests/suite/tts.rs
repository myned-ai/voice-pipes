use super::super::*;
use super::common::*;
use crate::audio::AudioFrame;
use crate::audit::StreamingAuditSink;
use crate::audit::store::LocalFsAuditStore;
use crate::config::OutputChunkingConfig;
use crate::text::TextFragment;
use crate::tts::WordBoundary;
use crate::tts::{TtsError, TtsProvider};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_tts_task_end_to_end() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let (evt_tx, err_flag) = test_event_channel();

    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx
        .send(TextFragment::new("hello").with_is_end_of_response(true))
        .await
        .unwrap();

    // First chunk: synthesized audio (not marked as end)
    let chunk = audio_rx.recv().await.unwrap();
    assert_eq!(chunk.sample_rate, 22050);
    assert_eq!(chunk.channels, 1);
    assert!(!chunk.is_end_of_utterance);
    assert_eq!(chunk.samples.len(), 500); // "hello" = 5 chars * 100

    // Metadata update chunk
    let meta = audio_rx.recv().await.unwrap();
    assert!(!meta.is_end_of_utterance);
    assert!(meta.samples.is_empty());

    // Third chunk: sentinel marking end-of-response
    let sentinel = audio_rx.recv().await.unwrap();
    assert!(sentinel.is_end_of_utterance);
    assert!(sentinel.samples.is_empty());
}

/// Stateful TTS session that emits far more frames per turn than the internal
/// 8-slot synthesis channel, used to reproduce the prefetch deadlock.
struct ManyFrameSession {
    frames_per_turn: usize,
}

#[async_trait]
impl crate::tts::TtsSession for ManyFrameSession {
    async fn push_user_audio(&mut self, _f: &AudioFrame) -> Result<(), crate::tts::TtsError> {
        Ok(())
    }
    async fn commit_user_turn(&mut self) -> Result<(), crate::tts::TtsError> {
        Ok(())
    }
    async fn synthesize_turn(
        &mut self,
        _text: &str,
        _hints: Option<&crate::tts::SynthesisHints>,
        tx: mpsc::Sender<AudioFrame>,
    ) -> Result<(), crate::tts::TtsError> {
        for _ in 0..self.frames_per_turn {
            if tx
                .send(AudioFrame {
                    samples: vec![5i16; 160],
                    sample_rate: 22050,
                    channels: 1,
                })
                .await
                .is_err()
            {
                break;
            }
        }
        Ok(())
    }
    async fn cancel(&mut self) -> Result<(), crate::tts::TtsError> {
        Ok(())
    }
    async fn close(self: Box<Self>) -> Result<(), crate::tts::TtsError> {
        Ok(())
    }
}

/// Regression test: with a stateful TTS session, a second fragment
/// arriving mid-synthesis must not deadlock. The synthesis task holds the
/// session lock while filling the 8-slot frame channel; the old prefetch arm
/// would `lock().await` that same mutex from the select loop, stopping it from
/// draining the channel -> mutual deadlock. The fix disables prefetch for
/// stateful sessions. A deadlock would hang past the timeout here.
#[tokio::test]
async fn test_tts_task_stateful_session_does_not_deadlock() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    // Small audio channel adds backpressure so the loop and synthesis pace each
    // other, matching the conditions under which the deadlock manifested.
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(4);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let (evt_tx, err_flag) = test_event_channel();
    let session: Arc<tokio::sync::Mutex<Option<Box<dyn crate::tts::TtsSession>>>> =
        Arc::new(tokio::sync::Mutex::new(Some(Box::new(ManyFrameSession {
            frames_per_turn: 30,
        }))));

    let handle = tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        session,
        None,
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    for (text, is_end) in [("one", false), ("two", false), ("three", true)] {
        frag_tx
            .send(TextFragment::new(text).with_is_end_of_response(is_end))
            .await
            .unwrap();
    }
    drop(frag_tx);

    let drain = async {
        let mut saw_audio = false;
        while let Some(chunk) = audio_rx.recv().await {
            if !chunk.samples.is_empty() {
                saw_audio = true;
            }
            if chunk.is_end_of_utterance {
                break;
            }
        }
        saw_audio
    };
    let saw_audio = tokio::time::timeout(std::time::Duration::from_secs(5), drain)
        .await
        .expect("tts_task deadlocked with a stateful session");
    assert!(
        saw_audio,
        "expected synthesized audio from the stateful session"
    );

    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

/// Regression test: `fragment_idx` must advance across fragments even
/// when audit is disabled (the normal production path). Previously the counter
/// was incremented inside the `if let Some(sink) = audit` block, so with audit
/// off every `CaptionMeta` carried `fragment_idx` = 0.
#[tokio::test]
async fn test_tts_task_fragment_idx_increments_without_audit() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(16);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let (evt_tx, err_flag) = test_event_channel();

    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        None, // audit disabled — the scenario regressed on
        evt_tx,
        err_flag,
        None,
    ));

    for (text, is_end) in [("hello", false), ("world", true)] {
        frag_tx
            .send(TextFragment::new(text).with_is_end_of_response(is_end))
            .await
            .unwrap();
    }

    // Collect the fragment_idx carried by each caption, keyed by caption text,
    // until the end-of-utterance sentinel.
    let mut idx_for_text: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    loop {
        let chunk = audio_rx.recv().await.expect("tts_task closed early");
        if let Some(caption) = &chunk.caption {
            idx_for_text.insert(caption.text.clone(), caption.fragment_idx);
        }
        if chunk.is_end_of_utterance {
            break;
        }
    }

    assert_eq!(
        idx_for_text.get("hello"),
        Some(&0),
        "first fragment should carry fragment_idx 0"
    );
    assert_eq!(
        idx_for_text.get("world"),
        Some(&1),
        "second fragment should carry fragment_idx 1 even with audit disabled"
    );
}

#[tokio::test]
async fn test_tts_task_skips_empty_fragment() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let (evt_tx, err_flag) = test_event_channel();

    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    // Send empty fragment followed by real one
    frag_tx.send(TextFragment::new("  ")).await.unwrap();
    frag_tx
        .send(TextFragment::new("real").with_is_end_of_response(true))
        .await
        .unwrap();

    let chunk = audio_rx.recv().await.unwrap();
    assert_eq!(chunk.samples.len(), 400); // "real" = 4 chars * 100
}

/// Regression test: when the LLM response ends on a sentence boundary,
/// the final `TextFragment` has empty text + `is_end_of_response=true`.
/// The `tts_task` must still emit a `tts_timing` audit event with the
/// accumulated metrics from previous non-empty fragments.
#[tokio::test]
async fn test_tts_task_emits_tts_timing_on_empty_final_fragment() {
    let dir = std::env::temp_dir().join("vp_tts_timing_audit_test");
    let _ = std::fs::remove_dir_all(&dir);

    let store: Arc<dyn crate::audit::store::AuditStore> =
        Arc::new(LocalFsAuditStore::new(dir.clone()));
    let session_id = "test-session";
    let audit = StreamingAuditSink::spawn(store, session_id.to_string(), 64);

    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let (evt_tx, err_flag) = test_event_channel();

    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        Some(audit),
        evt_tx,
        err_flag,
        None,
    ));

    // Fragment 1: real text (like a complete sentence from the LLM)
    frag_tx
        .send(TextFragment::new("Hello there."))
        .await
        .unwrap();

    // Fragment 2: empty final fragment (LLM ended on a sentence boundary)
    frag_tx
        .send(TextFragment::new(String::new()).with_is_end_of_response(true))
        .await
        .unwrap();

    // Drain audio output
    while let Some(c) = audio_rx.recv().await {
        if c.is_end_of_utterance {
            break;
        }
    }

    // The audit writer flushes on its own task; wait for the event rather
    // than for a fixed time, which a loaded machine can outlast.
    let events_path = dir.join(session_id).join("events.jsonl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let content = loop {
        let content = std::fs::read_to_string(&events_path).unwrap_or_default();
        if content.contains("\"tts_timing\"") || std::time::Instant::now() >= deadline {
            break content;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert!(events_path.exists(), "events.jsonl should exist");
    let tts_timing_events: Vec<&str> = content
        .lines()
        .filter(|line| line.contains("\"tts_timing\""))
        .collect();
    assert!(
        !tts_timing_events.is_empty(),
        "events.jsonl must contain a tts_timing event, got:\n{content}"
    );

    // Verify the payload has expected fields
    let evt: serde_json::Value = serde_json::from_str(tts_timing_events[0]).unwrap();
    let payload = &evt["payload"];
    assert!(payload["elapsed_ms"].as_f64().unwrap() >= 0.0);
    assert_eq!(payload["chars"].as_u64().unwrap(), 12); // "Hello there." = 12 chars
    assert!(payload["sample_rate"].as_u64().unwrap() > 0);

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_tts_task_resamples_to_output_rate() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });

    // Request resampling to 48000 Hz.
    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        Some(48000),
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx
        .send(TextFragment::new("hello").with_is_end_of_response(true))
        .await
        .unwrap();
    drop(frag_tx);

    // Collect all non-sentinel chunks.
    let mut total_samples = 0usize;
    loop {
        let chunk = audio_rx.recv().await.unwrap();
        assert_eq!(chunk.sample_rate, 48000, "output must be at target rate");
        if chunk.is_end_of_utterance {
            // Sentinel may carry flush tail samples.
            total_samples += chunk.samples.len();
            break;
        }
        total_samples += chunk.samples.len();
    }

    // Original: 500 samples at 22050.  Resampled ~= 500 * 48000/22050 ~= 1088.
    // Allow some tolerance for the windowed-sinc resampler.
    let expected = (500.0 * 48000.0 / 22050.0) as usize;
    let tolerance = expected / 10; // 10%
    assert!(
        total_samples.abs_diff(expected) < tolerance,
        "resampled count {total_samples} should be near {expected} (+/-{tolerance})"
    );
}

/// The output resampler is
/// created fresh per fragment, and `tts_task` returns on cancel, so a
/// fragment's resampled output cannot depend on an earlier fragment's residual
/// resampler state. Two identical consecutive fragments therefore resample to
/// the same number of samples. If the resampler were ever hoisted out of the
/// per-fragment loop (carrying fractional phase across fragments — an audible
/// pop at each boundary), the second fragment's count would drift and this
/// would fail.
#[tokio::test]
async fn test_tts_task_resampler_is_fresh_per_fragment() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(16);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        Some(48000), // force resampling 22050 -> 48000
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    // Two identical fragments back-to-back.
    for is_end in [false, true] {
        frag_tx
            .send(TextFragment::new("xxxx").with_is_end_of_response(is_end))
            .await
            .unwrap();
    }
    drop(frag_tx);

    // Bucket audio-sample counts per fragment. A metadata/sentinel chunk (empty
    // samples, but carrying a caption or the end flag) closes the current
    // fragment's bucket.
    let mut buckets: Vec<usize> = Vec::new();
    let mut cur = 0usize;
    loop {
        let chunk = audio_rx.recv().await.unwrap();
        assert_eq!(chunk.sample_rate, 48000, "output must be at target rate");
        let is_boundary =
            chunk.samples.is_empty() && (chunk.caption.is_some() || chunk.is_end_of_utterance);
        if is_boundary {
            buckets.push(cur);
            cur = 0;
            if chunk.is_end_of_utterance {
                break;
            }
        } else {
            cur += chunk.samples.len();
        }
    }

    assert!(
        buckets.len() >= 2,
        "expected at least two fragment buckets, got {buckets:?}"
    );
    assert_eq!(
        buckets[0], buckets[1],
        "identical fragments must resample identically — the per-fragment \
         resampler must start fresh (buckets: {buckets:?})"
    );
}

#[tokio::test]
async fn test_tts_task_no_resample_when_none() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });

    // No output_rate => no resampling.
    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx
        .send(TextFragment::new("hi").with_is_end_of_response(true))
        .await
        .unwrap();
    drop(frag_tx);

    let chunk = audio_rx.recv().await.unwrap();
    assert_eq!(chunk.sample_rate, 22050);
    assert_eq!(chunk.samples.len(), 200); // "hi" = 2 chars * 100

    let meta = audio_rx.recv().await.unwrap();
    assert!(!meta.is_end_of_utterance);
    assert!(meta.samples.is_empty());

    let sentinel = audio_rx.recv().await.unwrap();
    assert!(sentinel.is_end_of_utterance);
    assert!(sentinel.samples.is_empty(), "no resampler => no tail");
}

#[tokio::test]
async fn test_tts_task_fixed_chunking_splits_large_frames() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let chunking = Some(OutputChunkingConfig {
        enabled: true,
        frame_ms: 10,
    });
    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        chunking,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx
        .send(TextFragment::new("hello").with_is_end_of_response(true))
        .await
        .unwrap();

    let first = audio_rx.recv().await.unwrap();
    let second = audio_rx.recv().await.unwrap();
    let third = audio_rx.recv().await.unwrap();
    let meta = audio_rx.recv().await.unwrap();
    let sentinel = audio_rx.recv().await.unwrap();

    assert_eq!(first.samples.len(), 220);
    assert!(
        first.caption.is_some(),
        "first emitted rechunked chunk keeps caption metadata"
    );
    assert_eq!(second.samples.len(), 220);
    assert!(second.caption.is_none());
    assert_eq!(third.samples.len(), 60);
    assert!(third.caption.is_none());
    assert!(!meta.is_end_of_utterance);
    assert!(meta.samples.is_empty());
    assert!(sentinel.is_end_of_utterance);
    assert!(sentinel.samples.is_empty());
}

#[tokio::test]
async fn test_tts_task_fixed_chunking_preserves_fragment_boundaries() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let chunking = Some(OutputChunkingConfig {
        enabled: true,
        frame_ms: 13,
    });
    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        chunking,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx.send(TextFragment::new("hi")).await.unwrap();
    frag_tx
        .send(TextFragment::new("ok").with_is_end_of_response(true))
        .await
        .unwrap();

    let first = audio_rx.recv().await.unwrap();
    let meta1 = audio_rx.recv().await.unwrap();
    let second = audio_rx.recv().await.unwrap();
    let meta2 = audio_rx.recv().await.unwrap();
    let sentinel = audio_rx.recv().await.unwrap();

    assert_eq!(
        first.samples.len(),
        200,
        "first fragment should flush as a partial chunk at fragment boundary"
    );
    assert_eq!(first.caption.as_ref().map(|c| c.text.as_str()), Some("hi"));
    assert!(meta1.samples.is_empty());
    assert_eq!(
        second.samples.len(),
        200,
        "second fragment should not be merged with the previous fragment"
    );
    assert_eq!(second.caption.as_ref().map(|c| c.text.as_str()), Some("ok"));
    assert!(meta2.samples.is_empty());
    assert!(sentinel.is_end_of_utterance);
}

// ── cancellation & interrupt tests ─────────────

#[tokio::test]
async fn test_tts_task_cancel_stops_processing() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);

    let tts = Arc::new(MockTts { sample_rate: 22050 });
    let cancel = CancellationToken::new();

    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        cancel.clone(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    // Send first fragment.
    frag_tx.send(TextFragment::new("first")).await.unwrap();

    // Receive the first chunk so we know TTS is working.
    let chunk = audio_rx.recv().await.unwrap();
    assert_eq!(chunk.samples.len(), 500); // "first" = 5 * 100

    // Now cancel.
    cancel.cancel();

    // Send another fragment -- it should NOT be processed.
    let _ = frag_tx
        .send(TextFragment::new("second").with_is_end_of_response(true))
        .await;

    // The channel should close (task exits on cancel).
    let result = tokio::time::timeout(std::time::Duration::from_millis(500), audio_rx.recv()).await;

    // Either None (closed) or timeout -- no "second" audio.
    if let Ok(Some(c)) = result {
        assert!(
            c.samples.is_empty() || c.is_end_of_utterance,
            "should not receive 'second' audio after cancel"
        );
    }
}

struct MockTtsWithTimestamps {
    sample_rate: u32,
}

#[async_trait]
impl TtsProvider for MockTtsWithTimestamps {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        Ok(AudioFrame {
            samples: vec![0; text.len() * 10],
            sample_rate: self.sample_rate,
            channels: 1,
        })
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn name(&self) -> &'static str {
        "mock-tts-timestamps"
    }

    fn supports_word_timestamps(&self) -> bool {
        true
    }

    async fn synthesize_stream(
        &self,
        req: crate::tts::SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        _cancel: CancellationToken,
    ) -> Result<(), TtsError> {
        let text = req.text;
        let word_tx = req
            .word_tx
            .expect("the pipeline asks a word-timing provider for words");
        let _ = word_tx
            .send(WordBoundary {
                word: text.to_string(),
                start_ms: 0,
                duration_ms: 10,
            })
            .await;
        let _ = tx
            .send(AudioFrame {
                samples: vec![0; text.len() * 10],
                sample_rate: self.sample_rate,
                channels: 1,
            })
            .await;
        Ok(())
    }
}

#[tokio::test]
async fn test_tts_prefetch_word_timings() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);
    let tts = Arc::new(MockTtsWithTimestamps { sample_rate: 16000 });
    let (evt_tx, err_flag) = test_event_channel();

    let _handle = tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx.send(TextFragment::new("first")).await.unwrap();

    frag_tx
        .send(TextFragment::new("second").with_is_end_of_response(true))
        .await
        .unwrap();

    let mut got_second = false;
    while let Some(chunk) = audio_rx.recv().await {
        if let Some(caption) = &chunk.caption
            && caption.text == "second"
        {
            got_second = true;
            assert!(
                caption.word_timings.is_some(),
                "Prefetched fragment lost word timings!"
            );
            assert_eq!(caption.word_timings.as_ref().unwrap()[0].word, "second");
            break;
        }
    }
    assert!(got_second, "Should have received second fragment");
}

struct StereoTts {
    sample_rate: u32,
}

#[async_trait]
impl TtsProvider for StereoTts {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        Ok(AudioFrame {
            samples: vec![0; text.len() * 20], // 2 channels
            sample_rate: self.sample_rate,
            channels: 2,
        })
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn name(&self) -> &'static str {
        "stereo-tts"
    }
}

#[tokio::test]
async fn test_tts_sentinel_channel_metadata() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);
    let tts = Arc::new(StereoTts { sample_rate: 16000 });
    let (evt_tx, err_flag) = test_event_channel();

    let _handle = tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        Some(8000), // Resampling triggers tail_samples flush
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx
        .send(TextFragment::new("hello").with_is_end_of_response(true))
        .await
        .unwrap();

    let mut got_sentinel = false;
    while let Some(chunk) = audio_rx.recv().await {
        if chunk.is_end_of_utterance {
            got_sentinel = true;
            assert_eq!(
                chunk.channels, 2,
                "EOU sentinel did not preserve resampler channel format"
            );
            break;
        }
    }
    assert!(got_sentinel);
}

struct ShortMockTts {
    sample_rate: u32,
}

#[async_trait]
impl TtsProvider for ShortMockTts {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        Ok(AudioFrame {
            samples: vec![0; text.len() * 10],
            sample_rate: self.sample_rate,
            channels: 1,
        })
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn name(&self) -> &'static str {
        "mock-tts"
    }
}

#[tokio::test]
async fn test_tts_clean_exit_on_end() {
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(8);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(8);
    let tts = Arc::new(ShortMockTts { sample_rate: 16000 });
    let (evt_tx, err_flag) = test_event_channel();

    let handle = tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));

    frag_tx
        .send(TextFragment::new("hello").with_is_end_of_response(true))
        .await
        .unwrap();

    while let Some(chunk) = audio_rx.recv().await {
        if chunk.is_end_of_utterance {
            break;
        }
    }

    // The tts_task should exit cleanly after sentinel is processed and synth completed.
    let result = tokio::time::timeout(std::time::Duration::from_millis(200), handle).await;
    assert!(result.is_ok(), "tts_task did not exit cleanly on EOU");
}

/// A provider that reports every word of the fragment before its audio,
/// as an aligner that times the whole text first would.
struct WordsFirstTts {
    words: u32,
}

#[async_trait]
impl TtsProvider for WordsFirstTts {
    async fn synthesize(&self, _text: &str) -> Result<AudioFrame, TtsError> {
        unreachable!("the stage streams")
    }

    async fn synthesize_stream(
        &self,
        req: crate::tts::SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        _cancel: CancellationToken,
    ) -> Result<(), TtsError> {
        if let Some(words) = req.word_tx.as_ref() {
            for i in 0..self.words {
                let word = WordBoundary {
                    word: format!("w{i}"),
                    start_ms: i * 10,
                    duration_ms: 10,
                };
                if words.send(word).await.is_err() {
                    return Ok(());
                }
            }
        }
        let _ = tx
            .send(AudioFrame {
                samples: vec![0i16; 160],
                sample_rate: 16_000,
                channels: 1,
            })
            .await;
        Ok(())
    }

    fn supports_word_timestamps(&self) -> bool {
        true
    }

    fn sample_rate(&self) -> u32 {
        16_000
    }

    fn name(&self) -> &'static str {
        "words-first"
    }
}

/// More word boundaries than the channels hold, all before the first
/// frame: the stage takes them as they come, so the fragment completes
/// with every word on its caption instead of waiting on a frame the
/// provider cannot send.
#[tokio::test]
async fn words_sent_before_the_first_frame_do_not_stall_the_fragment() {
    let (frag_tx, frag_rx) = mpsc::channel(4);
    let (audio_tx, mut audio_rx) = mpsc::channel(16);
    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        Arc::new(WordsFirstTts { words: 300 }),
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        0,
        None,
        evt_tx,
        err_flag,
        None,
    ));
    frag_tx
        .send(TextFragment::new("a long sentence").with_is_end_of_response(true))
        .await
        .unwrap();

    let first = tokio::time::timeout(std::time::Duration::from_secs(5), audio_rx.recv())
        .await
        .expect("the fragment stalled behind its word boundaries")
        .expect("a chunk");
    let timings = first
        .caption
        .and_then(|c| c.word_timings)
        .expect("the caption carries the words");
    assert_eq!(timings.len(), 300);
}

struct CapturingTts {
    spoken: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl TtsProvider for CapturingTts {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        self.spoken.lock().await.push(text.to_string());
        Ok(AudioFrame {
            samples: vec![100; 160],
            sample_rate: 16_000,
            channels: 1,
        })
    }
    fn sample_rate(&self) -> u32 {
        16_000
    }
    fn name(&self) -> &'static str {
        "capturing-tts"
    }
    fn supported_inline_tags(&self) -> &'static [&'static str] {
        &["<break"]
    }
}

#[tokio::test]
async fn test_tts_task_dual_track_sanitizer_and_lexicon() {
    let captured = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let tts = Arc::new(CapturingTts {
        spoken: Arc::clone(&captured),
    });

    let (frag_tx, frag_rx) = mpsc::channel(4);
    let (audio_tx, mut audio_rx) = mpsc::channel(16);
    let (evt_tx, err_flag) = test_event_channel();

    let sanitize_cfg = voice_pipes_sdk::SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec!["<break".to_string()],
        provider_name: "capturing-tts".to_string(),
        lexicon: std::collections::BTreeMap::from([("SQL".to_string(), "sequel".to_string())]),
    };

    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts,
        Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        CancellationToken::new(),
        1,
        None,
        evt_tx,
        err_flag,
        Some(sanitize_cfg),
    ));

    frag_tx
        .send(
            TextFragment::new("Check **SQL** at [Docs](https://example.com) <break time=\"500ms\"/> [whispers] now!")
                .with_is_end_of_response(true),
        )
        .await
        .unwrap();

    let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), audio_rx.recv())
        .await
        .expect("audio received")
        .expect("chunk received");

    let caption = chunk.caption.expect("chunk carries caption");
    assert_eq!(
        caption.text,
        "Check **SQL** at [Docs](https://example.com) now!"
    );
    assert_eq!(
        caption.spoken_text.as_deref(),
        Some("Check sequel at Docs <break time=\"500ms\"/> now!")
    );
    assert_eq!(caption.tts_directives.len(), 2); // 1 TagPassthrough (<break>) + 1 Lexicon (SQL -> sequel)
    assert_eq!(caption.stripped_directives.len(), 1); // 1 stripped ([whispers])

    let spoken_calls = captured.lock().await;
    assert_eq!(spoken_calls.len(), 1);
    assert_eq!(spoken_calls[0], "Check sequel at Docs <break time=\"500ms\"/> now!");
}
