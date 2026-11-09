use super::*;

fn push_frag(ledger: &mut AudioLedger, text: &str, duration_ms: u64, wt: Option<Vec<WordTiming>>) {
    // sample_rate 1000 => sample_count == duration_ms (1 sample/ms)
    ledger.push(
        text.to_string(),
        None,
        0,
        ledger.entries.len() as u32,
        duration_ms,
        1000,
        wt,
    );
}

#[test]
fn proportional_prefix_snaps_to_word_boundary() {
    let s = "Kingdom stories often involve a hero and a quest to save a kingdom from a dark force.";
    // ~47% of audio heard -> roughly the first half of the sentence, cut at a word boundary.
    let out = proportional_prefix(s, 0.47);
    assert!(
        s.starts_with(out),
        "prefix must be a real prefix of the source"
    );
    assert!(
        !out.is_empty() && out.len() < s.len(),
        "must be a partial prefix"
    );
    // No trailing partial word / whitespace.
    assert!(!out.ends_with(char::is_whitespace));
    assert_eq!(out, "Kingdom stories often involve a hero and");
}

#[test]
fn proportional_prefix_edges() {
    let s = "one two three";
    assert_eq!(proportional_prefix(s, 1.0), s);
    assert_eq!(proportional_prefix(s, 0.0), "");
    assert_eq!(proportional_prefix(s, -1.0), "");
    // Single word, no interior whitespace: keep the proportional char prefix.
    assert_eq!(
        proportional_prefix("supercalifragilistic", 0.5),
        "supercalif"
    );
}

#[test]
fn text_at_offset_partial_fragment_without_word_timings_is_proportional() {
    // Single 3299ms fragment, no word timings — the pocket-http / case.
    let mut ledger = AudioLedger::default();
    let sentence =
        "Kingdom stories often involve a hero and a quest to save a kingdom from a dark force.";
    push_frag(&mut ledger, sentence, 3299, None);
    let heard = ledger.text_at_offset(1560).expect("some text heard");
    // Must NOT retain the whole sentence for a ~47% offset.
    assert_ne!(
        heard, sentence,
        "partial playback must not claim the full fragment"
    );
    assert!(sentence.starts_with(&heard));
    assert_eq!(heard, "Kingdom stories often involve a hero and");
}

#[test]
fn text_at_offset_fully_played_fragment_returns_all() {
    let mut ledger = AudioLedger::default();
    let sentence = "All of this was heard.";
    push_frag(&mut ledger, sentence, 1000, None);
    assert_eq!(ledger.text_at_offset(1000).as_deref(), Some(sentence));
    assert_eq!(ledger.text_at_offset(5000).as_deref(), Some(sentence));
}

#[test]
fn text_at_offset_zero_or_empty_is_none() {
    let mut ledger = AudioLedger::default();
    assert_eq!(ledger.text_at_offset(0), None);
    assert_eq!(ledger.text_at_offset(500), None); // empty ledger
    push_frag(&mut ledger, "hi there", 1000, None);
    assert_eq!(ledger.text_at_offset(0), None);
}

#[test]
fn text_at_offset_with_lexicon_replacement_matches_display_word_boundary() {
    let mut ledger = AudioLedger::default();
    let display = "I love **SQL** and databases.";
    let spoken = "I love sequel and databases.";
    let wt = vec![
        WordTiming {
            word: "I".into(),
            offset_ms: 0,
            duration_ms: 100,
        },
        WordTiming {
            word: "love".into(),
            offset_ms: 100,
            duration_ms: 200,
        },
        WordTiming {
            word: "sequel".into(),
            offset_ms: 300,
            duration_ms: 300,
        },
        WordTiming {
            word: "and".into(),
            offset_ms: 600,
            duration_ms: 150,
        },
        WordTiming {
            word: "databases.".into(),
            offset_ms: 750,
            duration_ms: 400,
        },
    ];
    ledger.push(
        display.to_string(),
        Some(spoken.to_string()),
        0,
        0,
        1150,
        1000,
        Some(wt),
    );

    // After word 3 ("sequel", offset 300ms) has started:
    let heard = ledger.text_at_offset(350).unwrap();
    assert_eq!(heard, "I love **SQL**");
}
