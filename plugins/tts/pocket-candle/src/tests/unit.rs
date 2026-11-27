use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{AudioFrame, SynthesisRequest, TtsConfig, TtsError, TtsFactory, TtsProvider};

use crate::config::{
    FRAME_SAMPLES, PocketCandleConfig, SAMPLE_RATE, SilentStartGuard, VoiceFingerprint,
    resolve_safe_voice_path,
};
use crate::factory::PocketCandleTtsFactory;
use crate::model::kv_cache::{LayerKvPrefix, split_causal_attention};
use crate::model::mimi::StreamingCausalConv1d;
use crate::model::state::VoiceState;
use crate::model::{PocketEngine, SilentStartProbe};
use crate::provider::PocketCandleTts;
use crate::tokenizer::PocketTokenizer;
use crate::voices::VoiceCache;

fn make_test_provider(max_concurrent: usize, max_frames: usize) -> PocketCandleTts {
    let engine = Arc::new(PocketEngine::tiny_for_tests().expect("tiny engine"));
    let voices = Arc::new(VoiceCache::new(8));
    let voice_state = Arc::new(engine.empty_voice_state("alba".to_string()));
    voices.insert(voice_state);

    let mut cfg = TtsConfig::new("pocket-candle").with_voice(Some("alba".into()));
    cfg.options
        .insert("max_concurrent".into(), serde_json::json!(max_concurrent));
    cfg.options
        .insert("max_frames".into(), serde_json::json!(max_frames));
    cfg.options
        .insert("silent_start_ms".into(), serde_json::json!(0));
    cfg.options
        .insert("eos_threshold".into(), serde_json::json!(10.0));
    let parsed = PocketCandleConfig::from_tts_config(&cfg).expect("valid test config");

    PocketCandleTts {
        engine,
        voices,
        config: parsed,
        audit_tx: None,
        core: Arc::new(ComponentCore::new("tts:pocket-candle", ComponentKind::Tts)),
        concurrency: Arc::new(Semaphore::new(max_concurrent)),
        warmed: AtomicBool::new(false),
    }
}

#[test]
fn factory_validates_config_and_rejects_empty_voice() {
    let f = PocketCandleTtsFactory;
    assert_eq!(f.name(), "pocket-candle");
    assert!(f.is_local());
    assert_eq!(
        f.output_sample_rate(&TtsConfig::new("pocket-candle")),
        Some(SAMPLE_RATE)
    );
    assert!(f.validate(&TtsConfig::new("pocket-candle")).is_empty());

    let bad = TtsConfig::new("pocket-candle").with_voice(Some("   ".into()));
    let errs = f.validate(&bad);
    assert!(errs.iter().any(|e| e.field == "voice"), "{errs:?}");
}

#[test]
fn security_rejects_path_traversal_and_remote_urls() {
    let dir = tempfile::tempdir().expect("tempdir");
    for bad in [
        "",
        "../secret.safetensors",
        "..\\secret.wav",
        "/etc/passwd",
        "http://169.254.169.254/latest",
        "https://evil.example/voice.wav",
        "hf://org/repo/voice.safetensors",
        "alba\0.wav",
    ] {
        let err = resolve_safe_voice_path(dir.path(), bad).expect_err(bad);
        assert!(matches!(err, TtsError::ConfigError(_)), "{bad}: {err:?}");
    }
}

#[test]
fn tokenizer_normalizes_and_enforces_max_chars() {
    let norm = PocketTokenizer::prepare_text("  Hello\t“world”  ", 100).expect("valid");
    assert_eq!(norm, "Hello \"world\".");

    let err = PocketTokenizer::prepare_text("a".repeat(25).as_str(), 16).expect_err("too long");
    assert!(matches!(err, TtsError::InferenceFailed(_)), "{err:?}");
}

