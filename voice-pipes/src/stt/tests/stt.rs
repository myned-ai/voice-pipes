use super::*;
use crate::audio::AudioFrame;
use async_trait::async_trait;
use std::collections::HashSet;
use std::time::Duration;

pub struct MockSttProvider;

#[async_trait]
impl SttProvider for MockSttProvider {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Ok(TranscriptionResult::new("mock transcription", true)
            .with_confidence(Some(0.99))
            .with_language(Some("en-US".into())))
    }

    async fn transcribe_stream(
        &self,
        _request: RecognitionRequest,
        _audio_rx: tokio::sync::mpsc::Receiver<AudioChunk>,
        _tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        HashSet::new()
    }
}

#[tokio::test]
async fn mock_provider_returns_transcription() {
    let provider = MockSttProvider;
    let audio = AudioFrame {
        samples: vec![0i16; 16000],
        sample_rate: 16000,
        channels: 1,
    };
    let result = provider.transcribe(&audio).await.unwrap();
    assert_eq!(result.text, "mock transcription");
    assert!(result.is_final);
    assert_eq!(result.confidence, Some(0.99));
}

#[test]
fn error_classification() {
    assert!(!SttError::ModelLoadFailed("x".into()).is_transient());
    assert!(!SttError::ConfigError("x".into()).is_transient());
    assert!(!SttError::InvalidAudio("x".into()).is_transient());
    assert!(SttError::InferenceFailed("x".into()).is_transient());
    assert!(SttError::Timeout(Duration::from_secs(5)).is_transient());
    assert!(!SttError::ChannelClosed.is_transient());
}

#[test]
fn error_display() {
    let err = SttError::ModelLoadFailed("bad path".into());
    assert_eq!(err.to_string(), "model load failed: bad path");
}

#[test]
fn default_recognition_request() {
    let req = RecognitionRequest::default();
    assert_eq!(req.sample_rate, 16000);
    assert_eq!(req.channels, 1);
    assert_eq!(req.encoding, AudioEncoding::PcmI16);
    assert!(req.punctuation);
    assert!(!req.interim_results);
}

#[test]
fn transcription_result_fields() {
    let result = TranscriptionResult::new("hello world", false)
        .with_confidence(Some(0.85))
        .with_language(Some("en-US".into()))
        .with_segment_index(2)
        .with_start_time(Some(Duration::from_millis(100)))
        .with_end_time(Some(Duration::from_millis(500)));
    assert!(!result.is_final);
    assert_eq!(result.segment_index, 2);
    assert_eq!(result.start_time, Some(Duration::from_millis(100)));
}

#[test]
fn capabilities_set_operations() {
    let mut caps = HashSet::new();
    caps.insert(SttCapability::Streaming);
    caps.insert(SttCapability::InterimResults);
    assert!(caps.contains(&SttCapability::Streaming));
    assert!(!caps.contains(&SttCapability::WordTimestamps));
}

#[test]
fn word_info_round_trip() {
    let w = WordInfo::new(
        "hello",
        Duration::from_millis(100),
        Duration::from_millis(400),
    )
    .with_confidence(Some(0.95));
    assert_eq!(w.word, "hello");
    assert_eq!(
        w.end.checked_sub(w.start).unwrap(),
        Duration::from_millis(300)
    );
}

#[test]
fn stt_factory_names_are_unique() {
    let factories = stt_factories();
    let names: Vec<String> = factories.into_iter().map(|f| f.name().to_owned()).collect();
    let unique: std::collections::HashSet<&str> =
        names.iter().map(std::string::String::as_str).collect();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate STT factory names: {names:?}"
    );
}

#[test]
fn stt_factory_lookup_unknown_returns_none() {
    let factories = stt_factories();
    assert!(
        !factories.iter().any(|f| f.name() == "nonexistent"),
        "should not find a nonexistent provider"
    );
}

// ── Plugin registry tests ─────────────────────────────────

#[test]
fn register_stt_factory_appears_in_stt_factories() {
    // This test registers a mock SDK factory and checks it shows up.
    // Note: OnceLock means the registry state persists across tests in the
    // same process; we just verify the registration mechanism compiles and runs.
    struct MockSdkFactory;
    impl voice_pipes_sdk::SttFactory for MockSdkFactory {
        fn name(&self) -> &'static str {
            "mock-sdk-plugin"
        }
        fn validate(&self, _cfg: &voice_pipes_sdk::SttConfig) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::SttConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::SttProvider>, voice_pipes_sdk::BuildError> {
            Err(voice_pipes_sdk::BuildError::failed("mock factory"))
        }
    }

    register_stt_factory(|| Box::new(MockSdkFactory)).expect("test name is free");
    let names: Vec<String> = stt_factories()
        .into_iter()
        .map(|f| f.name().to_owned())
        .collect();
    assert!(
        names.iter().any(|n| n == "mock-sdk-plugin"),
        "registered factory should appear: {names:?}"
    );
}
