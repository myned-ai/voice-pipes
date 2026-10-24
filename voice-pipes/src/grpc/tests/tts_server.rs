use super::*;
use crate::tts::TtsError;
use proto::tts_service_server::TtsService;

/// A provider with native streaming: several frames, each `send`
/// awaited. The default one-frame path never exercised this shape.
struct ThreeFrameTts;

#[async_trait::async_trait]
impl TtsProvider for ThreeFrameTts {
    async fn synthesize(&self, _text: &str) -> Result<AudioFrame, TtsError> {
        unreachable!("Synthesize with hints must drive the streaming path")
    }
    async fn synthesize_stream(
        &self,
        _req: crate::tts::SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        for _ in 0..3 {
            tx.send(AudioFrame {
                samples: vec![7; 240],
                sample_rate: 24_000,
                channels: 1,
            })
            .await
            .map_err(|_undelivered_frame| TtsError::InferenceFailed("receiver gone".into()))?;
        }
        Ok(())
    }
    fn sample_rate(&self) -> u32 {
        24_000
    }
    fn name(&self) -> &'static str {
        "three-frame"
    }
}

/// `Synthesize` with hints used to await the provider to
/// completion on a 1-slot channel before reading it, so a streaming
/// provider deadlocked on its second frame and the RPC never returned.
#[tokio::test]
async fn synthesize_with_hints_coalesces_a_streaming_provider() {
    let svc = TtsServiceImpl::new(Arc::new(ThreeFrameTts), Vec::new(), serde_json::Map::new());
    let req = proto::SynthesizeRequest {
        text: "hello".into(),
        hints: Some(proto::SynthesisHints {
            voice: Some("v".into()),
            language: None,
            speed: None,
            options: std::collections::HashMap::new(),
        }),
    };
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        svc.synthesize(Request::new(req)),
    )
    .await
    .expect("Synthesize must not hang on a streaming provider")
    .expect("synthesis ok")
    .into_inner();
    assert_eq!(resp.sample_rate, 24_000);
    assert_eq!(resp.samples_i16.len(), 3 * 240 * 2);
}
