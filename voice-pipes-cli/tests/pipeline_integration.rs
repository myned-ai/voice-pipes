//! Integration tests for the full voice pipeline: STT → `TextProcessor` → TTS,
//! on the default recogniser and voice. They live here because the
//! recogniser is a plugin this crate links; the cases that add the local LLM
//! are in `pipeline_llm.rs`.
//!
//! They need, under `voice-pipes/models/`, the Parakeet TDT model in
//! `parakeet-tdt/` and the Piper voice `en_US-lessac-medium.onnx`. Paths are
//! absolute, so no test changes the working directory the binary shares.
//!
//! Run with:
//! ```sh
//! cargo test --release -p voice-pipes-cli --test pipeline_integration -- --ignored --test-threads=1
//! ```

// Built on the default providers; a build without them has nothing to test here.
#![cfg(all(feature = "stt-parakeet", feature = "tts-piper"))]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Once, OnceLock};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use voice_pipes::config::{self, PipelineConfig};
use voice_pipes::pipeline::{
    AudioChunk, Utterance, llm_task, process_utterance, stt_task, tts_task,
};
use voice_pipes::stt::SttProvider;
use voice_pipes::text::{self, EchoProcessor, TextFragment, TextProcessor};
use voice_pipes::transport::ServerMessage;
use voice_pipes::tts::TtsProvider;

/// A file under the engine's `models/` directory.
fn model(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../voice-pipes/models")
        .join(relative)
}

/// Parakeet and Piper registered as the binary registers them: their
/// factories and wizard entries, and Parakeet as the recogniser a config
/// without `[stt]` falls back to.
fn register_parakeet() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        voice_pipes::plugins::register_plugin(
            voice_pipes::plugins::Plugin::Stt(voice_pipes_stt_parakeet::factory),
            voice_pipes_stt_parakeet::wizard_choices(),
        )
        .expect("Parakeet registers");
        voice_pipes::plugins::register_fallback(voice_pipes::plugins::Stage::Stt, "parakeet-tdt")
            .expect("Parakeet is the fallback");
        voice_pipes::plugins::register_plugin(
            voice_pipes::plugins::Plugin::Tts(voice_pipes_tts_piper::factory),
            voice_pipes_tts_piper::wizard_choices(),
        )
        .expect("Piper registers");
    });
}

/// The default recogniser, from its section-less fallback with the model
/// directory named, and Piper with its model named (it has no default path).
/// Written out rather than read from a local `voice-pipes.toml`, so the
/// suite tests the same providers everywhere.
fn test_config() -> PipelineConfig {
    register_parakeet();
    let quoted = |p: PathBuf| toml::Value::String(p.to_string_lossy().into_owned());
    let mut cfg: PipelineConfig = toml::from_str(&format!(
        "[tts]\nprovider = \"piper\"\nmodel = {}\n",
        quoted(model("en_US-lessac-medium.onnx"))
    ))
    .expect("valid test config");
    // No `[stt]`: the registered default is what the pipeline falls back to.
    assert_eq!(
        config::stt_provider_name(&cfg).as_deref(),
        Some("parakeet-tdt")
    );
    cfg.stt = Some(
        voice_pipes::config::SttConfig::new("parakeet-tdt")
            .with_model_dir(Some(model("parakeet-tdt"))),
    );
    cfg
}

/// Cached STT provider, built once and reused across tests; the error is
/// kept so every test that needs it fails with the reason.
static STT_PROVIDER: OnceLock<Result<Box<dyn SttProvider>, String>> = OnceLock::new();

/// The shared STT provider from the test config.
fn stt() -> &'static dyn SttProvider {
    match STT_PROVIDER
        .get_or_init(|| config::build_stt_provider(&test_config(), None).map_err(|e| e.to_string()))
    {
        Ok(p) => p.as_ref(),
        Err(e) => panic!("no STT provider from the test config: {e}"),
    }
}

/// A fresh TTS provider from the test config (not cached: tests may need
/// separate instances).
fn tts() -> Box<dyn TtsProvider> {
    config::build_tts_provider(&test_config(), None, None)
        .unwrap_or_else(|e| panic!("no TTS provider from the test config: {e}"))
}

