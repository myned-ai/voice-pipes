use std::time::Duration;

use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{TtsConfig, TtsError, TtsFactory, TtsProvider};

use crate::config::{DEFAULT_ENDPOINT, DEFAULT_MODEL, DEFAULT_VOICE, OpenAiConfig};
use crate::factory::OpenAiTtsFactory;
use crate::provider::{OpenAiTts, map_http_error};

#[test]
fn debug_redacts_api_key() {
    let src: TtsConfig =
        toml::from_str("provider = \"openai\"\napi_key = \"super-secret-key\"\n").unwrap();
    let cfg = OpenAiConfig::from_tts_config(&src).unwrap();
    let s = format!("{cfg:?}");
    assert!(!s.contains("super-secret-key"), "{s}");
    assert!(s.contains("***"), "{s}");
}

#[test]
fn config_defaults() {
    let cfg = TtsConfig::new("openai").with_api_key(Some("test-key".into()));

    let oc = OpenAiConfig::from_tts_config(&cfg).unwrap();
    assert_eq!(oc.model_name, DEFAULT_MODEL);
    assert_eq!(*oc.current_voice(), DEFAULT_VOICE);
    assert_eq!(oc.endpoint, DEFAULT_ENDPOINT);
    assert_eq!(oc.api_key.expose(), "test-key");
    assert_eq!(oc.request_timeout, Duration::from_secs(30));
    assert!(oc.speed.is_none());
    assert!(oc.instructions.is_none());
}

#[test]
fn config_custom_values() {
    let mut options = std::collections::HashMap::new();
    options.insert(
        "speed".into(),
        serde_json::Value::Number(serde_json::Number::from_f64(1.2).unwrap()),
    );
    options.insert(
        "request_timeout_ms".into(),
        serde_json::Value::Number(serde_json::Number::from(5000)),
    );

    options.insert(
        "instructions".into(),
        serde_json::Value::String("Speak calmly".into()),
    );

    let cfg = TtsConfig::new("openai")
        .with_endpoint(Some("https://my-local-endpoint.com/v1".into()))
        .with_model_name(Some("tts-1-hd".into()))
        .with_voice(Some("shimmer".into()))
        .with_options(options);

    let oc = OpenAiConfig::from_tts_config(&cfg).unwrap();
    assert_eq!(oc.model_name, "tts-1-hd");
    assert_eq!(*oc.current_voice(), "shimmer");
    assert_eq!(oc.endpoint, "https://my-local-endpoint.com/v1");
    assert_eq!(oc.api_key.expose(), "", "no key configured, none sent");
    assert_eq!(oc.request_timeout, Duration::from_secs(5));
    assert_eq!(oc.speed, Some(1.2));
    assert_eq!(oc.instructions, Some("Speak calmly".to_string()));
}

#[test]
fn request_url_construction() {
    let cfg = TtsConfig::new("openai").with_api_key(Some("key".into()));

    let oc = OpenAiConfig::from_tts_config(&cfg).unwrap();
    let provider = OpenAiTts {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: oc,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:openai", ComponentKind::Tts)),
        resolved_model: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
    };

    assert_eq!(provider.url(), "https://api.openai.com/v1/audio/speech");

    // Custom endpoint already ending with /audio/speech
    let cfg_custom = TtsConfig::new("openai")
        .with_api_key(Some("key".into()))
        .with_endpoint(Some("http://localhost:8000/v1/audio/speech".into()));
    let oc_custom = OpenAiConfig::from_tts_config(&cfg_custom).unwrap();
    let provider_custom = OpenAiTts {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: oc_custom,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:openai", ComponentKind::Tts)),
        resolved_model: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
    };
    assert_eq!(
        provider_custom.url(),
        "http://localhost:8000/v1/audio/speech"
    );
}

#[test]
fn models_url_construction() {
    let mk = |endpoint: Option<&str>| {
        let cfg = TtsConfig::new("openai")
            .with_api_key(Some("key".into()))
            .with_endpoint(endpoint.map(str::to_string));
        let oc = OpenAiConfig::from_tts_config(&cfg).unwrap();
        OpenAiTts {
            client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
                std::time::Duration::from_secs(5),
            ))
            .unwrap(),
            config: oc,
            audit_tx: None,
            core: std::sync::Arc::new(ComponentCore::new("tts:openai", ComponentKind::Tts)),
            resolved_model: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
        }
        .models_url()
    };

    assert_eq!(mk(None), "https://api.openai.com/v1/models");
    assert_eq!(
        mk(Some("http://localhost:8091/v1")),
        "http://localhost:8091/v1/models"
    );
    assert_eq!(
        mk(Some("http://localhost:8091/v1/audio/speech")),
        "http://localhost:8091/v1/models"
    );
}

