use super::*;

// A missing vocab file is a hard error, not a silent empty map that
// would make `detokenize` emit raw numeric token IDs as the transcript.
#[test]
fn missing_vocab_is_an_error() {
    let dir = std::env::temp_dir().join("vp_nemotron_vocab_missing_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(load_vocab(&dir).is_err());
}

#[test]
fn empty_vocab_file_is_an_error() {
    let dir = std::env::temp_dir().join("vp_nemotron_vocab_empty_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tokenizer.vocab"), "").unwrap();
    assert!(load_vocab(&dir).is_err());
}

#[test]
fn valid_vocab_loads_and_detokenizes() {
    let dir = std::env::temp_dir().join("vp_nemotron_vocab_ok_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Piece 0 = "\u{2581}hi" (leading-space marker), piece 1 = "there".
    std::fs::write(dir.join("tokenizer.vocab"), "\u{2581}hi\t0\nthere\t0\n").unwrap();
    let vocab = load_vocab(&dir).expect("valid vocab loads");
    assert_eq!(detokenize(&vocab, &[0, 1]), "hithere");
}
