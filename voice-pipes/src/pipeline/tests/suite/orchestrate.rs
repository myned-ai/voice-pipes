use super::super::*;
use super::common::*;
use crate::stt::SttProvider;
use crate::text::TextProcessorError;
use crate::text::{TextFragment, TextProcessor};
use crate::transport::ServerMessage;
use crate::tts::TtsProvider;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_mock_pipeline_end_to_end() {
    // Wire all tasks: mic -> stt -> llm -> tts -> playback (captured)
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (stt_tx, stt_rx) = mpsc::channel::<Utterance>(4);
    let (llm_tx, llm_rx) = mpsc::channel::<TextFragment>(8);
    let (tts_tx, mut tts_rx) = mpsc::channel::<AudioChunk>(8);

    let stt: Arc<dyn SttProvider> = Arc::new(MockStt {
        expected_rate: 16000,
        response: "test input".into(),
    });
    let tts: Arc<dyn TtsProvider> = Arc::new(MockTts { sample_rate: 22050 });

    // Use echo processor (identity)
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![]);

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    tokio::spawn(stt_task(
        mic_rx, stt_tx, None, stt, td, None, None, 0, None, None,
    ));
    tokio::spawn(llm_task(
        stt_rx,
        llm_tx,
        processors,
        CancellationToken::new(),
    ));
    let (evt_tx, err_flag) = test_event_channel();
    tokio::spawn(tts_task(
        llm_rx,
        tts_tx,
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

    // Feed an utterance
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

    // Drop mic_tx to eventually close the pipeline
    drop(mic_tx);

    // Should receive synthesized audio from TTS
    let chunk = tts_rx.recv().await.unwrap();
    assert_eq!(chunk.sample_rate, 22050);
    assert!(!chunk.samples.is_empty());

    // Metadata update chunk
    let meta = tts_rx.recv().await.unwrap();
    assert!(!meta.is_end_of_utterance);
    assert!(meta.samples.is_empty());

    // Sentinel marking end-of-response
    let sentinel = tts_rx.recv().await.unwrap();
    assert!(sentinel.is_end_of_utterance);
}

#[tokio::test]
async fn test_graceful_shutdown() {
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(8);
    let (stt_tx, stt_rx) = mpsc::channel::<Utterance>(4);
    let (llm_tx, llm_rx) = mpsc::channel::<TextFragment>(8);
    let (tts_tx, tts_rx) = mpsc::channel::<AudioChunk>(8);

    let stt: Arc<dyn SttProvider> = Arc::new(MockStt {
        expected_rate: 16000,
        response: "shutdown test".into(),
    });
    let tts: Arc<dyn TtsProvider> = Arc::new(MockTts { sample_rate: 22050 });
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![]);

    let td = std::sync::Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let h1 = tokio::spawn(stt_task(
        mic_rx, stt_tx, None, stt, td, None, None, 0, None, None,
    ));
    let h2 = tokio::spawn(llm_task(
        stt_rx,
        llm_tx,
        processors,
        CancellationToken::new(),
    ));
    let (evt_tx, err_flag) = test_event_channel();
    let h3 = tokio::spawn(tts_task(
        llm_rx,
        tts_tx,
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
    // Use a simple drain consumer instead of playback_task, because
    // cpal::Stream is !Send and cannot be spawned onto tokio.
    let h4 = tokio::spawn(async move {
        let mut rx = tts_rx;
        while rx.recv().await.is_some() {}
    });

    // Drop the sender -- pipeline should drain and all tasks should complete
    drop(mic_tx);

    let results = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(h1, h2, h3, h4)
    })
    .await;

    assert!(
        results.is_ok(),
        "pipeline should shut down within 2 seconds"
    );
}

