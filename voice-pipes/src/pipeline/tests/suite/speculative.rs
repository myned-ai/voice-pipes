use super::super::*;
use crate::audio::AudioFrame;
use crate::text::TextProcessorError;
use crate::text::{TextFragment, TextProcessor};
use crate::transport::ServerMessage;
use crate::tts::{TtsError, TtsProvider};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct MockLlmProcessor {
    responses: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>>,
}

#[async_trait]
impl TextProcessor for MockLlmProcessor {
    fn name(&self) -> &'static str {
        "mock-llm-spec"
    }
    async fn process(&self, _input: &str) -> Result<String, TextProcessorError> {
        Ok(String::new())
    }
    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let resp = {
            let guard = self.responses.lock().unwrap();
            guard.get(input).cloned()
        };
        if let Some(fragments) = resp {
            for text in fragments {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let _ = tx
                    .send(TextFragment::new(text.clone()).with_token_count(Some(1)))
                    .await;
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        }
        Ok(())
    }
}

struct SpecMockTts;

#[async_trait]
impl TtsProvider for SpecMockTts {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        Ok(AudioFrame {
            samples: vec![0i16; text.len() * 100],
            sample_rate: 16000,
            channels: 1,
        })
    }
    fn sample_rate(&self) -> u32 {
        16000
    }
    fn name(&self) -> &'static str {
        "spec-mock-tts"
    }
}

