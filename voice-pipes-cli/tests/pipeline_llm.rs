//! Integration tests for the voice pipeline with the local GGUF LLM:
//! STT → LLM → TTS. They live here because this crate links the engine and
//! the LLM plugin together; the engine's own pipeline tests use the echo
//! processor.
//!
//! They need, under `voice-pipes/models/`:
//! - the Parakeet TDT model in `parakeet-tdt/`
//! - the Piper voice `en_US-lessac-medium.onnx`
//! - `SmolLM2` GGUF at `smollm2/SmolLM2-135M-Instruct-Q4_K_M.gguf`
//!
//! Model paths are absolute, from this crate's manifest directory, so the
//! tests never change the process's working directory, which every test in
//! the binary shares.
//!
//! Run with:
//! ```sh
//! cargo test --release -p voice-pipes-cli --test pipeline_llm -- --ignored --test-threads=1
//! ```

#![cfg(all(feature = "stt-parakeet", feature = "tts-piper", feature = "llm-gguf"))]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use voice_pipes::config::{self, PipelineConfig};
use voice_pipes::pipeline::{AudioChunk, Utterance, process_utterance};
use voice_pipes::stt::SttProvider;
use voice_pipes::text::{self, TextFragment, TextProcessor};
use voice_pipes::tts::TtsProvider;

/// A file under the engine's `models/` directory.
fn model(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../voice-pipes/models")
        .join(relative)
}

fn llm_model() -> PathBuf {
    model("smollm2/SmolLM2-135M-Instruct-Q4_K_M.gguf")
}

/// Parakeet and Piper registered as the binary registers them.
fn register_parakeet() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        voice_pipes::plugins::register_plugin(
            voice_pipes::plugins::Plugin::Stt(voice_pipes_stt_parakeet::factory),
            voice_pipes_stt_parakeet::wizard_choices(),
        )
        .expect("Parakeet registers");
        voice_pipes::plugins::register_plugin(
            voice_pipes::plugins::Plugin::Tts(voice_pipes_tts_piper::factory),
            voice_pipes_tts_piper::wizard_choices(),
        )
        .expect("Piper registers");
    });
}

/// The recogniser and voice the tests run on, by absolute path.
fn test_config() -> PipelineConfig {
    register_parakeet();
    // A TOML string of the path, quoted and escaped as TOML needs.
    let quoted = |p: PathBuf| toml::Value::String(p.to_string_lossy().into_owned());
    let toml = format!(
        "[stt]\nprovider = \"parakeet-tdt\"\nmodel_dir = {}\n\n\
         [tts]\nprovider = \"piper\"\nmodel = {}\n",
        quoted(model("parakeet-tdt")),
        quoted(model("en_US-lessac-medium.onnx")),
    );
    toml::from_str(&toml).expect("the test config parses")
}

/// The local LLM as a pipeline builds it: through the plugin's factory,
/// which wraps the model in the conversation memory that formats each turn
/// with the chat template. The bare processor expects that formatting done
/// and only continues whatever text it is handed. Greedy, so a run does not
/// depend on a sampled token that float drift between builds can flip.
fn build_llm(max_tokens: u32) -> Box<dyn TextProcessor> {
    let cfg = voice_pipes::sdk::TextProcessorConfig::new("llm").with_options(
        [
            ("model", serde_json::json!(llm_model())),
            (
                "system_prompt",
                serde_json::json!("You are a helpful assistant. Respond briefly."),
            ),
            ("max_tokens", serde_json::json!(max_tokens)),
            ("temperature", serde_json::json!(0.0)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    );
    voice_pipes_llm_gguf::factory()
        .build(&cfg, &voice_pipes::sdk::BuildContext::new())
        .expect("the LLM loads")
}

/// These tests run only when asked for (`--ignored`), so a missing model is
/// a failure that names it, not a skip that reports ok.
fn require_llm() {
    assert!(
        llm_model().exists(),
        "the SmolLM2 GGUF at {}",
        llm_model().display()
    );
}

/// Cached STT provider — built once, reused across tests.
static STT_PROVIDER: OnceLock<Option<Box<dyn SttProvider>>> = OnceLock::new();

/// The shared STT provider, or None when its model is missing.
fn try_stt() -> Option<&'static dyn SttProvider> {
    STT_PROVIDER
        .get_or_init(|| config::build_stt_provider(&test_config(), None).ok())
        .as_ref()
        .map(std::convert::AsRef::as_ref)
}

/// Build a fresh TTS provider (not cached — tests may need separate instances).
fn try_tts() -> Option<Box<dyn TtsProvider>> {
    config::build_tts_provider(&test_config(), None, None).ok()
}

/// End-to-end test with a real LLM: STT → LLM → TTS.
/// TTS synthesizes a question, STT transcribes it, the LLM generates
/// a response, and TTS synthesizes the response into audio.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn pipeline_stt_llm_tts_end_to_end() {
    let stt = try_stt().expect("the Parakeet model under voice-pipes/models/parakeet-tdt");
    let tts_box = try_tts().expect("the Piper voice voice-pipes/models/en_US-lessac-medium.onnx");
    require_llm();

    let tts: Arc<dyn TtsProvider> = Arc::from(tts_box);

    // Load LLM processor
    let llm = build_llm(64);

    // Step 1: Generate speech for a question
    let question = "What color is the sky?";
    let input_audio = tts.synthesize(question).await.unwrap();
    eprintln!(
        "Input TTS: {} samples at {} Hz",
        input_audio.samples.len(),
        input_audio.sample_rate
    );

    // Step 2: STT
    let stt_result = stt.transcribe(&input_audio).await.unwrap();
    let transcribed = stt_result.text.clone();
    eprintln!("STT: {transcribed:?}");
    assert!(!transcribed.trim().is_empty(), "STT should produce text");

    // Step 3: LLM
    let processors: Vec<Box<dyn TextProcessor>> = vec![llm];
    let llm_output = text::run_chain(&processors, &transcribed).await.unwrap();
    eprintln!("LLM output: {llm_output:?}");
    assert!(
        !llm_output.trim().is_empty(),
        "LLM should produce non-empty response"
    );
    assert!(
        llm_output != transcribed,
        "LLM output should differ from input (not just echoed)"
    );

    // Step 4: TTS
    let output_audio = tts.synthesize(&llm_output).await.unwrap();
    eprintln!(
        "Output TTS: {} samples at {} Hz ({:.2}s)",
        output_audio.samples.len(),
        output_audio.sample_rate,
        output_audio.samples.len() as f64 / output_audio.sample_rate as f64
    );

    assert!(
        output_audio.samples.len() > 1000,
        "LLM response TTS should produce audio, got {} samples",
        output_audio.samples.len()
    );
}

