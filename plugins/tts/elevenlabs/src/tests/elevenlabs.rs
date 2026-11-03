use std::time::Duration;

use voice_pipes_sdk::audio::PcmFramer;
use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{SynthesisHints, TtsConfig, TtsError, TtsFactory};

use crate::config::{
    DEFAULT_ENDPOINT, DEFAULT_MODEL, DEFAULT_SAMPLE_RATE, DEFAULT_VOICE, ElevenLabsConfig,
};
use crate::factory::ElevenLabsTtsFactory;
use crate::provider::ElevenLabsTts;
use crate::wire::{Alignment, TimestampChunk, map_http_error};
use voice_pipes_sdk::tts::WordGrouper;

/// the key never appears in Debug output.
#[test]
fn debug_redacts_api_key() {
    let mut src = base_cfg();
    src.api_key = Some("super-secret-key".into());
    let cfg = ElevenLabsConfig::from_tts_config(&src).unwrap();
    let s = format!("{cfg:?}");
    assert!(!s.contains("super-secret-key"), "{s}");
    assert!(s.contains("***"), "{s}");
}

fn base_cfg() -> TtsConfig {
    TtsConfig::new("elevenlabs").with_api_key(Some("test-key".into()))
}

fn provider_from(cfg: &TtsConfig) -> ElevenLabsTts {
    ElevenLabsTts {
        client: voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            std::time::Duration::from_secs(5),
        ))
        .unwrap(),
        config: ElevenLabsConfig::from_tts_config(cfg).unwrap(),
        audit_tx: None,
        core: std::sync::Arc::new(ComponentCore::new("tts:elevenlabs", ComponentKind::Tts)),
    }
}

#[test]
fn config_defaults() {
    let ec = ElevenLabsConfig::from_tts_config(&base_cfg()).unwrap();
    assert_eq!(ec.model_id, DEFAULT_MODEL);
    assert_eq!(*ec.current_voice(), DEFAULT_VOICE);
    assert_eq!(ec.endpoint, DEFAULT_ENDPOINT);
    assert_eq!(ec.api_key.expose(), "test-key");
    assert_eq!(ec.sample_rate, DEFAULT_SAMPLE_RATE);
    assert_eq!(ec.request_timeout, Duration::from_secs(30));
    assert!(ec.word_timestamps, "word timestamps default on");
    assert!(ec.speed.is_none());
    assert!(ec.language_code.is_none());
}

#[test]
fn config_custom_values() {
    let mut cfg = base_cfg();
    cfg.model_name = Some("eleven_multilingual_v2".into());
    cfg.voice = Some("voice123".into());
    cfg.options
        .insert("sample_rate".into(), serde_json::json!(16000));
    cfg.options
        .insert("word_timestamps".into(), serde_json::json!(false));
    cfg.options
        .insert("stability".into(), serde_json::json!(0.4));
    cfg.options
        .insert("language_code".into(), serde_json::json!("de"));

    let ec = ElevenLabsConfig::from_tts_config(&cfg).unwrap();
    assert_eq!(ec.model_id, "eleven_multilingual_v2");
    assert_eq!(*ec.current_voice(), "voice123");
    assert_eq!(ec.sample_rate, 16000);
    assert!(!ec.word_timestamps);
    assert_eq!(ec.stability, Some(0.4));
    assert_eq!(ec.language_code.as_deref(), Some("de"));
}

#[test]
fn config_rejects_bad_sample_rate() {
    let mut cfg = base_cfg();
    cfg.options
        .insert("sample_rate".into(), serde_json::json!(48000));
    assert!(ElevenLabsConfig::from_tts_config(&cfg).is_err());
}

/// A rate past `u32` is refused, not truncated into an allowed one:
/// 2^32 + 16 000 must not pass as 16 kHz.
#[test]
fn a_sample_rate_past_u32_is_refused_not_truncated() {
    let mut cfg = base_cfg();
    cfg.options.insert(
        "sample_rate".into(),
        serde_json::json!((1_u64 << 32) + 16_000),
    );
    assert!(ElevenLabsConfig::from_tts_config(&cfg).is_err());
    assert!(
        !ElevenLabsTtsFactory.validate(&cfg).is_empty(),
        "validation refuses it too"
    );
}

