use super::*;

#[test]
fn split_sentence_with_remainder() {
    let (sentence, rest) = split_first_sentence("Hello world. How are you?", 36, 6, true).unwrap();
    assert_eq!(sentence, "Hello world. ");
    assert_eq!(rest, "How are you?");
}

#[test]
fn split_no_boundary() {
    assert!(split_first_sentence("No boundary yet", 36, 6, true).is_none());
}

#[test]
fn split_ends_with_period() {
    let (sentence, rest) = split_first_sentence("End with period.", 36, 6, true).unwrap();
    assert_eq!(sentence, "End with period.");
    assert_eq!(rest, "");
}

#[test]
fn split_exclamation() {
    let (sentence, rest) = split_first_sentence("Wow! That's great.", 36, 6, true).unwrap();
    assert_eq!(sentence, "Wow! ");
    assert_eq!(rest, "That's great.");
}

#[test]
fn split_question_mark() {
    let (sentence, rest) = split_first_sentence("Really? Yes.", 36, 6, true).unwrap();
    assert_eq!(sentence, "Really? ");
    assert_eq!(rest, "Yes.");
}

#[test]
fn split_newline() {
    let (sentence, rest) = split_first_sentence("Line one.\nLine two.", 36, 6, true).unwrap();
    assert_eq!(sentence, "Line one.\n");
    assert_eq!(rest, "Line two.");
}

#[test]
fn split_period_in_abbreviation_no_space() {
    // "Dr.Smith" has no space after the period, so no split
    assert!(split_first_sentence("Dr.Smith said hello", 36, 6, true).is_none());
}

#[test]
fn split_does_not_break_common_abbreviation() {
    let (sentence, rest) = split_first_sentence("Dr. Smith is here. Next.", 36, 6, true).unwrap();
    assert_eq!(sentence, "Dr. Smith is here. ");
    assert_eq!(rest, "Next.");
}

#[test]
fn split_does_not_break_eg_abbreviation() {
    let (sentence, rest) = split_first_sentence(
        "Use examples, e.g. apples and pears. Continue.",
        36,
        6,
        true,
    )
    .unwrap();
    assert_eq!(sentence, "Use examples, e.g. apples and pears. ");
    assert_eq!(rest, "Continue.");
}

#[test]
fn split_does_not_break_decimal_number() {
    let (sentence, rest) =
        split_first_sentence("Pi is about 3.14 in math. Continue.", 36, 6, true).unwrap();
    assert_eq!(sentence, "Pi is about 3.14 in math. ");
    assert_eq!(rest, "Continue.");
}

#[test]
fn split_soft_clause_boundary_on_comma_when_long_enough() {
    let (sentence, rest) = split_first_sentence(
        "Given the production latency profile and ongoing traffic spikes, we should lower first audio time before rollout",
    36, 6, true)
    .unwrap();
    assert_eq!(
        sentence,
        "Given the production latency profile and ongoing traffic spikes, "
    );
    assert_eq!(rest, "we should lower first audio time before rollout");
}

#[test]
fn split_does_not_soft_split_short_clause() {
    assert!(split_first_sentence("Quick test, keep going", 36, 6, true).is_none());
}

#[test]
fn split_keeps_url_token_intact() {
    let (sentence, rest) =
        split_first_sentence("See https://example.com/docs now. Next line.", 36, 6, true).unwrap();
    assert_eq!(sentence, "See https://example.com/docs now. ");
    assert_eq!(rest, "Next line.");
}

// ── Streaming: the same sentences however the text arrives ──

/// Every chunk of `text` split whole.
fn whole(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some((sentence, tail)) = split_first_sentence(rest, 36, 6, true) {
        out.push(sentence.to_string());
        rest = tail;
    }
    if !rest.trim().is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// Every chunk of `text` as a streaming caller produces it: the pieces
/// arrive one at a time and the buffer is split after each, then once
/// more at the end.
fn streamed<'a>(pieces: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut buffer = String::new();
    for piece in pieces {
        buffer.push_str(piece);
        while let Some((sentence, tail)) = split_first_sentence(&buffer, 36, 6, false) {
            out.push(sentence.to_string());
            buffer = tail.to_string();
        }
    }
    out.extend(whole(&buffer));
    out
}

const STREAMED_CASES: &[&str] = &[
    "It is about 3.14 metres long. Next.",
    "Dr. Smith will see you now. Thanks.",
    "Bring fruit, e.g. this apple. Done.",
    "The author is J. R. R. Tolkien. Read him.",
    "Wait! Really? Yes.\nNew line here.",
    "It costs 3,50 euros. Pay now.",
    "Given the production latency profile and ongoing traffic spikes, we should wait.",
    "Ends on an abbreviation, see Dr.",
    "We shipped the items. Then more.",
    "1. Milk
2. Bread
That is all.",
];

