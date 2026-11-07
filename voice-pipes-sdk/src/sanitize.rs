//! Text sanitization and dual-track conversion for streaming TTS.
//!
//! Separates LLM response text into two synchronized views:
//! - `display_text`: For UI captions, session transcripts, and LLM conversation history
//!   (retaining markdown styling, unclosed tags safely stripped or balanced).
//! - `spoken_text`: For TTS synthesis (formatting markdown stripped, emojis/pictographs
//!   removed, pronunciation lexicon substitutions applied, and inline directives filtered
//!   against the active provider's supported tags).

use std::collections::BTreeMap;

/// Record of an active directive or substitution applied to `spoken_text`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TtsDirectiveRecord {
    /// An inline tag or bracket directive preserved in `spoken_text` (e.g. `<break time="300ms"/>`, `[laughs]`).
    TagPassthrough {
        /// The exact raw tag string preserved.
        raw: String,
    },
    /// An inline IPA phoneme link preserved for TTS and unwrapped in display (e.g. `[Kokoro](/kˈoʊkəɹoʊ/)`).
    IpaPhoneme {
        /// The base word displayed.
        word: String,
        /// The IPA phoneme string passed to TTS.
        ipa: String,
    },
    /// A whole-word lexicon substitution applied to `spoken_text` (e.g. `"SQL"` -> `"sequel"`).
    Lexicon {
        /// The source word.
        from: String,
        /// The spoken replacement.
        to: String,
    },
}

/// Record of a directive that was stripped because the active TTS provider does not support it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StrippedDirectiveRecord {
    /// The raw tag or directive that was removed.
    pub raw: String,
    /// Human-readable explanation of why it was stripped.
    pub reason: String,
}

/// Result of dual-track sanitization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DualTrackText {
    /// Text for UI captions, transcripts, and LLM history.
    pub display: String,
    /// Text for TTS synthesis.
    pub spoken: String,
    /// Directives and substitutions active in `spoken`.
    pub directives: Vec<TtsDirectiveRecord>,
    /// Directives stripped from `spoken` due to lack of provider support.
    pub stripped: Vec<StrippedDirectiveRecord>,
}

/// Configuration options for dual-track sanitization.
#[derive(Debug, Clone, Default)]
pub struct SanitizeConfig {
    /// Whether to strip Markdown wrappers (**bold**, *italic*, `code`, URLs) from spoken text.
    pub sanitize_markdown: bool,
    /// Tag prefixes or exact names permitted to pass through to `spoken_text` (e.g. `["<break", "[laughs]"]` or `["*"]`).
    pub passthrough_tags: Vec<String>,
    /// Name of the active TTS provider (for logging stripped directive reasons).
    pub provider_name: String,
    /// Whole-word pronunciation lexicon replacements applied to `spoken_text`.
    pub lexicon: BTreeMap<String, String>,
}

impl SanitizeConfig {
    /// Build a sanitize config from optional [`TtsConfig`](crate::config::TtsConfig) and provider supported tags.
    #[must_use]
    pub fn from_tts(
        tts_cfg: Option<&crate::config::TtsConfig>,
        provider_name: impl Into<String>,
        provider_tags: &[&str],
    ) -> Self {
        let provider_name = provider_name.into();
        let mut sanitize_markdown = true;
        let mut passthrough_tags = Vec::new();
        let mut lexicon = BTreeMap::new();

        for tag in provider_tags {
            passthrough_tags.push((*tag).to_string());
        }

        if let Some(cfg) = tts_cfg {
            sanitize_markdown = cfg.sanitize_markdown;
            for tag in &cfg.passthrough_tags {
                if !passthrough_tags.contains(tag) {
                    passthrough_tags.push(tag.clone());
                }
            }
            lexicon.clone_from(&cfg.lexicon);
        }

        Self {
            sanitize_markdown,
            passthrough_tags,
            provider_name,
            lexicon,
        }
    }
}

/// True for characters TTS cannot speak: emoji / pictographs and their
/// variation selectors, zero-width joiner, keycap combiner, and tag characters.
#[must_use]
pub fn is_unspeakable_symbol(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1F000..=0x1FAFF // pictographs, emoticons, transport, supplemental
            | 0x2600..=0x27BF // misc symbols + dingbats (☀ ❤ ✅)
            | 0x2B00..=0x2BFF // misc symbols and arrows (⭐ ⬆ ⬛)
            | 0xFE00..=0xFE0F // variation selectors
            | 0x200D // zero-width joiner
            | 0x20E3 // combining enclosing keycap
            | 0xE0020..=0xE007F // tag characters
    )
}

