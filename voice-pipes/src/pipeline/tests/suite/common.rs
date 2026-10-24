use crate::audio::AudioFrame;
use crate::stt::SttProvider;
use crate::stt::{SttCapability, SttError, TranscriptionResult};
use crate::transport::ServerMessage;
use crate::tts::{TtsError, TtsProvider};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;

/// Create a no-op event channel and error flag for `tts_task` tests.
pub(super) fn test_event_channel() -> (mpsc::Sender<ServerMessage>, Arc<AtomicBool>) {
    let (tx, _rx) = mpsc::channel(8);
    (tx, Arc::new(AtomicBool::new(false)))
}

pub(super) struct MockStt {
    pub(super) expected_rate: u32,
    pub(super) response: String,
}

#[async_trait]
impl SttProvider for MockStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Ok(TranscriptionResult::new(self.response.clone(), true).with_confidence(Some(0.99)))
    }

    async fn transcribe_stream(
        &self,
        _request: crate::stt::RecognitionRequest,
        _audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        let _ = tx
            .send(TranscriptionResult::new(self.response.clone(), true).with_confidence(Some(0.99)))
            .await;
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        self.expected_rate
    }

    fn name(&self) -> &'static str {
        "mock-stt"
    }

    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        std::collections::HashSet::new()
    }
}

pub(super) struct MockTts {
    pub(super) sample_rate: u32,
}

#[async_trait]
impl TtsProvider for MockTts {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        // Generate a simple frame with length proportional to text
        let num_samples = text.len() * 100;
        Ok(AudioFrame {
            samples: vec![0i16; num_samples],
            sample_rate: self.sample_rate,
            channels: 1,
        })
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn name(&self) -> &'static str {
        "mock-tts"
    }
}

/// A streaming provider that stays silent until it has consumed
/// `finalize_after_samples` samples, then emits one final result and keeps
/// draining. Models a long open-mic stretch before the first final.
pub(super) struct FinalizeAfterSamplesStt {
    pub(super) finalize_after_samples: usize,
}

#[async_trait]
impl SttProvider for FinalizeAfterSamplesStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Err(SttError::NetworkError("mock".into()))
    }

    async fn transcribe_stream(
        &self,
        _request: crate::stt::RecognitionRequest,
        mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        let mut seen = 0usize;
        let mut sent = false;
        while let Some(chunk) = audio_rx.recv().await {
            seen += chunk.samples.len();
            if !sent && seen >= self.finalize_after_samples {
                sent = true;
                let _ = tx
                    .send(
                        TranscriptionResult::new("a long monologue finally ends", true)
                            .with_confidence(Some(0.9))
                            .with_is_end_of_turn(Some(true)),
                    )
                    .await;
            }
        }
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "mock-late-final-stt"
    }

    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        std::collections::HashSet::new()
    }
}

/// A streaming provider that fails instantly, to exercise the respawn path.
pub(super) struct FailingStreamStt {
    pub(super) spawns: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl SttProvider for FailingStreamStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Err(SttError::NetworkError("mock failure".into()))
    }

    async fn transcribe_stream(
        &self,
        _request: crate::stt::RecognitionRequest,
        _audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        _result_tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        self.spawns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(SttError::NetworkError("mock failure".into()))
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "mock-failing-stt"
    }

    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        std::collections::HashSet::new()
    }
}

/// A streaming provider that drops its input at once (so the next chunk
/// triggers a respawn) but ignores cancel and never returns — the shape of
/// a worker wedged on a provider socket. `live` counts streams whose future
/// is still alive; an aborted future drops its guard.
pub(super) struct HangingStreamStt {
    pub(super) spawns: Arc<std::sync::atomic::AtomicUsize>,
    pub(super) live: Arc<std::sync::atomic::AtomicUsize>,
}

pub(super) struct LiveGuard(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl SttProvider for HangingStreamStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Err(SttError::NetworkError("mock".into()))
    }

    async fn transcribe_stream(
        &self,
        _request: crate::stt::RecognitionRequest,
        audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
        _result_tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        self.spawns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.live.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _guard = LiveGuard(Arc::clone(&self.live));
        drop(audio_rx);
        std::future::pending::<()>().await;
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "mock-hanging-stt"
    }

    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        std::collections::HashSet::new()
    }
}