#[tokio::test]
async fn test_llm_task_cancel_stops_processing() {
    let (text_tx, text_rx) = mpsc::channel::<Utterance>(4);
    let (frag_tx, mut frag_rx) = mpsc::channel::<TextFragment>(8);

    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![]);
    let cancel = CancellationToken::new();

    tokio::spawn(llm_task(text_rx, frag_tx, processors, cancel.clone()));

    // Send an utterance.
    text_tx
        .send(Utterance {
            text: "hello world".into(),
            utterance_id: 1,
            confidence: None,
            stt_latency: None,
            endpointing_latency: None,
            source: UtteranceSource::default(),
            inference_metrics: None,
            committed_at: None,
        })
        .await
        .unwrap();

    // Receive the echo fragment (empty chain passes through).
    let frag = frag_rx.recv().await.unwrap();
    assert_eq!(frag.text, "hello world");

    // Now cancel.
    cancel.cancel();

    // Send another utterance -- should NOT be processed.
    let _ = text_tx
        .send(Utterance {
            text: "should not appear".into(),
            utterance_id: 2,
            confidence: None,
            stt_latency: None,
            endpointing_latency: None,
            source: UtteranceSource::default(),
            inference_metrics: None,
            committed_at: None,
        })
        .await;

    // Channel should close.
    let result = tokio::time::timeout(std::time::Duration::from_millis(500), frag_rx.recv()).await;

    if let Ok(Some(_)) = result {
        panic!("should not receive fragments after cancel")
    }
}

#[tokio::test]
async fn test_mic_audio_splitter() {
    let (input_tx, input_rx) = mpsc::channel::<AudioChunk>(8);
    let (stt_tx, mut stt_rx) = mpsc::channel::<AudioChunk>(8);
    let (mon_tx, mut mon_rx) = mpsc::channel::<AudioChunk>(8);

    tokio::spawn(mic_audio_splitter(input_rx, stt_tx, mon_tx));

    let chunk = AudioChunk {
        samples: vec![42i16; 50],
        sample_rate: 16000,
        channels: 1,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    input_tx.send(chunk.clone()).await.unwrap();

    let stt_chunk = stt_rx.recv().await.unwrap();
    let mon_chunk = mon_rx.recv().await.unwrap();
    assert_eq!(stt_chunk.samples, vec![42i16; 50]);
    assert_eq!(mon_chunk.samples, vec![42i16; 50]);

    // Drop input to close the splitter.
    drop(input_tx);
    assert!(stt_rx.recv().await.is_none());
    assert!(mon_rx.recv().await.is_none());
}

#[tokio::test]
async fn test_turn_state_transitions() {
    let state = new_turn_state();

    assert_eq!(get_turn_state(&state), TurnState::Listening);

    set_turn_state(&state, TurnState::Processing);
    assert_eq!(get_turn_state(&state), TurnState::Processing);

    set_turn_state(&state, TurnState::Speaking);
    assert_eq!(get_turn_state(&state), TurnState::Speaking);

    set_turn_state(&state, TurnState::Listening);
    assert_eq!(get_turn_state(&state), TurnState::Listening);
}

#[tokio::test]
async fn test_process_utterance_cancel() {
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(32);
    let cancel = CancellationToken::new();
    let tts: Arc<dyn TtsProvider> = Arc::new(MockTts { sample_rate: 22050 });
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![]);
    let utterance = Utterance {
        text: "hello world".into(),
        utterance_id: 1,
        confidence: None,
        stt_latency: None,
        endpointing_latency: None,
        source: UtteranceSource::default(),
        inference_metrics: None,
        committed_at: None,
    };

    // Cancel immediately.
    cancel.cancel();

    let handle = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
            let ledger = std::sync::Arc::new(parking_lot::Mutex::new(
                crate::pipeline::types::AudioLedger::default(),
            ));
            process_utterance(
                &utterance,
                audio_tx,
                event_tx,
                processors,
                crate::pipeline::empty_chain(),
                tts,
                None,
                None,
                None,
                cancel,
                None,
                None,
                ledger,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                None,
                None,
            )
            .await;
        }
    });

    // Should complete quickly without producing audio.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle)
        .await
        .expect("should finish within 1s");

    // No audio should be received.
    let result = tokio::time::timeout(std::time::Duration::from_millis(100), audio_rx.recv()).await;

    assert!(
        result.is_err() || result.unwrap().is_none(),
        "no audio when cancelled before start"
    );
}

// ── min_utterance_chars filtering tests ──────────────────

struct BlockedTextProcessor;

#[async_trait]
impl TextProcessor for BlockedTextProcessor {
    fn name(&self) -> &'static str {
        "blocked-llm"
    }
    async fn process(&self, _input: &str) -> Result<String, TextProcessorError> {
        Ok(String::new())
    }
    async fn process_stream(
        &self,
        _input: &str,
        _tx: mpsc::Sender<TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        Ok(())
    }
}

