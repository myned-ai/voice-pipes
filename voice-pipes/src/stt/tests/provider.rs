use super::*;
use crate::audio::AudioFrame;
use crate::stt::{AudioChunk, RecognitionRequest, TranscriptionResult};
use async_trait::async_trait;

struct DummyProvider {
    tag: &'static str,
    caps: HashSet<SttCapability>,
}

#[async_trait]
impl SttProvider for DummyProvider {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        unimplemented!()
    }
    async fn transcribe_stream(
        &self,
        _req: RecognitionRequest,
        _rx: tokio::sync::mpsc::Receiver<AudioChunk>,
        _tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        unimplemented!()
    }
    fn expected_sample_rate(&self) -> u32 {
        16000
    }
    fn name(&self) -> &str {
        self.tag
    }
    fn capabilities(&self) -> HashSet<SttCapability> {
        self.caps.clone()
    }
}

fn make_provider(tag: &'static str, caps: &[SttCapability]) -> Arc<dyn SttProvider> {
    Arc::new(DummyProvider {
        tag,
        caps: caps.iter().cloned().collect(),
    })
}

#[test]
fn single_provider_active() {
    let reg = SttProviderRegistry::new("local", make_provider("local", &[]));
    assert_eq!(reg.active_name(), "local");
    assert_eq!(reg.active().name(), "local");
}

#[test]
fn switch_provider() {
    let mut reg = SttProviderRegistry::new("local", make_provider("local", &[]));
    reg.register(
        "cloud",
        make_provider(
            "cloud",
            &[SttCapability::Streaming, SttCapability::BuiltinVad],
        ),
    );
    assert_eq!(reg.active_name(), "local");

    reg.switch("cloud").unwrap();
    assert_eq!(reg.active_name(), "cloud");
    assert!(reg.active().supports(&SttCapability::BuiltinVad));
}

#[test]
fn switch_unknown_errors() {
    let mut reg = SttProviderRegistry::new("local", make_provider("local", &[]));
    let err = reg.switch("nonexistent").unwrap_err();
    assert!(matches!(err, SttError::ConfigError(_)));
}

#[test]
fn list_all_providers() {
    let mut reg =
        SttProviderRegistry::new("a", make_provider("a", &[SttCapability::InterimResults]));
    reg.register(
        "b",
        make_provider(
            "b",
            &[SttCapability::Streaming, SttCapability::WordTimestamps],
        ),
    );
    let list = reg.list();
    assert_eq!(list.len(), 2);
}
