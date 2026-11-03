//! The Parakeet recogniser against the real model, built through
//! `factory()` as a pipeline builds it.
//!
//! The model is not in the repository, so these tests are ignored; asked
//! for without it they fail naming the directory. Point
//! `PARAKEET_MODEL_DIR` at it (default `voice-pipes/models/parakeet-tdt`),
//! `PARAKEET_PRECISION=fp32` at a full-precision export, and run in
//! release:
//!
//! ```sh
//! cargo test --release -p voice-pipes-stt-parakeet --test provider -- --ignored --test-threads=1
//! ```
//!
//! The speech is the recorded fixtures in `voice-pipes/fixtures`, whose
//! transcripts `manifest.json` gives.

use std::path::PathBuf;
use std::sync::OnceLock;

use voice_pipes_sdk::{
    AudioChunk, AudioFrame, BuildContext, RecognitionRequest, SttConfig, SttProvider,
};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn model_dir() -> PathBuf {
    std::env::var_os("PARAKEET_MODEL_DIR").map_or_else(
        || workspace().join("voice-pipes/models/parakeet-tdt"),
        PathBuf::from,
    )
}

fn build(dir: PathBuf) -> Box<dyn SttProvider> {
    assert!(
        dir.join("nemo128.onnx").exists(),
        "no Parakeet model in {}: set PARAKEET_MODEL_DIR",
        dir.display()
    );
    let mut cfg = SttConfig::new("parakeet-tdt").with_model_dir(Some(dir));
    // `PARAKEET_PRECISION=fp32` loads the full-precision export.
    if let Ok(precision) = std::env::var("PARAKEET_PRECISION") {
        cfg.options.insert("precision".into(), precision.into());
    }
    voice_pipes_stt_parakeet::factory()
        .build(&cfg, &BuildContext::new())
        .unwrap_or_else(|e| panic!("Parakeet builds: {e}"))
}

/// One provider for the suite: loading the three graphs takes seconds.
fn stt() -> &'static dyn SttProvider {
    static STT: OnceLock<Box<dyn SttProvider>> = OnceLock::new();
    STT.get_or_init(|| build(model_dir())).as_ref()
}

/// A recorded fixture, 16-bit mono.
fn fixture(name: &str) -> AudioFrame {
    let path = workspace().join(format!("voice-pipes/fixtures/{name}.wav"));
    let mut reader =
        hound::WavReader::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let spec = reader.spec();
    let samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .expect("16-bit samples");
    AudioFrame {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    }
}

fn silence(rate: u32, channels: u16, secs: f32) -> AudioFrame {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a short test duration"
    )]
    let n = (rate as f32 * secs) as usize * usize::from(channels);
    AudioFrame {
        samples: vec![0; n],
        sample_rate: rate,
        channels,
    }
}

async fn text_of(frame: &AudioFrame) -> String {
    stt()
        .transcribe(frame)
        .await
        .expect("transcribes")
        .text
        .to_lowercase()
}

// ── Silence and noise ───────────────────────────────────────

/// Digital silence may draw a filler token or two, never words.
#[tokio::test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn silence_yields_no_words() {
    for secs in [2.0, 0.2] {
        let text = text_of(&silence(16_000, 1, secs)).await;
        assert!(text.split_whitespace().count() <= 2, "{secs} s: {text:?}");
    }
}

/// A pure tone is not speech.
#[tokio::test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn a_pure_tone_yields_no_sentence() {
    let samples = (0..16_000_u32)
        .map(|i| {
            let t = f64::from(i) / 16_000.0;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "within i16 by its amplitude"
            )]
            let s = (f64::sin(2.0 * std::f64::consts::PI * 440.0 * t) * 16_000.0) as i16;
            s
        })
        .collect();
    let text = text_of(&AudioFrame {
        samples,
        sample_rate: 16_000,
        channels: 1,
    })
    .await;
    assert!(text.split_whitespace().count() <= 3, "{text:?}");
}