// ── Echo processor: STT → Echo → TTS ───────────────────────

/// Full pipeline with `EchoProcessor`: TTS synthesizes input text,
/// STT transcribes it, `EchoProcessor` passes the text through unchanged,
/// then TTS synthesizes the result. Verifies audio is produced.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn pipeline_stt_echo_tts_produces_audio() {
    let stt = stt();
    let tts = tts();
    let tts: Arc<dyn TtsProvider> = Arc::from(tts);

    // Step 1: Generate speech audio from known text
    let input_text = "Hello, how are you today?";
    let input_audio = tts.synthesize(input_text).await.unwrap();
    eprintln!(
        "Input TTS: {} samples at {} Hz ({:.2}s)",
        input_audio.samples.len(),
        input_audio.sample_rate,
        input_audio.samples.len() as f64 / input_audio.sample_rate as f64
    );

    // Step 2: STT — transcribe the audio
    let stt_result = stt.transcribe(&input_audio).await.unwrap();
    let transcribed = stt_result.text.clone();
    eprintln!("STT result: {transcribed:?}");
    assert!(
        !transcribed.trim().is_empty(),
        "STT should produce non-empty text"
    );

    // Step 3: Text processor chain (echo — passthrough)
    let processors: Vec<Box<dyn TextProcessor>> = vec![Box::new(EchoProcessor)];
    let processed = text::run_chain(&processors, &transcribed).await.unwrap();
    eprintln!("Echo result: {processed:?}");
    assert_eq!(
        processed, transcribed,
        "EchoProcessor should return input unchanged"
    );

    // Step 4: TTS — synthesize the processed text
    let output_audio = tts.synthesize(&processed).await.unwrap();
    eprintln!(
        "Output TTS: {} samples at {} Hz ({:.2}s)",
        output_audio.samples.len(),
        output_audio.sample_rate,
        output_audio.samples.len() as f64 / output_audio.sample_rate as f64
    );

    assert!(
        output_audio.samples.len() > 1000,
        "TTS output should contain meaningful audio, got {} samples",
        output_audio.samples.len()
    );
}

// ── Echo processor via streaming pipeline tasks ─────────────

