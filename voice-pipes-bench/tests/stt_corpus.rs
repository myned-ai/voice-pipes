//! Speech-recognition accuracy over a corpus of WAV files with reference
//! transcripts, scored with the benchmark's WER/CER metrics.
//!
//! The per-turn diagnostic grades latency only, so a provider that answers
//! fast with the wrong words reads as healthy. This measures the other half:
//! point it at a model and a corpus and it prints a per-entry table plus the
//! mean error rates, grouped by the corpus's `source` field (e.g. clean
//! synthetic speech vs real microphone recordings, which stress a local model
//! very differently).
//!
//! ```sh
//! VP_STT_CORPUS=models/stt-corpus-hr/corpus.json \
//! VP_TEST_PARAKEET_DIR=models/parakeet-tdt-v3 \
//!   cargo test --release -p voice-pipes-bench --test stt_corpus -- --ignored --nocapture
//! ```
//!
//! Corpus format: a JSON array of `{id, source, wav, reference}`, with paths
//! relative to the `voice-pipes/` directory.
//!
//! Each source has its own mean-WER budget, since one mean over clean speech
//! and real microphone turns says little about either: `synthetic` 0.15 and
//! `microphone` 0.85 by default (see [`default_budget`]), any other source
//! `VP_STT_MAX_WER` (default 0.35), and `VP_STT_MAX_WER_<SOURCE>` (the source
//! upper-cased) overrides one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use voice_pipes::audio::wav_reader::read_wav;
use voice_pipes::config::SttConfig;
use voice_pipes::stt::BuildContext;
use voice_pipes_bench::scoring::{compute_cer, compute_wer};

#[derive(Debug, Deserialize)]
struct CorpusEntry {
    id: String,
    /// Free-form grouping label, e.g. `synthetic` or `microphone`.
    source: String,
    wav: PathBuf,
    reference: String,
}

/// Corpus paths are written relative to `voice-pipes/`; the test runs from
/// the crate root, so resolve against the workspace sibling too.
fn resolve(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    let sibling = Path::new("../voice-pipes").join(path);
    if sibling.exists() {
        return sibling;
    }
    path.to_path_buf()
}

/// The mean WER a source must stay under unless `VP_STT_MAX_WER_<SOURCE>`
/// says otherwise.
///
/// Measured on the Croatian corpus with Parakeet v3 (2026-10-04): synthetic
/// speech 0.08, so 0.15 catches a real regression in the recogniser or the
/// audio path. Microphone 0.78: the model detects the language itself and,
/// on short quiet turns of a few words, picks English or Portuguese ("Bog,
/// kako si?" came back as "Bacusy"); trimming the turns' silence did not
/// help (0.90). It has no way to be told the language, which is why
/// Croatian pipelines use Google's recogniser. 0.85 keeps that from sliding
/// further unnoticed.
fn default_budget(source: &str) -> f64 {
    match source {
        "synthetic" => 0.15,
        "microphone" => 0.85,
        _ => env_f64("VP_STT_MAX_WER", 0.35),
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::test]
#[ignore = "requires model files and a corpus on disk"]
async fn parakeet_transcribes_the_corpus_within_the_error_budget() {
    let Some(corpus_path) = std::env::var("VP_STT_CORPUS").ok() else {
        panic!("set VP_STT_CORPUS to a corpus manifest")
    };
    let corpus_path = resolve(Path::new(&corpus_path));
    let model_dir = resolve(Path::new(
        &std::env::var("VP_TEST_PARAKEET_DIR")
            .unwrap_or_else(|_| "models/parakeet-tdt".to_string()),
    ));
    assert!(
        model_dir.join("nemo128.onnx").exists(),
        "no Parakeet model in {}: set VP_TEST_PARAKEET_DIR",
        model_dir.display()
    );
    let entries: Vec<CorpusEntry> =
        serde_json::from_str(&std::fs::read_to_string(&corpus_path).expect("read corpus manifest"))
            .expect("corpus manifest parses");

    let stt = voice_pipes_stt_parakeet::factory()
        .build(
            &SttConfig::new("parakeet-tdt").with_model_dir(Some(model_dir.clone())),
            &BuildContext::new(),
        )
        .expect("load parakeet model");
    let mut by_source: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();

    println!(
        "\ncorpus {} | model {}",
        corpus_path.display(),
        model_dir.display()
    );
    println!(
        "{:<18} {:<11} {:>6} {:>6}  transcript",
        "entry", "source", "wer", "cer"
    );
    for entry in &entries {
        let wav = resolve(&entry.wav);
        let frame = read_wav(&wav).unwrap_or_else(|e| panic!("read {}: {e}", wav.display()));
        let result = stt.transcribe(&frame).await.expect("transcription");
        let wer = compute_wer(&entry.reference, &result.text);
        let cer = compute_cer(&entry.reference, &result.text);
        by_source
            .entry(entry.source.clone())
            .or_default()
            .push((wer, cer));
        println!(
            "{:<18} {:<11} {:>6.2} {:>6.2}  {}",
            entry.id,
            entry.source,
            wer,
            cer,
            result.text.chars().take(70).collect::<String>()
        );
    }

    let mean = |v: &[(f64, f64)], f: fn(&(f64, f64)) -> f64| -> f64 {
        v.iter().map(f).sum::<f64>() / v.len() as f64
    };
    println!(
        "\n{:<18} {:>5} {:>6} {:>6} {:>7}",
        "source", "n", "wer", "cer", "budget"
    );
    let mut over_budget = Vec::new();
    for (source, rows) in &by_source {
        let wer = mean(rows, |r| r.0);
        let budget = env_f64(
            &format!("VP_STT_MAX_WER_{}", source.to_uppercase()),
            default_budget(source),
        );
        println!(
            "{:<18} {:>5} {:>6.2} {:>6.2} {:>7.2}",
            source,
            rows.len(),
            wer,
            mean(rows, |r| r.1),
            budget
        );
        if wer > budget {
            over_budget.push(format!("{source}: mean WER {wer:.2} > {budget:.2}"));
        }
    }
    let all: Vec<(f64, f64)> = by_source.values().flatten().copied().collect();
    println!(
        "{:<18} {:>5} {:>6.2} {:>6.2}\n",
        "all",
        all.len(),
        mean(&all, |r| r.0),
        mean(&all, |r| r.1)
    );

    assert!(
        over_budget.is_empty(),
        "over budget for {}: {}",
        model_dir.display(),
        over_budget.join("; ")
    );
}
