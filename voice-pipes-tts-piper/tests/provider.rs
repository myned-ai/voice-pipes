//! The Piper voice against the real model, built through `factory()` as a
//! pipeline builds it.
//!
//! The voice is not in the repository, and phonemes come from espeak-ng, so
//! these tests are ignored; asked for without either they fail naming it.
//! Point `PIPER_MODEL` at a `.onnx` with its `.onnx.json` beside it (default
//! `voice-pipes/models/en_US-lessac-medium.onnx`):
//!
//! ```sh
//! cargo test --release -p voice-pipes-tts-piper --test provider -- --ignored
//! ```

use std::path::PathBuf;

use voice_pipes_sdk::{BuildContext, SynthesisRequest, TtsConfig, TtsProvider};

fn model() -> PathBuf {
    std::env::var_os("PIPER_MODEL").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../voice-pipes/models/en_US-lessac-medium.onnx")
        },
        PathBuf::from,
    )
}

fn config() -> TtsConfig {
    let model = model();
    assert!(
        model.exists(),
        "no Piper voice at {}: set PIPER_MODEL",
        model.display()
    );
    TtsConfig::new("piper").with_model(Some(model))
}

fn build() -> Box<dyn TtsProvider> {
    voice_pipes_tts_piper::factory()
        .build(&config(), &BuildContext::new())
        .unwrap_or_else(|e| panic!("Piper builds: {e}"))
}

/// A sentence comes back as audio at the rate the factory reports for the
/// voice before building it.
#[tokio::test]
#[ignore = "needs the Piper voice and espeak-ng: set PIPER_MODEL"]
async fn a_sentence_is_spoken_at_the_voices_rate() {
    let rate = voice_pipes_tts_piper::factory()
        .output_sample_rate(&config())
        .expect("the voice's config names its rate");
    let audio = build().synthesize("Hello world.").await.expect("synthesis");
    assert_eq!(audio.sample_rate, rate);
    // Well over a tenth of a second of sound, and not silence.
    assert!(
        audio.samples.len() > rate as usize / 10,
        "{}",
        audio.samples.len()
    );
    assert!(audio.samples.iter().any(|&s| s.unsigned_abs() > 1000));
}

/// The provider reports where the voice runs, decided in-process from
/// `VP_ORT_EP`: the session diagnostic shows a device badge for it.
#[tokio::test]
#[ignore = "needs the Piper voice: set PIPER_MODEL"]
async fn it_reports_its_device() {
    let device = build().device_info().await.expect("a device report");
    assert!(matches!(device.kind.as_str(), "cpu" | "cuda"), "{device:?}");
    assert_eq!(device.source.as_deref(), Some("config"));
}

/// Streamed text is spoken sentence by sentence: the stream carries both
/// sentences, more audio than the first alone, in several frames. Piper
/// draws its phoneme durations with noise, so lengths are not compared
/// exactly.
#[tokio::test]
#[ignore = "needs the Piper voice and espeak-ng: set PIPER_MODEL"]
async fn streamed_text_is_spoken_sentence_by_sentence() {
    let tts = build();
    let first = tts
        .synthesize("Good morning.")
        .await
        .expect("first")
        .samples
        .len();

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    tts.synthesize_stream(
        SynthesisRequest::new("Good morning. How are you?"),
        tx,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("streamed synthesis");
    let (mut frames, mut streamed) = (0, 0);
    while let Some(frame) = rx.recv().await {
        frames += 1;
        streamed += frame.samples.len();
    }
    assert!(frames > 1, "{frames} frames");
    assert!(
        streamed > first * 3 / 2,
        "{streamed} samples against {first} for the first sentence"
    );
}

/// Text espeak-ng gives no phonemes for ("..." comes back empty) is an
/// error, not a silent success: whole, and streamed.
#[tokio::test]
#[ignore = "needs the Piper voice and espeak-ng: set PIPER_MODEL"]
async fn text_with_no_phonemes_is_an_error() {
    let tts = build();
    let whole = tts.synthesize("...").await;
    assert!(whole.is_err(), "{:?}", whole.map(|a| a.samples.len()));

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let streamed = tts
        .synthesize_stream(
            SynthesisRequest::new("..."),
            tx,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(streamed.is_err(), "{streamed:?}");
    assert!(rx.recv().await.is_none(), "no audio was sent");
}

/// A silent sentence among spoken ones is skipped, not an error; and the
/// warm-up still works.
#[tokio::test]
#[ignore = "needs the Piper voice and espeak-ng: set PIPER_MODEL"]
async fn a_silent_sentence_among_spoken_ones_is_skipped() {
    let tts = build();
    tts.warm_up().await.expect("warm-up");
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    tts.synthesize_stream(
        SynthesisRequest::new("Hello there. ... Goodbye now."),
        tx,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("the spoken sentences carry the request");
    let mut streamed = 0;
    while let Some(frame) = rx.recv().await {
        streamed += frame.samples.len();
    }
    assert!(streamed > 0);
}
