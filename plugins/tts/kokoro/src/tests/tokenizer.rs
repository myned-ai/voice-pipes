use super::*;

#[test]
fn load_bundled_vocab() {
    let vocab = KokoroVocab::load_bundled().unwrap();
    // The bundled vocab has ~100+ entries
    assert!(vocab.len() > 100, "vocab too small: {}", vocab.len());
}

#[test]
fn tokenize_known_chars() {
    let vocab = KokoroVocab::load_bundled().unwrap();
    // Space is mapped to 16 in the vocab
    let tokens = vocab.tokenize(" ");
    assert_eq!(tokens, vec![16]);
}

#[test]
fn tokenize_drops_unknown_chars() {
    let vocab = KokoroVocab::load_bundled().unwrap();
    // '@' is not in the vocab -> should be dropped
    let tokens = vocab.tokenize("@@@");
    assert!(tokens.is_empty());
}

#[test]
fn tokenize_truncates_to_max_length() {
    let vocab = KokoroVocab::load_bundled().unwrap();
    // Create a very long string of spaces
    let long_input = " ".repeat(1000);
    let tokens = vocab.tokenize(&long_input);
    assert_eq!(tokens.len(), MAX_PHONEME_LENGTH);
}

#[test]
fn from_json_parses_simple_vocab() {
    let json = r#"{ " ": 16, "a": 43 }"#;
    let vocab = KokoroVocab::from_json(json).unwrap();
    assert_eq!(vocab.tokenize("a a"), vec![43, 16, 43]);
}
