//! Kokoro tokenizer -- IPA phoneme string -> integer token IDs.
//!
//! Loads the vocabulary from a bundled JSON file (char -> i64 mapping) and
//! converts espeak-ng IPA output into the `input_ids` tensor expected by
//! the Kokoro ONNX model.

use std::collections::HashMap;

use voice_pipes_sdk::TtsError;

/// Maximum number of phoneme tokens the model accepts (excluding the two
/// zero-padding positions).
const MAX_PHONEME_LENGTH: usize = 510;

/// Character-to-ID vocabulary for the Kokoro tokenizer.
#[derive(Debug, Clone)]
pub struct KokoroVocab {
    map: HashMap<char, i64>,
}

impl KokoroVocab {
    /// Load the vocabulary from the bundled JSON file.
    pub fn load_bundled() -> Result<Self, TtsError> {
        let json = include_str!("vocab.json");
        Self::from_json(json)
    }

    /// Parse vocabulary from a JSON string (`{ "char": id, ... }`).
    fn from_json(json: &str) -> Result<Self, TtsError> {
        let raw: HashMap<String, i64> = serde_json::from_str(json)
            .map_err(|e| TtsError::ModelLoadFailed(format!("invalid Kokoro vocab JSON: {e}")))?;

        let mut map = HashMap::with_capacity(raw.len());
        for (key, id) in raw {
            // Each key should be a single character.
            let ch = key.chars().next().ok_or_else(|| {
                TtsError::ModelLoadFailed(format!("empty key in vocab for id {id}"))
            })?;
            map.insert(ch, id);
        }

        tracing::debug!(vocab_size = map.len(), "loaded Kokoro vocabulary");
        Ok(Self { map })
    }

    /// Convert an IPA phoneme string into token IDs.
    ///
    /// Unknown characters are silently dropped. The output is truncated to
    /// [`MAX_PHONEME_LENGTH`] tokens.
    pub fn tokenize(&self, phonemes: &str) -> Vec<i64> {
        phonemes
            .chars()
            .filter_map(|ch| self.map.get(&ch).copied())
            .take(MAX_PHONEME_LENGTH)
            .collect()
    }

    /// Number of entries in the vocabulary.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Convert text to phoneme token IDs using espeak-ng G2P.
///
/// 1. Runs espeak-ng with the given language code.
/// 2. Tokenizes the IPA output via the vocabulary.
pub fn text_to_tokens(
    text: &str,
    language: &str,
    vocab: &KokoroVocab,
) -> Result<Vec<i64>, TtsError> {
    let ipa = voice_pipes_sdk::phonemize::run_espeak_with_language(text, language)?;
    Ok(vocab.tokenize(&ipa))
}

#[cfg(test)]
#[path = "tests/tokenizer.rs"]
mod tests;
