//! BPE vocab loading and detokenization for Nemotron-Speech-Streaming.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::Path;

use voice_pipes_sdk::stt::SttError;

/// Load `tokenizer.vocab` from `model_dir`.
///
/// Errors if the file is missing, unreadable, or empty: with an empty map
/// `detokenize` would emit raw numeric token IDs ("17 254 …") as the
/// user-visible transcript. Failing at
/// construction surfaces the misconfiguration instead.
pub fn load_vocab(model_dir: &Path) -> Result<HashMap<u32, String>, SttError> {
    let vocab_path = model_dir.join("tokenizer.vocab");
    let raw = std::fs::read_to_string(&vocab_path).map_err(|e| {
        SttError::ModelLoadFailed(format!(
            "failed to read Nemotron vocab at {}: {e}",
            vocab_path.display()
        ))
    })?;
    let mut map = HashMap::new();
    for (idx, line) in raw.lines().enumerate() {
        let piece = line.split('\t').next().unwrap_or(line).to_string();
        let id = u32::try_from(idx).map_err(|_too_long| {
            SttError::ModelLoadFailed(format!(
                "Nemotron vocab at {} is too long",
                vocab_path.display()
            ))
        })?;
        map.insert(id, piece);
    }
    if map.is_empty() {
        return Err(SttError::ModelLoadFailed(format!(
            "Nemotron vocab at {} is empty",
            vocab_path.display()
        )));
    }
    Ok(map)
}

/// Detokenize a stream of BPE token IDs into a single string.
pub fn detokenize<S: BuildHasher>(vocab: &HashMap<u32, String, S>, ids: &[u32]) -> String {
    if vocab.is_empty() {
        return ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
    }
    ids.iter()
        .filter_map(|i| vocab.get(i))
        .map(String::as_str)
        .collect::<String>()
        .replace('\u{2581}', " ")
        .trim()
        .to_string()
}

#[cfg(test)]
#[path = "tests/vocab.rs"]
mod tests;
