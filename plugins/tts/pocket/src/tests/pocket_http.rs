use voice_pipes_sdk::{TtsConfig, TtsError, TtsFactory, TtsProvider};

use crate::config::{
    DEFAULT_SILENT_START_PEAK, DEFAULT_SILENT_START_RETRIES, PocketHttpConfig, SAMPLE_RATE,
    SilentStartGuard, is_remote_voice,
};
use crate::factory::PocketHttpTtsFactory;
use crate::provider::SilentStartProbe;

/// A URL-shaped voice hint must not reach the sidecar unless
/// the operator opted in — the sidecar would fetch it (SSRF surface).
#[test]
fn remote_voice_urls_are_refused_by_default() {
    let cfg = TtsConfig::new("pocket-http");
    let parsed = PocketHttpConfig::from_tts_config(&cfg).unwrap();
    assert!(!parsed.allow_voice_urls);
    for v in [
        "http://169.254.169.254/latest",
        "https://x.example/voice.wav",
        "hf://org/repo",
    ] {
        assert!(is_remote_voice(v), "{v}");
    }
    assert!(!is_remote_voice("alba"));
    assert!(!is_remote_voice("voices/alba.wav"));

    let mut opts = std::collections::HashMap::new();
    opts.insert("allow_voice_urls".to_string(), serde_json::json!(true));
    let cfg_on = cfg.with_options(opts);
    assert!(
        PocketHttpConfig::from_tts_config(&cfg_on)
            .unwrap()
            .allow_voice_urls
    );
}

fn make_wav_header(data_len: u32) -> Vec<u8> {
    // Canonical 44-byte PCM WAV header: 24 kHz mono i16.
    let sr: u32 = SAMPLE_RATE;
    let byte_rate: u32 = sr * 2;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36u32 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&1u16.to_le_bytes()); // mono
    h.extend_from_slice(&sr.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&2u16.to_le_bytes()); // block align
    h.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

#[test]
fn factory_validates_endpoint_scheme() {
    let f = PocketHttpTtsFactory;
    let cfg = TtsConfig::new("pocket-http").with_endpoint(Some("ftp://nope".into()));
    let errs = f.validate(&cfg);
    assert!(errs.iter().any(|e| e.field == "endpoint"));
}

#[test]
fn factory_accepts_minimal_config() {
    let f = PocketHttpTtsFactory;
    let cfg = TtsConfig::new("pocket-http");
    assert!(f.validate(&cfg).is_empty());
}

// ── Silent-start guard ───────────────────────────────────

#[test]
fn silent_start_guard_defaults_and_disable() {
    let mut cfg = TtsConfig::new("pocket-http");
    let g = PocketHttpConfig::from_tts_config(&cfg)
        .unwrap()
        .silent_start
        .expect("guard on by default");
    assert_eq!(g.window_samples, 48_000); // 2 s @ 24 kHz
    assert_eq!(g.peak_threshold, DEFAULT_SILENT_START_PEAK);
    assert_eq!(g.retries, DEFAULT_SILENT_START_RETRIES);

    cfg.options
        .insert("silent_start_ms".into(), serde_json::json!(0));
    assert!(
        PocketHttpConfig::from_tts_config(&cfg)
            .unwrap()
            .silent_start
            .is_none()
    );
}

#[test]
fn probe_trips_only_after_a_silent_window() {
    let guard = SilentStartGuard {
        window_samples: 100,
        peak_threshold: 500,
        retries: 1,
    };
    let mut p = SilentStartProbe::new(guard);
    p.observe(&[10i16; 60]);
    assert_eq!(p.silent(), None, "window not complete yet");
    p.observe(&[-20i16; 60]);
    assert_eq!(p.silent(), Some(20), "whole window quiet");

    let mut p = SilentStartProbe::new(guard);
    p.observe(&[10i16; 50]);
    p.observe(&[0, 0, 800, 0]); // sound inside the window
    p.observe(&[0i16; 200]);
    assert_eq!(
        p.silent(),
        None,
        "a peak above threshold clears it for good"
    );
}

/// In-process `pocket-tts serve` stand-in: answers `POST /tts` with a
/// scripted sequence of WAV bodies and counts the requests.
type MockState = (
    std::sync::Arc<Vec<Vec<u8>>>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
);

