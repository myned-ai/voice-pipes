use super::*;

/// These tests run the real program, so they are ignored by default and
/// fail, rather than pass, when it is missing.
fn require_espeak() {
    assert!(
        phonemize("espeak-ng", "test", None).is_ok() || phonemize("espeak", "test", None).is_ok(),
        "espeak-ng is not on PATH"
    );
}

#[test]
#[ignore = "needs espeak-ng on PATH"]
fn espeak_returns_ipa_for_hello() {
    require_espeak();
    let ipa = run_espeak("hello").unwrap();
    assert!(!ipa.is_empty(), "IPA output should not be empty");
}

#[test]
#[ignore = "needs espeak-ng on PATH"]
fn espeak_with_language_returns_ipa() {
    require_espeak();
    let ipa = run_espeak_with_language("hello", "en-us").unwrap();
    assert!(!ipa.is_empty(), "IPA output should not be empty");
}

/// Text that looks like a command-line option (a markdown bullet, a
/// flag-shaped word) is phonemised as text.
#[test]
#[ignore = "needs espeak-ng on PATH"]
fn espeak_treats_dash_prefixed_text_as_text() {
    require_espeak();
    let bullet = run_espeak("- hello").unwrap();
    assert!(
        bullet.contains('h'),
        "expected IPA for 'hello', got {bullet:?}"
    );
    let flag = run_espeak("-w foo").unwrap();
    assert!(flag.contains('f'), "expected IPA for 'foo', got {flag:?}");
    let with_lang = run_espeak_with_language("-w foo", "en-us").unwrap();
    assert!(with_lang.contains('f'), "{with_lang:?}");
}

/// A paragraph far beyond any argv limit goes through in one call.
#[test]
#[ignore = "needs espeak-ng on PATH"]
fn espeak_handles_a_long_paragraph() {
    require_espeak();
    let sentence = "The quick brown fox jumps over the lazy dog. ";
    let paragraph = sentence.repeat(20_000 / sentence.len() + 1);
    assert!(paragraph.len() > 20_000);
    let ipa = run_espeak(&paragraph).unwrap();
    assert!(
        ipa.len() > 10_000,
        "expected IPA for the whole text, got {} bytes",
        ipa.len()
    );
}

#[test]
fn invalid_language_codes_are_rejected() {
    assert!(run_espeak_with_language("hello", "-v").is_err());
    assert!(run_espeak_with_language("hello", "--path=/evil").is_err());
    assert!(run_espeak_with_language("hello", "en; rm -rf /").is_err());
    assert!(
        run_espeak_with_language("hello", "a_very_long_language_code_that_exceeds_limit").is_err()
    );
    assert!(run_espeak_with_language("hello", "").is_err());
}
