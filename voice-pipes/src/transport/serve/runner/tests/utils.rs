use super::{sanitize_file_component, speech_stats};

/// The production failure: a "successful" synthesis whose audio is
/// near-silent noise (peak ≈ 280) must read as zero speech, while a
/// normal clip (leading pause, then speech) reads as mostly speech.
#[test]
fn speech_stats_separates_silence_from_speech() {
    let rate = 24_000;
    let silent: Vec<i16> = (0..rate * 2)
        .map(|i| if i % 7 == 0 { 250 } else { -3 })
        .collect();
    let (peak, ratio) = speech_stats(&silent, rate as u32);
    assert!(peak < 300, "{peak}");
    assert_eq!(ratio, 0.0);

    let mut spoken: Vec<i16> = vec![0; rate / 2]; // 0.5 s pause
    spoken.extend((0..rate).map(|i| if i % 2 == 0 { 6000 } else { -6000 })); // 1 s speech
    let (peak, ratio) = speech_stats(&spoken, rate as u32);
    assert_eq!(peak, 6000);
    assert!((ratio - 10.0 / 15.0).abs() < 0.01, "{ratio}");

    assert_eq!(speech_stats(&[], rate as u32), (0, 0.0));
}

#[test]
fn sanitize_blocks_traversal_and_separators() {
    assert_eq!(sanitize_file_component("..\\..\\x"), "x");
    assert_eq!(sanitize_file_component("../../etc/passwd"), "etcpasswd");
    assert_eq!(sanitize_file_component("C:\\evil"), "Cevil");
    assert_eq!(
        sanitize_file_component("req-42_final.v2"),
        "req-42_final.v2"
    );
    assert_eq!(sanitize_file_component("///"), "unnamed");
}
