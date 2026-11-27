//! End-to-end CLI tests for `voice-pipes verify-audio` and optional `pocket-candle`
//! real-checkpoint audio verification.

use std::f64::consts::PI;
use std::path::Path;
use std::process::Command;

fn write_test_wav(path: &Path, samples: &[f32], sample_rate: u32) {
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = u32::try_from(samples.len() * 2).expect("len");
    let byte_rate = sample_rate * 2;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write wav");
}

fn synth_clean_speech_like(sr: u32, dur_s: f64) -> Vec<f32> {
    let n = (dur_s * f64::from(sr)) as usize;
    let mut out = Vec::with_capacity(n);
    let mut phase = 0.0f64;
    for i in 0..n {
        let t = (i as f64) / f64::from(sr);
        let f0 = 175.0 + 30.0 * (2.0 * PI * 1.5 * t).sin();
        phase += 2.0 * PI * f0 / f64::from(sr);
        let env = 0.55 + 0.45 * (2.0 * PI * 3.5 * t).sin();
        let s = 0.22 * env * (phase.sin() + 0.5 * (2.0 * phase).sin() + 0.25 * (3.0 * phase).sin());
        out.push(s as f32);
    }
    out
}

#[test]
fn verify_audio_cli_passes_clean_wav_and_rejects_silence() {
    let bin = env!("CARGO_BIN_EXE_voice-pipes");
    let tmp = tempfile::tempdir().expect("tempdir");
    let clean_wav = tmp.path().join("clean.wav");
    let silent_wav = tmp.path().join("silent.wav");
    let json_out = tmp.path().join("report.json");
    let approved_wav = tmp.path().join("golden/approved.wav");

    write_test_wav(&clean_wav, &synth_clean_speech_like(24_000, 1.2), 24_000);
    write_test_wav(&silent_wav, &vec![0.0f32; 24_000], 24_000);

    // 1. Clean WAV passes and writes structured JSON report.
    let ok = Command::new(bin)
        .args([
            "verify-audio",
            "--audio",
            clean_wav.to_str().expect("utf8"),
            "--golden",
            clean_wav.to_str().expect("utf8"),
            "--json-out",
            json_out.to_str().expect("utf8"),
        ])
        .output()
        .expect("run verify-audio");
    assert!(
        ok.status.success(),
        "expected pass, stderr={}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_out).expect("json")).expect("parse");
    assert_eq!(parsed["passed"], serde_json::Value::Bool(true));

    // 2. Silent WAV fails with non-zero exit code.
    let bad = Command::new(bin)
        .args([
            "verify-audio",
            "--audio",
            silent_wav.to_str().expect("utf8"),
        ])
        .output()
        .expect("run verify-audio on silence");
    assert!(!bad.status.success(), "silent WAV must exit non-zero");

    // 3. `--approve-to` copies a passing WAV to the golden path.
    let approve = Command::new(bin)
        .args([
            "verify-audio",
            "--audio",
            clean_wav.to_str().expect("utf8"),
            "--approve-to",
            approved_wav.to_str().expect("utf8"),
        ])
        .output()
        .expect("run approve-to");
    assert!(
        approve.status.success(),
        "approve-to should pass on clean WAV"
    );
    assert!(approved_wav.exists());
}

/// Real-checkpoint regression gate: synthesizes speech with `pocket-candle` (`alba.safetensors`)
/// and asserts that `voice_pipes_bench::audio_verify::AudioVerifier` passes every acoustic,
/// crackle, and prosody gate.
///
/// Run with:
/// `cargo test --release -p voice-pipes-cli --features tts-pocket-candle --test verify_audio_cli -- pocket_candle_real_checkpoint_passes_audio_verifier --ignored --nocapture`
#[cfg(feature = "tts-pocket-candle")]
#[tokio::test]
#[ignore = "requires locally cached kyutai/pocket-tts checkpoint in HF cache"]
async fn pocket_candle_real_checkpoint_passes_audio_verifier() {
    use std::path::PathBuf;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use voice_pipes_bench::audio_verify::AudioVerifier;
    use voice_pipes_sdk::config::TtsConfig;
    use voice_pipes_sdk::tts::SynthesisRequest;

    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("USERPROFILE/HOME set");
    let hub = PathBuf::from(home).join(".cache/huggingface/hub");
    let model_st = hub.join(
        "models--kyutai--pocket-tts/snapshots/39592ff23c9ef80098bb74895d104c26275fe2c9/languages/english/model.safetensors",
    );
    let tok_model = hub.join(
        "models--kyutai--pocket-tts-without-voice-cloning/snapshots/d29db7978e464fb90cb3359ee0c69a273b9142cc/tokenizer.model",
    );
    let alba_st = hub.join(
        "models--kyutai--pocket-tts-without-voice-cloning/snapshots/e81d79e8194ad4c7ce879c87a4258ef20cbf2487/languages/english/embeddings/alba.safetensors",
    );
    assert!(model_st.exists(), "missing {}", model_st.display());

    let tmp = tempfile::tempdir().expect("tempdir");
    let voices_dir = tmp.path().join("voices");
    std::fs::create_dir_all(&voices_dir).unwrap();
    for (src, dst) in [
        (&model_st, tmp.path().join("model.safetensors")),
        (&tok_model, tmp.path().join("tokenizer.model")),
        (&alba_st, voices_dir.join("alba.safetensors")),
    ] {
        std::fs::hard_link(src, &dst)
            .or_else(|_| std::fs::copy(src, &dst).map(|_| ()))
            .unwrap();
    }

    let mut cfg = TtsConfig::new("pocket-candle").with_voice(Some("alba".into()));
    cfg.options.insert(
        "model_dir".into(),
        serde_json::json!(tmp.path().display().to_string()),
    );
    cfg.options.insert(
        "voices_dir".into(),
        serde_json::json!(voices_dir.display().to_string()),
    );

    let factory = voice_pipes_tts_pocket_candle::factory();
    let provider = factory
        .build(&cfg, &voice_pipes_sdk::BuildContext::default())
        .expect("build pocket-candle");

    let (tx, mut rx) = mpsc::channel(32);
    let synth = tokio::spawn(async move {
        provider
            .synthesize_stream(
                SynthesisRequest::new(
                    "Hello! Welcome to Voice Pipes running natively in pure Rust.",
                ),
                tx,
                CancellationToken::new(),
            )
            .await
    });

    let mut pcm = Vec::<f32>::new();
    let mut sr = 24_000u32;
    while let Some(frame) = rx.recv().await {
        sr = frame.sample_rate;
        pcm.extend(frame.samples.iter().map(|&s| f32::from(s) / 32768.0));
    }
    synth.await.expect("join").expect("synthesize");

    let report = AudioVerifier::default().verify_samples(&pcm, sr, None, None);
    println!("{report}");
    assert!(
        report.passed(),
        "pocket-candle failed AudioVerifier:\n{report}"
    );
}
