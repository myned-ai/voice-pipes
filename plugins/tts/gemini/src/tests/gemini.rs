use std::time::Duration;

use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{TtsConfig, TtsError, TtsFactory};

use crate::config::{DEFAULT_ENDPOINT, DEFAULT_MODEL, DEFAULT_VOICE, GeminiConfig};
use crate::factory::GeminiTtsFactory;
use crate::provider::{GeminiTtsProvider, MAX_TEXT_BYTES, decode_response, map_api_error};

/// The Debug rendering must never contain the key value.
#[test]
fn debug_redacts_api_key() {
    let cfg = GeminiConfig {
        model_name: "gemini-2.5-flash-preview-tts".into(),
        voice: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee("Puck".to_string())),
        prompt: String::new(),
        endpoint: "https://example".into(),
        api_key: "super-secret-key".into(),
        request_timeout: Duration::from_secs(1),
    };
    let s = format!("{cfg:?}");
    assert!(!s.contains("super-secret-key"), "{s}");
    assert!(s.contains("***"), "{s}");
}

#[test]
fn config_defaults() {
    let cfg = TtsConfig::new("gemini").with_api_key(Some("test-key".into()));

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    assert_eq!(gc.model_name, DEFAULT_MODEL);
    assert_eq!(**gc.voice.load(), DEFAULT_VOICE);
    assert_eq!(gc.endpoint, DEFAULT_ENDPOINT);
    assert!(gc.prompt.is_empty());
    assert_eq!(gc.request_timeout, Duration::from_secs(30));
}

#[test]
fn config_custom_values() {
    let mut options = std::collections::HashMap::new();
    options.insert(
        "prompt".into(),
        serde_json::Value::String("Speak cheerfully".into()),
    );
    options.insert(
        "request_timeout_ms".into(),
        serde_json::Value::Number(serde_json::Number::from(5000)),
    );

    let cfg = TtsConfig::new("gemini")
        .with_api_key(Some("my-key".into()))
        .with_endpoint(Some("https://custom.endpoint.com/v1".into()))
        .with_model_name(Some("gemini-2.5-flash-preview-tts".into()))
        .with_voice(Some("Puck".into()))
        .with_options(options);

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    assert_eq!(gc.model_name, "gemini-2.5-flash-preview-tts");
    assert_eq!(**gc.voice.load(), "Puck");
    assert_eq!(gc.endpoint, "https://custom.endpoint.com/v1");
    assert_eq!(gc.prompt, "Speak cheerfully");
    assert_eq!(gc.request_timeout, Duration::from_secs(5));
}

#[test]
fn config_missing_api_key_errors() {
    let cfg = TtsConfig::new("gemini");

    assert!(GeminiConfig::from_tts_config(&cfg).is_err());
}

#[test]
fn request_url_construction() {
    let cfg = TtsConfig::new("gemini").with_api_key(Some("key".into()));

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    let provider = GeminiTtsProvider {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: gc,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:gemini", ComponentKind::Tts)),
    };

    assert_eq!(
        provider.url(),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-tts-preview:generateContent"
    );
}

/// The default model id above WILL go stale — `model_name` in config is
/// the supported way to pin a current one; prove the override reaches
/// the request URL.
#[test]
fn request_url_honors_model_name_override() {
    let cfg = TtsConfig::new("gemini")
        .with_api_key(Some("key".into()))
        .with_model_name(Some("gemini-9.9-flash-tts".into()));

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    let provider = GeminiTtsProvider {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: gc,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:gemini", ComponentKind::Tts)),
    };

    assert_eq!(
        provider.url(),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-9.9-flash-tts:generateContent"
    );
}