#[tokio::test]
async fn test_orchestrator_nonblocking_llm_cancel() {
    let utterance = Utterance {
        text: "hello".into(),
        utterance_id: 0,
        confidence: None,
        stt_latency: None,
        source: UtteranceSource::default(),
        endpointing_latency: None,
        inference_metrics: None,
        committed_at: None,
    };
    let (audio_tx, _audio_rx) = mpsc::channel::<AudioChunk>(8);
    let (event_tx, _event_rx) = mpsc::channel::<ServerMessage>(8);
    let processors = Arc::new(vec![
        Box::new(BlockedTextProcessor) as Box<dyn TextProcessor>
    ]);
    let cancel = CancellationToken::new();

    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let before = std::time::Instant::now();
    let ledger = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));

    let _metrics = process_utterance(
        &utterance,
        audio_tx,
        event_tx,
        processors,
        Arc::new(vec![]),
        Arc::new(MockTts { sample_rate: 16000 }),
        None,
        None,
        None,
        cancel,
        None,
        None,
        ledger,
        Arc::new(AtomicBool::new(false)),
        None,
        None,
    )
    .await;

    let elapsed = before.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "process_utterance blocked on cancelled LLM task: {elapsed:?}"
    );
}

// Speculative execution coordinator tests

/// An LLM whose stream stays open after its final fragment, the way a
/// provider does while it parses usage or updates its history.
struct SlowCloseLlm;

#[async_trait]
impl TextProcessor for SlowCloseLlm {
    fn name(&self) -> &'static str {
        "slow-close-llm"
    }
    async fn process(&self, input: &str) -> Result<String, crate::text::TextProcessorError> {
        Ok(input.to_string())
    }
    async fn process_stream(
        &self,
        _input: &str,
        tx: mpsc::Sender<TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), crate::text::TextProcessorError> {
        let _ = tx
            .send(TextFragment::new("hello there").with_is_end_of_response(true))
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        Ok(())
    }
}

/// The end-of-utterance sentinel carries the response text and follows
/// the last audio chunk promptly, even though the LLM stream has not
/// closed yet.
#[tokio::test]
async fn eou_sentinel_does_not_wait_for_the_llm_stream_to_close() {
    let (audio_tx, mut audio_rx) = mpsc::channel(32);
    let (event_tx, _flag) = test_event_channel();
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(SlowCloseLlm)]);
    let utterance = Utterance {
        text: "hi".into(),
        utterance_id: 1,
        confidence: None,
        stt_latency: None,
        source: UtteranceSource::default(),
        endpointing_latency: None,
        inference_metrics: None,
        committed_at: None,
    };
    let ledger = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
    let task = tokio::spawn(async move {
        process_utterance(
            &utterance,
            audio_tx,
            event_tx,
            processors,
            Arc::new(vec![]),
            Arc::new(MockTts { sample_rate: 16000 }),
            None,
            None,
            None,
            CancellationToken::new(),
            None,
            None,
            ledger,
            Arc::new(AtomicBool::new(false)),
            None,
            None,
        )
        .await
    });

    let mut last_audio_at: Option<std::time::Instant> = None;
    let mut eou: Option<(AudioChunk, std::time::Duration)> = None;
    while let Ok(Some(chunk)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), audio_rx.recv()).await
    {
        let now = std::time::Instant::now();
        if chunk.is_end_of_utterance {
            let gap = last_audio_at.map_or(std::time::Duration::ZERO, |t| now - t);
            eou = Some((chunk, gap));
            break;
        }
        last_audio_at = Some(now);
    }
    let (chunk, gap) = eou.expect("end-of-utterance sentinel");
    assert_eq!(chunk.full_text.as_deref(), Some("hello there"));
    assert!(
        gap < std::time::Duration::from_millis(300),
        "sentinel must not wait for the LLM stream to close: {gap:?}"
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
}