#[test]
fn split_causal_attention_matches_full_concatenated_attention() {
    let dev = Device::Cpu;
    let h = 2;
    let d = 4;
    let p_len = 3;
    let d_len = 4;

    let q = Tensor::arange(0.0_f32, (h * d_len * d) as f32, &dev)
        .unwrap()
        .reshape((1, h, d_len, d))
        .unwrap()
        * 0.1;
    let q = q.unwrap();
    let kp = Tensor::arange(1.0_f32, (1 + h * p_len * d) as f32, &dev)
        .unwrap()
        .reshape((1, h, p_len, d))
        .unwrap()
        * 0.05;
    let kp = kp.unwrap();
    let vp = Tensor::arange(2.0_f32, (2 + h * p_len * d) as f32, &dev)
        .unwrap()
        .reshape((1, h, p_len, d))
        .unwrap()
        * 0.07;
    let vp = vp.unwrap();
    let kd = Tensor::arange(3.0_f32, (3 + h * d_len * d) as f32, &dev)
        .unwrap()
        .reshape((1, h, d_len, d))
        .unwrap()
        * 0.06;
    let kd = kd.unwrap();
    let vd = Tensor::arange(4.0_f32, (4 + h * d_len * d) as f32, &dev)
        .unwrap()
        .reshape((1, h, d_len, d))
        .unwrap()
        * 0.08;
    let vd = vd.unwrap();

    let scale = 1.0 / (d as f64).sqrt();
    let prefix = LayerKvPrefix {
        k: kp.clone(),
        v: vp.clone(),
    };
    let split = split_causal_attention(&q, Some(&prefix), &kd, &vd, scale).unwrap();

    // Reference: concatenate prefix + delta and run standard causal attention.
    let k_full = Tensor::cat(&[&kp, &kd], 2).unwrap();
    let v_full = Tensor::cat(&[&vp, &vd], 2).unwrap();
    let mut scores = (q.matmul(&k_full.transpose(2, 3).unwrap()).unwrap() * scale).unwrap();
    let mut mask = vec![0.0_f32; d_len * (p_len + d_len)];
    for qi in 0..d_len {
        for ki in (p_len + qi + 1)..(p_len + d_len) {
            mask[qi * (p_len + d_len) + ki] = -1.0e9_f32;
        }
    }
    let mask_t = Tensor::from_vec(mask, (1, 1, d_len, p_len + d_len), &dev).unwrap();
    scores = scores.broadcast_add(&mask_t).unwrap();
    let ref_out = candle_nn::ops::softmax_last_dim(&scores)
        .unwrap()
        .matmul(&v_full)
        .unwrap();

    let diff = (&split - &ref_out)
        .unwrap()
        .abs()
        .unwrap()
        .flatten_all()
        .unwrap()
        .to_vec1::<f32>()
        .unwrap();
    let max_diff = diff.into_iter().fold(0.0_f32, f32::max);
    assert!(max_diff < 1e-5, "max split attention diff = {max_diff}");
}

#[test]
fn streaming_causal_conv1d_matches_offline_conv() {
    let dev = Device::Cpu;
    let vb = VarBuilder::from_tensors(
        [
            (
                "weight".to_string(),
                Tensor::arange(1.0_f32, 1.0 + (2 * 2 * 3) as f32, &dev)
                    .unwrap()
                    .reshape((2, 2, 3))
                    .unwrap()
                    * 0.1,
            ),
            (
                "bias".to_string(),
                Ok(Tensor::from_vec(vec![0.1_f32, -0.2], (2,), &dev).unwrap()),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.unwrap()))
        .collect(),
        DType::F32,
        &dev,
    );
    let conv = StreamingCausalConv1d::load(2, 2, 3, 1, 1, vb).unwrap();
    let x = Tensor::arange(0.0_f32, 12.0, &dev)
        .unwrap()
        .reshape((1, 2, 6))
        .unwrap();
    let offline = conv.forward_offline(&x).unwrap();

    let mut state = conv.init_state(&dev).unwrap();
    let mut chunks = Vec::new();
    for t in 0..6 {
        let step = x.narrow(2, t, 1).unwrap();
        chunks.push(conv.forward_streaming(&step, &mut state).unwrap());
    }
    let refs: Vec<&Tensor> = chunks.iter().collect();
    let streamed = Tensor::cat(&refs, 2).unwrap();

    let max_diff = (&offline - &streamed)
        .unwrap()
        .abs()
        .unwrap()
        .flatten_all()
        .unwrap()
        .to_vec1::<f32>()
        .unwrap()
        .into_iter()
        .fold(0.0_f32, f32::max);
    assert!(max_diff < 1e-5, "max streaming conv diff = {max_diff}");
}

