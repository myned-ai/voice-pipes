use super::*;

const MINIMAL_CONFIG: &str = r#"{
    "audio": { "sample_rate": 22050 },
    "inference": { "noise_scale": 0.667, "length_scale": 1.0, "noise_w": 0.8 },
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
}"#;

#[test]
fn parse_minimal_config() {
    let config: PiperModelConfig = serde_json::from_str(MINIMAL_CONFIG).unwrap();
    assert_eq!(config.audio.sample_rate, 22050);
    assert_eq!(config.audio.channels, 1); // default
    assert_eq!(config.inference.noise_scale, 0.667);
    assert!(config.phoneme_id_map.contains_key("h"));
    assert_eq!(config.phoneme_id_map["_"], vec![0]);
}

#[test]
fn parse_malformed_json() {
    let result: Result<PiperModelConfig, _> = serde_json::from_str("{ broken }");
    assert!(result.is_err());
}

#[test]
fn phoneme_lookup() {
    let config: PiperModelConfig = serde_json::from_str(MINIMAL_CONFIG).unwrap();
    assert_eq!(config.phoneme_to_ids("h"), Some(&vec![10]));
    assert_eq!(config.phoneme_to_ids("missing"), None);
}

/// A voice whose config states no sample rate or no channels cannot be
/// played: it is refused at load, naming the file, instead of producing
/// frames a sink cannot interpret.
#[test]
fn zero_sample_rate_or_channels_is_refused_naming_the_file() {
    let path = Path::new("voices/broken.onnx.json");
    let cases = [
        MINIMAL_CONFIG.replace("\"sample_rate\": 22050", "\"sample_rate\": 0"),
        MINIMAL_CONFIG.replace(
            "\"sample_rate\": 22050",
            "\"sample_rate\": 22050, \"channels\": 0",
        ),
    ];
    for json in cases {
        match PiperModelConfig::parse(&json, path) {
            Err(TtsError::ModelLoadFailed(msg)) => {
                assert!(msg.contains("broken.onnx.json"), "{msg}");
            }
            other => panic!("expected ModelLoadFailed, got {other:?}"),
        }
    }
    assert!(PiperModelConfig::parse(MINIMAL_CONFIG, path).is_ok());
}

/// A two-speaker voice needs a speaker id the engine does not pass, so it
/// is refused at load naming the file, not on the first turn.
#[test]
fn multi_speaker_voice_is_refused_naming_the_file() {
    let path = Path::new("voices/two-speakers.onnx.json");
    let two = MINIMAL_CONFIG.replacen(
        "\"audio\"",
        "\"num_speakers\": 2, \"speaker_id_map\": { \"a\": 0, \"b\": 1 }, \"audio\"",
        1,
    );
    match PiperModelConfig::parse(&two, path) {
        Err(TtsError::ModelLoadFailed(msg)) => {
            assert!(msg.contains("two-speakers.onnx.json"), "{msg}");
        }
        other => panic!("expected ModelLoadFailed, got {other:?}"),
    }
    let one = MINIMAL_CONFIG.replacen("\"audio\"", "\"num_speakers\": 1, \"audio\"", 1);
    assert!(PiperModelConfig::parse(&one, path).is_ok());
}
