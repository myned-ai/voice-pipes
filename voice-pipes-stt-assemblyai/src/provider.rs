//! [`SttProvider`] implementation for `AssemblyAI` V3 streaming API.
//!
//! Delegates the WebSocket lifecycle to [`WsClient`](super::ws_client::WsClient)
//! and maps Turn messages to the pipeline's [`TranscriptionResult`].

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::debug;

use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::{
    AudioChunk, RecognitionRequest, SttCapability, SttError, SttProvider, TranscriptionResult,
};

use super::config::AssemblyAiConfig;
use super::ws_client::WsClient;

/// `AssemblyAI` streaming STT provider.
#[derive(Debug)]
pub struct AssemblyAiStt {
    /// Validated configuration for this provider instance.
    config: AssemblyAiConfig,
    /// Optional channel to emit trace events to the audit system.
    audit_tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
    core: Arc<ComponentCore>,
}

impl AssemblyAiStt {
    /// Create a new provider from a validated configuration.
    pub fn new(config: AssemblyAiConfig) -> Self {
        Self {
            config,
            audit_tx: None,
            core: Arc::new(ComponentCore::new("stt:assemblyai", ComponentKind::Stt)),
        }
    }

    /// Attach an audit trace channel for emitting STT usage events.
    #[must_use]
    pub fn with_audit_tx(
        mut self,
        tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
    ) -> Self {
        self.audit_tx = tx;
        self
    }
}

/// Channel buffer size for the single-shot `transcribe` shim.
const TRANSCRIBE_CHANNEL_CAPACITY: usize = 8;

#[async_trait]
impl SttProvider for AssemblyAiStt {
    async fn transcribe(&self, audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = std::time::Instant::now();
        // Stream the audio in small chunks to give AssemblyAI time to
        // process incrementally, then send a final `is_last` chunk.
        let (tx, rx) = mpsc::channel::<AudioChunk>(TRANSCRIBE_CHANNEL_CAPACITY);
        let (res_tx, mut res_rx) =
            mpsc::channel::<TranscriptionResult>(TRANSCRIBE_CHANNEL_CAPACITY);

        // Chunk size: 100ms of audio at the configured sample rate.
        let chunk_size = (self.config.sample_rate as usize) / 10;
        let samples = audio.samples.clone();

        // Spawn a task to feed audio chunks with pacing.
        tokio::spawn(async move {
            let chunks: Vec<&[i16]> = samples.chunks(chunk_size).collect();
            let total = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                let is_last = i + 1 == total;
                let _ = tx
                    .send(AudioChunk {
                        samples: chunk.to_vec(),
                        is_last,
                    })
                    .await;
                if !is_last {
                    // Pace at ~100ms per chunk to simulate real-time streaming.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        });

        // Run the stream in parallel and collect the last final result.

        // Drop-guard: if this call is abandoned mid-await, the spawned
        // stream is cancelled instead of left running with a live session.
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_guard = cancel.clone().drop_guard();
        let stream_handle = {
            let config = self.config.clone();
            let audit_tx = self.audit_tx.clone();
            tokio::spawn(async move {
                let client = WsClient::new(config).with_audit_tx(audit_tx);
                client.run(rx, res_tx, cancel).await
            })
        };

        let mut last_result: Option<TranscriptionResult> = None;
        let mut last_any: Option<TranscriptionResult> = None;
        while let Some(result) = res_rx.recv().await {
            if !result.text.is_empty() {
                last_any = Some(result.clone());
            }
            if result.is_final {
                last_result = Some(result);
            }
        }

        // Await the streaming task to propagate errors.
        if let Err(e) = stream_handle
            .await
            .map_err(|e| SttError::InferenceFailed(format!("task join error: {e}")))
            .and_then(|r| r)
        {
            self.core.record_error(&e);
            return Err(e);
        }

        // Prefer a final (end-of-turn) result, but fall back to the last
        // non-empty transcript for short audio that terminates before an
        // end-of-turn is emitted.
        let out = last_result.or(last_any).ok_or_else(|| {
            SttError::InferenceFailed("no final result received from AssemblyAI".into())
        });
        match &out {
            Ok(_) => self.core.record_ok(start.elapsed()),
            Err(e) => self.core.record_error(e),
        }
        out
    }

    async fn transcribe_stream(
        &self,
        _request: RecognitionRequest,
        audio_rx: mpsc::Receiver<AudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        debug!(
            speech_model = %self.config.speech_model,
            endpoint = %self.config.endpoint,
            "starting AssemblyAI streaming session"
        );

        let start = std::time::Instant::now();
        let client = WsClient::new(self.config.clone()).with_audit_tx(self.audit_tx.clone());
        let res = client.run(audio_rx, tx, cancel).await;
        match &res {
            Ok(()) => self.core.record_ok(start.elapsed()),
            Err(e) => self.core.record_error(e),
        }
        res
    }

    fn expected_sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    fn name(&self) -> &'static str {
        "assemblyai"
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        let mut caps = HashSet::new();
        caps.insert(SttCapability::Streaming);
        caps.insert(SttCapability::InterimResults);
        caps.insert(SttCapability::WordTimestamps);
        caps.insert(SttCapability::BuiltinVad);
        caps.insert(SttCapability::EndOfTurn);
        caps.insert(SttCapability::CustomVocabulary);
        // Eager mid-turn finals repeat the whole turn so far under the same
        // segment index as the end-of-turn final that follows them.
        caps.insert(SttCapability::CumulativeFinals);

        if self.config.language_detection == Some(true) {
            caps.insert(SttCapability::LanguageDetection);
        }

        caps
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}
