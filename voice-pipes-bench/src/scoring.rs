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

/// Deterministic text normalizer for WER/CER scoring (aligned with Whisper's
/// `EnglishTextNormalizer` conventions without external dependencies):
///
/// - Lowercases Unicode alphanumerics and normalizes curly apostrophes
/// - Strips punctuation and collapses whitespace
/// - Expands common English contractions (`don't` -> `do not`)
/// - Normalizes spoken single-digit words (`zero`..`nine` -> `0`..`9`)
/// - Drops hesitation fillers (`uh`, `um`, `umm`, `uhh`)
pub fn normalize_text_for_wer(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut last_space = true;
    for ch in text.chars() {
        match ch {
            '\u{2018}' | '\u{2019}' | '\'' => {
                cleaned.push('\'');
                last_space = false;
            }
            c if c.is_alphanumeric() => {
                for lc in c.to_lowercase() {
                    cleaned.push(lc);
                }
                last_space = false;
            }
            _ => {
                if !last_space {
                    cleaned.push(' ');
                    last_space = true;
                }
            }
        }
    }

    let mut tokens: Vec<&str> = Vec::new();
    for raw in cleaned.split_whitespace() {
        let tok = raw.trim_matches('\'');
        if tok.is_empty() || matches!(tok, "uh" | "um" | "umm" | "uhh") {
            continue;
        }
        if let Some(expanded) = expand_english_token(tok) {
            tokens.extend(expanded.split_whitespace());
        } else {
            tokens.push(tok);
        }
    }
    tokens.join(" ")
}

fn expand_english_token(tok: &str) -> Option<&'static str> {
    match tok {
        "don't" => Some("do not"),
        "can't" => Some("cannot"),
        "won't" => Some("will not"),
        "isn't" => Some("is not"),
        "aren't" => Some("are not"),
        "wasn't" => Some("was not"),
        "weren't" => Some("were not"),
        "didn't" => Some("did not"),
        "doesn't" => Some("does not"),
        "haven't" => Some("have not"),
        "hasn't" => Some("has not"),
        "hadn't" => Some("had not"),
        "wouldn't" => Some("would not"),
        "couldn't" => Some("could not"),
        "shouldn't" => Some("should not"),
        "i'm" => Some("i am"),
        "i've" => Some("i have"),
        "i'll" => Some("i will"),
        "i'd" => Some("i would"),
        "it's" => Some("it is"),
        "that's" => Some("that is"),
        "there's" => Some("there is"),
        "we're" => Some("we are"),
        "they're" => Some("they are"),
        "you're" => Some("you are"),
        "you've" => Some("you have"),
        "you'll" => Some("you will"),
        "we've" => Some("we have"),
        "we'll" => Some("we will"),
        "let's" => Some("let us"),
        "he's" => Some("he is"),
        "she's" => Some("she is"),
        "what's" => Some("what is"),
        "who's" => Some("who is"),
        "where's" => Some("where is"),
        "zero" => Some("0"),
        "one" => Some("1"),
        "two" => Some("2"),
        "three" => Some("3"),
        "four" => Some("4"),
        "five" => Some("5"),
        "six" => Some("6"),
        "seven" => Some("7"),
        "eight" => Some("8"),
        "nine" => Some("9"),
        _ => None,
    }
}

/// Word error rate: Levenshtein distance on normalized word tokens
/// divided by the reference length. Returns `0.0` when both sides are
/// empty and `1.0` when only the hypothesis is empty (or vice versa).
pub fn compute_wer(reference: &str, hypothesis: &str) -> f64 {
    let ref_norm = normalize_text_for_wer(reference);
    let hyp_norm = normalize_text_for_wer(hypothesis);
    let r: Vec<&str> = ref_norm.split_whitespace().collect();
    let h: Vec<&str> = hyp_norm.split_whitespace().collect();
    levenshtein_normalised(&r, &h)
}

/// Character error rate: Levenshtein on normalized chars (whitespace
/// preserved). Normalised by reference length.
pub fn compute_cer(reference: &str, hypothesis: &str) -> f64 {
    let r: Vec<char> = normalize_text_for_wer(reference).chars().collect();
    let h: Vec<char> = normalize_text_for_wer(hypothesis).chars().collect();
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