#[tokio::test]
async fn agent_say_turn_has_null_llm_ttft_and_skip_reason() {
    let (audio_tx, mut audio_rx) = mpsc::channel(32);
    let (event_tx, _flag) = test_event_channel();
    let processors: Arc<Vec<Box<dyn TextProcessor>>> =
        Arc::new(vec![Box::new(BlockedTextProcessor)]);
    let utterance = Utterance {
        text: "Welcome aboard.".into(),
        utterance_id: 7,
        confidence: None,
        stt_latency: None,
        source: UtteranceSource::AgentSay,
        endpointing_latency: None,
        inference_metrics: None,
        committed_at: None,
    };
    let ledger = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
    let drain = tokio::spawn(async move { while audio_rx.recv().await.is_some() {} });
    let snap = process_utterance(
        &utterance,
        audio_tx,
        event_tx,
        processors,
        Arc::new(vec![]),
        Arc::new(MockTts { sample_rate: 16000 }),
        None,
        None,
        None,
        CancellationToken::new(),
        None,
        None,
        ledger,
        Arc::new(AtomicBool::new(false)),
        None,
        None,
    )
    .await;
    let _ = drain.await;

    let llm = snap.component_by_kind("llm").expect("llm component span");
    assert!(
        llm.extras.get("ttft_ms").is_none(),
        "AgentSay turn skips LLM and must not report ttft_ms=0: {:?}",
        llm.extras
    );
    assert!(
        llm.extras.get("generation_ms").is_none(),
        "AgentSay turn skips LLM and must not report generation_ms: {:?}",
        llm.extras
    );
    assert_eq!(
        llm.extras
            .get("stage_skipped_reason")
            .and_then(|v| v.as_str()),
        Some("agent_say")
    );
}

#[tokio::test]
async fn speculative_accepted_turn_has_null_llm_ttft_and_skip_reason() {
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![]);
    let tts = Arc::new(MockTts { sample_rate: 16000 });
    let spec_cfg = crate::config::SpeculativeConfig {
        enabled: true,
        speculative_tts: true,
        min_partial_interval_ms: 0,
    };
    let manager = Arc::new(crate::pipeline::SpeculativeSessionManager::new(
        Arc::clone(&processors),
        Arc::<MockTts>::clone(&tts),
        spec_cfg,
    ));
    manager.start_turn().await;
    manager.handle_partial("hello world").await;

    let start_wait = std::time::Instant::now();
    loop {
        let state = manager.state.lock().await;
        if !state.buffered_audio.is_empty() {
            break;
        }
        assert!(
            start_wait.elapsed() < std::time::Duration::from_secs(1),
            "timed out waiting for speculative audio"
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let (audio_tx, mut audio_rx) = mpsc::channel(32);
    let (event_tx, _flag) = test_event_channel();
    let utterance = Utterance {
        text: "hello world".into(),
        utterance_id: 9,
        confidence: None,
        stt_latency: None,
        source: UtteranceSource::default(),
        endpointing_latency: None,
        inference_metrics: None,
        committed_at: None,
    };
    let ledger = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
    let drain = tokio::spawn(async move { while audio_rx.recv().await.is_some() {} });
    let snap = process_utterance(
        &utterance,
        audio_tx,
        event_tx,
        processors,
        Arc::new(vec![]),
        tts,
        None,
        None,
        None,
        CancellationToken::new(),
        None,
        None,
        ledger,
        Arc::new(AtomicBool::new(false)),
        Some(manager),
        None,
    )
    .await;
    let _ = drain.await;

    let llm = snap.component_by_kind("llm").expect("llm component span");
    assert!(
        llm.extras.get("ttft_ms").is_none(),
        "Accepted speculative turn must not report hard-coded ttft_ms=0: {:?}",
        llm.extras
    );
    assert!(
        llm.extras.get("generation_ms").is_none(),
        "Accepted speculative turn must not report hard-coded generation_ms=0: {:?}",
        llm.extras
    );
    assert_eq!(
        llm.extras
            .get("stage_skipped_reason")
            .and_then(|v| v.as_str()),
        Some("speculative_accept")
    );

    let tts_span = snap.component_by_kind("tts").expect("tts component span");
    assert!(
        tts_span.extras.get("ttfb_ms").is_none(),
        "Accepted speculative turn must not report tts.ttfb_ms: {:?}",
        tts_span.extras
    );
    assert!(
        tts_span.extras.get("tts_wait_ms").is_none(),
        "Accepted speculative turn must not report tts.tts_wait_ms: {:?}",
        tts_span.extras
    );
    assert!(
        tts_span.extras.get("synthesis_ms").is_none(),
        "Accepted speculative turn must not report tts.synthesis_ms: {:?}",
        tts_span.extras
    );
    assert_eq!(
        tts_span
            .extras
            .get("stage_skipped_reason")
            .and_then(|v| v.as_str()),
        Some("speculative_accept")
    );
}

struct DelayedLlm {
    delay_ms: u64,
}

#[async_trait]
impl TextProcessor for DelayedLlm {
    fn name(&self) -> &'static str {
        "delayed-llm"
    }
    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        Ok(input.to_string())
    }
    async fn process_stream(
        &self,
        _input: &str,
        tx: mpsc::Sender<TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        let _ = tx
            .send(TextFragment::new("Hello from delayed LLM.").with_is_end_of_response(true))
            .await;
        Ok(())
    }
}

