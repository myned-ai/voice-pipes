//! The Silero detector against the real model, built through `factory()` as
//! a pipeline builds it, on the recorded fixtures in `voice-pipes/fixtures`.
//!
//! The model is not in the repository, so these tests are ignored; asked
//! for without it they fail naming the file. Point `SILERO_MODEL` at it
//! (default `voice-pipes/models/silero_vad.onnx`):
//!
//! ```sh
//! cargo test --release -p voice-pipes-vad-silero --test provider -- --ignored --test-threads=1
//! ```

use std::path::PathBuf;

use voice_pipes_sdk::audio::resample::resample;
use voice_pipes_sdk::vad::VadBackend;
use voice_pipes_sdk::{AudioFrame, BuildContext, VadConfig};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Recorded speech: "Hello, how are you?" at 16 kHz.
fn speech() -> AudioFrame {
    let path = workspace().join("voice-pipes/fixtures/hello-how-are-you.wav");
    let mut reader =
        hound::WavReader::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
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

// ── Silero VAD integration tests ────────────────────────────

fn model() -> PathBuf {
    std::env::var_os("SILERO_MODEL").map_or_else(
        || workspace().join("voice-pipes/models/silero_vad.onnx"),
        PathBuf::from,
    )
}

/// The model's path, failing the test that needs it when it is missing.
fn have_silero() -> PathBuf {
    let model = model();
    assert!(
        model.exists(),
        "no Silero model at {}: set SILERO_MODEL",
        model.display()
    );
    model
}

/// The model as an ONNX session, opened as the backend opens it (one
/// thread, the CPU), for the tests that drive the graph themselves.
fn session() -> ort::session::Session {
    let options = voice_pipes_onnx::SessionOptions::new(1);
    voice_pipes_onnx::open_session(&have_silero(), &options)
        .unwrap_or_else(|e| panic!("the model opens: {e}"))
}

/// Silero at `[vad]`'s default thresholds (0.5 to enter speech, 0.35 to
/// leave it).
fn silero() -> Box<dyn VadBackend> {
    voice_pipes_vad_silero::factory()
        .build(
            &VadConfig::new("silero").with_model(Some(have_silero())),
            &BuildContext::new(),
        )
        .unwrap_or_else(|e| panic!("Silero builds: {e}"))
}

#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_loads_model() {
    let vad = silero();
    assert_eq!(vad.name(), "silero");
    let component = vad.as_component().expect("a component");
    assert_eq!(component.id(), "vad:silero");
}

#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_model_io_info() {
    let session = session();
    eprintln!("Inputs:");
    for input in session.inputs() {
        eprintln!("  name={}, dtype={:?}", input.name(), input.dtype());
    }
    eprintln!("Outputs:");
    for output in session.outputs() {
        eprintln!("  name={}, dtype={:?}", output.name(), output.dtype());
    }
}

#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_silence_is_not_speech() {
    let mut vad = silero();

    // 512 samples of silence at 16kHz
    let silence = vec![0i16; 512];
    let result = vad.process_chunk(&silence, 16000).unwrap();
    eprintln!("silence speech_prob: {:.4}", result.speech_prob);
    assert!(
        !result.is_speech,
        "silence should not be classified as speech"
    );
    assert!(
        result.speech_prob < 0.3,
        "silence prob should be low, got {}",
        result.speech_prob
    );
}

#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_speech_is_detected() {
    let mut vad = silero();

    // A 440Hz sine wave at moderate amplitude — not perfect speech, but
    // exercises the full inference path.  Real speech would score higher.
    let samples: Vec<i16> = (0..512)
        .map(|i| {
            let t = i as f64 / 16000.0;
            (f64::sin(2.0 * std::f64::consts::PI * 440.0 * t) * 10000.0) as i16
        })
        .collect();

    let result = vad.process_chunk(&samples, 16000).unwrap();
    eprintln!("tone speech_prob: {:.4}", result.speech_prob);
    // We don't assert speech=true since a sine isn't real speech.
    // The point is that inference runs without error and returns a valid probability.
    assert!(
        (0.0..=1.0).contains(&result.speech_prob),
        "prob out of range: {}",
        result.speech_prob
    );
}

#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_state_resets() {
    let mut vad = silero();

    // Run some chunks to build up state
    let silence = vec![0i16; 512];
    for _ in 0..5 {
        vad.process_chunk(&silence, 16000).unwrap();
    }

    // Reset and verify inference still works
    vad.reset();
    let result = vad.process_chunk(&silence, 16000).unwrap();
    assert!((0.0..=1.0).contains(&result.speech_prob));
}

