use std::time::Duration;
use tokio_util::sync::CancellationToken;
use voice_pipes_sdk::{AudioFrame, TtsConfig};

#[test]
fn factory_metadata() {
    let f = super::factory();
    assert_eq!(f.name(), "neutts");
    assert!(f.is_local());
    assert!(f.aliases().contains(&"neutts-air"));
}

#[test]
fn config_validation_requires_model_and_vocoder() {
    let f = super::factory();
    let empty_cfg: TtsConfig = toml::from_str("provider = \"neutts\"").unwrap();
    let errs = f.validate(&empty_cfg);
    assert_eq!(errs.len(), 2);
}

#[tokio::test]
async fn metrics_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<AudioFrame>, _cancel: CancellationToken| async move {
        tokio::time::sleep(Duration::from_millis(15)).await;
        let _ = tx
            .send(AudioFrame {
                samples: vec![0i16; 1600],
                sample_rate: 16_000,
                channels: 1,
            })
            .await;
        Ok(())
    };

    let res = voice_pipes_sdk::testing::assert_tts_metrics_conformance(
        run,
        Duration::from_millis(15),
    )
    .await;
    assert!(res.is_ok(), "{res:?}");
}

#[test]
fn test_prompt_tokenization() {
    let model_path = std::path::Path::new("../../../voice-pipes/models/neutts-air-Q4_0.gguf");
    let vocoder_path = std::path::Path::new("../../../voice-pipes/models/neucodec_decoder.onnx");
    if !model_path.exists() || !vocoder_path.exists() {
        return;
    }
    let engine = super::engine::NeuTtsEngine::load(
        model_path,
        vocoder_path,
        voice_pipes_onnx::ExecutionTarget::Cpu,
        1,
    )
    .expect("engine load");

    let input_p = "həlˈoʊ wˈɜːld ðɪs ɪz ɐ ɹˈiːəltˈaɪm spˈiːtʃ tˈɛst ˌɔn sˌiːpˌiːjˈuː";
    let prompt_tokens = engine.build_prompt_tokens(input_p).expect("build prompt tokens");
    assert!(!prompt_tokens.is_empty(), "prompt tokens non-empty");

    // Also test real espeak phonemization on the exact phrase from the session:
    if let Ok(espeak_out) = voice_pipes_sdk::phonemize::run_espeak_with_language("Hello! How can I assist you today?", "en-us") {
        println!("espeak output: {espeak_out:?}");
        let tokens = engine.build_prompt_tokens(&espeak_out).expect("tokenize espeak output");
        println!("tokens len: {}", tokens.len());
        for &tok in &tokens {
            let piece = engine.model.vocab().token_to_piece(llama_cpp_2::token::LlamaToken::new(tok as i32), false, None);
            print!("[{}:{}]", tok, String::from_utf8_lossy(&piece));
        }
        println!();
    }

    // Verify reference codes start with JO_REFERENCE_CODES[0]
    let expected_ref0 = super::engine::SPEECH_CODE_OFFSET + super::voices::JO_REFERENCE_CODES[0] as u32;
    assert!(prompt_tokens.contains(&expected_ref0));
}