#[tokio::test]
async fn test_speculative_session_manager_accepted() {
    let responses = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    responses.lock().unwrap().insert(
        "what is the cost".to_string(),
        vec![
            "one ".to_string(),
            "million ".to_string(),
            "dollars".to_string(),
        ],
    );

    let processor = MockLlmProcessor { responses };
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(processor)]);
    let tts = Arc::new(SpecMockTts);

    let config = crate::config::SpeculativeConfig {
        enabled: true,
        speculative_tts: false,
        min_partial_interval_ms: 0,
    };

    let manager = SpeculativeSessionManager::new(processors, tts, config);
    manager.start_turn().await;

    manager.handle_partial("what is the cost").await;

    let start = std::time::Instant::now();
    loop {
        let state = manager.state.lock().await;
        if state.tokens_generated >= 3 {
            break;
        }
        assert!(
            start.elapsed() <= std::time::Duration::from_millis(500),
            "Timed out waiting for speculative LLM fragments; tokens_generated={}",
            state.tokens_generated
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let commit_res = manager.commit("what is the cost.").await;
    match commit_res {
        SpeculativeCommitResult::Accepted {
            rollbacks,
            tokens_generated,
            tokens_accepted,
            accepted_text,
            audio_stream,
            text_stream,
        } => {
            assert_eq!(rollbacks, 0);
            assert_eq!(tokens_generated, 3);
            assert_eq!(tokens_accepted, 3);
            // `full_text` must be the assistant's generated response,
            // not the user transcript ("what is the cost").
            assert_eq!(accepted_text, "one million dollars");
            assert!(audio_stream.is_none());
            let (buffered, mut _subsequent_rx) = text_stream.unwrap();
            assert_eq!(buffered.len(), 3);
            assert_eq!(buffered[0].text, "one ");
            assert_eq!(buffered[1].text, "million ");
            assert_eq!(buffered[2].text, "dollars");
        }
        SpeculativeCommitResult::Rejected { .. } => {
            panic!("Expected SpeculativeCommitResult::Accepted")
        }
    }
}

#[tokio::test]
async fn test_speculative_session_manager_rejected() {
    let responses = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    responses.lock().unwrap().insert(
        "what is the cost".to_string(),
        vec![
            "one ".to_string(),
            "million ".to_string(),
            "dollars".to_string(),
        ],
    );

    let processor = MockLlmProcessor { responses };
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(processor)]);
    let tts = Arc::new(SpecMockTts);

    let config = crate::config::SpeculativeConfig {
        enabled: true,
        speculative_tts: false,
        min_partial_interval_ms: 0,
    };

    let manager = SpeculativeSessionManager::new(processors, tts, config);
    manager.start_turn().await;

    manager.handle_partial("what is the cost").await;

    let start = std::time::Instant::now();
    loop {
        let state = manager.state.lock().await;
        if state.tokens_generated >= 3 {
            break;
        }
        assert!(
            start.elapsed() <= std::time::Duration::from_millis(500),
            "Timed out waiting for speculative LLM fragments"
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let commit_res = manager.commit("what is the capital").await;
    match commit_res {
        SpeculativeCommitResult::Rejected {
            rollbacks,
            tokens_generated,
        } => {
            assert_eq!(rollbacks, 1);
            assert_eq!(tokens_generated, 3);
        }
        SpeculativeCommitResult::Accepted { .. } => {
            panic!("Expected SpeculativeCommitResult::Rejected")
        }
    }
}

#[tokio::test]
async fn test_speculative_session_manager_flicker() {
    let responses = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    responses.lock().unwrap().insert(
        "what is the cost".to_string(),
        vec!["one ".to_string(), "million ".to_string()],
    );
    responses
        .lock()
        .unwrap()
        .insert("what is the capital".to_string(), vec!["paris".to_string()]);

    let processor = MockLlmProcessor { responses };
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(processor)]);
    let tts = Arc::new(SpecMockTts);

    let config = crate::config::SpeculativeConfig {
        enabled: true,
        speculative_tts: false,
        min_partial_interval_ms: 0,
    };

    let manager = SpeculativeSessionManager::new(processors, tts, config);
    manager.start_turn().await;

    manager.handle_partial("what is the cost").await;

    // Wait until first run is fully generated (2 tokens)
    let start_first = std::time::Instant::now();
    loop {
        let state = manager.state.lock().await;
        if state.tokens_generated >= 2 {
            break;
        }
        assert!(
            start_first.elapsed() <= std::time::Duration::from_millis(500),
            "Timed out waiting for first run to generate tokens"
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    manager.handle_partial("what is the capital").await;

    let start = std::time::Instant::now();
    loop {
        let state = manager.state.lock().await;
        if state.tokens_generated >= 1
            && state.buffered_fragments.len() == 1
            && state.buffered_fragments[0].text == "paris"
        {
            break;
        }
        assert!(
            start.elapsed() <= std::time::Duration::from_millis(500),
            "Timed out waiting for paris fragment; state.buffered={:?}",
            state.buffered_fragments
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let commit_res = manager.commit("what is the capital").await;
    match commit_res {
        SpeculativeCommitResult::Accepted {
            rollbacks,
            tokens_generated,
            tokens_accepted,
            text_stream,
            ..
        } => {
            assert_eq!(rollbacks, 1);
            assert_eq!(tokens_generated, 3);
            assert_eq!(tokens_accepted, 1);
            let (buffered, _) = text_stream.unwrap();
            assert_eq!(buffered.len(), 1);
            assert_eq!(buffered[0].text, "paris");
        }
        SpeculativeCommitResult::Rejected { .. } => {
            panic!("Expected SpeculativeCommitResult::Accepted")
        }
    }
}

#[tokio::test]
async fn test_speculative_session_manager_tts() {
    let responses = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    responses
        .lock()
        .unwrap()
        .insert("hello".to_string(), vec!["world".to_string()]);

    let processor = MockLlmProcessor { responses };
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(processor)]);
    let tts = Arc::new(SpecMockTts);

    let config = crate::config::SpeculativeConfig {
        enabled: true,
        speculative_tts: true,
        min_partial_interval_ms: 0,
    };

    let manager = SpeculativeSessionManager::new(processors, tts, config);
    manager.start_turn().await;

    manager.handle_partial("hello").await;

    let start = std::time::Instant::now();
    loop {
        let state = manager.state.lock().await;
        if !state.buffered_audio.is_empty() {
            break;
        }
        assert!(
            start.elapsed() <= std::time::Duration::from_secs(1),
            "Timed out waiting for audio chunks"
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let commit_res = manager.commit("hello").await;
    match commit_res {
        SpeculativeCommitResult::Accepted {
            rollbacks,
            tokens_generated,
            tokens_accepted,
            accepted_text,
            audio_stream,
            text_stream,
        } => {
            assert_eq!(rollbacks, 0);
            assert_eq!(tokens_generated, 1);
            assert_eq!(tokens_accepted, 1);
            // `accepted_text` is the assistant response ("world"),
            // not the user transcript ("hello"), even on the audio path.
            assert_eq!(accepted_text, "world");
            assert!(text_stream.is_none());
            let (buffered_audio, _) = audio_stream.unwrap();
            assert!(!buffered_audio.is_empty());
            assert_eq!(buffered_audio[0].samples.len(), 500);
        }
        SpeculativeCommitResult::Rejected { .. } => {
            panic!("Expected SpeculativeCommitResult::Accepted with audio_stream")
        }
    }
}

/// An accepted speculative stream must END. Before the fix the
/// sender for `subsequent_rx` lived in `SpeculativeState` until the next
/// `start_turn`, so `process_utterance`'s forward loop never saw `None`
/// and every accepted turn parked forever (no `TurnMetrics`, leaked task).
/// Covers both orders: commit while production is still running, and
/// commit after it already finished.
#[tokio::test]
async fn speculative_accepted_streams_end_when_production_finishes() {
    for commit_after_done in [false, true] {
        let responses = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        responses
            .lock()
            .unwrap()
            .insert("hello".to_string(), vec!["world".to_string()]);
        let processors: Arc<Vec<Box<dyn TextProcessor>>> =
            Arc::new(vec![Box::new(MockLlmProcessor { responses })]);
        let manager = SpeculativeSessionManager::new(
            processors,
            Arc::new(SpecMockTts),
            crate::config::SpeculativeConfig {
                enabled: true,
                speculative_tts: true,
                min_partial_interval_ms: 0,
            },
        );
        manager.start_turn().await;
        manager.handle_partial("hello").await;

        let start = std::time::Instant::now();
        loop {
            let s = manager.state.lock().await;
            let ready = if commit_after_done {
                s.production_done
            } else {
                !s.buffered_audio.is_empty()
            };
            if ready {
                break;
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(2),
                "speculation stalled"
            );
            drop(s);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let SpeculativeCommitResult::Accepted { audio_stream, .. } = manager.commit("hello").await
        else {
            panic!("expected Accepted");
        };
        let (buffered, mut rx) = audio_stream.unwrap();
        assert!(!buffered.is_empty());

        // Drain whatever production still emits; the stream must close.
        let closed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while rx.recv().await.is_some() {}
        })
        .await;
        assert!(
            closed.is_ok(),
            "audio_stream never ended (commit_after_done = {commit_after_done})"
        );
    }
}

#[derive(Clone)]
struct LatencyMockLlmProcessor {
    responses: Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>>,
    token_delay: std::time::Duration,
}

#[async_trait]
impl TextProcessor for LatencyMockLlmProcessor {
    fn name(&self) -> &'static str {
        "latency-mock-llm"
    }
    async fn process(&self, _input: &str) -> Result<String, TextProcessorError> {
        Ok(String::new())
    }
    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let resp = {
            let guard = self.responses.lock().unwrap();
            guard.get(input).cloned()
        };
        if let Some(fragments) = resp {
            let count = fragments.len();
            for (i, text) in fragments.into_iter().enumerate() {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                tokio::time::sleep(self.token_delay).await;
                let _ = tx
                    .send(
                        TextFragment::new(text.clone())
                            .with_is_end_of_response(i == count - 1)
                            .with_token_count(Some(1)),
                    )
                    .await;
            }
        }
        Ok(())
    }
}

