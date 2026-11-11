//! The model directory: the export's config file, the token table, and the
//! checks that turn a missing file into a message naming it.
//!
//! The encoder weights are external data: each `encoder_*.onnx` is a small
//! graph beside a 1.2 GB `.onnx.data` that the runtime resolves by relative
//! path. A missing data file fails only at session creation, with a runtime
//! error that does not say which file, so `validate` looks for every file by
//! name first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const CONFIG_FILE: &str = "nemotron_onnx_config.json";
pub const TOKENS_FILE: &str = "tokens.txt";

/// The chunk size this plugin runs, in milliseconds. One size is shipped:
/// each size is its own pair of 1.2 GB encoders.
pub const CHUNK_MS: u32 = 320;

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    /// The checkpoint this export was made from, e.g. `nvidia/nemotron-3.5-asr-streaming-0.6b`.
    #[serde(default)]
    pub base_model: String,
    pub vocab_size: usize,
    pub blank_token_id: i64,
    pub decoder_hidden_size: usize,
    pub num_decoder_layers: usize,
    pub max_symbols_per_step: usize,
    pub default_prompt_id: i64,
    /// Locale → prompt id, with aliases (`hr`, `hr-HR`) both present.
    pub prompt_dictionary: BTreeMap<String, i64>,
    pub chunk_ms_to_lookahead: BTreeMap<String, usize>,
    pub cache_shapes: BTreeMap<String, Vec<i64>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("reading {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("parsing {0}: {1}")]
    Parse(PathBuf, serde_json::Error),
    #[error("missing model file {0}")]
    Missing(PathBuf),
    #[error("the export was not made for a {0} ms chunk (it offers {1})")]
    NoSuchChunk(u32, String),
    #[error("{0} has {1} tokens but the config says the vocabulary is {2}")]
    VocabMismatch(PathBuf, usize, usize),
}

impl ModelConfig {
    pub fn load(model_dir: &Path) -> Result<Self, ModelError> {
        let path = model_dir.join(CONFIG_FILE);
        let raw = std::fs::read_to_string(&path).map_err(|e| ModelError::Read(path.clone(), e))?;
        let cfg: Self = serde_json::from_str(&raw).map_err(|e| ModelError::Parse(path, e))?;
        if !cfg
            .chunk_ms_to_lookahead
            .contains_key(&CHUNK_MS.to_string())
        {
            let offered = cfg
                .chunk_ms_to_lookahead
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ModelError::NoSuchChunk(CHUNK_MS, offered));
        }
        Ok(cfg)
    }

    pub fn lookahead(&self) -> usize {
        // Loading refuses a config without this chunk size.
        self.chunk_ms_to_lookahead
            .get(&CHUNK_MS.to_string())
            .copied()
            .unwrap_or_default()
    }

    /// The first-chunk and steady-chunk encoder files for the shipped size.
    pub fn encoder_files(&self) -> (String, String) {
        (
            format!("encoder_{CHUNK_MS}ms_first_fp16.onnx"),
            format!("encoder_{CHUNK_MS}ms_fp16.onnx"),
        )
    }

    /// The prompt id for a locale, accepting `hr`, `hr-HR` and `hr_HR`.
    /// `None` when the model has no such locale.
    pub fn prompt_for(&self, locale: &str) -> Option<i64> {
        let norm = locale.trim().replace('_', "-");
        self.prompt_dictionary
            .get(&norm)
            .or_else(|| self.prompt_dictionary.get(&norm.to_ascii_lowercase()))
            .or_else(|| {
                // `hr-hr` → `hr-HR`
                let mut parts = norm.splitn(2, '-');
                let lang = parts.next()?.to_ascii_lowercase();
                let region = parts.next()?.to_ascii_uppercase();
                self.prompt_dictionary.get(&format!("{lang}-{region}"))
            })
            .copied()
    }

    /// Every file a session needs, so a missing one is named before the
    /// runtime is asked for it.
    pub fn required_files(&self) -> Vec<String> {
        let (first, steady) = self.encoder_files();
        vec![
            CONFIG_FILE.to_string(),
            TOKENS_FILE.to_string(),
            "decoder.onnx".to_string(),
            "joiner.onnx".to_string(),
            format!("{first}.data"),
            format!("{steady}.data"),
            first,
            steady,
        ]
    }

    pub fn validate_files(&self, model_dir: &Path) -> Result<(), ModelError> {
        for f in self.required_files() {
            let p = model_dir.join(&f);
            if !p.is_file() {
                return Err(ModelError::Missing(p));
            }
        }
        Ok(())
    }
}

/// The token table: id → `SentencePiece` piece, with `▁` marking a word start.
pub struct Tokens {
    pub(crate) pieces: Vec<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("vocab_size", &self.pieces.len())
            .finish()
    }
}

impl Tokens {
    pub fn load(model_dir: &Path, cfg: &ModelConfig) -> Result<Self, ModelError> {
        let path = model_dir.join(TOKENS_FILE);
        let raw = std::fs::read_to_string(&path).map_err(|e| ModelError::Read(path.clone(), e))?;
        let mut pieces = vec![String::new(); cfg.vocab_size];
        let mut count = 0;
        for line in raw.lines() {
            let Some((id, piece)) = line.split_once('\t') else {
                continue;
            };
            if let Ok(id) = id.trim().parse::<usize>()
                && let Some(slot) = pieces.get_mut(id)
            {
                *slot = piece.to_string();
                count += 1;
            }
        }
        if count != cfg.vocab_size {
            return Err(ModelError::VocabMismatch(path, count, cfg.vocab_size));
        }
        Ok(Self { pieces })
    }

    /// Whether a piece is a locale tag such as `<hr-HR>`, which the model
    /// emits first when it detects the language and which is not speech.
    pub fn is_locale_tag(piece: &str) -> bool {
        piece.len() > 2
            && piece.starts_with('<')
            && piece.ends_with('>')
            && piece[1..piece.len() - 1]
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == '-')
            && piece.contains('-')
    }

    /// The ids of every locale-tag piece, for the engine to keep apart.
    pub fn locale_tag_ids(&self) -> std::collections::HashSet<u32> {
        self.pieces
            .iter()
            .enumerate()
            .filter(|(_, p)| Self::is_locale_tag(p))
            .filter_map(|(i, _)| u32::try_from(i).ok())
            .collect()
    }

    /// The locale a tag token names, `<hr-HR>` → `hr-HR`.
    pub fn locale_of(&self, id: u32) -> Option<&str> {
        let p = self.pieces.get(id as usize)?;
        Self::is_locale_tag(p).then(|| &p[1..p.len() - 1])
    }

    /// Text for a token sequence, and the locale tags seen, in order.
    pub fn decode(&self, ids: &[u32]) -> (String, Vec<String>) {
        let mut text = String::new();
        let mut tags = Vec::new();
        for &id in ids {
            let Some(piece) = self.pieces.get(id as usize) else {
                continue;
            };
            if Self::is_locale_tag(piece) {
                tags.push(piece.clone());
                continue;
            }
            if piece.starts_with('<') && piece.ends_with('>') {
                continue; // <unk>, <pad>
            }
            for ch in piece.chars() {
                if ch == '▁' {
                    text.push(' ');
                } else {
                    text.push(ch);
                }
            }
        }
        (text.trim().to_string(), tags)
    }
}

#[cfg(test)]
#[path = "tests/model.rs"]
mod tests;