#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_downsamples_48khz() {
    let mut vad = silero();

    // 960 samples at 48kHz = 20ms = same duration as 320 samples at 16kHz
    // After decimation to 16kHz we get 320 samples, which is < 512,
    // so this will buffer. Send two chunks to trigger inference.
    let silence_48k = vec![0i16; 960];
    let r1 = vad.process_chunk(&silence_48k, 48000).unwrap();
    let r2 = vad.process_chunk(&silence_48k, 48000).unwrap();
    eprintln!(
        "48kHz chunk 1 prob: {:.4}, chunk 2 prob: {:.4}",
        r1.speech_prob, r2.speech_prob
    );
    // Second chunk should have triggered inference, giving a real probability
    assert!((0.0..=1.0).contains(&r2.speech_prob));
}

#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_rejects_tones() {
    let mut vad = silero();

    // Pure tones are not speech — silero should give low probability.
    // Amplitude peaks around ~600i16 out of 32768 (< 2%).
    let samples: Vec<i16> = (0..512)
        .map(|i| {
            let t = i as f32 / 16000.0;
            let val = f32::sin(2.0 * std::f32::consts::PI * 400.0 * t) * 400.0
                + f32::sin(2.0 * std::f32::consts::PI * 1000.0 * t) * 200.0;
            val as i16
        })
        .collect();

    let result = vad.process_chunk(&samples, 16000).unwrap();
    eprintln!("tone speech_prob: {:.6}", result.speech_prob);

    // Pure tones should not trigger speech detection.
    assert!(
        result.speech_prob < 0.3,
        "Tones should not be classified as speech: {}",
        result.speech_prob
    );
}

// ── Silero VAD on recorded speech ───────────────────────────

/// Feed recorded speech through Silero at 16 kHz, and verify it produces
/// speech probabilities matching the config used in test-live-llama.toml.
#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_detects_speech_16khz() {
    // The defaults are the values test-live-llama.toml uses.
    let mut vad = silero();

    let audio = speech();
    // Already 16 kHz; the resample is the identity the pipeline applies.
    let resampled = resample(audio.clone(), 16000).unwrap();
    eprintln!(
        "resampled to {} samples at 16000 Hz",
        resampled.samples.len()
    );

    // Log amplitude stats to diagnose gain/clipping issues
    let max_amp = resampled
        .samples
        .iter()
        .map(|&s| (s as i32).abs())
        .max()
        .unwrap_or(0);
    let rms = {
        let sum: f64 = resampled
            .samples
            .iter()
            .map(|&s| (s as f64) * (s as f64))
            .sum();
        (sum / resampled.samples.len() as f64).sqrt()
    };
    eprintln!("16 kHz input: max_amp={max_amp}, rms={rms:.1}");

    // Feed in 512-sample chunks (= 32 ms at 16 kHz)
    let mut max_prob: f32 = 0.0;
    let mut speech_chunk_count = 0;
    let chunk_size = 512;
    for (i, chunk) in resampled.samples.chunks(chunk_size).enumerate() {
        if chunk.len() < chunk_size {
            break; // skip incomplete tail
        }
        let result = vad.process_chunk(chunk, 16000).unwrap();
        if result.speech_prob > max_prob {
            max_prob = result.speech_prob;
        }
        if result.is_speech {
            speech_chunk_count += 1;
        }
        if i < 5 || result.speech_prob > 0.1 {
            eprintln!(
                "  chunk {:>3}: prob={:.4} is_speech={}",
                i, result.speech_prob, result.is_speech,
            );
        }
    }
    eprintln!("peak prob: {max_prob:.4}, speech chunks: {speech_chunk_count}");
    assert!(
        max_prob > 0.5,
        "silero should detect real speech with prob > 0.5, got peak={max_prob:.4}",
    );
    assert!(
        speech_chunk_count > 0,
        "silero should classify at least some chunks as speech",
    );
}

