//! Layer 2: Punctuation-normalized word alignment (`WER`, `CER`, word recall,
//! missing/substituted/inserted words) for round-trip STT intelligibility checks.

use serde::{Deserialize, Serialize};

/// Exact word-level alignment report between expected prompt and STT transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PronunciationReport {
    pub expected_normalized: String,
    pub heard_normalized: String,
    pub wer: f64,
    pub cer: f64,
    /// Fraction of expected words matched in order (`0.0..=1.0`).
    pub word_recall: f64,
    pub matched_words: usize,
    pub expected_word_count: usize,
    pub missing_words: Vec<String>,
    pub substituted_words: Vec<(String, String)>,
    pub inserted_words: Vec<String>,
}

/// Normalize text for fair pronunciation scoring (delegates to the shared
/// deterministic WER normalizer in [`crate::scoring::normalize_text_for_wer`]).
pub fn normalize_for_alignment(text: &str) -> String {
    crate::scoring::normalize_text_for_wer(text)
}

/// Compute normalized `WER`, `CER`, word recall, and exact missing/substituted/inserted words.
pub fn align_pronunciation(expected: &str, heard: &str) -> PronunciationReport {
    let exp_norm = normalize_for_alignment(expected);
    let hrd_norm = normalize_for_alignment(heard);
    let r: Vec<String> = exp_norm.split_whitespace().map(str::to_string).collect();
    let h: Vec<String> = hrd_norm.split_whitespace().map(str::to_string).collect();

    let n = r.len();
    let m = h.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        if let Some(first) = row.first_mut() {
            *first = i;
        }
    }
    if let Some(first_row) = dp.first_mut() {
        for (j, cell) in first_row.iter_mut().enumerate() {
            *cell = j;
        }
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(r.get(i - 1) != h.get(j - 1));
            let del = dp
                .get(i - 1)
                .and_then(|row| row.get(j))
                .copied()
                .unwrap_or(0)
                + 1;
            let ins = dp
                .get(i)
                .and_then(|row| row.get(j - 1))
                .copied()
                .unwrap_or(0)
                + 1;
            let sub = dp
                .get(i - 1)
                .and_then(|row| row.get(j - 1))
                .copied()
                .unwrap_or(0)
                + cost;
            if let Some(cell) = dp.get_mut(i).and_then(|row| row.get_mut(j)) {
                *cell = del.min(ins).min(sub);
            }
        }
    }

    let dist = dp.get(n).and_then(|row| row.get(m)).copied().unwrap_or(0);
    let wer = if n == 0 {
        if m == 0 { 0.0 } else { 1.0 }
    } else {
        (dist as f64) / (n as f64)
    };
    let cer = crate::scoring::compute_cer(&exp_norm, &hrd_norm);

    let ops = backtrace_word_ops(&dp, &r, &h);
    let word_recall = if n == 0 {
        1.0
    } else {
        (ops.matched as f64) / (n as f64)
    };

    PronunciationReport {
        expected_normalized: exp_norm,
        heard_normalized: hrd_norm,
        wer,
        cer,
        word_recall,
        matched_words: ops.matched,
        expected_word_count: n,
        missing_words: ops.missing,
        substituted_words: ops.substituted,
        inserted_words: ops.inserted,
    }
}

struct WordOps {
    matched: usize,
    missing: Vec<String>,
    substituted: Vec<(String, String)>,
    inserted: Vec<String>,
}

fn backtrace_word_ops(dp: &[Vec<usize>], r: &[String], h: &[String]) -> WordOps {
    let mut i = r.len();
    let mut j = h.len();
    let mut matched = 0usize;
    let mut missing = Vec::new();
    let mut substituted = Vec::new();
    let mut inserted = Vec::new();

    while i > 0 || j > 0 {
        let cur = dp.get(i).and_then(|row| row.get(j)).copied().unwrap_or(0);
        if i > 0 && j > 0 {
            let eq = r.get(i - 1) == h.get(j - 1);
            let diag = dp
                .get(i - 1)
                .and_then(|row| row.get(j - 1))
                .copied()
                .unwrap_or(usize::MAX);
            if eq && diag == cur {
                matched += 1;
                i -= 1;
                j -= 1;
                continue;
            }
            if !eq && diag + 1 == cur {
                if let (Some(rw), Some(hw)) = (r.get(i - 1), h.get(j - 1)) {
                    substituted.push((rw.clone(), hw.clone()));
                }
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0
            && dp
                .get(i - 1)
                .and_then(|row| row.get(j))
                .copied()
                .unwrap_or(usize::MAX)
                + 1
                == cur
        {
            if let Some(rw) = r.get(i - 1) {
                missing.push(rw.clone());
            }
            i -= 1;
        } else {
            if j > 0
                && let Some(hw) = h.get(j - 1)
            {
                inserted.push(hw.clone());
            }
            j = j.saturating_sub(1);
        }
    }
    missing.reverse();
    substituted.reverse();
    inserted.reverse();
    WordOps {
        matched,
        missing,
        substituted,
        inserted,
    }
}