#[test]
fn mimi_encoder_builds_voice_prefix_and_validates_fingerprint() {
    let engine = Arc::new(PocketEngine::tiny_for_tests().unwrap());
    let ref_audio = vec![0.05_f32; FRAME_SAMPLES * 2];
    let voice = engine
        .build_voice_state_from_audio("cloned".into(), &ref_audio)
        .expect("Mimi encode + FlowLM prefix should succeed");
    // 1 bos_before_voice + 2 Mimi frames = 3 prefix tokens.
    assert_eq!(voice.kv_prefix.prefix_len, 3);
    assert_eq!(voice.kv_prefix.layers.len(), engine.arch.num_layers);

    let bad_voice = VoiceState {
        name: "mismatched".into(),
        fingerprint: VoiceFingerprint {
            d_model: 9999,
            num_layers: 1,
            num_heads: 1,
            latent_dim: 1,
        },
        kv_prefix: Arc::clone(&voice.kv_prefix),
    };
    let cfg = PocketCandleConfig::from_tts_config(&TtsConfig::new("pocket-candle")).unwrap();
    let (tx, _rx) = mpsc::channel(4);
    let err = engine
        .synthesize_blocking("hi", &bad_voice, &cfg, &tx, &CancellationToken::new())
        .expect_err("fingerprint mismatch must fail");
    assert!(matches!(err, TtsError::ConfigError(_)), "{err:?}");
}

#[test]
fn silent_start_probe_trips_only_on_silent_window() {
    let guard = SilentStartGuard {
        window_samples: 100,
        peak_threshold: 500,
        retries: 2,
    };
    let mut probe = SilentStartProbe::new(guard);
    probe.observe(&[15i16; 60]);
    assert_eq!(probe.is_silent(), None);
    probe.observe(&[-25i16; 60]);
    assert_eq!(probe.is_silent(), Some(25));
}

#[tokio::test]
async fn pipelined_synthesis_streams_24khz_frames_and_supports_parallel_sessions() {
    let tts: Arc<dyn TtsProvider> = Arc::new(make_test_provider(4, 8));
    let (tx1, mut rx1) = mpsc::channel::<AudioFrame>(16);
    let (tx2, mut rx2) = mpsc::channel::<AudioFrame>(16);

    let t1 = {
        let tts = Arc::clone(&tts);
        tokio::spawn(async move {
            tts.synthesize_stream(
                SynthesisRequest::new("Hello first user."),
                tx1,
                CancellationToken::new(),
            )
            .await
        })
    };
    let t2 = {
        let tts = Arc::clone(&tts);
        tokio::spawn(async move {
            tts.synthesize_stream(
                SynthesisRequest::new("Hello second user."),
                tx2,
                CancellationToken::new(),
            )
            .await
        })
    };

    let (r1, r2) = tokio::join!(t1, t2);
    r1.unwrap().unwrap();
    r2.unwrap().unwrap();

    let mut frames1 = 0usize;
    while let Some(f) = rx1.recv().await {
        assert_eq!(f.sample_rate, SAMPLE_RATE);
        assert_eq!(f.channels, 1);
        assert_eq!(f.samples.len(), FRAME_SAMPLES);
        frames1 += 1;
    }
    let mut frames2 = 0usize;
    while let Some(f) = rx2.recv().await {
        assert_eq!(f.sample_rate, SAMPLE_RATE);
        frames2 += 1;
    }
    assert_eq!(frames1, 8);
    assert_eq!(frames2, 8);
}

