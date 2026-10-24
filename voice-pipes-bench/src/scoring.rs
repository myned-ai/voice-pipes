//! TTS round-trip scoring -- feeds synthesised audio back through an STT
//! provider and computes WER/CER vs the original input text.
//!
//! As specified for TTS benchmarking stratification,
//! the scorer is intentionally decoupled from the variant-under-test so
//! that an STT bias toward its own audio cannot inflate quality scores.
//! In practice any [`SttProvider`] can be plugged in (Whisper Tiny is
//! the documented default once integrated).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use voice_pipes::audio::wav_reader::read_wav;
use voice_pipes::stt::SttProvider;

/// Outcome of scoring a single synthesised utterance.
#[derive(Debug, Clone, PartialEq)]
pub struct TtsScore {
    /// Word error rate vs the expected text (Levenshtein on word tokens,
    /// normalised by reference length). Range `[0.0, ∞)` but typically
    /// clamped at 1.0 by callers.
    pub wer: f64,
    /// Character error rate (Levenshtein on chars, normalised by ref len).
    pub cer: f64,
    /// Raw transcript returned by the reference STT.
    pub transcript: String,
}

/// Pluggable scorer that maps `(expected_text, audio_file) -> TtsScore`.
#[async_trait]
pub trait TtsScorer: Send + Sync {
    async fn score(&self, expected: &str, audio_path: &Path) -> anyhow::Result<TtsScore>;
}

/// Feeds the synthesised WAV through an [`SttProvider`] and computes
/// WER/CER against the input text.
pub struct SttRoundTripScorer {
    stt: Arc<dyn SttProvider>,
}

impl std::fmt::Debug for SttRoundTripScorer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttRoundTripScorer")
            .field("stt", &self.stt.name())
            .finish()
    }
}

impl SttRoundTripScorer {
    pub fn new(stt: Arc<dyn SttProvider>) -> Self {
        Self { stt }
    }

    /// Provider name -- surfaced by callers for logging.
    pub fn provider_name(&self) -> &str {
        self.stt.name()
    }
}

#[async_trait]
impl TtsScorer for SttRoundTripScorer {
    async fn score(&self, expected: &str, audio_path: &Path) -> anyhow::Result<TtsScore> {
        let frame = read_wav(audio_path)?;
        let result = self
            .stt
            .transcribe(&frame)
            .await
            .map_err(|e| anyhow::anyhow!("stt round-trip failed: {e}"))?;
        Ok(TtsScore {
            wer: compute_wer(expected, &result.text),
            cer: compute_cer(expected, &result.text),
            transcript: result.text,
        })
    }
}

/// Word error rate: Levenshtein distance on lowercased word tokens
/// divided by the reference length. Returns `0.0` when both sides are
/// empty and `1.0` when only the hypothesis is empty (or vice versa).
pub fn compute_wer(reference: &str, hypothesis: &str) -> f64 {
    let r: Vec<String> = reference
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let h: Vec<String> = hypothesis
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    levenshtein_normalised(&r, &h)
}

/// Character error rate: Levenshtein on lowercased chars (whitespace
/// preserved). Normalised by reference length.
pub fn compute_cer(reference: &str, hypothesis: &str) -> f64 {
    let r: Vec<char> = reference.to_lowercase().chars().collect();
    let h: Vec<char> = hypothesis.to_lowercase().chars().collect();
    levenshtein_normalised(&r, &h)
}

fn levenshtein_normalised<T: Eq>(reference: &[T], hypothesis: &[T]) -> f64 {
    if reference.is_empty() {
        return if hypothesis.is_empty() { 0.0 } else { 1.0 };
    }
    let n = reference.len();
    let m = hypothesis.len();
    // Two-row DP -- O(n) memory.
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = usize::from(reference[i - 1] != hypothesis[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    // Safe: distance <= max(n, m); we tolerate the precision floor at
    // ~= 2^53 tokens, which is unreachable for benchmark inputs.
    prev[m] as f64 / n as f64
}

#[cfg(test)]
#[path = "tests/scoring.rs"]
mod tests;
