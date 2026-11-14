//! Sentence boundaries for streaming text into speech.
//!
//! A streaming LLM or TTS provider holds text until the first chunk worth
//! speaking is complete, then hands it on and keeps the rest. This is the
//! splitter every built-in provider uses, so a plugin that chunks with it
//! breaks speech in the same places; [`crate::TextChunkingConfig`] carries
//! the two soft-boundary thresholds it takes.

const COMMON_ABBREVIATIONS: &[&str] = &[
    "dr.", "mr.", "mrs.", "ms.", "prof.", "sr.", "jr.", "e.g.", "i.e.", "etc.", "vs.", "u.s.",
    "u.k.", "a.m.", "p.m.",
];

fn is_boundary_followup(after: &str) -> bool {
    after.is_empty() || after.starts_with(char::is_whitespace)
}

fn prev_char(buffer: &str, idx: usize) -> Option<char> {
    buffer[..idx].chars().next_back()
}

fn next_char(buffer: &str, idx_after: usize) -> Option<char> {
    buffer[idx_after..].chars().next()
}

/// Whether the token ending at `idx` is a common abbreviation, after any
/// opening quote or bracket: the whole token, so "items." is not "ms.".
/// Only that token is compared, so a long buffer costs nothing extra per
/// call.
fn ends_with_common_abbreviation(buffer: &str, idx: usize) -> bool {
    let token = token_before(buffer, idx).trim_start_matches(|c: char| !c.is_alphanumeric());
    COMMON_ABBREVIATIONS
        .iter()
        .any(|abbr| token.eq_ignore_ascii_case(abbr))
}

fn token_before(buffer: &str, idx: usize) -> &str {
    // After the whole whitespace character: a no-break or em space is more
    // than one byte.
    let start = buffer[..idx]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(p, c)| p + c.len_utf8());
    &buffer[start..idx]
}

/// The next token after `idx_after`, past the whitespace that separates it.
fn token_after(buffer: &str, idx_after: usize) -> &str {
    let after = buffer[idx_after..].trim_start();
    let end = after.find(char::is_whitespace).unwrap_or(after.len());
    &after[..end]
}

/// A single letter and a full stop: "J.". "I" and "A" are words in their
/// own right, and a sentence ends on them ("So do I.").
fn is_initial_token(token: &str) -> bool {
    let mut chars = token.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(c), Some('.'), None) if c.is_ascii_alphabetic() && !matches!(c, 'I' | 'A' | 'a')
    )
}

fn should_skip_period_boundary(buffer: &str, idx: usize, len: usize) -> bool {
    let after = &buffer[idx + len..];

    // Keep trailing abbreviations flushable at EOS.
    if !after.is_empty() && ends_with_common_abbreviation(buffer, idx + len) {
        return true;
    }

    // 3.14 style decimal point.
    if matches!((prev_char(buffer, idx), next_char(buffer, idx + len)), (Some(a), Some(b)) if a.is_ascii_digit() && b.is_ascii_digit())
    {
        return true;
    }

    // Initials such as "J. R. R." or "J. Smith".
    let before_tok = token_before(buffer, idx + len);
    if !after.is_empty() && is_initial_token(before_tok) {
        let next_tok =
            token_after(buffer, idx + len).trim_start_matches(|c: char| !c.is_alphanumeric());
        if next_tok
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        {
            return true;
        }
    }

    // Common URL/email shapes around the token.
    let before = token_before(buffer, idx + len).to_ascii_lowercase();
    before.contains("http://")
        || before.contains("https://")
        || before.contains("www.")
        || before.contains('@')
}

fn should_split_on_soft_boundary(
    buffer: &str,
    idx: usize,
    len: usize,
    min_chars: usize,
    min_words: usize,
) -> bool {
    let head = buffer[..idx + len].trim_end();
    if head.chars().count() < min_chars {
        return false;
    }
    let words = head.split_whitespace().count();
    if words < min_words {
        return false;
    }
    let after = &buffer[idx + len..];
    is_boundary_followup(after)
}

/// Detects the first streamable text chunk boundary in a growing buffer.
///
/// Strong boundaries: `.`, `!`, `?` before whitespace (with abbreviation,
/// decimal and initials guards for `.`), and `\n` whatever follows it.
/// Soft boundaries: `,`, `;`, `:` once minimal length guardrails are satisfied.
///
/// `at_end` says whether the text is complete. While it is still arriving
/// (`false`), a mark is a boundary only once what follows it has arrived:
/// a `.` waits for the next visible character, since "3." may become
/// "3.14" and "J." may be followed by "R.", and the other marks wait for
/// one more character. A newline is final either way. At the end (`true`),
/// a mark that ends the text is a boundary. Fed piece by piece and then
/// once more at the end, a text splits exactly as it does whole.
///
/// Returns `Some((chunk, remainder))`. The chunk keeps the whitespace that
/// followed its mark and the remainder starts at the next visible
/// character, so the chunks of a text concatenate back to the text: a
/// consumer that joins them (a transcript, a conversation history, the
/// played part of a reply) gets the words apart without guessing where a
/// space belongs.
pub fn split_first_sentence(
    buffer: &str,
    min_chars: usize,
    min_words: usize,
    at_end: bool,
) -> Option<(&str, &str)> {
    let mut in_tag = false;
    let mut in_paren = false;
    for (i, c) in buffer.char_indices() {
        let len = c.len_utf8();
        let after = &buffer[i + len..];

        // Track XML/SSML tag enclosure `<...>`
        if c == '<' {
            in_tag = true;
        } else if c == '>' && in_tag {
            in_tag = false;
        }

        // Track markdown/phoneme link parenthesis enclosure `(...)`
        if c == '(' {
            in_paren = true;
        } else if c == ')' && in_paren {
            in_paren = false;
        }

        // If inside an angle-bracket tag or link parenthesis, never split.
        if in_tag || in_paren {
            continue;
        }

        if !at_end && matches!(c, '.' | '!' | '?' | ',' | ';' | ':') {
            let arrived = if c == '.' {
                !after.trim_start().is_empty()
            } else {
                !after.is_empty()
            };
            if !arrived {
                // Only whitespace follows, so no later mark is settled either.
                return None;
            }
        }

        if matches!(c, '.' | '!' | '?' | '\n') {
            if (c == '\n' || is_boundary_followup(after))
                && !(c == '.' && should_skip_period_boundary(buffer, i, len))
            {
                return Some(split_after_whitespace(buffer, i + len));
            }
            continue;
        }

        if matches!(c, ',' | ';' | ':')
            && should_split_on_soft_boundary(buffer, i, len, min_chars, min_words)
        {
            return Some(split_after_whitespace(buffer, i + len));
        }
    }
    None
}

/// Split `buffer` after the mark ending at `end` and the whitespace after it.
fn split_after_whitespace(buffer: &str, end: usize) -> (&str, &str) {
    let after = &buffer[end..];
    buffer.split_at(end + after.len() - after.trim_start().len())
}

#[cfg(test)]
#[path = "tests/sentence.rs"]
mod tests;