/// Strip emoticons/pictographs from text.
#[must_use]
pub fn strip_unspeakable(text: &str) -> String {
    if !text.chars().any(is_unspeakable_symbol) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.chars() {
        if is_unspeakable_symbol(c) {
            continue;
        }
        let is_space = c == ' ';
        if is_space && prev_space {
            continue;
        }
        prev_space = is_space;
        out.push(c);
    }
    out.trim().to_string()
}

/// Stateful filter for stripping `<think>...</think>` and `<|think|>...</|think|>`
/// blocks across arbitrary streaming token boundaries.
#[derive(Debug, Default, Clone)]
pub struct ThinkTagFilter {
    in_think: bool,
    pending: String,
}

impl ThinkTagFilter {
    /// Create a new idle filter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a chunk of streaming text, returning the sanitized portion.
    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut out = String::new();

        while !self.pending.is_empty() {
            if self.in_think {
                // Look for closing </think> or </|think|>
                if let Some(pos) = self.pending.find("</think>") {
                    let end_pos = pos + "</think>".len();
                    self.pending.drain(..end_pos);
                    self.in_think = false;
                    continue;
                }
                if let Some(pos) = self.pending.find("</|think|>") {
                    let end_pos = pos + "</|think|>".len();
                    self.pending.drain(..end_pos);
                    self.in_think = false;
                    continue;
                }
                // Check if the tail could be a partial closing tag
                let tail_len = Self::potential_closing_prefix_len(&self.pending);
                if tail_len > 0 {
                    let drain_len = self.pending.len() - tail_len;
                    self.pending.drain(..drain_len);
                } else {
                    self.pending.clear();
                }
                break;
            }

            // Not in think: look for opening <think> or <|think|>
            let pos1 = self.pending.find("<think>");
            let pos2 = self.pending.find("<|think|>");
            let open_pos = match (pos1, pos2) {
                (Some(p1), Some(p2)) => Some((
                    p1.min(p2),
                    if p1 < p2 {
                        "<think>".len()
                    } else {
                        "<|think|>".len()
                    },
                )),
                (Some(p1), None) => Some((p1, "<think>".len())),
                (None, Some(p2)) => Some((p2, "<|think|>".len())),
                (None, None) => None,
            };

            if let Some((pos, tag_len)) = open_pos {
                out.push_str(self.pending.get(..pos).unwrap_or_default());
                let drain_len = pos + tag_len;
                self.pending.drain(..drain_len);
                self.in_think = true;
                continue;
            }

            // Check for partial opening tag at the end of pending
            let tail_len = Self::potential_opening_prefix_len(&self.pending);
            if tail_len > 0 {
                let safe_len = self.pending.len() - tail_len;
                out.push_str(self.pending.get(..safe_len).unwrap_or_default());
                self.pending.drain(..safe_len);
            } else {
                out.push_str(&self.pending);
                self.pending.clear();
            }
            break;
        }

        out
    }

    /// Flush any remaining pending text (e.g. at end of response).
    pub fn flush(&mut self) -> String {
        let mut out = String::new();
        if !self.in_think {
            out.push_str(&self.pending);
        }
        self.pending.clear();
        out
    }

    fn potential_opening_prefix_len(text: &str) -> usize {
        for prefix in ["<think>", "<|think|>"] {
            for len in (1..prefix.len()).rev() {
                if let Some(cand) = prefix.get(..len)
                    && text.ends_with(cand)
                {
                    return len;
                }
            }
        }
        0
    }

    fn potential_closing_prefix_len(text: &str) -> usize {
        for prefix in ["</think>", "</|think|>"] {
            for len in (1..prefix.len()).rev() {
                if let Some(cand) = prefix.get(..len)
                    && text.ends_with(cand)
                {
                    return len;
                }
            }
        }
        0
    }
}

fn is_tag_allowed(tag_str: &str, cfg: &SanitizeConfig, allow_all: bool) -> bool {
    if allow_all {
        return true;
    }
    cfg.passthrough_tags.iter().any(|allowed| {
        tag_str.eq_ignore_ascii_case(allowed)
            || tag_str.to_ascii_lowercase().starts_with(&allowed.to_ascii_lowercase())
    })
}