/// Audio at the rates and channel counts a client sends is converted to
/// what the model takes.
#[tokio::test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn other_rates_and_stereo_are_accepted() {
    for (rate, channels) in [(48_000, 1), (48_000, 2), (44_100, 1)] {
        let text = text_of(&silence(rate, channels, 1.0)).await;
        assert!(text.trim().is_empty(), "{rate} Hz x{channels}: {text:?}");
    }
}

// ── Recorded speech ─────────────────────────────────────────

/// Each fixture comes back with the words its manifest gives.
#[tokio::test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn the_recorded_fixtures_come_back_as_spoken() {
    for (name, words) in [
        ("hello-how-are-you", &["hello", "how", "you"][..]),
        ("short-yes", &["yes"]),
        ("mid-sentence-pause", &["thinking", "other"]),
    ] {
        let text = text_of(&fixture(name)).await;
        for word in words {
            assert!(
                text.contains(word),
                "{name}: {word:?} missing from {text:?}"
            );
        }
    }
    let three = text_of(&fixture("short-about-three")).await;
    assert!(three.contains("three") || three.contains('3'), "{three:?}");
}

/// Speech resampled to 48 kHz, as the playback-to-microphone path delivers
/// it, still transcribes.
#[tokio::test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn speech_at_48_khz_transcribes() {
    let audio = voice_pipes_sdk::audio::resample::resample(fixture("hello-how-are-you"), 48_000)
        .expect("16 to 48 kHz");
    let text = text_of(&audio).await;
    assert!(text.contains("hello"), "{text:?}");
}

#[test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
fn it_names_itself_and_its_rate() {
    assert_eq!(stt().name(), "parakeet-tdt");
    assert_eq!(stt().expected_sample_rate(), 16_000);
}

/// The provider reports where its sessions run, decided in-process from
/// `VP_ORT_EP`: the session diagnostic shows a device badge for it.
#[tokio::test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn it_reports_its_device() {
    let device = stt().device_info().await.expect("a device report");
    assert!(matches!(device.kind.as_str(), "cpu" | "cuda"), "{device:?}");
    assert_eq!(device.source.as_deref(), Some("config"));
}

// ── Streaming against batch ─────────────────────────────────

/// The pseudo-streaming path ends a turn with the transcript the batch path
/// gives for the same audio: the pipeline only ever uses
/// `transcribe_stream`, so a regression there is invisible to the tests
/// above. `VP_TEST_UTTERANCE_WAV` substitutes a recording of your own.
#[tokio::test]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn a_streamed_utterance_ends_as_its_batch_transcript() {
    let audio = std::env::var_os("VP_TEST_UTTERANCE_WAV").map_or_else(
        || fixture("hello-how-are-you"),
        |p| {
            let p = PathBuf::from(p);
            let mut r = hound::WavReader::open(&p).expect("utterance wav");
            let spec = r.spec();
            AudioFrame {
                samples: r
                    .samples::<i16>()
                    .collect::<Result<_, _>>()
                    .expect("16-bit"),
                sample_rate: spec.sample_rate,
                channels: spec.channels,
            }
        },
    );
    let batch = stt().transcribe(&audio).await.expect("batch transcription");

    // As the pipeline feeds it: 20 ms chunks, then an empty end-of-utterance
    // sentinel.
    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel(256);
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel(64);
    let rate = audio.sample_rate;
    let feed = tokio::spawn(async move {
        for chunk in audio.samples.chunks((rate as usize / 50).max(1)) {
            let _ = chunk_tx
                .send(AudioChunk {
                    samples: chunk.to_vec(),
                    is_last: false,
                })
                .await;
        }
        let _ = chunk_tx
            .send(AudioChunk {
                samples: Vec::new(),
                is_last: true,
            })
            .await;
    });
    stt()
        .transcribe_stream(
            RecognitionRequest::default()
                .with_request_id("parity".into())
                .with_sample_rate(rate),
            chunk_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("streaming transcription");
    feed.await.expect("feeder");

    let mut final_text = None;
    while let Some(r) = result_rx.recv().await {
        if r.is_final {
            final_text = Some(r.text);
        }
    }
    assert_eq!(
        final_text.expect("a final streaming result").trim(),
        batch.text.trim()
    );
}
