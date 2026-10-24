//! The driver must produce the reference engine's tokens, exactly.
//!
//! `tests/data/reference_tokens.json` is the community Python engine run
//! over five fixtures at 320 ms, fed in 100 ms pieces as a live client
//! would, for the `en-US` prompt and for `auto`. Token-for-token agreement
//! is the acceptance for WP-C: the encoder cache, the chunk schedule, the
//! attention mask and the greedy loop all have to be right for the ids to
//! match, and a single frame out of place shows as a different sequence.
//!
//! These tests need the model files under `voice-pipes/models/nemotron-3.5`
//! (2.6 GB, gitignored) and are skipped when they are absent, so the rest
//! of the suite runs anywhere.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use voice_pipes_stt_nemotron35::engine::{Engine, Sessions};
use voice_pipes_stt_nemotron35::model::{ModelConfig, Tokens};

#[derive(Deserialize)]
struct Reference {
    tokens: Vec<u32>,
    text: String,
    prompt_id: i64,
    #[serde(default)]
    wav: Option<String>,
}

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

fn fixture_pcm(name: &str) -> Vec<f32> {
    let path = format!(
        "{}/../voice-pipes/fixtures/{name}.wav",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut r = hound::WavReader::open(&path).expect("fixture wav");
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}

fn references(file: &str) -> BTreeMap<String, Reference> {
    let path = format!("{}/tests/data/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn wav_pcm(path: &str) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("wav");
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}

/// Decode every clip of a reference set and collect the mismatches.
fn mismatches(
    dir: &std::path::Path,
    refs: &BTreeMap<String, Reference>,
    pcm_for: impl Fn(&str, &Reference) -> Vec<f32>,
) -> Vec<String> {
    let cfg = ModelConfig::load(dir).unwrap();
    cfg.validate_files(dir).unwrap();
    let tokens = Tokens::load(dir, &cfg).unwrap();
    let mut sessions = Sessions::open(dir, &cfg, 4).unwrap();
    let mut failures = Vec::new();
    for (key, want) in refs {
        let pcm = pcm_for(key, want);
        let mut engine =
            Engine::new(&mut sessions, &cfg, want.prompt_id, tokens.locale_tag_ids()).unwrap();
        for piece in pcm.chunks(1600) {
            engine.accept(piece).unwrap();
        }
        engine.finish().unwrap();
        let got = engine.tokens().to_vec();
        let (text, _) = tokens.decode(&got);
        eprintln!("{key:28} {text:?}");
        if got != want.tokens {
            failures.push(format!(
                "{key}: got {got:?} ({text:?})
{:28} want {:?} ({:?})",
                "", want.tokens, want.text
            ));
        }
    }
    failures
}

#[test]
#[ignore = "needs the Nemotron 3.5 model in voice-pipes/models/nemotron-3.5"]
fn every_fixture_decodes_to_the_reference_engines_tokens() {
    let Some(dir) = model_dir() else {
        panic!(
            "no Nemotron 3.5 model: run voice-pipes/scripts/download_nemotron35.py or set NEMOTRON35_MODEL_DIR"
        )
    };
    let failures = mismatches(&dir, &references("reference_tokens.json"), |key, _| {
        fixture_pcm(key.split_once('/').unwrap().1)
    });
    assert!(
        failures.is_empty(),
        "token sequences differ from the reference:
{}",
        failures.join(
            "
"
        )
    );
}

/// The Croatian corpus, `hr-HR` and `auto`: the same identity, on speech in
/// the locale the plugin exists for, including the clips the model gets
/// badly wrong -- the plugin must reproduce the reference's mistakes too.
#[test]
#[ignore = "needs the Nemotron 3.5 model in voice-pipes/models/nemotron-3.5"]
fn croatian_clips_decode_to_the_reference_engines_tokens() {
    let Some(dir) = model_dir() else {
        panic!(
            "no Nemotron 3.5 model: run voice-pipes/scripts/download_nemotron35.py or set NEMOTRON35_MODEL_DIR"
        )
    };
    let root = format!("{}/../voice-pipes/", env!("CARGO_MANIFEST_DIR"));
    let failures = mismatches(&dir, &references("reference_tokens_hr.json"), |_, want| {
        wav_pcm(&format!("{root}{}", want.wav.as_deref().expect("wav path")))
    });
    assert!(
        failures.is_empty(),
        "token sequences differ from the reference:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn metric_conformance() {
    let run = |mut audio_rx: tokio::sync::mpsc::Receiver<voice_pipes_sdk::AudioChunk>,
               result_tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::TranscriptionResult>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        let chunk = audio_rx.recv().await.unwrap();
        let delay = std::time::Duration::from_millis(20);
        tokio::time::sleep(delay).await;
        let audio_secs = chunk.samples.len() as f64 / 16_000.0;
        let metrics = voice_pipes_sdk::InferenceMetrics::measured(audio_secs, delay, 1)
            .with_delay_floor_ms(80);
        let res = voice_pipes_sdk::TranscriptionResult::new("hello", true)
            .with_segment_index(0)
            .with_inference_metrics(Some(metrics));
        result_tx.send(res).await.unwrap();
        Ok(())
    };
    voice_pipes_sdk::testing::assert_stt_metrics_conformance(
        run,
        vec![0; 16_000],
        16_000,
        std::time::Duration::from_millis(20),
    )
    .await
    .unwrap();
}

