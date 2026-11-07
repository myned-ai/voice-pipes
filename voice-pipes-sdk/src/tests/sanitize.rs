//! Tests for dual-track text sanitization, think tags, and pronunciation lexicon.

use std::collections::BTreeMap;
use crate::sanitize::{
    sanitize_dual_track, SanitizeConfig, ThinkTagFilter, TtsDirectiveRecord,
};

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
    assert_eq!(res.spoken, "Run sequel and MySQL queries <break time=\"SQL\"/> now.");
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