/// Same test but feeding 48 kHz audio (like the real mic path) to verify
/// the sinc resampler in `process_chunk` works correctly.
#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_detects_speech_48khz() {
    let mut vad = silero();

    let audio = speech();

    // Resample to 48 kHz to simulate real mic input
    let audio_48k = resample(audio, 48000).unwrap();
    eprintln!(
        "upsampled to {} samples at 48000 Hz",
        audio_48k.samples.len()
    );

    // Log amplitude stats
    let max_amp = audio_48k
        .samples
        .iter()
        .map(|&s| (s as i32).abs())
        .max()
        .unwrap_or(0);
    let rms = {
        let sum: f64 = audio_48k
            .samples
            .iter()
            .map(|&s| (s as f64) * (s as f64))
            .sum();
        (sum / audio_48k.samples.len() as f64).sqrt()
    };
    eprintln!("48 kHz input: max_amp={max_amp}, rms={rms:.1}");

    // Feed in 960-sample chunks (= 20 ms at 48 kHz, same as real mic)
    let mut max_prob: f32 = 0.0;
    let mut speech_chunk_count = 0;
    let chunk_size = 960; // 20 ms at 48 kHz
    for (i, chunk) in audio_48k.samples.chunks(chunk_size).enumerate() {
        if chunk.len() < chunk_size {
            break;
        }
        let result = vad.process_chunk(chunk, 48000).unwrap();
        if result.speech_prob > max_prob {
            max_prob = result.speech_prob;
        }
        if result.is_speech {
            speech_chunk_count += 1;
        }
        if i < 5 || result.speech_prob > 0.1 {
            eprintln!(
                "  chunk {:>3}: prob={:.4} is_speech={}",
                i, result.speech_prob, result.is_speech,
            );
        }
    }
    eprintln!("48 kHz path — peak prob: {max_prob:.4}, speech chunks: {speech_chunk_count}");
    assert!(
        max_prob > 0.5,
        "silero should detect 48 kHz-resampled speech with prob > 0.5, got peak={max_prob:.4}",
    );
    assert!(
        speech_chunk_count > 0,
        "silero should classify at least some 48 kHz chunks as speech",
    );
}

/// Bypasses `process_chunk` entirely — takes recorded speech, converts it to
/// 16 kHz f32 ourselves, and feeds windows directly to `infer_window`.
/// This isolates whether the issue is in the model or in `process_chunk`.
#[test]
#[ignore = "requires model files on disk"]
fn silero_vad_raw_inference_on_speech() {
    use ort::value::Tensor;

    let mut session = session();

    let audio = speech();

    // Proper resample to 16 kHz
    let audio_16k = resample(audio, 16000).unwrap();

    // Convert to f32 [-1, 1]
    let samples_f32: Vec<f32> = audio_16k
        .samples
        .iter()
        .map(|&s| s as f32 / 32768.0)
        .collect();

    let max_f32 = samples_f32.iter().copied().fold(0.0_f32, f32::max);
    let min_f32 = samples_f32.iter().copied().fold(0.0_f32, f32::min);
    eprintln!(
        "f32 range: [{:.4}, {:.4}], {} samples",
        min_f32,
        max_f32,
        samples_f32.len()
    );

    // Run raw inference with 512-sample windows + 64-sample context (official Silero approach)
    let window_size = 512;
    let context_size = 64;
    let mut state = vec![0.0_f32; 2 * 128];
    let mut context = vec![0.0_f32; context_size];
    let mut max_prob: f32 = 0.0;
    let mut chunks_above_05 = 0;

    for (i, window) in samples_f32.chunks(window_size).enumerate() {
        if window.len() < window_size {
            break;
        }

        // Prepend context (64 samples) to window (512 samples) → 576 total
        let mut input_data = Vec::with_capacity(context_size + window_size);
        input_data.extend_from_slice(&context);
        input_data.extend_from_slice(window);

        let input = Tensor::from_array(([1usize, context_size + window_size], input_data)).unwrap();
        let state_t = Tensor::from_array(([2usize, 1usize, 128usize], state.clone())).unwrap();
        let sr_t = Tensor::from_array(([] as [usize; 0], vec![16000_i64])).unwrap();

        let outputs = session.run(ort::inputs![input, state_t, sr_t]).unwrap();

        let prob_tensor = outputs[0].try_extract_tensor::<f32>().unwrap();
        let prob = prob_tensor.1.iter().copied().next().unwrap_or(0.0);

        let state_out = outputs[1].try_extract_tensor::<f32>().unwrap();
        state.clear();
        state.extend(state_out.1.iter().copied());

        // Update context with last 64 samples of raw window
        context.clear();
        context.extend_from_slice(&window[window_size - context_size..]);

        if prob > max_prob {
            max_prob = prob;
        }
        if prob > 0.5 {
            chunks_above_05 += 1;
        }

        if prob > 0.01 || i < 3 {
            eprintln!("  window {i:>3}: prob={prob:.4}");
        }
    }

    eprintln!("peak prob: {max_prob:.4}, chunks > 0.5: {chunks_above_05}");
    assert!(
        max_prob > 0.5,
        "raw silero inference should detect speech, got peak={max_prob:.4}",
    );
}
