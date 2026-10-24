use super::*;

#[test]
fn extra_bytes_counts_only_free_form_parts_and_cap_is_enforceable() {
    let mut cfg = crate::transport::SessionConfig::default();
    assert!(
        handshake_extra_bytes(&cfg) < 16,
        "an empty config is nearly free"
    );

    cfg.options
        .insert("k".into(), serde_json::Value::String("v".repeat(1000)));
    cfg.tags = vec!["a".repeat(500)];
    let mid = handshake_extra_bytes(&cfg);
    assert!(mid > 1500 && mid <= MAX_HANDSHAKE_EXTRA_BYTES, "{mid}");

    cfg.app_data = Some(serde_json::Value::String(
        "x".repeat(MAX_HANDSHAKE_EXTRA_BYTES),
    ));
    assert!(handshake_extra_bytes(&cfg) > MAX_HANDSHAKE_EXTRA_BYTES);
}

/// Tags are trimmed to the cap at the handshake, cutting long ones at a
/// character boundary rather than rejecting the session.
#[test]
fn tags_are_capped_in_count_and_length() {
    let mut tags: Vec<String> = (0..MAX_TAGS + 5).map(|i| format!("t{i}")).collect();
    tags[0] = "é".repeat(MAX_TAG_BYTES); // 2 bytes per char
    cap_tags(&mut tags, "s");
    assert_eq!(tags.len(), MAX_TAGS);
    assert_eq!(tags[0].len(), MAX_TAG_BYTES);
    assert_eq!(tags[0].chars().count(), MAX_TAG_BYTES / 2);
    assert_eq!(tags[1], "t1");

    let mut fine = vec!["ok".to_string()];
    cap_tags(&mut fine, "s");
    assert_eq!(fine, vec!["ok".to_string()]);
}

#[test]
fn handshake_error_names_the_variant_not_the_payload() {
    let msg = ClientMessage::Audio(crate::pipeline::AudioChunk {
        samples: vec![1; 100_000],
        ..Default::default()
    });
    assert_eq!(client_message_kind(&msg), "audio");
}

/// The STT mode written to `metadata.json` follows the provider's declared
/// `Streaming` capability.
#[test]
fn stt_mode_follows_the_streaming_capability() {
    struct Stt(std::collections::HashSet<crate::stt::SttCapability>);

    #[async_trait::async_trait]
    impl crate::stt::SttProvider for Stt {
        async fn transcribe(
            &self,
            _audio: &crate::audio::AudioFrame,
        ) -> Result<crate::stt::TranscriptionResult, crate::stt::SttError> {
            Err(crate::stt::SttError::NetworkError("unused".into()))
        }
        async fn transcribe_stream(
            &self,
            _req: crate::stt::RecognitionRequest,
            _audio_rx: tokio::sync::mpsc::Receiver<crate::stt::AudioChunk>,
            _tx: tokio::sync::mpsc::Sender<crate::stt::TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), crate::stt::SttError> {
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16_000
        }
        fn name(&self) -> &'static str {
            "stt-mode-test"
        }
        fn capabilities(&self) -> std::collections::HashSet<crate::stt::SttCapability> {
            self.0.clone()
        }
    }

    let streaming = Stt([crate::stt::SttCapability::Streaming].into());
    let batch = Stt([crate::stt::SttCapability::WordTimestamps].into());
    assert_eq!(stt_mode_of(&streaming), "streaming");
    assert_eq!(stt_mode_of(&batch), "batch");
}
