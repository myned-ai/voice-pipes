//! `SentencePiece` text normalization and tokenization for Pocket-TTS.
//!
//! Supports both Kyutai's native binary `tokenizer.model` (`SentencePiece`
//! Unigram protobuf, parsed in pure Rust with `#![forbid(unsafe_code)]`) and
//! Hugging Face `tokenizer.json`.

use std::collections::HashMap;
use std::path::Path;

use tokenizers::Tokenizer;
use voice_pipes_sdk::TtsError;

/// Text tokenizer backed by either `tokenizer.model` (`SentencePiece` Unigram),
/// `tokenizer.json` (Hugging Face), or an in-memory byte fallback for tests.
pub(crate) struct PocketTokenizer {
    inner: TokenizerKind,
    vocab_size: usize,
}

enum TokenizerKind {
    SentencePiece(Box<SentencePieceUnigram>),
    Hf(Box<Tokenizer>),
    #[cfg(test)]
    SimpleByte,
}

impl std::fmt::Debug for PocketTokenizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PocketTokenizer")
            .field("vocab_size", &self.vocab_size)
            .finish_non_exhaustive()
    }
}

impl PocketTokenizer {
    /// Load `tokenizer.model` (preferred, matching Kyutai repos) or `tokenizer.json` from `model_dir`.
    pub(crate) fn from_model_dir(
        model_dir: &Path,
        expected_vocab: usize,
    ) -> Result<Self, TtsError> {
        let spm_path = model_dir.join("tokenizer.model");
        if spm_path.exists() {
            let bytes = std::fs::read(&spm_path).map_err(|e| {
                TtsError::ModelLoadFailed(format!(
                    "pocket-candle: failed to read {}: {e}",
                    spm_path.display()
                ))
            })?;
            let spm = SentencePieceUnigram::from_protobuf(&bytes)?;
            let vocab_size = spm.vocab_size().max(expected_vocab);
            return Ok(Self {
                inner: TokenizerKind::SentencePiece(Box::new(spm)),
                vocab_size,
            });
        }

        let json_path = model_dir.join("tokenizer.json");
        let tok = Tokenizer::from_file(&json_path).map_err(|e| {
            TtsError::ModelLoadFailed(format!(
                "pocket-candle: failed to load {} (and no tokenizer.model found): {e}",
                json_path.display()
            ))
        })?;
        let vocab_size = tok.get_vocab_size(true).max(expected_vocab);
        Ok(Self {
            inner: TokenizerKind::Hf(Box::new(tok)),
            vocab_size,
        })
    }

    /// Lightweight deterministic tokenizer for unit & conformance tests.
    #[cfg(test)]
    pub(crate) fn for_tests(vocab_size: usize) -> Self {
        Self {
            inner: TokenizerKind::SimpleByte,
            vocab_size: vocab_size.max(16),
        }
    }

    /// Normalize input text to match Kyutai's Pocket-TTS conditioning rules:
    /// collapse whitespace, replace smart quotes/dashes, ensure terminal punctuation,
    /// and enforce `max_chars`.
    pub(crate) fn prepare_text(raw: &str, max_chars: usize) -> Result<String, TtsError> {
        let mut out = String::with_capacity(raw.len().min(max_chars) + 2);
        let mut last_space = true;
        let mut count = 0usize;

        for ch in raw.chars() {
            let mapped = match ch {
                '\u{2018}' | '\u{2019}' => '\'',
                '\u{201C}' | '\u{201D}' => '"',
                '\u{2013}' | '\u{2014}' => '-',
                c if c.is_whitespace() => ' ',
                c if c.is_control() => continue,
                c => c,
            };
            if mapped == ' ' {
                if last_space {
                    continue;
                }
                last_space = true;
            } else {
                last_space = false;
            }
            count += 1;
            if count > max_chars {
                return Err(TtsError::InferenceFailed(format!(
                    "pocket-candle: input text exceeds max_chars limit ({max_chars})"
                )));
            }
            out.push(mapped);
        }

        let trimmed = out.trim();
        if trimmed.is_empty() {
            return Err(TtsError::InferenceFailed(
                "pocket-candle: input text is empty after normalization".into(),
            ));
        }

        let mut normalized = trimmed.to_string();
        if !normalized.ends_with(['.', '!', '?', ',', ';', ':']) {
            normalized.push('.');
        }
        Ok(normalized)
    }

    /// Encode normalized text into token IDs clamped to `vocab_size`.
    pub(crate) fn encode(&self, text: &str, max_chars: usize) -> Result<Vec<u32>, TtsError> {
        let prepared = Self::prepare_text(text, max_chars)?;
        let ids: Vec<u32> = match &self.inner {
            TokenizerKind::SentencePiece(spm) => spm.encode(&prepared),
            TokenizerKind::Hf(tok) => {
                let enc = tok.encode(prepared, true).map_err(|e| {
                    TtsError::InferenceFailed(format!("pocket-candle: tokenization failed: {e}"))
                })?;
                enc.get_ids().to_vec()
            }
            #[cfg(test)]
            TokenizerKind::SimpleByte => prepared
                .bytes()
                .map(|b| u32::from(b) % u32::try_from(self.vocab_size).unwrap_or(128))
                .collect(),
        };

        if ids.is_empty() {
            return Err(TtsError::InferenceFailed(
                "pocket-candle: tokenizer produced zero tokens".into(),
            ));
        }
        let max_id = u32::try_from(self.vocab_size.saturating_sub(1)).unwrap_or(u32::MAX);
        Ok(ids.into_iter().map(|id| id.min(max_id)).collect())
    }
}