#[test]
fn test_synthesize_hello() {
    let model_path = std::path::Path::new("../../../voice-pipes/models/neutts-air-Q4_0.gguf");
    let vocoder_path = std::path::Path::new("../../../voice-pipes/models/neucodec_decoder.onnx");
    if !model_path.exists() || !vocoder_path.exists() {
        return;
    }
    let mut engine = super::engine::NeuTtsEngine::load(
        model_path,
        vocoder_path,
        voice_pipes_onnx::ExecutionTarget::Cpu,
        4,
    )
    .expect("engine load");

    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);

    let espeak_out = voice_pipes_sdk::phonemize::run_espeak_with_language("Hello, how can I help you today?", "en-us")
        .expect("espeak hello");

    let start = std::time::Instant::now();
    engine.synthesize(&espeak_out, &cancel, &tx).expect("synthesize hello");
    let elapsed = start.elapsed();

    let mut samples = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        samples.extend_from_slice(&frame.samples);
    }
    println!(
        "Synthesize finished in {elapsed:?}, samples={} (duration={:.2}s, RTF={:.2})",
        samples.len(),
        samples.len() as f64 / 24_000.0,
        elapsed.as_secs_f64() / (samples.len() as f64 / 24_000.0)
    );

    let _ = std::fs::create_dir_all("target");
    let wav_path = "target/hello_output.wav";
    let data_len = (samples.len() * 2) as u32;
    let file_len = data_len + 36;
    if let Ok(mut file) = std::fs::File::create(wav_path) {
        use std::io::Write;
        let _ = file.write_all(b"RIFF");
        let _ = file.write_all(&file_len.to_le_bytes());
        let _ = file.write_all(b"WAVEfmt ");
        let _ = file.write_all(&16u32.to_le_bytes());
        let _ = file.write_all(&1u16.to_le_bytes());
        let _ = file.write_all(&1u16.to_le_bytes());
        let _ = file.write_all(&24_000u32.to_le_bytes());
        let _ = file.write_all(&48_000u32.to_le_bytes());
        let _ = file.write_all(&2u16.to_le_bytes());
        let _ = file.write_all(&16u16.to_le_bytes());
        let _ = file.write_all(b"data");
        let _ = file.write_all(&data_len.to_le_bytes());
        for &sample in &samples {
            let _ = file.write_all(&sample.to_le_bytes());
        }
    }
}

#[test]
fn engine_decode_tokens_with_real_vocoder() {
    let model_path = std::path::Path::new("../../../voice-pipes/models/neutts-air-Q4_0.gguf");
    let vocoder_path = std::path::Path::new("../../../voice-pipes/models/neucodec_decoder.onnx");
    if !model_path.exists() || !vocoder_path.exists() {
        return;
    }
    let mut engine = super::engine::NeuTtsEngine::load(
        model_path,
        vocoder_path,
        voice_pipes_onnx::ExecutionTarget::Cpu,
        1,
    )
    .expect("engine load");

    let samples = engine.decode_tokens(super::voices::JO_REFERENCE_CODES).expect("decode reference codes");
    assert!(!samples.is_empty(), "expected non-empty audio samples");

    let _ = std::fs::create_dir_all("target");
    let wav_path = "target/neutts_reference_audio.wav";
    let data_len = (samples.len() * 2) as u32;
    let file_len = data_len + 36;
    if let Ok(mut file) = std::fs::File::create(wav_path) {
        use std::io::Write;
        let _ = file.write_all(b"RIFF");
        let _ = file.write_all(&file_len.to_le_bytes());
        let _ = file.write_all(b"WAVEfmt ");
        let _ = file.write_all(&16u32.to_le_bytes());
        let _ = file.write_all(&1u16.to_le_bytes());
        let _ = file.write_all(&1u16.to_le_bytes());
        let _ = file.write_all(&24_000u32.to_le_bytes());
        let _ = file.write_all(&48_000u32.to_le_bytes());
        let _ = file.write_all(&2u16.to_le_bytes());
        let _ = file.write_all(&16u16.to_le_bytes());
        let _ = file.write_all(b"data");
        let _ = file.write_all(&data_len.to_le_bytes());
        for &sample in &samples {
            let _ = file.write_all(&sample.to_le_bytes());
        }
        println!("Saved reference audio to {wav_path}, len: {}", samples.len());
    }
}