#[tokio::test]
async fn cancellation_token_aborts_immediately() {
    let tts = make_test_provider(2, 200);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (tx, mut rx) = mpsc::channel(8);
    tts.synthesize_stream(SynthesisRequest::new("Cancel me."), tx, cancel)
        .await
        .expect("cancelled synthesis returns Ok");
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn warm_up_runs_once_per_provider_instance() {
    let tts = make_test_provider(2, 8);
    tts.warm_up().await.expect("first warm-up");
    tts.warm_up().await.expect("second warm-up is no-op");
    let comp = tts.as_component().expect("component");
    assert_eq!(
        comp.telemetry().processed_total,
        0,
        "warm_up must not inflate turn synthesis counts"
    );
}

#[tokio::test]
async fn metric_conformance() {
    let run = |tx: mpsc::Sender<AudioFrame>, _cancel: CancellationToken| async move {
        for _ in 0..2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            tx.send(AudioFrame {
                samples: vec![0; FRAME_SAMPLES],
                sample_rate: SAMPLE_RATE,
                channels: 1,
            })
            .await
            .unwrap();
        }
        Ok(())
    };
    let res =
        voice_pipes_sdk::testing::assert_tts_metrics_conformance(run, Duration::from_millis(15))
            .await;
    assert!(res.is_ok(), "{res:?}");
}