/// Pure-Rust `SentencePiece` Unigram tokenizer (Viterbi DP + byte fallback).
struct SentencePieceUnigram {
    pieces: HashMap<String, (u32, f32)>,
    byte_tokens: [Option<u32>; 256],
    unk_id: u32,
    max_piece_bytes: usize,
    total_vocab: usize,
}

impl SentencePieceUnigram {
    fn from_protobuf(data: &[u8]) -> Result<Self, TtsError> {
        let mut pieces = HashMap::new();
        let mut byte_tokens = [None; 256];
        let mut unk_id = 0_u32;
        let mut max_piece_bytes = 1usize;
        let mut idx = 0usize;
        let mut token_id = 0_u32;

        while idx < data.len() {
            let (tag, next) = read_varint(data, idx)?;
            idx = next;
            let field_num = tag >> 3;
            let wire_type = tag & 7;
            if field_num == 1 && wire_type == 2 {
                let (len, start) = read_varint(data, idx)?;
                let len_usize = usize::try_from(len).unwrap_or(0);
                let end = start.saturating_add(len_usize);
                let sub = data.get(start..end).ok_or_else(|| {
                    TtsError::ModelLoadFailed("pocket-candle: truncated tokenizer.model".into())
                })?;
                idx = end;

                let (piece, score, ptype) = parse_sentence_piece_entry(sub)?;
                if ptype == 2 {
                    unk_id = token_id;
                } else if ptype == 6
                    && let Some(byte_val) = parse_hex_byte_piece(&piece)
                    && let Some(slot) = byte_tokens.get_mut(usize::from(byte_val))
                {
                    *slot = Some(token_id);
                } else if ptype == 1 {
                    max_piece_bytes = max_piece_bytes.max(piece.len());
                    pieces.insert(piece, (token_id, score));
                }
                token_id = token_id.saturating_add(1);
            } else {
                idx = skip_wire_field(data, idx, wire_type)?;
            }
        }

        if token_id == 0 {
            return Err(TtsError::ModelLoadFailed(
                "pocket-candle: tokenizer.model contained zero pieces".into(),
            ));
        }
        Ok(Self {
            pieces,
            byte_tokens,
            unk_id,
            max_piece_bytes,
            total_vocab: usize::try_from(token_id).unwrap_or(4000),
        })
    }

    fn vocab_size(&self) -> usize {
        self.total_vocab
    }

    /// Encode text using `SentencePiece` prefix (`▁`) + Viterbi maximum-likelihood segmentation.
    fn encode(&self, text: &str) -> Vec<u32> {
        // SentencePiece replaces spaces with U+2581 ('▁') and prepends '▁'.
        let mut sp_str = String::with_capacity(text.len() + 4);
        sp_str.push('\u{2581}');
        for ch in text.chars() {
            if ch == ' ' {
                sp_str.push('\u{2581}');
            } else {
                sp_str.push(ch);
            }
        }

        let n = sp_str.len();
        let mut best_score = vec![f32::NEG_INFINITY; n + 1];
        let mut prev_pos = vec![0usize; n + 1];
        let mut best_id = vec![self.unk_id; n + 1];
        if let Some(first) = best_score.first_mut() {
            *first = 0.0;
        }

        let mut i = 0usize;
        while i < n {
            let cur_score = best_score.get(i).copied().unwrap_or(f32::NEG_INFINITY);
            if !cur_score.is_finite() {
                i += 1;
                continue;
            }
            let max_end = (i + self.max_piece_bytes).min(n);
            let mut matched = false;
            let mut end = i + 1;
            while end <= max_end {
                if sp_str.is_char_boundary(end)
                    && let Some(sub) = sp_str.get(i..end)
                    && let Some(&(id, score)) = self.pieces.get(sub)
                {
                    let cand = cur_score + score;
                    if best_score.get(end).copied().unwrap_or(f32::NEG_INFINITY) < cand {
                        if let Some(s) = best_score.get_mut(end) {
                            *s = cand;
                        }
                        if let Some(p) = prev_pos.get_mut(end) {
                            *p = i;
                        }
                        if let Some(t) = best_id.get_mut(end) {
                            *t = id;
                        }
                    }
                    matched = true;
                }
                end += 1;
            }

            // Byte fallback if no piece matched starting at `i`.
            if !matched {
                let next_char_end = sp_str
                    .get(i..)
                    .and_then(|s| s.chars().next())
                    .map_or(i + 1, |c| i + c.len_utf8());
                let cand = cur_score - 100.0;
                if best_score
                    .get(next_char_end)
                    .copied()
                    .unwrap_or(f32::NEG_INFINITY)
                    < cand
                {
                    if let Some(s) = best_score.get_mut(next_char_end) {
                        *s = cand;
                    }
                    if let Some(p) = prev_pos.get_mut(next_char_end) {
                        *p = i;
                    }
                    if let Some(t) = best_id.get_mut(next_char_end) {
                        *t = self.unk_id;
                    }
                }
            }
            i += sp_str
                .get(i..)
                .and_then(|s| s.chars().next())
                .map_or(1, char::len_utf8);
        }

        // Backtrack segments.
        self.backtrack_tokens(&sp_str, &prev_pos, &best_id)
    }