/// Tests the streaming pipeline tasks (`stt_task` → `llm_task` → `tts_task`)
/// wired together with an `EchoProcessor`, using the configured STT + TTS providers.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn pipeline_streaming_stt_echo_tts() {
    let cfg = test_config();
    let tts_box = tts();
    let tts_provider: Arc<dyn TtsProvider> = Arc::from(tts_box);
    // Build a dedicated STT instance for the streaming test (provider may not be Clone)
    let stt_provider: Arc<dyn SttProvider> = match config::build_stt_provider(&cfg, None) {
        Ok(p) => Arc::from(p),
        Err(e) => panic!("STT not available: {e}"),
    };

    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(EchoProcessor)]);

    // Wire: mic_tx → stt_task → llm_task → tts_task → audio_rx
    let (mic_tx, mic_rx) = mpsc::channel::<AudioChunk>(32);
    let (utt_tx, utt_rx) = mpsc::channel::<Utterance>(4);
    let (frag_tx, frag_rx) = mpsc::channel::<TextFragment>(16);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(32);

    let cancel = CancellationToken::new();

    let td = std::sync::Arc::new(voice_pipes::stt::turn::SilenceTimeoutDetector::new(0));
    tokio::spawn(stt_task(
        mic_rx,
        utt_tx,
        None,
        stt_provider,
        td,
        None,
        None,
        0,
        None,
        None,
    ));
    tokio::spawn(llm_task(utt_rx, frag_tx, processors, cancel.clone()));
    let (evt_tx, _evt_rx) = mpsc::channel::<ServerMessage>(8);
    let err_flag = Arc::new(AtomicBool::new(false));
    tokio::spawn(tts_task(
        frag_rx,
        audio_tx,
        tts_provider,
        std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        None,
        None,
        cancel.clone(),
        1,
        None,
        evt_tx,
        err_flag,
    ));

    // Synthesize known text to feed as "microphone" input
    let input_tts =
        config::build_tts_provider(&cfg, None, None).expect("failed to load TTS for input");
    let input_audio = input_tts
        .synthesize("What is the weather like?")
        .await
        .unwrap();

    // Feed the audio as mic chunks (16kHz, split into 20ms chunks)
    let resampled = voice_pipes::audio::resample::resample(input_audio, 16000).unwrap();
    let chunk_size = 320; // 20ms at 16kHz
    let chunks: Vec<&[i16]> = resampled.samples.chunks(chunk_size).collect();
    let num_chunks = chunks.len();

    for (i, chunk) in chunks.into_iter().enumerate() {
        mic_tx
            .send(AudioChunk {
                samples: chunk.to_vec(),
                sample_rate: 16000,
                channels: 1,
                is_end_of_utterance: i == num_chunks - 1,
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
    drop(mic_tx);

    // Collect output audio chunks with a timeout
    let mut output_samples: Vec<i16> = Vec::new();
    let mut got_sentinel = false;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    // Drain until the sentinel; stop on channel close or timeout.
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, audio_rx.recv()).await {
        if chunk.is_end_of_utterance {
            output_samples.extend_from_slice(&chunk.samples);
            got_sentinel = true;
            break;
        }
        output_samples.extend_from_slice(&chunk.samples);
    }

    eprintln!(
        "Pipeline output: {} samples, got_sentinel={}",
        output_samples.len(),
        got_sentinel
    );

    assert!(
        output_samples.len() > 1000,
        "streaming pipeline should produce audio, got {} samples",
        output_samples.len()
    );
    assert!(got_sentinel, "should receive end-of-utterance sentinel");
}

// ── process_utterance with EchoProcessor ────────────────────

/// Tests `process_utterance()` directly with `EchoProcessor` + real Piper TTS.
/// This is the exact function called in live mode after STT produces text.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn process_utterance_echo_produces_tts_audio() {
    let tts_box = tts();

    let tts: Arc<dyn TtsProvider> = Arc::from(tts_box);
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(EchoProcessor)]);

    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(32);
    let cancel = CancellationToken::new();

    let utterance = Utterance {
        text: "Hello world, this is a test.".to_string(),
        utterance_id: 1,
        confidence: Some(0.95),
        stt_latency: None,
        endpointing_latency: None,
        source: voice_pipes::pipeline::UtteranceSource::default(),
        inference_metrics: None,
        committed_at: None,
    };

    // Spawn a task that calls process_utterance (takes &Utterance)
    tokio::spawn({
        let utterance = utterance.clone();
        let (evt_tx, _) = tokio::sync::mpsc::channel(8);
        async move {
            process_utterance(
                &utterance,
                audio_tx,
                evt_tx,
                processors,
                voice_pipes::pipeline::empty_chain(),
                tts,
                None,
                None,
                None,
                cancel,
                None,
                None,
                std::sync::Arc::new(parking_lot::Mutex::new(
                    voice_pipes::pipeline::AudioLedger::default(),
                )),
                std::sync::Arc::new(AtomicBool::new(false)),
                None,
            )
            .await;
        }
    });

    let mut output_samples: Vec<i16> = Vec::new();
    let mut got_sentinel = false;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    // Drain until the sentinel; stop on channel close or timeout.
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, audio_rx.recv()).await {
        if chunk.is_end_of_utterance {
            output_samples.extend_from_slice(&chunk.samples);
            got_sentinel = true;
            break;
        }
        output_samples.extend_from_slice(&chunk.samples);
    }

    eprintln!(
        "process_utterance output: {} samples, sentinel={}",
        output_samples.len(),
        got_sentinel
    );

    assert!(
        output_samples.len() > 1000,
        "process_utterance should produce TTS audio, got {} samples",
        output_samples.len()
    );
    assert!(got_sentinel, "should receive end-of-utterance sentinel");
}

// ── Text processor chain (streaming) with Echo ──────────────