fn handle_bracket_link_or_directive(
    rest: &str,
    cfg: &SanitizeConfig,
    allow_all: bool,
    display_buf: &mut String,
    spoken_buf: &mut String,
    dual: &mut DualTrackText,
) -> Option<usize> {
    if let Some(link) = parse_markdown_link(rest) {
        if is_ipa_target(link.target) {
            display_buf.push_str(link.text);
            if is_tag_allowed("/ipa/", cfg, allow_all) || is_tag_allowed(link.target, cfg, allow_all) {
                spoken_buf.push_str(link.full);
                dual.directives.push(TtsDirectiveRecord::IpaPhoneme {
                    word: link.text.to_string(),
                    ipa: link.target.to_string(),
                });
            } else {
                spoken_buf.push_str(link.text);
                dual.stripped.push(StrippedDirectiveRecord {
                    raw: link.target.to_string(),
                    reason: format!(
                        "phoneme target not in passthrough_tags for provider '{}'",
                        cfg.provider_name
                    ),
                });
            }
        } else {
            display_buf.push_str(link.full);
            spoken_buf.push_str(link.text);
        }
        return Some(link.full.len());
    }

    if let Some(bracket_dir) = parse_bracket_directive(rest) {
        if is_tag_allowed(bracket_dir, cfg, allow_all) {
            spoken_buf.push_str(bracket_dir);
            dual.directives.push(TtsDirectiveRecord::TagPassthrough {
                raw: bracket_dir.to_string(),
            });
        } else {
            dual.stripped.push(StrippedDirectiveRecord {
                raw: bracket_dir.to_string(),
                reason: format!(
                    "bracket tag not in passthrough_tags for provider '{}'",
                    cfg.provider_name
                ),
            });
        }
        return Some(bracket_dir.len());
    }

    None
}

fn handle_xml_tag(
    rest: &str,
    cfg: &SanitizeConfig,
    allow_all: bool,
    spoken_buf: &mut String,
    dual: &mut DualTrackText,
) -> Option<usize> {
    let tag_str = parse_xml_tag(rest)?;
    if tag_str.starts_with("<phoneme") {
        if is_tag_allowed("<phoneme", cfg, allow_all) {
            spoken_buf.push_str(tag_str);
            dual.directives.push(TtsDirectiveRecord::TagPassthrough {
                raw: tag_str.to_string(),
            });
        } else {
            dual.stripped.push(StrippedDirectiveRecord {
                raw: tag_str.to_string(),
                reason: format!(
                    "tag not in passthrough_tags for provider '{}'",
                    cfg.provider_name
                ),
            });
        }
    } else if tag_str == "</phoneme>" {
        if is_tag_allowed("</phoneme>", cfg, allow_all) || is_tag_allowed("<phoneme", cfg, allow_all) {
            spoken_buf.push_str(tag_str);
        }
    } else if is_tag_allowed(tag_str, cfg, allow_all) {
        spoken_buf.push_str(tag_str);
        dual.directives.push(TtsDirectiveRecord::TagPassthrough {
            raw: tag_str.to_string(),
        });
    } else {
        dual.stripped.push(StrippedDirectiveRecord {
            raw: tag_str.to_string(),
            reason: format!(
                "tag not in passthrough_tags for provider '{}'",
                cfg.provider_name
            ),
        });
    }
    Some(tag_str.len())
}

/// Convert input text into synchronized `display` and `spoken` views.
#[must_use]
pub fn sanitize_dual_track(raw: &str, cfg: &SanitizeConfig) -> DualTrackText {
    let mut dual = DualTrackText::default();
    let allow_all = cfg.passthrough_tags.iter().any(|t| t == "*");

    let mut display_buf = String::with_capacity(raw.len());
    let mut spoken_buf = String::with_capacity(raw.len());

    let mut idx = 0;
    while idx < raw.len() {
        let Some(rest) = raw.get(idx..) else { break };

        // 1. Check for markdown / phoneme link or bracket directive
        if rest.starts_with('[')
            && let Some(consumed) = handle_bracket_link_or_directive(
                rest,
                cfg,
                allow_all,
                &mut display_buf,
                &mut spoken_buf,
                &mut dual,
            )
        {
            idx += consumed;
            continue;
        }

        // 2. Check for XML/SSML tag: `<...>`
        if rest.starts_with('<')
            && let Some(consumed) = handle_xml_tag(
                rest,
                cfg,
                allow_all,
                &mut spoken_buf,
                &mut dual,
            )
        {
            idx += consumed;
            continue;
        }

        // 3. Regular characters
        let mut char_iter = rest.chars();
        if let Some(c) = char_iter.next() {
            display_buf.push(c);
            spoken_buf.push(c);
            idx += c.len_utf8();
        } else {
            break;
        }
    }

    // 4. Clean up spoken text: strip markdown wrappers, bare URLs, and emojis
    let clean_spoken = if cfg.sanitize_markdown {
        strip_markdown_and_urls(&spoken_buf)
    } else {
        spoken_buf
    };
    let clean_spoken = strip_unspeakable(&clean_spoken);

    // 5. Apply pronunciation lexicon to spoken text
    let (final_spoken, lexicon_records) = apply_lexicon(&clean_spoken, &cfg.lexicon);
    dual.directives.extend(lexicon_records);

    dual.display = collapse_whitespace(&display_buf);
    dual.spoken = collapse_whitespace(&final_spoken);
    dual
}