struct DelayedTts {
    delay_ms: u64,
}

#[async_trait]
impl TtsProvider for DelayedTts {
    async fn synthesize(
        &self,
        _text: &str,
    ) -> Result<crate::audio::AudioFrame, crate::tts::TtsError> {
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        Ok(crate::audio::AudioFrame {
            samples: vec![100i16; 1600],
            sample_rate: 16000,
            channels: 1,
        })
    }
    fn sample_rate(&self) -> u32 {
        16000
    }
    fn name(&self) -> &'static str {
        "delayed-tts"
    }
}

#[tokio::test]
async fn telescoping_stage_latencies_sum_to_commit_to_first_audio() {
    let (audio_tx, mut audio_rx) = mpsc::channel(32);
    let (event_tx, _flag) = test_event_channel();
    let processors: Arc<Vec<Box<dyn TextProcessor>>> =
        Arc::new(vec![Box::new(DelayedLlm { delay_ms: 40 })]);
    let tts: Arc<dyn TtsProvider> = Arc::new(DelayedTts { delay_ms: 30 });
    let commit_start = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(10))
        .unwrap_or_else(std::time::Instant::now);
    let utterance = Utterance {
        text: "test telescoping stages".into(),
        utterance_id: 11,
        confidence: Some(0.95),
        stt_latency: Some(std::time::Duration::from_millis(25)),
        source: UtteranceSource::default(),
        endpointing_latency: Some(std::time::Duration::from_millis(320)),
        inference_metrics: None,
        committed_at: Some(commit_start),
    };
    let ledger = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
    let drain = tokio::spawn(async move { while audio_rx.recv().await.is_some() {} });
    let snap = process_utterance(
        &utterance,
        audio_tx,
        event_tx,
        processors,
        Arc::new(vec![]),
        tts,
        None,
        None,
        None,
        CancellationToken::new(),
        None,
        None,
        ledger,
        Arc::new(AtomicBool::new(false)),
        None,
        None,
    )
    .await;
    let _ = drain.await;

    let llm = snap.component_by_kind("llm").expect("llm span");
    let tts_span = snap.component_by_kind("tts").expect("tts span");
    let stt_span = snap.component_by_kind("stt").expect("stt span");
    let out_span = snap.component_by_kind("output").expect("output span");

    let llm_wait = llm.extras["llm_wait_ms"].as_f64().expect("llm_wait_ms");
    let tts_wait = tts_span.extras["tts_wait_ms"]
        .as_f64()
        .expect("tts_wait_ms");
    let output_wait = tts_span.extras["output_wait_ms"]
        .as_f64()
        .expect("output_wait_ms");
    let commit_fa = tts_span.extras["commit_to_first_audio_ms"]
        .as_f64()
        .expect("commit_to_first_audio_ms");

    assert!(
        llm_wait >= 40.0,
        "llm_wait_ms should include commit queue + 40ms LLM wait: {llm_wait}"
    );
    assert!(
        tts_wait >= 25.0,
        "tts_wait_ms should include ~30ms TTS wait: {tts_wait}"
    );
    assert!(output_wait >= 0.0);
    let sum = llm_wait + tts_wait + output_wait;
    assert!(
        (sum - commit_fa).abs() <= 1.0,
        "telescoping stages ({llm_wait} + {tts_wait} + {output_wait} = {sum}) must equal commit_to_first_audio_ms ({commit_fa}) within 1ms"
    );

    assert_eq!(snap.stt_latency_ms(), Some(25));
    assert!(stt_span.latency_ms.is_some());
    assert!(llm.latency_ms.is_some());
    assert!(llm.finished_at_ms.is_some());
    assert!(tts_span.latency_ms.is_some());
    assert!(tts_span.finished_at_ms.is_some());
    assert!(out_span.latency_ms.is_some());
    assert!(out_span.finished_at_ms.is_some());
}
