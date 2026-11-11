use super::*;

#[test]
fn locale_tags_are_told_from_special_tokens_and_text() {
    assert!(Tokens::is_locale_tag("<hr-HR>"));
    assert!(Tokens::is_locale_tag("<en-US>"));
    assert!(!Tokens::is_locale_tag("<unk>"));
    assert!(!Tokens::is_locale_tag("<pad>"));
    assert!(!Tokens::is_locale_tag("▁hello"));
}

#[test]
fn pieces_join_into_words_at_the_word_marker() {
    let t = Tokens {
        pieces: vec![
            "<unk>".into(),
            "<hr-HR>".into(),
            "▁Do".into(),
            "bar".into(),
            "▁dan".into(),
            ".".into(),
        ],
    };
    let (text, tags) = t.decode(&[1, 2, 3, 4, 5]);
    assert_eq!(text, "Dobar dan.");
    assert_eq!(tags, vec!["<hr-HR>"]);
}
