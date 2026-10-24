//! Integration tests for the Kokoro TTS provider.
//!
//! These tests require the Kokoro model files (fetched by the setup wizard)
//! under `voice-pipes/models/`, and `espeak-ng` on `PATH`:
//! - `voice-pipes/models/kokoro-v1.0.onnx`
//! - `voice-pipes/models/voices-v1.0.bin`
//!
//! Run with:
//! ```sh
//! cargo test --release -p voice-pipes-tts-kokoro --test kokoro_integration -- --ignored --test-threads=1
//! ```

use std::path::Path;

use voice_pipes_onnx::ExecutionTarget;
use voice_pipes_sdk::{TextChunkingConfig, TtsProvider};
use voice_pipes_tts_kokoro::KokoroTts;

const MODEL_PATH: &str = "../voice-pipes/models/kokoro-v1.0.onnx";
const VOICES_PATH: &str = "../voice-pipes/models/voices-v1.0.bin";
const DEFAULT_VOICE: &str = "af_heart";

fn have_kokoro_models() -> bool {
    Path::new(MODEL_PATH).exists() && Path::new(VOICES_PATH).exists()
}

#[test]
#[ignore = "requires Kokoro model files"]
fn kokoro_loads_successfully() {
    assert!(have_kokoro_models(), "Kokoro model files not found");
    let tts = KokoroTts::new(
        Path::new(MODEL_PATH),
        Path::new(VOICES_PATH),
        DEFAULT_VOICE,
        TextChunkingConfig::default(),
        ExecutionTarget::from_env(),
    )
    .expect("KokoroTts::new should succeed");

    assert_eq!(tts.sample_rate(), 24_000);
    assert_eq!(tts.name(), "kokoro");
}

#[tokio::test]
#[ignore = "requires Kokoro model files"]
async fn kokoro_synthesizes_audio() {
    assert!(have_kokoro_models(), "Kokoro model files not found");
    let tts = KokoroTts::new(
        Path::new(MODEL_PATH),
        Path::new(VOICES_PATH),
        DEFAULT_VOICE,
        TextChunkingConfig::default(),
        ExecutionTarget::from_env(),
    )
    .unwrap();

    let frame = tts.synthesize("Hello world").await.unwrap();
    assert!(!frame.samples.is_empty(), "should produce audio samples");
    assert_eq!(frame.sample_rate, 24_000);
    assert_eq!(frame.channels, 1);
}

#[tokio::test]
#[ignore = "requires Kokoro model files"]
async fn kokoro_streams_audio_with_hints() {
    assert!(have_kokoro_models(), "Kokoro model files not found");
    let tts = KokoroTts::new(
        Path::new(MODEL_PATH),
        Path::new(VOICES_PATH),
        DEFAULT_VOICE,
        TextChunkingConfig::default(),
        ExecutionTarget::from_env(),
    )
    .unwrap();

    let hints = voice_pipes_sdk::SynthesisHints::default().with_speed(Some(1.2));

    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    tts.synthesize_stream(
        voice_pipes_sdk::SynthesisRequest::new("Hello from Kokoro.").with_hints(Some(&hints)),
        tx,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();

    let mut total_samples = 0;
    while let Some(frame) = rx.recv().await {
        total_samples += frame.samples.len();
    }
    assert!(total_samples > 0, "should stream audio frames");
}

/// Text espeak-ng gives no phonemes for ("..." comes back empty) is an
/// error, not a silent success: whole, and streamed.
#[tokio::test]
#[ignore = "requires Kokoro model files"]
async fn kokoro_text_with_no_phonemes_is_an_error() {
    assert!(have_kokoro_models(), "Kokoro model files not found");
    let tts = KokoroTts::new(
        Path::new(MODEL_PATH),
        Path::new(VOICES_PATH),
        DEFAULT_VOICE,
        TextChunkingConfig::default(),
        ExecutionTarget::from_env(),
    )
    .unwrap();

    let whole = tts.synthesize("...").await;
    assert!(whole.is_err(), "{whole:?}");

    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let streamed = tts
        .synthesize_stream(
            voice_pipes_sdk::SynthesisRequest::new("..."),
            tx,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(streamed.is_err(), "{streamed:?}");
    assert!(rx.recv().await.is_none(), "no audio was sent");
}

/// A silent sentence among spoken ones is skipped, not an error.
#[tokio::test]
#[ignore = "requires Kokoro model files"]
async fn kokoro_skips_a_silent_sentence_among_spoken_ones() {
    assert!(have_kokoro_models(), "Kokoro model files not found");
    let tts = KokoroTts::new(
        Path::new(MODEL_PATH),
        Path::new(VOICES_PATH),
        DEFAULT_VOICE,
        TextChunkingConfig::default(),
        ExecutionTarget::from_env(),
    )
    .unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    tts.synthesize_stream(
        voice_pipes_sdk::SynthesisRequest::new("Hello there. ... Goodbye now."),
        tx,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("the spoken sentences carry the request");
    let mut total_samples = 0;
    while let Some(frame) = rx.recv().await {
        total_samples += frame.samples.len();
    }
    assert!(total_samples > 0);
}

#[tokio::test]
#[ignore = "requires Kokoro model files"]
async fn kokoro_config_integration() {
    assert!(have_kokoro_models(), "Kokoro model files not found");

    let toml_str = format!(
        r#"
provider = "kokoro"
model = "{MODEL_PATH}"
voice = "{DEFAULT_VOICE}"
voices = "{VOICES_PATH}"
"#
    );

    let cfg: voice_pipes_sdk::TtsConfig =
        toml::from_str(&toml_str).expect("should parse kokoro config");
    let factory = voice_pipes_tts_kokoro::factory();
    assert!(factory.validate(&cfg).is_empty());

    let tts = factory
        .build(&cfg, &voice_pipes_sdk::BuildContext::new())
        .expect("should build kokoro provider from config");

    assert_eq!(tts.name(), "kokoro");
    assert_eq!(tts.sample_rate(), 24_000);
}
