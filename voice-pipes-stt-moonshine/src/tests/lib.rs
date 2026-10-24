use super::*;

#[test]
fn factory_name_and_locality() {
    let factory = factory();
    assert_eq!(factory.name(), "moonshine");
    assert!(factory.is_local());
    assert!(factory.validate(&SttConfig::new("moonshine")).is_empty());
}

/// Options out of range are refused by name, before any model loads.
#[test]
fn an_out_of_range_option_is_refused() {
    let mut options = std::collections::HashMap::new();
    options.insert("pool_size".to_string(), serde_json::json!(0));
    let errs = factory().validate(&SttConfig::new("moonshine").with_options(options));
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(errs[0].field.contains("pool_size"), "{errs:?}");
}

#[test]
fn its_one_wizard_entry_keeps_its_built_in_place() {
    let choices = wizard_choices();
    let [choice] = choices.as_slice() else {
        panic!("one entry: {choices:?}");
    };
    assert_eq!(choice.menu_rank, 2);
    assert!(
        matches!(&choice.deploy, voice_pipes_sdk::WizardDeploy::Registry { key } if key == "moonshine")
    );
}

#[test]
fn an_empty_final_does_not_erase_what_was_already_heard() {
    // Interim "Neither.", then a final pass over the same audio plus trailing
    // silence that came back empty: the caller was heard, and must be answered.
    assert_eq!(
        settle_final_text(String::new(), "Neither.", true),
        "Neither."
    );
    assert_eq!(settle_final_text("  ".into(), "Neither.", true), "Neither.");
}

#[test]
fn a_final_with_words_stands_and_silence_stays_silent() {
    assert_eq!(
        settle_final_text("No, it doesn't.".into(), "Neither.", true),
        "No, it doesn't."
    );
    // Nothing was ever heard: an empty final is the right answer.
    assert_eq!(settle_final_text(String::new(), "", true), "");
    // An empty interim is not a verdict on the utterance and is left alone.
    assert_eq!(settle_final_text(String::new(), "Neither.", false), "");
}

/// The recorded fixtures, in the engine crate.
fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../voice-pipes/fixtures")
}

/// A clip of speech in the pipeline's 100 ms pieces.
fn fixture_utterance() -> Vec<Vec<i16>> {
    let path = fixtures().join("hello-how-are-you.wav");
    let samples: Vec<i16> = hound::WavReader::open(&path)
        .expect("fixture")
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .expect("16-bit samples");
    samples.chunks(1600).map(<[i16]>::to_vec).collect()
}

/// Two utterances queued back to back, the second before the first is
/// finalised, each get their own final: the stream's drain stops at
/// `is_last` instead of decoding the second utterance's audio as part of
/// the first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Moonshine model: set MOONSHINE_MODEL_DIR"]
async fn back_to_back_utterances_are_finalised_apart() {
    let Ok(dir) = std::env::var("MOONSHINE_MODEL_DIR") else {
        panic!("set MOONSHINE_MODEL_DIR to the model directory")
    };
    let stt = std::sync::Arc::new(
        MoonshineStt::with_options(std::path::Path::new(&dir), 1, DEFAULT_THREADS)
            .expect("model loads"),
    );
    voice_pipes_sdk::testing::back_to_back_utterances_stay_apart(
        move |audio, tx, cancel| async move {
            stt.transcribe_stream(RecognitionRequest::default(), audio, tx, cancel)
                .await
        },
        fixture_utterance,
        std::time::Duration::from_secs(120),
    )
    .await;
}

/// The cached decoder must give the same tokens as the full-prefix one:
/// it is a speed-up, not a different recogniser. Every 16 kHz mono fixture
/// is decoded both ways, plus the 16 kHz WAVs in `VP_TEST_MOONSHINE_WAV_DIR`
/// when set (recorded sessions, long utterances). Gated on real models: set
/// `VP_TEST_MOONSHINE_DIR` to a directory holding `encoder_model.onnx`,
/// `decoder_model.onnx`, `decoder_model_merged.onnx` and `tokenizer.json`.
#[test]
#[ignore = "needs the Moonshine models; set VP_TEST_MOONSHINE_DIR"]
fn cached_decoder_matches_full_prefix_decoder() {
    let Ok(dir) = std::env::var("VP_TEST_MOONSHINE_DIR") else {
        panic!("set VP_TEST_MOONSHINE_DIR to the model directory")
    };
    let dir = std::path::PathBuf::from(dir);
    let tokenizer = dir.join("tokenizer.json");
    let mut encoder = MoonshineEncoder::new(&dir.join("encoder_model.onnx"), 4).expect("encoder");
    let mut full = MoonshineDecoder::new(&dir.join("decoder_model.onnx"), &tokenizer, 4)
        .expect("full-prefix decoder");
    let mut cached = MoonshineDecoder::new(&dir.join("decoder_model_merged.onnx"), &tokenizer, 4)
        .expect("cached decoder");
    assert!(!full.is_cached());
    assert!(cached.is_cached());

    let mut dirs = vec![fixtures()];
    if let Ok(extra) = std::env::var("VP_TEST_MOONSHINE_WAV_DIR") {
        dirs.push(extra.into());
    }
    let paths: Vec<std::path::PathBuf> = dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).expect("wav dir"))
        .map(|e| e.expect("entry").path())
        .collect();
    let mut compared = 0;
    let mut spoke = 0;
    let (mut full_time, mut cached_time) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        let mut reader = hound::WavReader::open(&path).expect("wav");
        let spec = reader.spec();
        if spec.sample_rate != 16_000 || spec.channels != 1 || spec.bits_per_sample != 16 {
            continue;
        }
        let audio: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| f32::from(s.expect("sample")) / 32768.0)
            .collect();
        let enc = encoder.encode(&audio).expect("encode");
        let t = std::time::Instant::now();
        let a = full.decode(&enc, 256).expect("full-prefix decode");
        full_time += t.elapsed();
        let t = std::time::Instant::now();
        let b = cached.decode(&enc, 256).expect("cached decode");
        cached_time += t.elapsed();
        assert_eq!(a, b, "{} decodes differently", path.display());
        compared += 1;
        spoke += usize::from(!a.is_empty());
    }
    eprintln!(
        "{compared} clips identical ({spoke} with words); decode time full-prefix {full_time:?}, cached {cached_time:?}"
    );
    assert!(
        compared > 0 && spoke > 0,
        "no fixture exercised the decoders"
    );
}

#[tokio::test]
async fn metric_conformance() {
    let run = |mut audio_rx: tokio::sync::mpsc::Receiver<voice_pipes_sdk::AudioChunk>,
               result_tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::TranscriptionResult>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        let chunk = audio_rx.recv().await.unwrap();
        let delay = std::time::Duration::from_millis(20);
        tokio::time::sleep(delay).await;
        let audio_secs = chunk.samples.len() as f64 / 16_000.0;
        let metrics = voice_pipes_sdk::InferenceMetrics::measured(audio_secs, delay, 1)
            .with_delay_floor_ms(20);
        let res = voice_pipes_sdk::TranscriptionResult::new("hello", true)
            .with_segment_index(0)
            .with_inference_metrics(Some(metrics));
        result_tx.send(res).await.unwrap();
        Ok(())
    };
    voice_pipes_sdk::testing::assert_stt_metrics_conformance(
        run,
        vec![0; 16_000],
        16_000,
        std::time::Duration::from_millis(20),
    )
    .await
    .unwrap();
}

