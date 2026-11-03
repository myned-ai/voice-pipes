//! Grapheme-to-phoneme conversion via espeak-ng.

use voice_pipes_sdk::TtsError;
use voice_pipes_sdk::phonemize::run_espeak;

use super::model_config::PiperModelConfig;

/// Convert text to a sequence of Piper phoneme IDs using espeak-ng.
///
/// Steps:
/// 1. Shell out to `espeak-ng --ipa -q` to get IPA transcription
/// 2. Map each IPA phoneme to integer IDs via the model config
/// 3. Insert inter-phoneme padding (ID 0), BOS (^) and EOS ($)
pub fn phonemize(text: &str, config: &PiperModelConfig) -> Result<Vec<i64>, TtsError> {
    let ipa = run_espeak(text)?;
    tracing::debug!(ipa_chars = ipa.chars().count(), "espeak-ng IPA output");
    let ids = ipa_to_ids(&ipa, config)?;
    tracing::debug!(phoneme_id_count = ids.len(), "phoneme IDs generated");
    Ok(ids)
}

/// Convert IPA string to Piper phoneme ID sequence.
///
/// Piper expects: BOS(^) + pad(0) + [phoneme + pad(0)]... + EOS($) + pad(0)
///
/// IPA with no phoneme in it besides spaces (espeak-ng gives nothing for
/// "..." and other bare punctuation) comes back empty rather than as the
/// framing alone, which the voice would still render as a breath of noise.
fn ipa_to_ids(ipa: &str, config: &PiperModelConfig) -> Result<Vec<i64>, TtsError> {
    let pad = config
        .phoneme_to_ids("_")
        .ok_or_else(|| TtsError::ConfigError("missing pad symbol '_' in phoneme map".into()))?
        .clone();
    let bos = config
        .phoneme_to_ids("^")
        .ok_or_else(|| TtsError::ConfigError("missing BOS symbol '^' in phoneme map".into()))?
        .clone();
    let eos = config
        .phoneme_to_ids("$")
        .ok_or_else(|| TtsError::ConfigError("missing EOS symbol '$' in phoneme map".into()))?
        .clone();

    let mut ids: Vec<i64> = Vec::new();
    let mut spoken = false;

    // BOS + pad
    ids.extend(&bos);
    ids.extend(&pad);

    // Process each character of the IPA string
    let chars: Vec<char> = ipa.chars().collect();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        // Try two-character phonemes first (e.g. "oʊ", "aɪ")
        if let Some(&next) = chars.get(i + 1) {
            let digraph: String = [c, next].iter().collect();
            if let Some(phoneme_ids) = config.phoneme_to_ids(&digraph) {
                ids.extend(phoneme_ids);
                ids.extend(&pad);
                spoken = true;
                i += 2;
                continue;
            }
        }
        // Characters not in the map are skipped (stress marks, etc.)
        if let Some(phoneme_ids) = config.phoneme_to_ids(&c.to_string()) {
            ids.extend(phoneme_ids);
            ids.extend(&pad);
            spoken |= !c.is_whitespace();
        }
        i += 1;
    }

    if !spoken {
        return Ok(Vec::new());
    }

    // EOS + pad
    ids.extend(&eos);
    ids.extend(&pad);

    Ok(ids)
}

#[cfg(test)]
#[path = "tests/phonemizer.rs"]
mod tests;