#[test]
fn request_body_shape() {
    let cfg = TtsConfig::new("gemini").with_api_key(Some("key".into()));

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    let provider = GeminiTtsProvider {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: gc,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:gemini", ComponentKind::Tts)),
    };

    let req = provider.build_request("Hello world", None);
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(json["contents"][0]["parts"][0]["text"], "Hello world");
    assert_eq!(json["generationConfig"]["responseModalities"][0], "AUDIO");
    assert_eq!(
        json["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]["voiceName"],
        "Kore"
    );
}

#[test]
fn request_body_with_prompt() {
    let mut options = std::collections::HashMap::new();
    options.insert(
        "prompt".into(),
        serde_json::Value::String("Say cheerfully:".into()),
    );

    let cfg = TtsConfig::new("gemini")
        .with_api_key(Some("key".into()))
        .with_options(options);

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    let provider = GeminiTtsProvider {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: gc,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:gemini", ComponentKind::Tts)),
    };

    let req = provider.build_request("Hello", None);
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(
        json["contents"][0]["parts"][0]["text"],
        "Say cheerfully:\n\nHello"
    );
}

#[test]
fn request_body_voice_override() {
    let cfg = TtsConfig::new("gemini")
        .with_api_key(Some("key".into()))
        .with_voice(Some("Kore".into()));

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    let provider = GeminiTtsProvider {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: gc,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:gemini", ComponentKind::Tts)),
    };

    let req = provider.build_request("Hello", Some("Puck"));
    let json = serde_json::to_value(&req).unwrap();

    assert_eq!(
        json["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]["voiceName"],
        "Puck"
    );
}

#[test]
fn error_mapping() {
    assert!(matches!(
        map_api_error(401, "bad key"),
        TtsError::AuthError(_)
    ));
    assert!(matches!(
        map_api_error(403, "denied"),
        TtsError::AuthError(_)
    ));
    assert!(matches!(
        map_api_error(429, "slow down"),
        TtsError::RateLimited { .. }
    ));
    assert!(matches!(
        map_api_error(400, "invalid"),
        TtsError::ConfigError(_)
    ));
    assert!(matches!(
        map_api_error(500, "internal"),
        TtsError::InferenceFailed(_)
    ));
    assert!(matches!(
        map_api_error(503, "unavailable"),
        TtsError::InferenceFailed(_)
    ));
    assert!(matches!(
        map_api_error(418, "teapot"),
        TtsError::ApiError { .. }
    ));
}

#[test]
fn text_truncation() {
    let cfg = TtsConfig::new("gemini").with_api_key(Some("key".into()));

    let gc = GeminiConfig::from_tts_config(&cfg).unwrap();
    let provider = GeminiTtsProvider {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: gc,
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:gemini", ComponentKind::Tts)),
    };

    // ASCII: the cut lands exactly on the cap.
    let long_text = "x".repeat(5000);
    let req = provider.build_request(&long_text, None);
    let json = serde_json::to_value(&req).unwrap();
    let sent_text = json["contents"][0]["parts"][0]["text"].as_str().unwrap();
    assert_eq!(sent_text.len(), MAX_TEXT_BYTES);

    // Multi-byte: with 2-byte "é" (and an odd cap offset from the ASCII
    // "x"), byte MAX_TEXT_BYTES falls inside a character — the old fixed
    // slice panicked here. The cut must back off to a char boundary.
    for prefix_len in 0..4 {
        let long_text = format!("{}{}", "x".repeat(prefix_len), "é".repeat(3000));
        let req = provider.build_request(&long_text, None);
        let json = serde_json::to_value(&req).unwrap();
        let sent_text = json["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(sent_text.len() <= MAX_TEXT_BYTES);
        assert!(
            sent_text.len() >= MAX_TEXT_BYTES - 4,
            "cut backed off too far"
        );
        assert!(long_text.starts_with(sent_text));
    }

    // CJK (3-byte chars) as well.
    let long_text = "語".repeat(2000);
    let req = provider.build_request(&long_text, None);
    let json = serde_json::to_value(&req).unwrap();
    let sent_text = json["contents"][0]["parts"][0]["text"].as_str().unwrap();
    assert!(sent_text.len() <= MAX_TEXT_BYTES);
    assert!(sent_text.chars().all(|c| c == '語'));
}

#[test]
fn factory_validate_missing_key() {
    let factory = GeminiTtsFactory;
    let cfg = TtsConfig::new("gemini");

    let errors = factory.validate(&cfg);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("api_key"));
}

#[test]
fn factory_validate_with_key() {
    let factory = GeminiTtsFactory;
    let cfg = TtsConfig::new("gemini").with_api_key(Some("present".into()));

    let errors = factory.validate(&cfg);
    assert!(errors.is_empty());
}

#[test]
fn factory_metadata() {
    let factory = GeminiTtsFactory;
    assert_eq!(factory.name(), "gemini");
    assert!(!factory.is_local());
}

// ── Malformed responses ─────────────────────────────────────

/// A success response that is not JSON, has no audio part, or carries bad
/// base64 is an error, never a panic; an odd trailing byte is dropped.
#[test]
fn malformed_responses_are_errors_never_panics() {
    for body in [
        &b"not json"[..],
        br"{}",
        br#"{"candidates": []}"#,
        br#"{"candidates": [{"content": {"parts": []}}]}"#,
        br#"{"candidates": [{"content": {"parts": [{"inlineData": {"mimeType": "audio/pcm", "data": "@@@"}}]}}]}"#,
        // A success with no whole sample is no audio, not an empty reply.
        br#"{"candidates": [{"content": {"parts": [{"inlineData": {"mimeType": "audio/pcm", "data": ""}}]}}]}"#,
        br#"{"candidates": [{"content": {"parts": [{"inlineData": {"mimeType": "audio/pcm", "data": "AQ=="}}]}}]}"#,
    ] {
        assert!(decode_response(body).is_err(), "{}", String::from_utf8_lossy(body));
    }
    let samples = decode_response(
        br#"{"candidates": [{"content": {"parts": [{"inlineData": {"mimeType": "audio/pcm", "data": "AQAC"}}]}}]}"#,
    )
    .unwrap();
    assert_eq!(samples, vec![1]);
    // A wrong-typed value is located, never repeated: here it is the audio.
    let err = decode_response(br#"{"candidates": "QUFBQUFBQUFBQUFB"}"#).unwrap_err();
    assert!(!err.to_string().contains("QUFB"), "{err}");
}

/// Answer every `generateContent` with a little audio and record each
/// request body, from a local port.
async fn recording_server() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use base64::Engine as _;
    use tokio::io::AsyncWriteExt;
    let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&bodies);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let audio = base64::engine::general_purpose::STANDARD.encode([0u8; 20]);
    let reply = serde_json::json!({
        "candidates": [{ "content": { "parts": [{ "inlineData": { "mimeType": "audio/L16", "data": audio } }] } }]
    })
    .to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let Some((_head, body)) = voice_pipes_net::testing::read_request(&mut sock).await
            else {
                continue;
            };
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                )
                .await;
        }
    });
    (format!("http://{addr}"), bodies)
}

/// The voice the console shows as live-editable is the one the next
/// synthesis uses. It was declared live but read from the build-time
/// config, so an edit changed nothing.
#[tokio::test]
async fn a_live_voice_edit_reaches_the_next_request() {
    let (endpoint, bodies) = recording_server().await;
    let cfg = TtsConfig::new("gemini")
        .with_api_key(Some("key".into()))
        .with_endpoint(Some(endpoint))
        .with_voice(Some("Kore".into()));
    let store = voice_pipes_sdk::params::AtomicParamStore::builder()
        .string("voice", "Kore")
        .build();
    let tts = GeminiTtsFactory
        .build(
            &cfg,
            &voice_pipes_sdk::BuildContext::new().with_params(Some(&store)),
        )
        .unwrap();
    store
        .string_cell("voice")
        .expect("voice cell")
        .store(std::sync::Arc::new("Puck".to_string()));
    tts.synthesize("hello").await.unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&bodies.lock().unwrap()[0]).expect("JSON body");
    assert_eq!(
        body["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]["voiceName"],
        "Puck"
    );
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