#[test]
fn phonemize_keeps_numbers_times_and_abbreviations_intact() {
    let out = super::phonemize_preserving_punctuation(
        "Dr. Smith paid $4.99 for 1,500 items at 2:30 PM, right?",
        "en-us",
    )
    .expect("phonemize succeeds");
    // Only one comma before "right?" and the final question mark should appear as clause punctuation
    assert!(
        out.ends_with('?'),
        "expected trailing question mark, got: {out}"
    );
    assert_eq!(
        out.matches('.').count(),
        0,
        "Dr. and $4.99 must not split into mid-sentence period clauses: {out}"
    );
    assert_eq!(
        out.matches(':').count(),
        0,
        "2:30 must not split into a colon clause: {out}"
    );
    assert_eq!(
        out.matches(',').count(),
        1,
        "1,500 must not split into a comma clause (only ', right?' has a comma): {out}"
    );
}

#[test]
fn test_neutts_engine_full_synthesis() {
    let model_path = std::path::Path::new("../../../voice-pipes/models/neutts-air-Q4_0.gguf");
    let vocoder_path = std::path::Path::new("../../../voice-pipes/models/neucodec_decoder.onnx");
    if !model_path.exists() || !vocoder_path.exists() {
        return;
    }
    let mut engine = super::engine::NeuTtsEngine::load(
        model_path,
        vocoder_path,
        voice_pipes_onnx::ExecutionTarget::Cpu,
        4,
    )
    .expect("engine load");

    let cancel = CancellationToken::new();

    for (idx, phrase) in [
        "Why did the scarecrow win an award?",
        "Because he was outstanding in his field!",
        "Sure, here's one for you: Why don't scientists trust atoms?",
    ]
    .iter()
    .enumerate()
    {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let espeak_out =
            super::phonemize_preserving_punctuation(phrase, "en-us").expect("phonemize phrase");
        println!("Phrase [{idx}]: {phrase:?} -> phonemes: {espeak_out:?}");
        let start = std::time::Instant::now();
        engine
            .synthesize(&espeak_out, &cancel, &tx)
            .expect("synthesis succeeds");
        let elapsed = start.elapsed();

        let mut total_samples = 0;
        let mut all_samples = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            assert_eq!(frame.sample_rate, 24_000);
            assert_eq!(frame.channels, 1);
            total_samples += frame.samples.len();
            all_samples.extend_from_slice(&frame.samples);
        }
        let dur_s = total_samples as f64 / 24_000.0;
        println!(
            "Phrase [{idx}] completed in {elapsed:?}: {total_samples} samples ({dur_s:.2}s, RTF={:.2})",
            elapsed.as_secs_f64() / dur_s.max(0.01)
        );
        assert!(total_samples > 0, "expected generated audio frames");

        let wav_path = format!("../../../voice-pipes/captures/neutts_phrase_{idx}.wav");
        let data_len = (all_samples.len() * 2) as u32;
        let file_len = data_len + 36;
        if let Ok(mut file) = std::fs::File::create(&wav_path) {
            use std::io::Write;
            let _ = file.write_all(b"RIFF");
            let _ = file.write_all(&file_len.to_le_bytes());
            let _ = file.write_all(b"WAVEfmt ");
            let _ = file.write_all(&16u32.to_le_bytes());
            let _ = file.write_all(&1u16.to_le_bytes());
            let _ = file.write_all(&1u16.to_le_bytes());
            let _ = file.write_all(&24_000u32.to_le_bytes());
            let _ = file.write_all(&48_000u32.to_le_bytes());
            let _ = file.write_all(&2u16.to_le_bytes());
            let _ = file.write_all(&16u16.to_le_bytes());
            let _ = file.write_all(b"data");
            let _ = file.write_all(&data_len.to_le_bytes());
            for &sample in &all_samples {
                let _ = file.write_all(&sample.to_le_bytes());
            }
        }

        let max_amp = all_samples.iter().map(|&s| s.abs()).max().unwrap_or(0);
        let rms = (all_samples.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
            / (total_samples as f64))
            .sqrt();
        assert!(max_amp > 1000, "expected non-silent synthesized audio");
        assert!(rms > 100.0, "expected audible RMS energy");
    }
}