/// A key sent to an internet host over plain HTTP is a config error on the
/// endpoint field; the operator's opt-in and the wizard's own sidecars
/// (single-label container names) pass.
#[test]
fn a_key_is_never_sent_in_plain_text_to_an_internet_host() {
    let errors_for = |extra: &str| {
        let cfg: TtsConfig = toml::from_str(&format!(
            "provider = \"openai\"\napi_key = \"sk-test\"\n{extra}"
        ))
        .unwrap();
        OpenAiTtsFactory
            .validate(&cfg)
            .into_iter()
            .filter(|e| e.field == "tts.endpoint")
            .collect::<Vec<_>>()
    };
    let refused = errors_for("endpoint = \"http://tts.example.com/v1\"\n");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].message.contains("allow_insecure_endpoint"),
        "{refused:?}"
    );
    assert!(!refused[0].message.contains("sk-test"));

    assert!(errors_for("endpoint = \"https://tts.example.com/v1\"\n").is_empty());
    assert!(errors_for("endpoint = \"http://vibevoice:8091/v1\"\n").is_empty());
    assert!(
        errors_for(
            "endpoint = \"http://tts.example.com/v1\"\n[options]\nallow_insecure_endpoint = true\n"
        )
        .is_empty()
    );
}

// ── Malformed input from a compatible server ────────────────

/// An error body that is not the vendor's JSON is reported as text, capped,
/// rather than parsed or dropped.
#[test]
fn a_malformed_error_body_is_reported_as_text() {
    match map_http_error(500, "{not json") {
        TtsError::InferenceFailed(m) => assert!(m.contains("{not json"), "{m}"),
        other => panic!("{other:?}"),
    }
    match map_http_error(
        401,
        r#"{"error": {"message": "Incorrect API key", "type": "x"}}"#,
    ) {
        TtsError::AuthError(m) => assert_eq!(m, "OpenAI TTS: Incorrect API key"),
        other => panic!("{other:?}"),
    }
    match map_http_error(418, &"x".repeat(10_000)) {
        TtsError::ApiError { status, message } => {
            assert_eq!(status, 418);
            assert_eq!(message.len(), 512);
        }
        other => panic!("{other:?}"),
    }
}

/// The factory reports the rate its audio comes at without building.
#[test]
fn the_factory_reports_its_output_rate() {
    let cfg: TtsConfig = toml::from_str("provider = \"openai\"\n").unwrap();
    assert_eq!(OpenAiTtsFactory.output_sample_rate(&cfg), Some(24_000));
}

// ── Malformed responses ─────────────────────────────────

