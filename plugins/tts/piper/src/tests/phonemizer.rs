use super::*;

fn test_config() -> PiperModelConfig {
    serde_json::from_str(
        r#"{
            "audio": { "sample_rate": 22050 },
            "inference": {},
            "phoneme_id_map": {
                "_": [0],
                "^": [1],
                "$": [2],
                " ": [3],
                "h": [10],
                "ɛ": [11],
                "l": [12],
                "oʊ": [13]
            }
        }"#,
    )
    .unwrap()
}

#[test]
fn ipa_to_ids_simple() {
    let config = test_config();
    // Simulating IPA "hɛloʊ"
    let ids = ipa_to_ids("hɛloʊ", &config).unwrap();

    // Expected: BOS(1) pad(0) h(10) pad(0) ɛ(11) pad(0) l(12) pad(0) oʊ(13) pad(0) EOS(2) pad(0)
    assert_eq!(ids, vec![1, 0, 10, 0, 11, 0, 12, 0, 13, 0, 2, 0]);
}

#[test]
fn ipa_to_ids_with_space() {
    let config = test_config();
    let ids = ipa_to_ids("h ɛ", &config).unwrap();
    // BOS pad h pad SPACE pad ɛ pad EOS pad
    assert_eq!(ids, vec![1, 0, 10, 0, 3, 0, 11, 0, 2, 0]);
}

#[test]
fn ipa_to_ids_skips_unknown_chars() {
    let config = test_config();
    // ˈ is a stress mark not in our map -- should be skipped
    let ids = ipa_to_ids("ˈhɛl", &config).unwrap();
    assert_eq!(ids, vec![1, 0, 10, 0, 11, 0, 12, 0, 2, 0]);
}

#[test]
fn ipa_to_ids_empty_input() {
    let config = test_config();
    assert!(ipa_to_ids("", &config).unwrap().is_empty());
}

#[test]
fn ipa_to_ids_of_spaces_and_unknown_marks_is_empty() {
    let config = test_config();
    assert!(ipa_to_ids(" ˈ ", &config).unwrap().is_empty());
}

#[test]
fn extract_terminators_handles_commas_periods_and_runs() {
    assert_eq!(extract_clause_terminators("Hello, world!"), vec![',', '!']);
    assert_eq!(
        extract_clause_terminators("Wait... really? Yes!"),
        vec!['.', '?', '!']
    );
    assert_eq!(
        extract_clause_terminators("Decimals: 3.14 is pi, right?"),
        vec![':', ',', '?']
    );
    assert_eq!(
        extract_clause_terminators("... Hello — world!"),
        vec![',', '!']
    );
    assert_eq!(
        extract_clause_terminators("Version 1.2.3 is out!"),
        vec!['!']
    );
}

#[test]
fn reconstruct_clause_punctuation_rebuilds_pauses_and_endings() {
    let raw = "həlˈoʊ\nwˈɜːld";
    let res = reconstruct_clause_punctuation("Hello, world!", raw);
    assert_eq!(res, "həlˈoʊ, wˈɜːld!");

    let raw_single = "kˈɑːntɛkst";
    let res_single = reconstruct_clause_punctuation("context", raw_single);
    assert_eq!(res_single, "kˈɑːntɛkst.");
}

#[tokio::test]
async fn metric_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::AudioFrame>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        for _ in 0..2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            tx.send(voice_pipes_sdk::AudioFrame {
                samples: vec![0; 320],
                sample_rate: 22_050,
                channels: 1,
            })
            .await
            .unwrap();
        }
        Ok(())
    };

    let res = voice_pipes_sdk::testing::assert_tts_metrics_conformance(
        run,
        std::time::Duration::from_millis(15),
    )
    .await;
    assert!(res.is_ok(), "{res:?}");
}