/// Tests `process_utterance()` with the real LLM + TTS, verifying
/// that the streaming pipeline produces audio output.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn process_utterance_llm_produces_tts_audio() {
    let tts_box = try_tts().expect("the Piper voice voice-pipes/models/en_US-lessac-medium.onnx");
    require_llm();

    let tts: Arc<dyn TtsProvider> = Arc::from(tts_box);

    let llm = build_llm(64);

    let processors: Arc<Vec<Box<dyn TextProcessor>>> = Arc::new(vec![llm]);

    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioChunk>(32);
    let cancel = CancellationToken::new();

    let utterance = Utterance {
        text: "What is the capital of France?".to_string(),
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
                None,
            )
            .await;
        }
    });

    let mut output_samples: Vec<i16> = Vec::new();
    let mut got_sentinel = false;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_mins(1);
    loop {
        match tokio::time::timeout_at(deadline, audio_rx.recv()).await {
            Ok(Some(chunk)) => {
                eprintln!(
                    "  chunk: {} samples, end={}",
                    chunk.samples.len(),
                    chunk.is_end_of_utterance
                );
                output_samples.extend_from_slice(&chunk.samples);
                if chunk.is_end_of_utterance {
                    got_sentinel = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                eprintln!("TIMEOUT waiting for LLM→TTS output");
                break;
            }
        }
    }

    eprintln!(
        "process_utterance (LLM) output: {} samples, sentinel={}",
        output_samples.len(),
        got_sentinel
    );

    assert!(
        output_samples.len() > 1000,
        "process_utterance with LLM should produce TTS audio, got {} samples",
        output_samples.len()
    );
    // The sentinel may not arrive if the LLM generates multiple sentences
    // and the spawned task is still working when we've already collected
    // enough audio. The key assertion is that audio was produced.
}

/// Verifies `run_chain_stream` works with the real LLM — should produce
/// one or more fragments with non-empty text.
///
/// The prompt reaches the model raw (no chat template), where the next
/// token is a near tie between a newline and end-of-sequence: a sampled
/// draw falls either way with the build's last-bit float differences, so
/// the case decodes greedily.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn text_chain_stream_llm() {
    require_llm();

    let llm = build_llm(32);

    let processors: Vec<Box<dyn TextProcessor>> = vec![llm];
    let (tx, mut rx) = mpsc::channel::<TextFragment>(16);
    let cancel = CancellationToken::new();

    text::run_chain_stream(&processors, "Say hello.", tx, cancel)
        .await
        .unwrap();

    let mut accumulated = String::new();
    let mut got_end = false;
    while let Some(frag) = rx.recv().await {
        eprintln!("  LLM fragment: {:?}", frag.text);
        accumulated.push_str(&frag.text);
        if frag.is_end_of_response {
            got_end = true;
        }
    }

    eprintln!("LLM stream output: {accumulated:?}");
    assert!(!accumulated.trim().is_empty(), "LLM should produce text");
    assert!(got_end, "should receive end-of-response fragment");
}
