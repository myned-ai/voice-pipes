//! Smoke tests for the TTS benchmarking pipeline — Layer-0/3 fixtures.
//!
//! Anchors the contract that the bundled
//! [`examples/benchmark/tts-corpus.toml`](../examples/benchmark/tts-corpus.toml)
//! corpus stays in lock-step with the
//! [`TtsOnlyCorpus`](voice_pipes_bench::types::ScenarioInput) schema.
//! This is the cheapest test that catches schema drift and is the unit the
//! `tts-bench-smoke` CI job exercises.

use std::path::PathBuf;

use serde::Deserialize;

use voice_pipes_bench::types::{TtsContentClass, TtsCorpusEntry, TtsLengthBucket};

/// Top-level shape of `examples/benchmark/tts-corpus.toml`.
#[derive(Debug, Deserialize)]
struct CorpusFile {
    #[expect(
        dead_code,
        reason = "part of the corpus file's shape; the test checks only the entries"
    )]
    schema_version: u32,
    #[expect(
        dead_code,
        reason = "part of the corpus file's shape; the test checks only the entries"
    )]
    language: String,
    entries: Vec<TtsCorpusEntry>,
}

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("benchmark")
        .join("tts-corpus.toml")
}

#[test]
fn tts_corpus_parses_into_typed_entries() {
    let raw = std::fs::read_to_string(corpus_path()).expect("corpus file present");
    let parsed: CorpusFile = toml::from_str(&raw).expect("corpus parses against TtsCorpusEntry");

    assert!(!parsed.entries.is_empty(), "corpus must contain entries");

    // Every entry must have a non-empty id and text — minimal sanity.
    for entry in &parsed.entries {
        assert!(!entry.id.is_empty(), "entry id must not be empty");
        assert!(!entry.text.is_empty(), "entry text must not be empty");
    }

    // Per the benchmark's stratification spec, each
    // bucket and content class must have at least one representative.
    let buckets: Vec<TtsLengthBucket> = parsed.entries.iter().map(|e| e.bucket).collect();
    assert!(buckets.contains(&TtsLengthBucket::Short));
    assert!(buckets.contains(&TtsLengthBucket::Medium));
    assert!(buckets.contains(&TtsLengthBucket::Long));

    let classes: Vec<TtsContentClass> = parsed.entries.iter().map(|e| e.class).collect();
    assert!(classes.contains(&TtsContentClass::Plain));
    assert!(classes.contains(&TtsContentClass::Numerics));
    assert!(classes.contains(&TtsContentClass::Punctuation));
    assert!(classes.contains(&TtsContentClass::Mixed));
}

#[test]
fn tts_corpus_ids_are_unique() {
    let raw = std::fs::read_to_string(corpus_path()).expect("corpus file present");
    let parsed: CorpusFile = toml::from_str(&raw).expect("parse");

    let mut seen = std::collections::HashSet::new();
    for entry in &parsed.entries {
        assert!(
            seen.insert(entry.id.clone()),
            "duplicate corpus id: {}",
            entry.id
        );
    }
}