    fn backtrack_tokens(&self, sp_str: &str, prev_pos: &[usize], best_id: &[u32]) -> Vec<u32> {
        let mut segments = Vec::new();
        let mut cursor = sp_str.len();
        while cursor > 0 {
            let start = prev_pos.get(cursor).copied().unwrap_or(0);
            let id = best_id.get(cursor).copied().unwrap_or(self.unk_id);
            segments.push((start, cursor, id));
            if start >= cursor {
                break;
            }
            cursor = start;
        }
        segments.reverse();

        let mut out = Vec::with_capacity(segments.len());
        for (start, end, id) in segments {
            if id == self.unk_id
                && let Some(raw) = sp_str.get(start..end)
            {
                for &b in raw.as_bytes() {
                    let tok = self
                        .byte_tokens
                        .get(usize::from(b))
                        .and_then(|opt| *opt)
                        .unwrap_or(self.unk_id);
                    out.push(tok);
                }
            } else {
                out.push(id);
            }
        }
        out
    }
}

fn parse_sentence_piece_entry(sub: &[u8]) -> Result<(String, f32, u64), TtsError> {
    let mut j = 0usize;
    let mut piece = String::new();
    let mut score = 0.0_f32;
    let mut ptype = 1_u64;
    while j < sub.len() {
        let (tag, next) = read_varint(sub, j)?;
        j = next;
        let fnum = tag >> 3;
        let wtype = tag & 7;
        match (fnum, wtype) {
            (1, 2) => {
                let (len, start) = read_varint(sub, j)?;
                let end = start.saturating_add(usize::try_from(len).unwrap_or(0));
                let slice = sub.get(start..end).ok_or_else(|| {
                    TtsError::ModelLoadFailed("pocket-candle: truncated piece string".into())
                })?;
                piece = String::from_utf8_lossy(slice).into_owned();
                j = end;
            }
            (2, 5) => {
                let b = sub.get(j..j + 4).ok_or_else(|| {
                    TtsError::ModelLoadFailed("pocket-candle: truncated piece score".into())
                })?;
                let arr = [
                    b.first().copied().unwrap_or(0),
                    b.get(1).copied().unwrap_or(0),
                    b.get(2).copied().unwrap_or(0),
                    b.get(3).copied().unwrap_or(0),
                ];
                score = f32::from_le_bytes(arr);
                j += 4;
            }
            (3, 0) => {
                let (val, next) = read_varint(sub, j)?;
                ptype = val;
                j = next;
            }
            _ => {
                j = skip_wire_field(sub, j, wtype)?;
            }
        }
    }
    Ok((piece, score, ptype))
}

fn parse_hex_byte_piece(piece: &str) -> Option<u8> {
    let inner = piece.strip_prefix("<0x")?.strip_suffix('>')?;
    u8::from_str_radix(inner, 16).ok()
}

fn read_varint(buf: &[u8], mut idx: usize) -> Result<(u64, usize), TtsError> {
    let mut val = 0_u64;
    let mut shift = 0_u32;
    loop {
        let &byte = buf.get(idx).ok_or_else(|| {
            TtsError::ModelLoadFailed("pocket-candle: unexpected EOF in tokenizer.model".into())
        })?;
        idx += 1;
        val |= u64::from(byte & 0x7F) << shift;
        if (byte & 0x80) == 0 {
            return Ok((val, idx));
        }
        shift = shift.saturating_add(7);
        if shift >= 64 {
            return Err(TtsError::ModelLoadFailed(
                "pocket-candle: malformed varint in tokenizer.model".into(),
            ));
        }
    }
}

fn skip_wire_field(buf: &[u8], idx: usize, wire_type: u64) -> Result<usize, TtsError> {
    match wire_type {
        0 => Ok(read_varint(buf, idx)?.1),
        1 => Ok(idx.saturating_add(8)),
        2 => {
            let (len, start) = read_varint(buf, idx)?;
            Ok(start.saturating_add(usize::try_from(len).unwrap_or(0)))
        }
        5 => Ok(idx.saturating_add(4)),
        _ => Err(TtsError::ModelLoadFailed(format!(
            "pocket-candle: unsupported protobuf wire type {wire_type}"
        ))),
    }
}