/// Answer every speech request with `status` and `body`, from a local port.
async fn mock_server(status: u16, body: Vec<u8>) -> String {
    use axum::{Router, routing::post};
    let app = Router::new().route(
        "/v1/audio/speech",
        post(move || {
            let body = body.clone();
            async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

fn provider_at(endpoint: &str) -> Box<dyn TtsProvider> {
    let cfg = TtsConfig::new("openai")
        .with_api_key(Some("sk-test".into()))
        .with_endpoint(Some(endpoint.into()));
    OpenAiTtsFactory
        .build(&cfg, &voice_pipes_sdk::BuildContext::new())
        .unwrap()
}

fn le(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn wav_header(data_len: u32) -> Vec<u8> {
    let mut h = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&[1, 0, 1, 0]);
    h.extend_from_slice(&24_000u32.to_le_bytes());
    h.extend_from_slice(&48_000u32.to_le_bytes());
    h.extend_from_slice(&[2, 0, 16, 0]);
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

/// Raw PCM, as `OpenAI` sends it, and WAV, as some compatible servers do,
/// both come out as the same samples; a chunk after a declared `data` size
/// is not played.
#[tokio::test]
async fn pcm_and_wav_bodies_give_the_same_samples() {
    let samples: Vec<i16> = (0..480).map(|i| (i * 37 % 2000 - 1000) as i16).collect();
    let mut wav = wav_header((samples.len() * 2) as u32);
    wav.extend_from_slice(&le(&samples));
    wav.extend_from_slice(b"LIST\x04\0\0\0junk");
    for body in [le(&samples), wav] {
        let tts = provider_at(&mock_server(200, body).await);
        let frame = tts.synthesize("hello").await.unwrap();
        assert_eq!(frame.samples, samples);
    }
}

/// A success status with no audio, or with a body that ends inside its WAV
/// header, is a failed synthesis, not a silent success.
#[tokio::test]
async fn a_success_without_audio_is_an_error() {
    for body in [
        Vec::new(),
        wav_header(0)[..30].to_vec(),
        wav_header(u32::MAX),
    ] {
        let len = body.len();
        let tts = provider_at(&mock_server(200, body).await);
        let err = tts.synthesize("hello").await.unwrap_err();
        assert!(
            matches!(err, TtsError::InferenceFailed(_)),
            "{len} bytes: {err:?}"
        );
    }
}

/// An unknown voice or model is a request the config made wrong: reported
/// as such, and not worth retrying.
#[tokio::test]
async fn a_rejected_request_is_a_config_error() {
    let body = br#"{"error": {"message": "voice not found", "type": "invalid_request_error"}}"#;
    let tts = provider_at(&mock_server(404, body.to_vec()).await);
    let err = tts.synthesize("hello").await.unwrap_err();
    assert!(
        matches!(err, TtsError::ConfigError(ref m) if m.contains("voice not found")),
        "{err:?}"
    );
    assert!(!err.is_transient());
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
    let (samples, pieces) = one_second_in_pieces(24_000);
    let base = drip_server(Vec::new(), pieces, std::time::Duration::from_millis(100)).await;
    let cfg = TtsConfig::new("openai")
        .with_api_key(Some("sk-test".into()))
        .with_endpoint(Some(format!("{base}/v1")))
        .with_options(
            [("request_timeout_ms".to_string(), serde_json::json!(300))]
                .into_iter()
                .collect(),
        );
    let tts = OpenAiTtsFactory
        .build(&cfg, &voice_pipes_sdk::BuildContext::new())
        .unwrap();
    let frame = tts.synthesize("hello").await.unwrap();
    assert_eq!(frame.samples, samples);
}

/// A stream cancelled part-way (a barge-in) ends quietly and is not
/// counted as a successful synthesis or its latency.
#[tokio::test]
async fn a_cancelled_stream_is_not_recorded_as_a_success() {
    let (_, pieces) = one_second_in_pieces(24_000);
    let base = drip_server(Vec::new(), pieces, std::time::Duration::from_millis(100)).await;
    let tts = provider_at(&format!("{base}/v1"));
    let component = tts.as_component().expect("a component");

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    let canceller = cancel.clone();
    let reader = tokio::spawn(async move {
        let first = rx.recv().await;
        canceller.cancel();
        while rx.recv().await.is_some() {}
        first.is_some()
    });
    let streamed = tts
        .synthesize_stream(voice_pipes_sdk::SynthesisRequest::new("hello"), tx, cancel)
        .await;
    assert!(streamed.is_ok(), "{streamed:?}");
    assert!(reader.await.unwrap(), "audio arrived before the cancel");

    let telemetry = component.telemetry();
    assert_eq!(telemetry.processed_total, 0);
    assert_eq!(telemetry.errors_total, 0);
}

// ── Model identity ──────────────────────────────────────

/// Answer `GET /v1/models` with `ids`, from a local port.
async fn models_server(ids: &[&str]) -> String {
    use axum::{Json, Router, routing::get};
    let data: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::json!({ "id": id }))
        .collect();
    let app = Router::new().route(
        "/v1/models",
        get(move || {
            let data = data.clone();
            async move { Json(serde_json::json!({ "data": data })) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

fn provider_for_model(endpoint: &str, model: &str) -> Box<dyn TtsProvider> {
    let cfg = TtsConfig::new("openai")
        .with_api_key(Some("sk-test".into()))
        .with_endpoint(Some(endpoint.into()))
        .with_model_name(Some(model.into()));
    OpenAiTtsFactory
        .build(&cfg, &voice_pipes_sdk::BuildContext::new())
        .unwrap()
}

/// The regression: the real API lists every model the account can use,
/// and the first of them was reported as the one synthesising. The listed
/// entry that is the configured model is reported; a server that lists
/// several without it leaves the configured name in place; a server with
/// one model (a local server behind this API) reports that one.
#[tokio::test]
async fn the_model_reported_is_the_configured_one_or_the_only_one() {
    let identity = |ids: &'static [&'static str]| async move {
        let url = models_server(ids).await;
        provider_for_model(&url, "gpt-4o-mini-tts")
            .model_identity()
            .await
            .map(|m| m.id)
    };
    assert_eq!(
        identity(&["gpt-4-0613", "gpt-4o-mini-tts", "whisper-1"])
            .await
            .as_deref(),
        Some("gpt-4o-mini-tts")
    );
    assert_eq!(identity(&["gpt-4-0613", "whisper-1"]).await, None);
    assert_eq!(
        identity(&["vibevoice-1.5b"]).await.as_deref(),
        Some("vibevoice-1.5b")
    );
}

/// A setting out of range, of the wrong kind, or not a setting at all is
/// refused by validation and by the build, which used to take a zero
/// timeout, any speed, and ignore a misspelt key.
#[test]
fn bad_options_are_refused_by_validate_and_build() {
    let cfg: TtsConfig = toml::from_str(
        "provider = \"openai\"\napi_key = \"k\"\nrequest_timeout_ms = 0\nspeed = 9.0\nspede = 1.0\n",
    )
    .unwrap();
    let fields: Vec<String> = OpenAiTtsFactory
        .validate(&cfg)
        .into_iter()
        .map(|e| e.field)
        .collect();
    assert_eq!(
        fields,
        ["tts.speed", "tts.request_timeout_ms", "tts.spede"],
        "each problem named"
    );
    assert!(OpenAiConfig::from_tts_config(&cfg).is_err());
}

#[tokio::test]
async fn metric_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::AudioFrame>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        for _ in 0..2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            tx.send(voice_pipes_sdk::AudioFrame {
                samples: vec![0; 320],
                sample_rate: 24_000,
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

