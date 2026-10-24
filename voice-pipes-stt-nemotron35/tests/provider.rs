//! The provider as the pipeline drives it: a factory that names its own
//! configuration faults, and a stream that turns utterances into finals
//! carrying the reference engine's text, one after another on the same
//! sessions.
//!
//! The model-backed tests are skipped without the export under
//! `voice-pipes/models/nemotron-3.5`.

use std::collections::HashMap;
use std::path::PathBuf;

use voice_pipes_sdk::{
    AudioChunk, AudioFrame, RecognitionRequest, SttCapability, SttConfig, SttProvider, WarmUpScope,
};
use voice_pipes_stt_nemotron35::factory;

/// The model under test: `NEMOTRON35_MODEL_DIR` when set, so a candidate
/// export can be gated before it is released, else the installed one.
fn model_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("NEMOTRON35_MODEL_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../voice-pipes/models/nemotron-3.5"),
        PathBuf::from,
    );
    dir.join("nemotron_onnx_config.json")
        .is_file()
        .then_some(dir)
}

fn fixture(name: &str) -> Vec<i16> {
    let path = format!(
        "{}/../voice-pipes/fixtures/{name}.wav",
        env!("CARGO_MANIFEST_DIR")
    );
    hound::WavReader::open(&path)
        .expect("fixture wav")
        .samples::<i16>()
        .map(|s| s.unwrap())
        .collect()
}

fn config(model_dir: Option<PathBuf>, options: &[(&str, serde_json::Value)]) -> SttConfig {
    SttConfig::new("nemotron-3.5")
        .with_model_dir(model_dir)
        .with_options(
            options
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect::<HashMap<_, _>>(),
        )
}

#[test]
fn the_factory_names_a_missing_model_dir() {
    let errs = factory().validate(&config(None, &[]));
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "model_dir");
}

#[test]
fn the_factory_rejects_a_zero_pool_and_a_bad_thread_count() {
    let errs = factory().validate(&config(
        Some(PathBuf::from("/nowhere")),
        &[
            ("pool_size", serde_json::json!(0)),
            ("threads", serde_json::json!("four")),
        ],
    ));
    let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
    assert!(fields.contains(&"model_dir"), "{errs:?}");
    assert!(fields.contains(&"stt.pool_size"), "{errs:?}");
    assert!(fields.contains(&"stt.threads"), "{errs:?}");
}

/// `threads = 0` leaves the count to ONNX Runtime, as in every other local
/// model; it is not refused, while a pool of none is.
#[test]
fn zero_threads_is_the_runtime_default_and_accepted() {
    let errs = factory().validate(&config(
        Some(PathBuf::from("/nowhere")),
        &[("threads", serde_json::json!(0))],
    ));
    let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
    assert!(!fields.contains(&"stt.threads"), "{errs:?}");
}

#[test]
fn the_factory_is_local_and_answers_to_its_aliases() {
    let f = factory();
    assert!(f.is_local());
    assert_eq!(f.name(), "nemotron-3.5");
    assert!(f.aliases().contains(&"nemotron35"));
}

#[test]
#[ignore = "needs the Nemotron 3.5 model in voice-pipes/models/nemotron-3.5"]
fn the_factory_rejects_a_locale_the_model_lacks() {
    let Some(dir) = model_dir() else {
        panic!(
            "no Nemotron 3.5 model: run voice-pipes/scripts/download_nemotron35.py or set NEMOTRON35_MODEL_DIR"
        )
    };
    let errs = factory().validate(&config(
        Some(dir.clone()),
        &[("language", serde_json::json!("xx-XX"))],
    ));
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "language");
    assert!(
        factory()
            .validate(&config(
                Some(dir),
                &[("language", serde_json::json!("hr-HR"))]
            ))
            .is_empty()
    );
}

#[test]
fn the_schema_names_every_option_with_its_default_or_value() {
    let cfg = config(
        Some(PathBuf::from("models/nemotron-3.5")),
        &[
            ("language", serde_json::json!("hr-HR")),
            ("extra", serde_json::json!(1)),
        ],
    );
    let s = factory().provider_schema(&cfg);
    let keys: Vec<&str> = s.descriptors.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "provider",
            "model_dir",
            "language",
            "pool_size",
            "threads",
            "extra"
        ]
    );
    assert_eq!(s.current_values["language"], "hr-HR");
    assert_eq!(s.current_values["pool_size"], 2);
    assert_eq!(s.current_values["threads"], 4);
    assert_eq!(s.current_values["extra"], 1);
    assert!(
        s.current_values["model_dir"]
            .as_str()
            .unwrap()
            .ends_with("nemotron-3.5")
    );
}

fn build(dir: PathBuf, language: &str) -> Box<dyn SttProvider> {
    factory()
        .build(
            &config(
                Some(dir),
                &[
                    ("pool_size", serde_json::json!(1)),
                    ("threads", serde_json::json!(4)),
                    ("language", serde_json::json!(language)),
                ],
            ),
            &voice_pipes_sdk::BuildContext::new(),
        )
        .expect("build")
}

