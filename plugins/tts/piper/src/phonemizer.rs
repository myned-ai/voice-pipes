//! Grapheme-to-phoneme conversion via espeak-ng.

use voice_pipes_sdk::TtsError;
use voice_pipes_sdk::phonemize::run_espeak_with_language;

use super::model_config::PiperModelConfig;

/// Convert text to a sequence of Piper phoneme IDs using espeak-ng.
///
/// Steps:
/// 1. Shell out to `espeak-ng --ipa -q -v <language>` with the model's voice
/// 2. Reconstruct clause punctuation (`,`, `.`, `?`, etc.) stripped by espeak-ng
/// 3. Map each IPA phoneme and punctuation mark to integer IDs via the model config
/// 4. Insert inter-phoneme padding (ID 0), BOS (^) and EOS ($)
pub fn phonemize(text: &str, config: &PiperModelConfig) -> Result<Vec<i64>, TtsError> {
    let language = config.espeak_voice();
    let raw_ipa = run_espeak_with_language(text, language)?;
    let ipa = reconstruct_clause_punctuation(text, &raw_ipa);
    tracing::debug!(
        language,
        raw_chars = raw_ipa.chars().count(),
        ipa = %ipa,
        "espeak-ng IPA output with clause punctuation"
    );
    let ids = ipa_to_ids(&ipa, config)?;
    tracing::debug!(phoneme_id_count = ids.len(), "phoneme IDs generated");
    Ok(ids)
}

/// Extract clause terminators from original text.
///
/// Recognizes punctuation marks `,`, `;`, `:`, `.`, `?`, `!`, as well as
/// em-dashes (`—`), en-dashes (`–`), and newlines (`\n`, normalized to `,`),
/// collapsing runs (like `...`, `?!`) into a single terminator, ignoring
/// leading/empty clauses without spoken characters, and ignoring decimal
/// points within numbers (e.g. `3.14`).
pub fn extract_clause_terminators(text: &str) -> Vec<char> {
    let is_clause_punct = |c: char| {
        matches!(
            c,
            ',' | ';' | ':' | '.' | '?' | '!' | '\u{2014}' | '\u{2013}' | '\n'
        )
    };
    let normalize_punct = |c: char| match c {
        '\u{2014}' | '\u{2013}' | '\n' => ',',
        other => other,
    };

    let chars: Vec<char> = text.chars().collect();
    let mut terminators = Vec::new();
    let mut has_spoken = false;
    let mut i = 0;
    while i < chars.len() {
        if let Some(&c) = chars.get(i) {
            if c.is_alphanumeric() {
                has_spoken = true;
            } else if is_clause_punct(c) {
                let is_decimal = c == '.'
                    && i > 0
                    && chars.get(i.saturating_sub(1)).is_some_and(char::is_ascii_digit)
                    && chars.get(i.saturating_add(1)).is_some_and(char::is_ascii_digit);

                if !is_decimal {
                    if has_spoken {
                        terminators.push(normalize_punct(c));
                        has_spoken = false;
                    }
                    while chars
                        .get(i.saturating_add(1))
                        .is_some_and(|&next| is_clause_punct(next))
                    {
                        i = i.saturating_add(1);
                    }
                }
            }
        }
        i = i.saturating_add(1);
    }
    terminators
}

/// Reconstruct clause boundaries and terminal punctuation into the raw IPA output.
pub fn reconstruct_clause_punctuation(text: &str, raw_ipa: &str) -> String {
    let terminators = extract_clause_terminators(text);
    let lines: Vec<&str> = raw_ipa.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut result = String::with_capacity(raw_ipa.len().saturating_add(16));
    let line_count = lines.len();

    for (i, &line) in lines.iter().enumerate() {
        if i > 0 && !result.ends_with(' ') {
            result.push(' ');
        }
        result.push_str(line);

        let term = terminators.get(i).copied().unwrap_or_else(|| {
            if i.saturating_add(1) == line_count {
                '.'
            } else {
                ','
            }
        });

        match term {
            ',' | ';' | ':' => {
                result.push(term);
                result.push(' ');
            }
            '.' | '?' | '!' => {
                result.push(term);
            }
            _ => {
                result.push('.');
            }
        }
    }

    result
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