#[test]
fn request_url_construction() {
    let provider = provider_from(&base_cfg());
    assert_eq!(
        provider.url("voiceABC", false),
        "https://api.elevenlabs.io/v1/text-to-speech/voiceABC/stream?output_format=pcm_24000"
    );
    assert_eq!(
        provider.url("voiceABC", true),
        "https://api.elevenlabs.io/v1/text-to-speech/voiceABC/stream/with-timestamps?output_format=pcm_24000"
    );

    // Path characters in a (hostile) voice id are percent-encoded.
    assert_eq!(
        provider.url("a/b?c", false),
        "https://api.elevenlabs.io/v1/text-to-speech/a%2Fb%3Fc/stream?output_format=pcm_24000"
    );

    // Custom endpoint with trailing slash.
    let mut cfg = base_cfg();
    cfg.endpoint = Some("https://api.eu.residency.elevenlabs.io/v1/".into());
    cfg.options
        .insert("sample_rate".into(), serde_json::json!(16000));
    let provider = provider_from(&cfg);
    assert_eq!(
        provider.url("v", false),
        "https://api.eu.residency.elevenlabs.io/v1/text-to-speech/v/stream?output_format=pcm_16000"
    );
}

#[test]
fn voice_settings_omitted_when_defaults() {
    let provider = provider_from(&base_cfg());
    assert!(provider.voice_settings(None).is_none());
}

#[test]
fn voice_settings_from_hints_override_config() {
    let mut cfg = base_cfg();
    cfg.options.insert("speed".into(), serde_json::json!(1.1));
    cfg.options
        .insert("stability".into(), serde_json::json!(0.5));
    let provider = provider_from(&cfg);

    let hints = SynthesisHints::default().with_speed(Some(0.9));
    let vs = provider.voice_settings(Some(&hints)).unwrap();
    assert_eq!(vs.speed, Some(0.9), "hint wins over config");
    assert_eq!(vs.stability, Some(0.5), "config fills the rest");
}

// ── Word aggregation tests ──────────────────────────────

fn alignment(chars: &[&str], starts_s: &[f64], ends_s: &[f64]) -> Alignment {
    Alignment {
        characters: chars.iter().map(|s| (*s).to_string()).collect(),
        character_start_times_seconds: starts_s.to_vec(),
        character_end_times_seconds: ends_s.to_vec(),
    }
}

#[test]
fn words_grouped_on_whitespace_punctuation_attached() {
    let a = alignment(
        &["H", "i", ",", " ", "y", "o", "u", "!"],
        &[0.0, 0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35],
        &[0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40],
    );
    let mut agg = WordGrouper::new();
    let mut out = Vec::new();
    a.group_into(&mut agg, &mut out);
    agg.flush(&mut out);

    assert_eq!(out.len(), 2);
    assert_eq!(out[0].word, "Hi,");
    assert_eq!(out[0].start_ms, 0);
    assert_eq!(out[0].duration_ms, 150);
    assert_eq!(out[1].word, "you!");
    assert_eq!(out[1].start_ms, 200);
    assert_eq!(out[1].duration_ms, 200);
}

#[test]
fn word_split_across_chunks_is_stitched() {
    // "hel" arrives in chunk 1, "lo" + trailing space in chunk 2.
    // Cumulative times mean the stitched word spans both chunks.
    let c1 = alignment(&["h", "e", "l"], &[0.0, 0.1, 0.2], &[0.1, 0.2, 0.3]);
    let c2 = alignment(&["l", "o", " "], &[0.3, 0.4, 0.5], &[0.4, 0.5, 0.55]);

    let mut agg = WordGrouper::new();
    let mut out = Vec::new();
    c1.group_into(&mut agg, &mut out);
    assert!(out.is_empty(), "no whitespace yet -- word still open");
    c2.group_into(&mut agg, &mut out);
    agg.flush(&mut out);

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].word, "hello");
    assert_eq!(out[0].start_ms, 0);
    assert_eq!(out[0].duration_ms, 500);
}

#[test]
fn ragged_alignment_truncates_without_panicking() {
    // 3 chars but only 2 start times: truncate to 2.
    let a = alignment(&["a", "b", " "], &[0.0, 0.1], &[0.1, 0.2, 0.3]);
    let mut agg = WordGrouper::new();
    let mut out = Vec::new();
    a.group_into(&mut agg, &mut out);
    agg.flush(&mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].word, "ab");
}

#[test]
fn stream_chunk_parses_documented_shape() {
    let json = r#"{"audio_base64":"AAA=","alignment":{"characters":["h","i"],"character_start_times_seconds":[0.0,0.1],"character_end_times_seconds":[0.1,0.2]},"normalized_alignment":null}"#;
    let chunk: TimestampChunk = serde_json::from_str(json).unwrap();
    assert_eq!(chunk.audio_base64.as_deref(), Some("AAA="));
    let a = chunk.alignment.unwrap();
    assert_eq!(a.characters, vec!["h", "i"]);
    assert_eq!(a.character_start_times_seconds, vec![0.0, 0.1]);
}

// ── Error mapping tests ─────────────────────────────────