async fn mock_sidecar(
    bodies: Vec<Vec<u8>>,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use axum::{Router, routing::post};
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = (std::sync::Arc::new(bodies), std::sync::Arc::clone(&hits));
    let app =
        Router::new()
            .route(
                "/tts",
                post(
                    move |axum::extract::State((bodies, hits)): axum::extract::State<
                        MockState,
                    >| async move {
                        let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let body = bodies[n.min(bodies.len() - 1)].clone();
                        ([("content-type", "audio/wav")], body)
                    },
                ),
            )
            .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

fn wav(samples: &[i16]) -> Vec<u8> {
    let mut out = make_wav_header((samples.len() * 2) as u32);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn silence(secs: f32) -> Vec<i16> {
    vec![7i16; (SAMPLE_RATE as f32 * secs) as usize]
}

fn speech(secs: f32) -> Vec<i16> {
    (0..(SAMPLE_RATE as f32 * secs) as usize)
        .map(|i| if i % 2 == 0 { 8000 } else { -8000 })
        .collect()
}

fn provider_for(endpoint: &str, options: &[(&str, serde_json::Value)]) -> Box<dyn TtsProvider> {
    let cfg = TtsConfig::new("pocket-http")
        .with_endpoint(Some(endpoint.into()))
        .with_voice(Some("hr".into()))
        .with_options(
            options
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        );
    PocketHttpTtsFactory
        .build(&cfg, &voice_pipes_sdk::BuildContext::new())
        .unwrap()
}

/// The failure seen in production: the first response is 3 s of
/// near-silence (the model ran to its cap). The guard must abandon it
/// after the 1 s window and fetch the fragment again.
#[tokio::test]
async fn silent_first_attempt_is_retried() {
    let mut good = silence(0.3);
    good.extend(speech(1.0));
    let (endpoint, hits) = mock_sidecar(vec![wav(&silence(3.0)), wav(&good)]).await;
    let tts = provider_for(&endpoint, &[("silent_start_ms", serde_json::json!(1000))]);

    let frame = tts.synthesize("Moj najdraži izum.").await.unwrap();
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    let peak = frame
        .samples
        .iter()
        .map(|s| s.saturating_abs())
        .max()
        .unwrap();
    assert_eq!(peak, 8000, "the retry's speech must reach the caller");
    // The abandoned attempt contributed at most its probe window.
    let total_s = frame.samples.len() as f32 / SAMPLE_RATE as f32;
    assert!(total_s < 2.6, "{total_s} s: silent attempt not cut short");
}

#[tokio::test]
async fn speaking_first_attempt_is_not_retried() {
    let mut good = silence(0.5);
    good.extend(speech(0.8));
    let (endpoint, hits) = mock_sidecar(vec![wav(&good), wav(&silence(3.0))]).await;
    let tts = provider_for(&endpoint, &[("silent_start_ms", serde_json::json!(1000))]);
    let expected_len = good.len();
    let frame = tts.synthesize("Dobar dan.").await.unwrap();
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(frame.samples.len(), expected_len);
}

/// Every attempt silent: the last one streams to completion so the
/// pipeline still gets a fragment (today's behaviour), and the request
/// count is bounded by `silent_start_retries`.
#[tokio::test]
async fn all_silent_attempts_fall_back_to_the_last_full_stream() {
    let (endpoint, hits) = mock_sidecar(vec![wav(&silence(3.0))]).await;
    let tts = provider_for(
        &endpoint,
        &[
            ("silent_start_ms", serde_json::json!(1000)),
            ("silent_start_retries", serde_json::json!(2)),
        ],
    );
    let frame = tts.synthesize("…").await.unwrap();
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    // Two probe windows (1 s each, cut at the next ~100 ms frame) + one
    // full 3 s stream.
    let total_s = frame.samples.len() as f32 / SAMPLE_RATE as f32;
    assert!((4.9..=5.6).contains(&total_s), "{total_s}");
}

#[tokio::test]
async fn guard_disabled_streams_silence_once() {
    let (endpoint, hits) = mock_sidecar(vec![wav(&silence(2.0))]).await;
    let tts = provider_for(&endpoint, &[("silent_start_ms", serde_json::json!(0))]);
    let frame = tts.synthesize("…").await.unwrap();
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(frame.samples.len(), (SAMPLE_RATE * 2) as usize);
}

// ── Malformed responses ─────────────────────────────────

/// The sidecar always sends WAV, so a body without a header is refused
/// rather than played as noise.
#[tokio::test]
async fn a_body_without_a_wav_header_is_refused() {
    let raw: Vec<u8> = speech(0.2).iter().flat_map(|s| s.to_le_bytes()).collect();
    let (endpoint, _) = mock_sidecar(vec![raw]).await;
    let err = provider_for(&endpoint, &[])
        .synthesize("Dobar dan.")
        .await
        .unwrap_err();
    assert!(
        matches!(err, TtsError::InferenceFailed(ref m) if m.contains("not WAV")),
        "{err:?}"
    );
}

/// A response cut off inside its header, or an empty one, is an error, not
/// a success with no audio.
#[tokio::test]
async fn a_body_ending_inside_its_header_is_an_error() {
    for body in [make_wav_header(0)[..30].to_vec(), Vec::new()] {
        let (endpoint, _) = mock_sidecar(vec![body.clone()]).await;
        let err = provider_for(&endpoint, &[])
            .synthesize("Dobar dan.")
            .await
            .unwrap_err();
        assert!(
            matches!(err, TtsError::InferenceFailed(_)),
            "{} bytes: {err:?}",
            body.len()
        );
    }
}

/// A header whose `data` chunk never starts within the header limit is an
/// error rather than a buffer that grows with the body.
#[tokio::test]
async fn a_header_without_a_data_chunk_is_an_error() {
    let mut body = b"RIFF\0\0\0\0WAVE".to_vec();
    for _ in 0..1200 {
        body.extend_from_slice(b"junk\0\0\0\0");
    }
    let (endpoint, _) = mock_sidecar(vec![body]).await;
    let err = provider_for(&endpoint, &[])
        .synthesize("Dobar dan.")
        .await
        .unwrap_err();
    assert!(
        matches!(err, TtsError::InferenceFailed(ref m) if m.contains("data chunk")),
        "{err:?}"
    );
}

/// A complete header with no samples after it is no audio: an error, not
/// a reply the caller never hears.
#[tokio::test]
async fn a_header_with_no_samples_is_an_error() {
    let (endpoint, _) = mock_sidecar(vec![wav(&[])]).await;
    let err = provider_for(&endpoint, &[("silent_start_retries", serde_json::json!(0))])
        .synthesize("Dobar dan.")
        .await
        .unwrap_err();
    assert!(matches!(err, TtsError::InferenceFailed(_)), "{err:?}");
}

/// A chunk after the declared `data` size is metadata, not audio.
#[tokio::test]
async fn a_chunk_after_the_audio_is_not_played() {
    let samples = speech(0.2);
    let mut body = wav(&samples);
    body.extend_from_slice(b"LIST\x08\0\0\0metadata");
    let (endpoint, _) = mock_sidecar(vec![body]).await;
    let frame = provider_for(&endpoint, &[])
        .synthesize("Dobar dan.")
        .await
        .unwrap();
    assert_eq!(frame.samples, samples);
}

// ── A stream longer than the request timeout ────────────────

use voice_pipes_net::testing::drip_server;

/// One second of audio in ten pieces 100 ms apart, as sample values and as
/// the little-endian bytes of each piece.
fn one_second_in_pieces(rate: usize) -> (Vec<i16>, Vec<Vec<u8>>) {
    let samples: Vec<i16> = (0..rate)
        .map(|i| if i % 2 == 0 { 8000 } else { -8000 })
        .collect();
    let pieces = samples
        .chunks(rate / 10)
        .map(|c| c.iter().flat_map(|s| s.to_le_bytes()).collect())
        .collect();
    (samples, pieces)
}

/// Synthesis streams for as long as the audio lasts: a reply longer than
/// `request_timeout_ms` is read to the end, not cut off at the timeout.
#[tokio::test]
async fn a_stream_longer_than_the_request_timeout_is_read_to_the_end() {
    let (samples, pieces) = one_second_in_pieces(SAMPLE_RATE as usize);
    let head = make_wav_header((samples.len() * 2) as u32);
    let base = drip_server(head, pieces, std::time::Duration::from_millis(100)).await;
    let tts = provider_for(&base, &[("request_timeout_ms", serde_json::json!(300))]);
    let frame = tts.synthesize("hello").await.unwrap();
    assert_eq!(frame.samples, samples);
}

/// Only what the provider reads per synthesis is offered as live: the
/// request timeout is fixed when the HTTP client is built.
#[test]
fn the_request_timeout_is_not_offered_as_live() {
    use voice_pipes_sdk::TtsFactory;
    let cfg = TtsConfig::new("pocket-http")
        .with_voice(Some("alba".into()))
        .with_options(
            [("request_timeout_ms".to_string(), serde_json::json!(5000))]
                .into_iter()
                .collect(),
        );
    let schema = PocketHttpTtsFactory.provider_schema(&cfg);
    let timeout = schema
        .descriptors
        .iter()
        .find(|d| d.key == "request_timeout_ms")
        .expect("described");
    assert!(!timeout.mutable, "{timeout:?}");
}

/// Every option the crate documentation's example offers is one the
/// provider reads: an advertised key it did not read would be refused as
/// unknown when an operator copied it into a config.
#[test]
fn every_documented_option_is_accepted() {
    let doc = include_str!("../lib.rs");
    let mut keys = Vec::new();
    for line in doc.lines() {
        let Some(rest) = line.strip_prefix("//! #") else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') || key.is_empty() {
            continue;
        }
        let value: i64 = value
            .split('#')
            .next()
            .unwrap_or_default()
            .trim()
            .parse()
            .expect("a documented number");
        keys.push((key.to_string(), value));
    }
    assert!(
        keys.len() >= 4,
        "the example's options were not found: {keys:?}"
    );
    for (key, value) in keys {
        let cfg = TtsConfig::new("pocket-http").with_options(
            [(key.clone(), serde_json::json!(value))]
                .into_iter()
                .collect(),
        );
        assert!(
            PocketHttpConfig::from_tts_config(&cfg).is_ok(),
            "documented option `{key} = {value}` is refused"
        );
    }
}

#[tokio::test]
async fn metric_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::AudioFrame>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        for _ in 0..2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            tx.send(voice_pipes_sdk::AudioFrame {
                samples: vec![0; 320],
                sample_rate: 16_000,
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