/// Verifies `run_chain_stream` works with `EchoProcessor` — the fragment
/// should contain the original text and be marked as end-of-response.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn text_chain_stream_echo() {
    let processors: Vec<Box<dyn TextProcessor>> = vec![Box::new(EchoProcessor)];
    let (tx, mut rx) = mpsc::channel::<TextFragment>(8);
    let cancel = CancellationToken::new();

    text::run_chain_stream(&processors, "test input", tx, cancel)
        .await
        .unwrap();

    let frag = rx.recv().await.expect("should receive a fragment");
    assert_eq!(frag.text, "test input");
    assert!(frag.is_end_of_response);

    // No more fragments
    assert!(rx.recv().await.is_none());
}

// ── Interrupt: next utterance must not be swallowed ─────────

/// Regression test for barge-in: when the first utterance is cancelled
/// mid-playback, stale audio chunks (including the end-of-utterance
/// sentinel) must not cause the *second* utterance's output to be lost.
///
/// Simulates the orchestrator loop pattern from `run_live()`:
///   1. Spawn `process_utterance` for utterance A on a shared audio channel.
///   2. Cancel A after receiving some audio (simulating barge-in).
///   3. Drain the shared channel (the fix).
///   4. Spawn `process_utterance` for utterance B on the *same* channel.
///   5. Assert B produces full audio output.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn interrupt_does_not_swallow_next_utterance() {
    let tts_box = tts();

    let tts: Arc<dyn TtsProvider> = Arc::from(tts_box);
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(EchoProcessor)]);

    // Shared audio channel — reused across utterances, just like run_live().
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(32);

    // ── Utterance A: start and then cancel (barge-in) ───────
    let cancel_a = CancellationToken::new();
    let handle_a = tokio::spawn({
        let utterance_a = Utterance {
            text: "This is a long sentence that should be interrupted before it finishes playing."
                .into(),
            utterance_id: 1,
            confidence: Some(0.9),
            stt_latency: None,
            endpointing_latency: None,
            source: voice_pipes::pipeline::UtteranceSource::default(),
            inference_metrics: None,
            committed_at: None,
        };
        let tx = audio_tx.clone();
        let procs = std::sync::Arc::clone(&processors);
        let tts = std::sync::Arc::clone(&tts);
        let cancel = cancel_a.clone();
        async move {
            let (evt_tx, _) = tokio::sync::mpsc::channel(8);
            process_utterance(
                &utterance_a,
                tx,
                evt_tx,
                procs,
                voice_pipes::pipeline::empty_chain(),
                tts,
                None,
                None,
                None,
                cancel,
                None,
                None,
                std::sync::Arc::new(parking_lot::Mutex::new(
                    voice_pipes::pipeline::AudioLedger::default(),
                )),
                std::sync::Arc::new(AtomicBool::new(false)),
                None,
            )
            .await;
        }
    });

    // Wait for at least one audio chunk, then cancel.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut a_samples = 0usize;
    loop {
        match tokio::time::timeout_at(deadline, audio_rx.recv()).await {
            Ok(Some(chunk)) => {
                a_samples += chunk.samples.len();
                if a_samples > 500 {
                    // Got enough — simulate barge-in.
                    cancel_a.cancel();
                    break;
                }
            }
            Ok(None) | Err(_) => panic!("utterance A should produce audio before cancel"),
        }
    }

    // Wait for the task to finish, then drain stale chunks (the fix).
    handle_a.abort();
    let _ = handle_a.await;
    while audio_rx.try_recv().is_ok() {}

    eprintln!("utterance A: interrupted after {a_samples} samples");

    // ── Utterance B: must produce full output ───────────────
    let cancel_b = CancellationToken::new();
    let handle_b = tokio::spawn({
        let utterance_b = Utterance {
            text: "Hello world.".into(),
            utterance_id: 2,
            confidence: Some(0.95),
            stt_latency: None,
            endpointing_latency: None,
            source: voice_pipes::pipeline::UtteranceSource::default(),
            inference_metrics: None,
            committed_at: None,
        };
        let tx = audio_tx.clone();
        let procs = std::sync::Arc::clone(&processors);
        let tts = std::sync::Arc::clone(&tts);
        let cancel = cancel_b.clone();
        async move {
            let (evt_tx, _) = tokio::sync::mpsc::channel(8);
            process_utterance(
                &utterance_b,
                tx,
                evt_tx,
                procs,
                voice_pipes::pipeline::empty_chain(),
                tts,
                None,
                None,
                None,
                cancel,
                None,
                None,
                std::sync::Arc::new(parking_lot::Mutex::new(
                    voice_pipes::pipeline::AudioLedger::default(),
                )),
                std::sync::Arc::new(AtomicBool::new(false)),
                None,
            )
            .await;
        }
    });

    let mut b_samples: Vec<i16> = Vec::new();
    let mut b_sentinel = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match tokio::time::timeout_at(deadline, audio_rx.recv()).await {
            Ok(Some(chunk)) => {
                b_samples.extend_from_slice(&chunk.samples);
                if chunk.is_end_of_utterance {
                    b_sentinel = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                eprintln!("TIMEOUT waiting for utterance B output");
                break;
            }
        }
    }

    let _ = handle_b.await;

    eprintln!(
        "utterance B: {} samples, sentinel={}",
        b_samples.len(),
        b_sentinel,
    );

    assert!(
        b_samples.len() > 1000,
        "utterance after interrupt must produce audio, got {} samples",
        b_samples.len()
    );
    assert!(
        b_sentinel,
        "utterance after interrupt must receive end-of-utterance sentinel"
    );
}