#[test]
fn error_mapping() {
    let body = r#"{"detail":{"status":"invalid_api_key","message":"Invalid API key"}}"#;
    match map_http_error(401, body) {
        TtsError::AuthError(m) => assert_eq!(m, "ElevenLabs TTS: Invalid API key"),
        other => panic!("expected AuthError, got {other:?}"),
    }

    let body = r#"{"detail":"Voice not found"}"#;
    match map_http_error(400, body) {
        TtsError::ConfigError(m) => assert_eq!(m, "ElevenLabs TTS: Voice not found"),
        other => panic!("expected ConfigError, got {other:?}"),
    }

    assert!(matches!(
        map_http_error(429, ""),
        TtsError::RateLimited { .. }
    ));
    assert!(matches!(
        map_http_error(500, ""),
        TtsError::InferenceFailed(_)
    ));
}

#[test]
fn factory_validate_requires_key() {
    let mut cfg = base_cfg();
    cfg.api_key = None;
    let errors = ElevenLabsTtsFactory.validate(&cfg);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].field, "api_key");
}

// ── Malformed stream chunks ─────────────────────────────────

/// A stream line that is not the documented chunk (not JSON, bad base64) is
/// an error; one missing optional parts, or blank, is skipped. None panics.
#[tokio::test]
async fn malformed_stream_lines_are_errors_or_skipped_never_panics() {
    let provider = provider_from(&base_cfg());
    let consume = |line: &'static [u8]| {
        let provider = &provider;
        async move {
            let (mut pcm, mut words) = (PcmFramer::new(24_000, 1024), WordGrouper::new());
            provider
                .consume_json_line(line, &mut pcm, &mut words, None)
                .await
                .map(|()| pcm.finish().map(|f| f.samples).unwrap_or_default())
        }
    };
    assert!(consume(b"not json").await.is_err());
    assert!(
        consume(br#"{"audio_base64": "***not base64***"}"#)
            .await
            .is_err()
    );
    assert!(consume(br#"{"audio_base64": 5}"#).await.is_err());
    // A wrong-typed field whose value is the spoken text: the error locates
    // the fault without repeating the words.
    let err = consume(br#"{"alignment": {"characters": "the caller's words"}}"#)
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("words"), "{err}");
    assert_eq!(consume(b"   ").await.unwrap(), Vec::<i16>::new());
    assert_eq!(consume(b"{}").await.unwrap(), Vec::<i16>::new());
    assert_eq!(
        consume(br#"{"audio_base64": "AQA=", "alignment": null}"#)
            .await
            .unwrap(),
        vec![1]
    );
}

/// The factory reports the configured rate, or the default.
#[test]
fn the_factory_reports_its_output_rate() {
    let mut cfg = base_cfg();
    assert_eq!(ElevenLabsTtsFactory.output_sample_rate(&cfg), Some(24_000));
    cfg.options
        .insert("sample_rate".into(), serde_json::json!(16_000));
    assert_eq!(ElevenLabsTtsFactory.output_sample_rate(&cfg), Some(16_000));
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
    let cfg = TtsConfig::new("elevenlabs")
        .with_api_key(Some("sk-test".into()))
        .with_endpoint(Some(format!("{base}/v1")))
        .with_voice(Some("voiceABC".into()))
        .with_options(
            [
                ("request_timeout_ms".to_string(), serde_json::json!(300)),
                ("word_timestamps".to_string(), serde_json::json!(false)),
            ]
            .into_iter()
            .collect(),
        );
    let tts = ElevenLabsTtsFactory
        .build(&cfg, &voice_pipes_sdk::BuildContext::new())
        .unwrap();
    let frame = tts.synthesize("hello").await.unwrap();
    assert_eq!(frame.samples, samples);
}

/// A success status with no audio is an error, not a reply the caller
/// never hears: the engine's retry and health count see it.
#[tokio::test]
async fn a_success_with_no_audio_is_an_error() {
    let base = drip_server(Vec::new(), Vec::new(), std::time::Duration::ZERO).await;
    let cfg = TtsConfig::new("elevenlabs")
        .with_api_key(Some("sk-test".into()))
        .with_endpoint(Some(format!("{base}/v1")))
        .with_voice(Some("voiceABC".into()))
        .with_options(
            [("word_timestamps".to_string(), serde_json::json!(false))]
                .into_iter()
                .collect(),
        );
    let tts = ElevenLabsTtsFactory
        .build(&cfg, &voice_pipes_sdk::BuildContext::new())
        .unwrap();
    let err = tts.synthesize("hello").await.unwrap_err();
    assert!(matches!(err, TtsError::InferenceFailed(_)), "{err:?}");
}

/// Voice settings outside what the API takes are refused, not sent.
#[test]
fn voice_settings_out_of_range_are_refused() {
    for (key, value) in [("speed", 2.0), ("stability", 1.5), ("style", -0.1)] {
        let mut cfg = base_cfg();
        cfg.options.insert(key.into(), serde_json::json!(value));
        assert!(
            ElevenLabsTtsFactory
                .validate(&cfg)
                .iter()
                .any(|e| e.field == format!("tts.{key}")),
            "{key} = {value}"
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