/// Feed one fixture as a live client would: 100 ms pieces, `is_last` on
/// the final one. Returns every result the provider sent for it.
async fn stream_utterances(
    provider: &dyn SttProvider,
    language: Option<&str>,
    clips: &[&str],
) -> Vec<voice_pipes_sdk::TranscriptionResult> {
    // A small buffer keeps the feeder at most a few pieces ahead, so the
    // provider runs mid-utterance and interims are exercised.
    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(2);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    let req = RecognitionRequest::default()
        .with_request_id("t".into())
        .with_language(language.map(str::to_string))
        .with_sample_rate(16_000);
    let clips: Vec<Vec<i16>> = clips.iter().map(|c| fixture(c)).collect();
    let feeder = tokio::spawn(async move {
        for pcm in clips {
            let pieces: Vec<&[i16]> = pcm.chunks(1600).collect();
            let n = pieces.len();
            for (i, piece) in pieces.into_iter().enumerate() {
                audio_tx
                    .send(AudioChunk {
                        samples: piece.to_vec(),
                        is_last: i + 1 == n,
                    })
                    .await
                    .unwrap();
            }
        }
    });
    let collector = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(r) = rx.recv().await {
            out.push(r);
        }
        out
    });
    provider
        .transcribe_stream(req, audio_rx, tx, cancel)
        .await
        .expect("stream");
    feeder.await.unwrap();
    collector.await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the Nemotron 3.5 model in voice-pipes/models/nemotron-3.5"]
async fn a_stream_yields_one_final_per_utterance_with_the_reference_text() {
    let Some(dir) = model_dir() else {
        panic!(
            "no Nemotron 3.5 model: run voice-pipes/scripts/download_nemotron35.py or set NEMOTRON35_MODEL_DIR"
        )
    };
    let provider = build(dir, "en-US");
    assert_eq!(provider.warm_up_scope(), WarmUpScope::PerProcess);
    assert_eq!(provider.concurrent_capacity(), Some(1));
    assert!(provider.capabilities().contains(&SttCapability::Streaming));
    provider.warm_up().await.expect("warm-up");

    // `short-yes` is the utterance the bare engine decodes to nothing: the
    // provider's trailing silence is what makes it a word. It comes out
    // lowercase here because the engine carries the previous utterance as
    // context and the model reads it as a continuation; alone (the batch
    // test below) it is "Yes".
    let results = stream_utterances(
        provider.as_ref(),
        Some("en-US"),
        &["hello-how-are-you", "short-yes", "quiet-speech"],
    )
    .await;
    let finals: Vec<_> = results.iter().filter(|r| r.is_final).collect();
    assert_eq!(finals.len(), 3, "{results:#?}");
    assert_eq!(finals[0].text, "Hello how are you");
    assert_eq!(finals[0].segment_index, 0);
    assert_eq!(finals[1].text, "yes");
    assert_eq!(finals[1].segment_index, 1);
    assert_eq!(finals[2].text, "can you hear me now");
    assert_eq!(finals[2].segment_index, 2);
    assert_eq!(finals[0].language.as_deref(), Some("en-US"));
    let m = finals[0]
        .inference_metrics
        .as_ref()
        .expect("metrics on the final");
    assert!(m.inference_count.unwrap_or(0) > 0);
    assert!(m.rtfx.unwrap_or(0.0) > 0.0);
    // Interims: text grows toward the final, and every one belongs to its segment.
    let interims: Vec<_> = results
        .iter()
        .filter(|r| !r.is_final && r.segment_index == 0)
        .collect();
    assert!(!interims.is_empty(), "no interim results");
    assert!(interims.iter().all(|r| !r.text.is_empty()));
    let identity = provider.model_identity().await.expect("identity");
    assert_eq!(identity.id, "nvidia/nemotron-3.5-asr-streaming-0.6b");
    assert!(provider.device_info().await.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the Nemotron 3.5 model in voice-pipes/models/nemotron-3.5"]
async fn the_session_language_overrides_the_pipeline_default() {
    let Some(dir) = model_dir() else {
        panic!(
            "no Nemotron 3.5 model: run voice-pipes/scripts/download_nemotron35.py or set NEMOTRON35_MODEL_DIR"
        )
    };
    // Built for Croatian, asked for English: the request wins.
    let provider = build(dir, "hr-HR");
    let results = stream_utterances(provider.as_ref(), Some("en-US"), &["hello-how-are-you"]).await;
    let last = results.last().expect("a final");
    assert!(last.is_final);
    assert_eq!(last.text, "Hello how are you");
    assert_eq!(last.language.as_deref(), Some("en-US"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the Nemotron 3.5 model in voice-pipes/models/nemotron-3.5"]
async fn a_batch_transcription_matches_the_stream() {
    let Some(dir) = model_dir() else {
        panic!(
            "no Nemotron 3.5 model: run voice-pipes/scripts/download_nemotron35.py or set NEMOTRON35_MODEL_DIR"
        )
    };
    let provider = build(dir, "en-US");
    let frame = AudioFrame {
        samples: fixture("hello-how-are-you"),
        sample_rate: 16_000,
        channels: 1,
    };
    let r = provider.transcribe(&frame).await.expect("transcribe");
    assert!(r.is_final);
    assert_eq!(r.text, "Hello how are you");
    let short = AudioFrame {
        samples: fixture("short-yes"),
        sample_rate: 16_000,
        channels: 1,
    };
    assert_eq!(
        provider.transcribe(&short).await.expect("transcribe").text,
        "Yes"
    );
    let wrong_rate = AudioFrame {
        samples: vec![0; 800],
        sample_rate: 8_000,
        channels: 1,
    };
    assert!(provider.transcribe(&wrong_rate).await.is_err());
}