/// The pipeline's stage latencies sum to commit-to-first-audio with no
/// measurement.* anomaly (§4.7, P5.3).
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn pipeline_metrics_conformance_stages_sum_and_no_anomalies() {
    let stt = stt();
    let tts = tts();
    let tts: Arc<dyn TtsProvider> = Arc::from(tts);

    let input_text = "Testing metric integrity on the local pipeline.";
    let input_audio = tts.synthesize(input_text).await.unwrap();
    let stt_result = stt.transcribe(&input_audio).await.unwrap();

    let utterance = Utterance {
        text: stt_result.text.clone(),
        utterance_id: 42,
        confidence: Some(0.99),
        stt_latency: Some(std::time::Duration::from_millis(50)),
        source: voice_pipes::pipeline::UtteranceSource::default(),
        endpointing_latency: Some(std::time::Duration::from_millis(150)),
        inference_metrics: stt_result.inference_metrics,
        committed_at: Some(std::time::Instant::now()),
    };

    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(16);
    let (evt_tx, _evt_rx) = mpsc::channel::<ServerMessage>(16);
    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![Box::new(EchoProcessor)]);
    let cancel = CancellationToken::new();

    let drain = tokio::spawn(async move {
        let mut count = 0;
        while let Some(chunk) = audio_rx.recv().await {
            count += chunk.samples.len();
            if chunk.is_end_of_utterance {
                break;
            }
        }
        count
    });

    let snapshot = process_utterance(
        &utterance,
        audio_tx,
        evt_tx,
        processors,
        voice_pipes::pipeline::empty_chain(),
        tts,
        None,
        None,
        None,
        cancel,
        None,
        None,
        Arc::new(parking_lot::Mutex::new(
            voice_pipes::pipeline::AudioLedger::default(),
        )),
        Arc::new(AtomicBool::new(false)),
        None,
    )
    .await;

    let total_samples = drain.await.unwrap();
    assert!(total_samples > 0, "must produce audio samples");

    let tts_span = snapshot.component_by_kind("tts").expect("tts component");
    let llm_span = snapshot.component_by_kind("llm").expect("llm component");

    let llm_wait = llm_span.extras["llm_wait_ms"]
        .as_f64()
        .expect("llm_wait_ms");
    let tts_wait = tts_span.extras["tts_wait_ms"]
        .as_f64()
        .expect("tts_wait_ms");
    let output_wait = tts_span.extras["output_wait_ms"]
        .as_f64()
        .expect("output_wait_ms");
    let commit_fa = tts_span.extras["commit_to_first_audio_ms"]
        .as_f64()
        .expect("commit_to_first_audio_ms");

    let sum = llm_wait + tts_wait + output_wait;
    let diff = (sum - commit_fa).abs();
    assert!(
        diff <= 1.0,
        "stages ({llm_wait} + {tts_wait} + {output_wait} = {sum}) must sum to commit_to_first_audio ({commit_fa}) within 1ms"
    );
}
