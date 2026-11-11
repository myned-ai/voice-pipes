use super::*;

/// A 16-bit mono WAV as a frame.
fn read_wav(path: &std::path::Path) -> AudioFrame {
    let mut reader =
        hound::WavReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let spec = reader.spec();
    let samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .expect("16-bit samples");
    AudioFrame {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    }
}

// The streaming buffer is bounded to the cap; oldest samples drop.
#[test]
fn trim_buffer_to_cap_bounds_length_and_keeps_tail() {
    let mut buf: Vec<f32> = (0..100).map(|i| i as f32).collect();
    let dropped = trim_buffer_to_cap(&mut buf, 30);
    assert_eq!(dropped, 70);
    assert_eq!(buf.len(), 30);
    // Kept the trailing window (values 70..100).
    assert_eq!(buf.first().copied(), Some(70.0));
    assert_eq!(buf.last().copied(), Some(99.0));
}

#[test]
fn trim_buffer_to_cap_noop_under_cap_or_disabled() {
    let mut buf: Vec<f32> = vec![1.0, 2.0, 3.0];
    assert_eq!(trim_buffer_to_cap(&mut buf, 10), 0);
    assert_eq!(buf.len(), 3);
    // max == 0 disables the cap.
    assert_eq!(trim_buffer_to_cap(&mut buf, 0), 0);
    assert_eq!(buf.len(), 3);
}

// ── Empty-final rescue ───────────────────────────────────

fn earlier(covered_whole_buffer: bool) -> EarlierWords {
    EarlierWords {
        text: "what I need for this trick".into(),
        words: vec![],
        buffer_len: 38_400,
        covered_whole_buffer,
    }
}

/// A final with words stands, whatever came before; so does an empty final
/// when no earlier decode found anything (the VAD passed a noise).
#[test]
fn a_final_with_words_or_nothing_before_it_stands() {
    assert_eq!(rescue_plan("hello", Some(&earlier(true))), Rescue::Keep);
    assert_eq!(rescue_plan("", None), Rescue::Keep);
    assert_eq!(rescue_plan("   ", None), Rescue::Keep);
}

/// An empty final after an earlier decode found words takes those words:
/// directly when that decode saw the whole buffer, by decoding the prefix it
/// came from when it saw only the trailing window.
#[test]
fn an_empty_final_after_words_is_rescued() {
    assert_eq!(rescue_plan("", Some(&earlier(true))), Rescue::UseEarlier);
    assert_eq!(
        rescue_plan(" ", Some(&earlier(false))),
        Rescue::Redecode(38_400)
    );
}

/// The export the model tests load: `PARAKEET_PRECISION=fp32` for the
/// full-precision files, the int8 ones otherwise.
fn precision_from_env() -> Precision {
    Precision::from_name(&std::env::var("PARAKEET_PRECISION").unwrap_or_default())
}