struct ParsedLink<'a> {
    full: &'a str,
    text: &'a str,
    target: &'a str,
}

fn parse_markdown_link(text: &str) -> Option<ParsedLink<'_>> {
    if !text.starts_with('[') {
        return None;
    }
    let close_bracket = text.find(']')?;
    let open_paren = close_bracket + 1;
    if text.get(open_paren..open_paren + 1)? != "(" {
        return None;
    }
    let close_paren = text.get(open_paren..)?.find(')')? + open_paren;

    let full = text.get(..=close_paren)?;
    let link_text = text.get(1..close_bracket)?;
    let target = text.get(open_paren + 1..close_paren)?;
    Some(ParsedLink {
        full,
        text: link_text,
        target,
    })
}

fn is_ipa_target(target: &str) -> bool {
    let t = target.trim();
    t.starts_with('/') && t.ends_with('/') && t.len() >= 2 && !t.starts_with("//")
}

fn parse_bracket_directive(text: &str) -> Option<&str> {
    if !text.starts_with('[') {
        return None;
    }
    let close = text.find(']')?;
    let content = text.get(1..close)?.trim();
    let is_directive = matches!(
        content.to_ascii_lowercase().as_str(),
        "laughs" | "whispers" | "sighs" | "gasp" | "gasps" | "laughter" | "pause" | "chuckle" | "snicker"
    );
    is_directive.then(|| text.get(..=close)).flatten()
}

fn parse_xml_tag(text: &str) -> Option<&str> {
    if !text.starts_with('<') {
        return None;
    }
    let close = text.find('>')?;
    let tag_inner = text.get(1..close)?.trim();
    let is_valid = tag_inner.starts_with('/')
        || tag_inner.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    is_valid.then(|| text.get(..=close)).flatten()
}

fn strip_markdown_and_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            in_tag = true;
            out.push(c);
            continue;
        }
        if c == '>' && in_tag {
            in_tag = false;
            out.push(c);
            continue;
        }
        if in_tag {
            out.push(c);
            continue;
        }

        // Strip formatting characters: `*`, `_`, `` ` ``
        if matches!(c, '*' | '_' | '`') {
            continue;
        }

        // Strip heading markers at line start
        if c == '#' && (out.is_empty() || out.ends_with('\n')) {
            while chars.peek().is_some_and(|&p| p == '#' || p == ' ') {
                chars.next();
            }
            continue;
        }

        // Strip list bullets at line start
        if (c == '-' || c == '+')
            && (out.is_empty() || out.ends_with('\n'))
            && chars.peek().is_some_and(|&p| p == ' ')
        {
            chars.next();
            continue;
        }

        out.push(c);
    }

    // Strip bare URLs (http:// or https://)
    let words: Vec<&str> = out.split_whitespace().collect();
    let mut filtered_words = Vec::with_capacity(words.len());
    for w in words {
        if w.starts_with("http://") || w.starts_with("https://") {
            continue;
        }
        filtered_words.push(w);
    }

    filtered_words.join(" ")
}

fn apply_lexicon(
    text: &str,
    lexicon: &BTreeMap<String, String>,
) -> (String, Vec<TtsDirectiveRecord>) {
    if lexicon.is_empty() {
        return (text.to_string(), Vec::new());
    }

    let mut out = String::with_capacity(text.len());
    let mut records = Vec::new();

    let mut i = 0;
    while i < text.len() {
        let Some(rest) = text.get(i..) else { break };

        if rest.starts_with('<')
            && let Some(tag_end) = rest.find('>')
        {
            out.push_str(rest.get(..=tag_end).unwrap_or_default());
            i += tag_end + 1;
            continue;
        }
        if rest.starts_with('[')
            && rest.contains("](")
            && let Some(link) = parse_markdown_link(rest)
        {
            out.push_str(link.full);
            i += link.full.len();
            continue;
        }

        let Some(c) = rest.chars().next() else { break };

        if c.is_alphanumeric() {
            let word_len = rest
                .char_indices()
                .take_while(|(_, ch)| ch.is_alphanumeric())
                .map(|(_, ch)| ch.len_utf8())
                .sum();
            let word = rest.get(..word_len).unwrap_or_default();
            if let Some(replacement) = lexicon.get(word) {
                out.push_str(replacement);
                records.push(TtsDirectiveRecord::Lexicon {
                    from: word.to_string(),
                    to: replacement.clone(),
                });
            } else {
                out.push_str(word);
            }
            i += word_len;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }

    (out, records)
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for c in text.trim().chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

#[cfg(test)]
#[path = "tests/sanitize.rs"]
mod tests;