/// The regression: fed one character at a time, "3." was spoken before
/// "14" arrived and "Dr." before the name. Each case splits as it does
/// whole, fed in pieces of every size and by word.
#[test]
fn a_streamed_text_splits_as_it_does_whole() {
    for text in STREAMED_CASES {
        let chars: Vec<char> = text.chars().collect();
        let split = crate::testing::chunking_invariant(&chars, |chunks| {
            let pieces: Vec<String> = chunks.iter().map(|c| c.iter().collect()).collect();
            streamed(pieces.iter().map(String::as_str))
        });
        assert_eq!(split, whole(text), "{text:?}");
        let by_word: Vec<&str> = text.split_inclusive(' ').collect();
        assert_eq!(streamed(by_word), whole(text), "by word: {text:?}");
    }
}

/// The chunks keep the whitespace between them, so joining them gives the
/// text back: a history or transcript built from them reads "Hello. How",
/// not "Hello.How".
#[test]
fn the_chunks_concatenate_back_to_the_text() {
    for text in STREAMED_CASES {
        assert_eq!(whole(text).concat(), *text);
        let by_char: Vec<String> = text.chars().map(String::from).collect();
        assert_eq!(streamed(by_char.iter().map(String::as_str)).concat(), *text);
    }
}

/// Mid-stream, a mark at the end of the buffer is held; at the end it is
/// flushed.
#[test]
fn a_trailing_mark_waits_for_the_next_piece_until_the_end() {
    for text in ["about 3.", "about 3. ", "see Dr.", "yes!", "well,"] {
        assert!(
            split_first_sentence(text, 1, 1, false).is_none(),
            "{text:?}"
        );
        assert!(split_first_sentence(text, 1, 1, true).is_some(), "{text:?}");
    }
}

/// Whitespace wider than a byte (a no-break space in "10 EUR", an em
/// space) before a full stop is split around, not inside: the token
/// before the mark starts after the whole whitespace character.
#[test]
fn multi_byte_whitespace_before_a_mark_does_not_panic() {
    for text in [
        "Price 10\u{a0}EUR. Next one.",
        "Hello\u{2003}world. Next one.",
        "Wait\u{3000}here. Next.",
        "See\u{a0}Dr. Smith. Done.",
    ] {
        let split = whole(text);
        assert_eq!(split.concat(), text, "{text:?}");
        assert_eq!(split.len(), 2, "{text:?}: {split:?}");
        let by_char: Vec<String> = text.chars().map(String::from).collect();
        assert_eq!(
            streamed(by_char.iter().map(String::as_str)),
            split,
            "{text:?}"
        );
    }
}

/// An abbreviation is a whole token: "items." and "systems." end in "ms."
/// and "devs." in "vs.", but they end their sentences.
#[test]
fn a_word_ending_like_an_abbreviation_ends_its_sentence() {
    for (text, first) in [
        ("We shipped the items. Next one.", "We shipped the items. "),
        ("Both systems. Then more.", "Both systems. "),
        ("Ask the devs. They know.", "Ask the devs. "),
        ("Call (Dr. Who) now. Done.", "Call (Dr. Who) now. "),
        ("Ask Mrs. Brown. Done.", "Ask Mrs. Brown. "),
    ] {
        assert_eq!(
            whole(text).first().map(String::as_str),
            Some(first),
            "{text:?}"
        );
    }
}

/// Initials stay with the name they belong to; a lone "I." or "A." is a
/// word and ends its sentence.
#[test]
fn initials_stay_with_the_name() {
    assert_eq!(
        whole("The author is J. R. R. Tolkien. Read him."),
        ["The author is J. R. R. Tolkien. ", "Read him."]
    );
    assert_eq!(whole("Ask J. Smith. Done."), ["Ask J. Smith. ", "Done."]);
    assert_eq!(whole("So do I. Then we go."), ["So do I. ", "Then we go."]);
    assert_eq!(
        whole("It got an A. Then a B."),
        ["It got an A. ", "Then a B."]
    );
}

/// A newline ends a chunk whatever follows it, whole or streamed: a list
/// is spoken line by line however it arrives.
#[test]
fn a_newline_ends_a_chunk_whole_and_streamed() {
    let text = "First line\nSecond line\nThird";
    assert_eq!(whole(text), ["First line\n", "Second line\n", "Third"]);
    let by_char: Vec<String> = text.chars().map(String::from).collect();
    assert_eq!(streamed(by_char.iter().map(String::as_str)), whole(text));
}

#[test]
fn sentence_splitter_does_not_split_inside_tts_tags_or_ipa() {
    let text = "Wait <break time=\"1.5s\"/> here. Next sentence.";
    assert_eq!(
        whole(text),
        ["Wait <break time=\"1.5s\"/> here. ", "Next sentence."]
    );

    let ipa_text = "See [Kokoro](/kˈoʊ.kə.ɹoʊ/) now. Next line.";
    assert_eq!(
        whole(ipa_text),
        ["See [Kokoro](/kˈoʊ.kə.ɹoʊ/) now. ", "Next line."]
    );
}