/// Streams the clip at `PARAKEET_CLIP` through the provider as the pipeline
/// does (100 ms chunks, the last marked `is_last`) and requires a final with
/// words. A clip that greedy decoding loses on its final pass exercises the
/// rescue; the output says whether it fired.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Parakeet model and a clip in PARAKEET_CLIP"]
async fn a_streamed_clip_yields_a_final_with_words() {
    let Ok(clip) = std::env::var("PARAKEET_CLIP") else {
        panic!("set PARAKEET_CLIP to a 16 kHz mono WAV")
    };
    let dir = std::path::PathBuf::from(
        std::env::var("PARAKEET_MODEL_DIR").unwrap_or_else(|_| "models/parakeet-tdt-v3".into()),
    );
    let stt = ParakeetStt::with_pool_size(&dir, precision_from_env(), 1).expect("model loads");
    let audio = read_wav(std::path::Path::new(&clip));
    assert_eq!(audio.sample_rate, 16_000);

    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let feeder = tokio::spawn(async move {
        let pieces: Vec<_> = audio.samples.chunks(1600).map(<[i16]>::to_vec).collect();
        let n = pieces.len();
        for (i, samples) in pieces.into_iter().enumerate() {
            let is_last = i + 1 == n;
            audio_tx
                .send(AudioChunk { samples, is_last })
                .await
                .unwrap();
            // Real time, so in-progress decodes run as they do live.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let run = stt.transcribe_stream(RecognitionRequest::default(), audio_rx, tx, cancel.clone());
    let collect = async {
        while let Some(r) = rx.recv().await {
            eprintln!("final={} text={:?}", r.is_final, r.text);
            if r.is_final {
                cancel.cancel();
                return r.text;
            }
        }
        String::new()
    };
    let (_ran, final_text) = tokio::join!(run, collect);
    feeder.abort();
    let rescues = stt
        .as_component()
        .map(|c| c.telemetry().extras.get("empty_final_rescues").cloned());
    eprintln!("empty_final_rescues={rescues:?}");
    assert!(!final_text.trim().is_empty(), "the final came back empty");
}

/// A clip of speech in the pipeline's 100 ms pieces.
fn fixture_utterance() -> Vec<Vec<i16>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../voice-pipes/fixtures/hello-how-are-you.wav");
    let audio = read_wav(&path);
    audio.samples.chunks(1600).map(<[i16]>::to_vec).collect()
}

/// Two utterances queued back to back, the second before the first is
/// finalised, each get their own final: the stream's drain stops at
/// `is_last` instead of decoding the second utterance's audio as part of
/// the first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Parakeet model: set PARAKEET_MODEL_DIR"]
async fn back_to_back_utterances_are_finalised_apart() {
    let Ok(dir) = std::env::var("PARAKEET_MODEL_DIR") else {
        panic!("set PARAKEET_MODEL_DIR to the model directory")
    };
    let stt = std::sync::Arc::new(
        ParakeetStt::with_pool_size(std::path::Path::new(&dir), precision_from_env(), 1)
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

// ── Options ──────────────────────────────────────────────

fn cfg_with(options: &[(&str, serde_json::Value)]) -> SttConfig {
    let mut cfg = SttConfig::new("parakeet-tdt");
    for (k, v) in options {
        cfg.options.insert((*k).to_string(), v.clone());
    }
    cfg
}

/// Unset, the files are the int8 export the wizard downloads and the
/// language the English v2 default; `fp32` names the full-precision files.
#[test]
fn precision_defaults_to_int8_and_fp32_names_the_other_export() {
    let o = ParakeetOptions::read(&cfg_with(&[])).expect("valid");
    assert_eq!(o.precision, Precision::Int8);
    assert_eq!(o.language, None);
    assert_eq!(Precision::Int8.encoder_file(), "encoder-model.int8.onnx");
    assert_eq!(
        Precision::Int8.decoder_file(),
        "decoder_joint-model.int8.onnx"
    );

    let o = ParakeetOptions::read(&cfg_with(&[
        ("precision", "fp32".into()),
        ("language", "el".into()),
    ]))
    .expect("valid");
    assert_eq!(o.precision, Precision::Fp32);
    assert_eq!(o.language.as_deref(), Some("el"));
    assert_eq!(Precision::Fp32.encoder_file(), "encoder-model.onnx");
    assert_eq!(Precision::Fp32.decoder_file(), "decoder_joint-model.onnx");
}

/// A precision that is not an export is refused at validation, not at load.
#[test]
fn an_unknown_precision_is_a_config_error() {
    let errs = ParakeetOptions::read(&cfg_with(&[("precision", "fp16".into())]))
        .err()
        .expect("refused");
    assert!(
        errs.iter().any(|e| e.to_string().contains("precision")),
        "{errs:?}"
    );
}

/// Greek writes its question mark as `;`; the Latin `?` the vocabulary
/// carries is swapped in the text and the words, for Greek only.
#[test]
fn greek_results_use_the_greek_question_mark() {
    let mut text = String::from("τι κάνεις?");
    let mut words = vec![
        WordInfo::new("τι", std::time::Duration::ZERO, std::time::Duration::ZERO),
        WordInfo::new(
            "κάνεις?",
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
        ),
    ];
    localise(&mut text, &mut words, "el-GR");
    assert_eq!(text, "τι κάνεις;");
    assert_eq!(words[1].word, "κάνεις;");

    let mut text = String::from("how are you?");
    let mut words = vec![];
    localise(&mut text, &mut words, "en-US");
    assert_eq!(text, "how are you?");
}

/// The schema lists only what the provider reads: the model directory, not
/// a `model` file nothing loads.
#[test]
fn schema_lists_model_dir_but_not_the_unread_model_field() {
    let cfg = SttConfig::new("parakeet-tdt")
        .with_model_dir(Some(PathBuf::from("models/parakeet-tdt")))
        .with_model(Some(PathBuf::from("models/ignored.onnx")));
    let keys: Vec<String> = schema::schema(&cfg)
        .descriptors
        .into_iter()
        .map(|d| d.key)
        .collect();
    assert!(keys.iter().any(|k| k == "model_dir"), "{keys:?}");
    assert!(!keys.iter().any(|k| k == "model"), "{keys:?}");
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

#[tokio::test]
async fn small_pool_serves_many_concurrent_streams_by_releasing_slots_between_steps() {
    let sets = (0..2)
        .map(|_| SessionSet::Fake {
            delay: std::time::Duration::ZERO,
            reply_words: "hello world".into(),
        })
        .collect();
    let pool = Arc::new(SessionPool::new(sets, "Parakeet session sets").expect("pool"));
    let stt = Arc::new(ParakeetStt {
        pool,
        language: DEFAULT_LANGUAGE.into(),
        min_initial_buffer_ms: 800.0,
        min_inference_step_ms: 1000.0,
        streaming_window_ms: DEFAULT_STREAMING_WINDOW_MS,
        core: Arc::new(ComponentCore::new("stt:parakeet", ComponentKind::Stt)),
    });

    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let stt = Arc::clone(&stt);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(8);
            let (res_tx, mut res_rx) = tokio::sync::mpsc::channel(8);
            let cancel = tokio_util::sync::CancellationToken::new();
            let cancel_stream = cancel.clone();
            let stream_task = tokio::spawn(async move {
                stt.transcribe_stream(
                    RecognitionRequest::default(),
                    audio_rx,
                    res_tx,
                    cancel_stream,
                )
                .await
            });

            // All 16 streams send a sub-threshold partial chunk (no slot checked out)
            // and rendezvous at the barrier simultaneously before sending the final chunk.
            audio_tx
                .send(AudioChunk {
                    samples: vec![100i16; 320],
                    is_last: false,
                })
                .await
                .expect("send partial");
            barrier.wait().await;
            audio_tx
                .send(AudioChunk {
                    samples: vec![100i16; 3200],
                    is_last: true,
                })
                .await
                .expect("send chunk");
            let final_res = res_rx.recv().await.expect("final result");
            assert!(final_res.is_final);
            assert_eq!(final_res.text, "hello world");
            cancel.cancel();
            stream_task.await.expect("join").expect("stream ok");
        }));
    }
    for h in handles {
        h.await.expect("session task completed");
    }
}
