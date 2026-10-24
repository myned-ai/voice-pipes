//! [`SttProvider`] implementation for Google Cloud Speech-to-Text v2.
//!
//! Delegates the gRPC stream lifecycle to [`GoogleClient`](super::client::GoogleClient)
//! and keeps the component-health bookkeeping here, mirroring the
//! `AssemblyAI` provider.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::debug;

use voice_pipes_sdk::audio::AudioFrame;
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::stt::{
    AudioChunk, RecognitionRequest, SttCapability, SttError, SttProvider, TranscriptionResult,
};

use super::auth::AuthSource;
use super::client::GoogleClient;
use super::config::GoogleSttConfig;

/// Google Cloud Speech-to-Text v2 streaming provider.
#[derive(Debug)]
pub struct GoogleStt {
    config: GoogleSttConfig,
    /// Shared across streams so the TLS connection and the cached token
    /// survive per-utterance stream rotation. Created by the first stream
    /// (tonic's channel needs a running runtime, `build()` may not have one).
    channel: Arc<tokio::sync::OnceCell<tonic::transport::Channel>>,
    auth: Arc<AuthSource>,
    audit_tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::audit::TraceEvent>>,
    core: Arc<ComponentCore>,
}

impl GoogleStt {
    /// Create a provider from a validated configuration. No I/O here.
    pub fn new(config: GoogleSttConfig) -> Self {
        let auth = Arc::new(AuthSource::new(
            config.access_token.clone(),
            config.credentials_path.clone(),
            config.project_id.clone(),
        ));
        Self {
            config,
            channel: Arc::new(tokio::sync::OnceCell::new()),
            auth,
            audit_tx: None,
            core: Arc::new(ComponentCore::new("stt:google", ComponentKind::Stt)),
        }
    }

    /// Attach an audit trace channel for emitting STT usage events.
    #[must_use]
    pub fn with_audit_tx(
        mut self,
        tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::audit::TraceEvent>>,
    ) -> Self {
        self.audit_tx = tx;
        self
    }

    fn client(&self) -> GoogleClient {
        GoogleClient::new(
            self.config.clone(),
            Arc::clone(&self.channel),
            Arc::clone(&self.auth),
        )
        .with_audit_tx(self.audit_tx.clone())
        .with_core(Arc::clone(&self.core))
    }
}

/// Channel buffer size for the single-shot `transcribe` shim.
const TRANSCRIBE_CHANNEL_CAPACITY: usize = 8;

#[async_trait]
impl SttProvider for GoogleStt {
    async fn transcribe(&self, audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = std::time::Instant::now();
        let (tx, rx) = mpsc::channel::<AudioChunk>(TRANSCRIBE_CHANNEL_CAPACITY);
        let (res_tx, mut res_rx) =
            mpsc::channel::<TranscriptionResult>(TRANSCRIBE_CHANNEL_CAPACITY);

        // Feed 100 ms chunks at real-time pace: Google's streaming endpoint
        // is built for live audio and its endpointing keys off wall time.
        let chunk_size = (self.config.sample_rate as usize) / 10;
        let samples = audio.samples.clone();
        tokio::spawn(async move {
            let chunks: Vec<&[i16]> = samples.chunks(chunk_size).collect();
            let total = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                let is_last = i + 1 == total;
                if tx
                    .send(AudioChunk {
                        samples: chunk.to_vec(),
                        is_last,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                if !is_last {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            // Dropping `tx` closes the audio channel, which ends the stream.
        });

        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_guard = cancel.clone().drop_guard();
        let stream_handle = {
            let client = self.client();
            tokio::spawn(async move { client.run(rx, res_tx, cancel).await })
        };

        // Google may split the clip into several final segments; join them.
        let mut finals: Vec<String> = Vec::new();
        let mut last_final: Option<TranscriptionResult> = None;
        let mut last_any: Option<TranscriptionResult> = None;
        while let Some(result) = res_rx.recv().await {
            if !result.text.is_empty() {
                last_any = Some(result.clone());
            }
            if result.is_final && !result.text.is_empty() {
                finals.push(result.text.clone());
                last_final = Some(result);
            }
        }

        if let Err(e) = stream_handle
            .await
            .map_err(|e| SttError::InferenceFailed(format!("task join error: {e}")))
            .and_then(|r| r)
        {
            self.core.record_error(&e);
            return Err(e);
        }

        let out = match (last_final, last_any) {
            (Some(mut r), _) => {
                r.text = finals.join(" ");
                Ok(r)
            }
            (None, Some(r)) => Ok(r),
            (None, None) => Err(SttError::InferenceFailed(
                "no result received from Google STT".into(),
            )),
        };
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
            model = %self.config.model,
            location = %self.config.location,
            endpoint = %self.config.endpoint,
            "starting Google STT streaming session"
        );
        // Latency samples are recorded per final result inside the client
        // (speech end → final); the session length is not a latency.
        let res = self.client().run(audio_rx, tx, cancel).await;
        if let Err(e) = &res {
            self.core.record_error(e);
        }
        res
    }

    fn expected_sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    fn name(&self) -> &'static str {
        "google"
    }

    async fn model_identity(&self) -> Option<voice_pipes_sdk::component::ModelIdentity> {
        Some(voice_pipes_sdk::component::ModelIdentity::from_config(
            self.config.model.clone(),
        ))
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        let mut caps = HashSet::new();
        caps.insert(SttCapability::Streaming);
        if self.config.interim_results {
            caps.insert(SttCapability::InterimResults);
        }
        if self.config.enable_word_time_offsets {
            caps.insert(SttCapability::WordTimestamps);
        }
        if self.config.enable_voice_activity_events {
            caps.insert(SttCapability::BuiltinVad);
            caps.insert(SttCapability::EndOfTurn);
        }
        if self.config.language_codes.len() > 1 {
            caps.insert(SttCapability::LanguageDetection);
        }
        caps
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}