/// Real-checkpoint end-to-end test + 1 vs 3 concurrent CPU benchmark using the
/// locally cached `kyutai/pocket-tts` (`model.safetensors`, `tokenizer.model`, `alba.safetensors`).
///
/// Run in release mode for realistic CPU timing:
/// `cargo test --release -p voice-pipes-tts-pocket-candle -- real_checkpoint_synthesize_and_benchmark --ignored --nocapture`
#[tokio::test]
#[ignore = "requires locally cached kyutai/pocket-tts checkpoint in HF cache"]
async fn real_checkpoint_synthesize_and_benchmark_1_vs_3_concurrent() {
    use std::path::PathBuf;
    use std::time::Instant;

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
    assert!(tok_model.exists(), "missing {}", tok_model.display());
    assert!(alba_st.exists(), "missing {}", alba_st.display());

    // Assemble a temporary model_dir with symlinks/copies of the cached files.
    let tmp = tempfile::tempdir().expect("tempdir");
    let voices_dir = tmp.path().join("voices");
    std::fs::create_dir_all(&voices_dir).unwrap();
    std::fs::hard_link(&model_st, tmp.path().join("model.safetensors"))
        .or_else(|_| std::fs::copy(&model_st, tmp.path().join("model.safetensors")).map(|_| ()))
        .unwrap();
    std::fs::hard_link(&tok_model, tmp.path().join("tokenizer.model"))
        .or_else(|_| std::fs::copy(&tok_model, tmp.path().join("tokenizer.model")).map(|_| ()))
        .unwrap();
    std::fs::hard_link(&alba_st, voices_dir.join("alba.safetensors"))
        .or_else(|_| std::fs::copy(&alba_st, voices_dir.join("alba.safetensors")).map(|_| ()))
        .unwrap();

    let t_load = Instant::now();
    let engine =
        Arc::new(PocketEngine::load_from_dir(tmp.path(), Device::Cpu).expect("load model"));
    let voices = Arc::new(VoiceCache::new(8));
    let voice_state = voices
        .get_or_load(&engine, &voices_dir, "alba")
        .expect("load alba.safetensors");
    println!(
        "[pocket-candle] loaded model.safetensors + alba.safetensors (prefix_len={}) in {:.2}s",
        voice_state.kv_prefix.prefix_len,
        t_load.elapsed().as_secs_f64()
    );

    let mut cfg = TtsConfig::new("pocket-candle").with_voice(Some("alba".into()));
    cfg.options.insert(
        "model_dir".into(),
        serde_json::json!(tmp.path().display().to_string()),
    );
    cfg.options.insert(
        "voices_dir".into(),
        serde_json::json!(voices_dir.display().to_string()),
    );
    cfg.options
        .insert("max_concurrent".into(), serde_json::json!(4));
    cfg.options
        .insert("max_frames".into(), serde_json::json!(60)); // up to 4.8s
    cfg.options
        .insert("silent_start_ms".into(), serde_json::json!(0));
    cfg.options.insert("seed".into(), serde_json::json!(42));
    let parsed = PocketCandleConfig::from_tts_config(&cfg).unwrap();

    let tts: Arc<dyn TtsProvider> = Arc::new(PocketCandleTts {
        engine,
        voices,
        config: parsed,
        audit_tx: None,
        core: Arc::new(ComponentCore::new("tts:pocket-candle", ComponentKind::Tts)),
        concurrency: Arc::new(Semaphore::new(4)),
        warmed: AtomicBool::new(false),
    });

    let prompt = "Hello! Welcome to Voice Pipes running natively in pure Rust.";

    // 1. Single-stream synthesis + save WAV to workspace target/pocket_candle_alba.wav.
    let t0 = Instant::now();
    let (tx, mut rx) = mpsc::channel::<AudioFrame>(64);
    let synth_task = {
        let tts = Arc::clone(&tts);
        tokio::spawn(async move {
            tts.synthesize_stream(SynthesisRequest::new(prompt), tx, CancellationToken::new())
                .await
        })
    };
    let mut ttfb_ms = None;
    let mut all_samples: Vec<i16> = Vec::new();
    while let Some(frame) = rx.recv().await {
        if ttfb_ms.is_none() {
            ttfb_ms = Some(t0.elapsed().as_secs_f64() * 1000.0);
        }
        all_samples.extend_from_slice(&frame.samples);
    }
    synth_task.await.unwrap().expect("single stream synth");
    let wall_1 = t0.elapsed().as_secs_f64();
    let audio_s = all_samples.len() as f64 / f64::from(SAMPLE_RATE);
    let peak = all_samples
        .iter()
        .map(|s| s.saturating_abs())
        .max()
        .unwrap_or(0);
    let rtf_1 = wall_1 / audio_s.max(1e-6);

    let wav_out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("target/pocket_candle_alba.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_out, spec).expect("create wav");
    for &s in &all_samples {
        writer.write_sample(s).unwrap();
    }
    writer.finalize().unwrap();
    println!(
        "[bench 1-stream] TTFB={:.1}ms wall={:.2}s audio={:.2}s RTF={:.3} peak={} saved={}",
        ttfb_ms.unwrap_or(0.0),
        wall_1,
        audio_s,
        rtf_1,
        peak,
        wav_out.display()
    );
    assert!(all_samples.len() >= FRAME_SAMPLES);

    // 2. Three concurrent streams in parallel sharing the same Arc<PocketEngine> + Arc<VoiceState>.
    let t_par = Instant::now();
    let mut handles = Vec::new();
    for user in 1..=3 {
        let tts = Arc::clone(&tts);
        handles.push(tokio::spawn(async move {
            let u_start = Instant::now();
            let (utx, mut urx) = mpsc::channel::<AudioFrame>(64);
            let h = tokio::spawn(async move {
                tts.synthesize_stream(SynthesisRequest::new(prompt), utx, CancellationToken::new())
                    .await
            });
            let mut u_ttfb = None;
            let mut u_samples = 0usize;
            while let Some(f) = urx.recv().await {
                if u_ttfb.is_none() {
                    u_ttfb = Some(u_start.elapsed().as_secs_f64() * 1000.0);
                }
                u_samples += f.samples.len();
            }
            h.await.unwrap().expect("parallel stream synth");
            (
                user,
                u_ttfb.unwrap_or(0.0),
                u_start.elapsed().as_secs_f64(),
                u_samples,
            )
        }));
    }
    let mut total_audio_s = 0.0;
    for h in handles {
        let (user, u_ttfb, u_wall, u_samples) = h.await.unwrap();
        let u_audio_s = u_samples as f64 / f64::from(SAMPLE_RATE);
        total_audio_s += u_audio_s;
        println!(
            "  [user {user}] TTFB={u_ttfb:.1}ms wall={u_wall:.2}s audio={u_audio_s:.2}s RTF={:.3}",
            u_wall / u_audio_s.max(1e-6)
        );
    }
    let wall_3 = t_par.elapsed().as_secs_f64();
    println!(
        "[bench 3-concurrent] total_wall={wall_3:.2}s total_audio={total_audio_s:.2}s aggregate_RTF={:.3} speedup_vs_serial={:.2}x",
        wall_3 / total_audio_s.max(1e-6),
        (wall_1 * 3.0) / wall_3.max(1e-6)
    );
}