struct LatencyMockTts {
    sample_rate: u32,
    delay: std::time::Duration,
}

#[async_trait]
impl TtsProvider for LatencyMockTts {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        tokio::time::sleep(self.delay).await;
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
        "latency-mock-tts"
    }
}

#[tokio::test]
async fn test_speculative_speedup_proof() {
    let responses = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    responses.lock().unwrap().insert(
        "what is the cost".to_string(),
        vec![
            "one ".to_string(),
            "million ".to_string(),
            "dollars".to_string(),
        ],
    );

    let token_delay = std::time::Duration::from_millis(40);
    let tts_delay = std::time::Duration::from_millis(20);

    let processor = LatencyMockLlmProcessor {
        responses: std::sync::Arc::clone(&responses),
        token_delay,
    };
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(processor)]);
    let tts = Arc::new(LatencyMockTts {
        sample_rate: 16000,
        delay: tts_delay,
    });

    // 1. Sequential Run (No Speculation)
    let utterance = Utterance {
        text: "what is the cost".into(),
        utterance_id: 1,
        confidence: None,
        stt_latency: None,
        source: UtteranceSource::default(),
        endpointing_latency: None,
        inference_metrics: None,
        committed_at: None,
    };

    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(32);
    let (event_tx, _event_rx) = mpsc::channel::<ServerMessage>(32);
    let ledger = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
    let cancel = CancellationToken::new();

    let start_seq = std::time::Instant::now();
    let utterance_clone = utterance.clone();
    let processors_clone = std::sync::Arc::clone(&processors);
    let tts_clone = std::sync::Arc::clone(&tts);
    let ledger_clone = std::sync::Arc::clone(&ledger);
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        let _ = process_utterance(
            &utterance_clone,
            audio_tx,
            event_tx,
            processors_clone,
            Arc::new(vec![]),
            tts_clone,
            None,
            None,
            None,
            cancel_clone,
            None,
            None,
            ledger_clone,
            Arc::new(AtomicBool::new(false)),
            None,
            None,
        )
        .await;
    });

    let mut first_audio_seq = None;
    while let Some(chunk) = audio_rx.recv().await {
        if !chunk.samples.is_empty() {
            first_audio_seq = Some(start_seq.elapsed());
            break;
        }
    }
    let ttf_seq = first_audio_seq.expect("Should receive audio in sequential run");

    // 2. Speculative Run (Text Speculation Only)
    let config_spec_text = crate::config::SpeculativeConfig {
        enabled: true,
        speculative_tts: false,
        min_partial_interval_ms: 0,
    };
    let manager_spec_text = SpeculativeSessionManager::new(
        std::sync::Arc::clone(&processors),
        std::sync::Arc::<LatencyMockTts>::clone(&tts),
        config_spec_text,
    );
    manager_spec_text.start_turn().await;
    manager_spec_text.handle_partial("what is the cost").await;

    let start_wait = std::time::Instant::now();
    loop {
        let state = manager_spec_text.state.lock().await;
        if state.tokens_generated >= 3 {
            break;
        }
        assert!(
            start_wait.elapsed() <= std::time::Duration::from_secs(1),
            "Timed out waiting for speculative LLM fragments; tokens={}",
            state.tokens_generated
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let (audio_tx_spec_text, mut audio_rx_spec_text) = mpsc::channel::<AudioChunk>(32);
    let (event_tx_spec_text, _event_rx_spec_text) = mpsc::channel::<ServerMessage>(32);
    let ledger_spec_text = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
    let cancel_spec_text = CancellationToken::new();

    let start_spec_text = std::time::Instant::now();
    let utterance_clone2 = utterance.clone();
    let processors_clone2 = std::sync::Arc::clone(&processors);
    let tts_clone2 = std::sync::Arc::clone(&tts);
    let ledger_spec_text_clone = std::sync::Arc::clone(&ledger_spec_text);
    let cancel_spec_text_clone = cancel_spec_text.clone();
    let manager_spec_text_clone = Arc::new(manager_spec_text);
    tokio::spawn(async move {
        let _ = process_utterance(
            &utterance_clone2,
            audio_tx_spec_text,
            event_tx_spec_text,
            processors_clone2,
            Arc::new(vec![]),
            tts_clone2,
            None,
            None,
            None,
            cancel_spec_text_clone,
            None,
            None,
            ledger_spec_text_clone,
            Arc::new(AtomicBool::new(false)),
            Some(manager_spec_text_clone),
            None,
        )
        .await;
    });

    let mut first_audio_spec_text = None;
    while let Some(chunk) = audio_rx_spec_text.recv().await {
        if !chunk.samples.is_empty() {
            first_audio_spec_text = Some(start_spec_text.elapsed());
            break;
        }
    }
    let ttf_spec_text =
        first_audio_spec_text.expect("Should receive audio in speculative text run");

    // 3. Speculative Run (Text + Audio Speculation)
    let config_spec_audio = crate::config::SpeculativeConfig {
        enabled: true,
        speculative_tts: true,
        min_partial_interval_ms: 0,
    };
    let manager_spec_audio = SpeculativeSessionManager::new(
        std::sync::Arc::clone(&processors),
        std::sync::Arc::<LatencyMockTts>::clone(&tts),
        config_spec_audio,
    );
    manager_spec_audio.start_turn().await;
    manager_spec_audio.handle_partial("what is the cost").await;

    let start_wait_audio = std::time::Instant::now();
    loop {
        let state = manager_spec_audio.state.lock().await;
        if !state.buffered_audio.is_empty() {
            break;
        }
        assert!(
            start_wait_audio.elapsed() <= std::time::Duration::from_millis(1500),
            "Timed out waiting for speculative audio chunks"
        );
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let (audio_tx_spec_audio, mut audio_rx_spec_audio) = mpsc::channel::<AudioChunk>(32);
    let (event_tx_spec_audio, _event_rx_spec_audio) = mpsc::channel::<ServerMessage>(32);
    let ledger_spec_audio = Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
    let cancel_spec_audio = CancellationToken::new();

    let start_spec_audio = std::time::Instant::now();
    let utterance_clone3 = utterance.clone();
    let processors_clone3 = std::sync::Arc::clone(&processors);
    let tts_clone3 = std::sync::Arc::clone(&tts);
    let ledger_spec_audio_clone = std::sync::Arc::clone(&ledger_spec_audio);
    let cancel_spec_audio_clone = cancel_spec_audio.clone();
    let manager_spec_audio_clone = Arc::new(manager_spec_audio);
    tokio::spawn(async move {
        let _ = process_utterance(
            &utterance_clone3,
            audio_tx_spec_audio,
            event_tx_spec_audio,
            processors_clone3,
            Arc::new(vec![]),
            tts_clone3,
            None,
            None,
            None,
            cancel_spec_audio_clone,
            None,
            None,
            ledger_spec_audio_clone,
            Arc::new(AtomicBool::new(false)),
            Some(manager_spec_audio_clone),
            None,
        )
        .await;
    });

    let mut first_audio_spec_audio = None;
    while let Some(chunk) = audio_rx_spec_audio.recv().await {
        if !chunk.samples.is_empty() {
            first_audio_spec_audio = Some(start_spec_audio.elapsed());
            break;
        }
    }
    let ttf_spec_audio =
        first_audio_spec_audio.expect("Should receive audio in speculative audio run");

    println!("\n=======================================================");
    println!("         AUTOMATED SPECULATIVE SPEEDUP PROOF");
    println!("=======================================================");
    println!("Sequential TTFA:            {ttf_seq:?}");
    println!("Speculative Text-only TTFA:  {ttf_spec_text:?}");
    println!("Speculative Text+Audio TTFA: {ttf_spec_audio:?}");
    println!("-------------------------------------------------------");

    let speedup_text = ttf_seq.as_secs_f64() / ttf_spec_text.as_secs_f64();
    let speedup_audio = ttf_seq.as_secs_f64() / ttf_spec_audio.as_secs_f64();

    println!("Text Speculation Speedup:   {speedup_text:.2}x");
    println!("Audio Speculation Speedup:  {speedup_audio:.2}x");
    println!("=======================================================");

    assert!(
        speedup_text >= 1.5,
        "Expected speculative text-only to be at least 1.5x faster, got {speedup_text:.2}x"
    );
    assert!(
        speedup_audio >= 6.0,
        "Expected speculative audio to be at least 6x faster, got {speedup_audio:.2}x"
    );
}
