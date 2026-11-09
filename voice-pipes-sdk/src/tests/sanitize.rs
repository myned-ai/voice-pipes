//! Tests for dual-track text sanitization, think tags, and pronunciation lexicon.

use crate::sanitize::{SanitizeConfig, ThinkTagFilter, TtsDirectiveRecord, sanitize_dual_track};
use std::collections::BTreeMap;

#[test]
fn think_tag_filter_strips_across_chunk_boundaries() {
    let mut filter = ThinkTagFilter::new();
    let c1 = filter.push("Hello <th");
    let c2 = filter.push("ink>some secret internal thoughts</thi");
    let c3 = filter.push("nk>world!");
    let c4 = filter.flush();

    let combined = format!("{c1}{c2}{c3}{c4}");
    assert_eq!(combined, "Hello world!");
}

#[test]
fn think_tag_filter_handles_pipe_variant() {
    let mut filter = ThinkTagFilter::new();
    let out = filter.push("Start <|think|>reasoning</|think|> and end.");
    assert_eq!(out, "Start  and end.");
}

#[test]
fn markdown_stripped_from_spoken_preserved_in_display() {
    let cfg = SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec![],
        provider_name: "piper".into(),
        lexicon: BTreeMap::new(),
    };

    let input = "Check **bold** and *italic* and `code` at [Docs](https://example.com) 🎉";
    let res = sanitize_dual_track(input, &cfg);

    assert_eq!(
        res.display,
        "Check **bold** and *italic* and `code` at [Docs](https://example.com) 🎉"
    );
    assert_eq!(res.spoken, "Check bold and italic and code at Docs");
}

#[test]
fn kokoro_ipa_link_preserved_for_kokoro_stripped_for_piper() {
    let cfg_kokoro = SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec!["/ipa/".into()],
        provider_name: "kokoro".into(),
        lexicon: BTreeMap::new(),
    };

    let input = "[Kokoro](/kˈoʊkəɹoʊ/) is fast.";
    let res_kokoro = sanitize_dual_track(input, &cfg_kokoro);

    assert_eq!(res_kokoro.display, "Kokoro is fast.");
    assert_eq!(res_kokoro.spoken, "[Kokoro](/kˈoʊkəɹoʊ/) is fast.");
    assert_eq!(
        res_kokoro.directives,
        vec![TtsDirectiveRecord::IpaPhoneme {
            word: "Kokoro".into(),
            ipa: "/kˈoʊkəɹoʊ/".into()
        }]
    );

    let cfg_piper = SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec![],
        provider_name: "piper".into(),
        lexicon: BTreeMap::new(),
    };

    let res_piper = sanitize_dual_track(input, &cfg_piper);
    assert_eq!(res_piper.display, "Kokoro is fast.");
    assert_eq!(res_piper.spoken, "Kokoro is fast.");
    assert_eq!(res_piper.stripped.len(), 1);
}

#[test]
fn passthrough_tags_preserved_in_spoken_stripped_in_display() {
    let cfg = SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec!["<emotion".into(), "<break".into(), "[laughs]".into()],
        provider_name: "elevenlabs".into(),
        lexicon: BTreeMap::new(),
    };

    let input = "<emotion value=\"happy\"/> Hi! <break time=\"500ms\"/> [laughs]";
    let res = sanitize_dual_track(input, &cfg);

    assert_eq!(res.display, "Hi!");
    assert_eq!(
        res.spoken,
        "<emotion value=\"happy\"/> Hi! <break time=\"500ms\"/> [laughs]"
    );
    assert_eq!(res.directives.len(), 3);
}

#[test]
fn unsupported_tags_stripped_from_both_and_recorded() {
    let cfg = SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec![],
        provider_name: "piper".into(),
        lexicon: BTreeMap::new(),
    };

    let input = "[whispers] Secret <break time=\"1s\"/>";
    let res = sanitize_dual_track(input, &cfg);

    assert_eq!(res.display, "Secret");
    assert_eq!(res.spoken, "Secret");
    assert_eq!(res.stripped.len(), 2);
}

#[test]
fn lexicon_respects_word_boundaries_and_skips_tags() {
    let mut lex = BTreeMap::new();
    lex.insert("SQL".into(), "sequel".into());

    let cfg = SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec!["<break".into()],
        provider_name: "kokoro".into(),
        lexicon: lex,
    };

    let input = "Run SQL and MySQL queries <break time=\"SQL\"/> now.";
    let res = sanitize_dual_track(input, &cfg);

    assert_eq!(res.display, "Run SQL and MySQL queries now.");
    assert_eq!(
        res.spoken,
        "Run sequel and MySQL queries <break time=\"SQL\"/> now."
    );
    assert_eq!(
        res.directives,
        vec![
            TtsDirectiveRecord::TagPassthrough {
                raw: "<break time=\"SQL\"/>".into()
            },
            TtsDirectiveRecord::Lexicon {
                from: "SQL".into(),
                to: "sequel".into()
            }
        ]
    );
}

#[test]
fn bare_urls_verbalized_naturally_in_spoken_track() {
    let cfg = SanitizeConfig {
        sanitize_markdown: true,
        passthrough_tags: vec![],
        provider_name: "kokoro".into(),
        lexicon: BTreeMap::new(),
    };

    // 1. Google URL ending with sentence punctuation
    let input1 = "The URL for Google is https://www.google.com.";
    let res1 = sanitize_dual_track(input1, &cfg);
    assert_eq!(
        res1.display,
        "The URL for Google is https://www.google.com."
    );
    assert_eq!(res1.spoken, "The URL for Google is google dot com.");

    // 2. Short GitHub repository path
    let input2 = "Check https://github.com/myned-ai/voice-pipes for code.";
    let res2 = sanitize_dual_track(input2, &cfg);
    assert_eq!(
        res2.display,
        "Check https://github.com/myned-ai/voice-pipes for code."
    );
    assert_eq!(
        res2.spoken,
        "Check github dot com slash myned-ai slash voice-pipes for code."
    );

    // 3. Localhost with port
    let input3 = "Connect to http://localhost:8080/api now.";
    let res3 = sanitize_dual_track(input3, &cfg);
    assert_eq!(res3.display, "Connect to http://localhost:8080/api now.");
    assert_eq!(res3.spoken, "Connect to localhost port 8080 slash api now.");

    // 4. Long URL with query string drops the messy query/hash
    let input4 =
        "See https://docs.google.com/document/d/1X98yZ30491823091283091283/edit?usp=sharing.";
    let res4 = sanitize_dual_track(input4, &cfg);
    assert_eq!(
        res4.display,
        "See https://docs.google.com/document/d/1X98yZ30491823091283091283/edit?usp=sharing."
    );
    assert_eq!(res4.spoken, "See docs dot google dot com.");

    // 5. Markdown autolink syntax <https://...>
    let input5 = "Autolink <https://www.google.com> here.";
    let res5 = sanitize_dual_track(input5, &cfg);
    assert_eq!(res5.display, "Autolink <https://www.google.com> here.");
    assert_eq!(res5.spoken, "Autolink google dot com here.");
}
