//! `TtsProvider` implementation for native Rust/Candle Pocket-TTS.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use voice_pipes_sdk::component::{Component, ComponentCore, CoreComponent, DeviceInfo};
use voice_pipes_sdk::{AudioFrame, SynthesisRequest, TraceEvent, TtsError, TtsProvider};

use crate::config::{PocketCandleConfig, SAMPLE_RATE};
use crate::model::PocketEngine;
use crate::voices::VoiceCache;

/// Native Rust/Candle Pocket-TTS provider.
///
/// Shares one immutable `Arc<PocketEngine>` and one bounded `Arc<VoiceCache>`
/// across all concurrent sessions, bounding active compute threads via
/// `Semaphore` (`max_concurrent`).
pub struct PocketCandleTts {
    pub(crate) engine: Arc<PocketEngine>,
    pub(crate) voices: Arc<VoiceCache>,
    pub(crate) config: PocketCandleConfig,
    pub(crate) audit_tx: Option<mpsc::Sender<TraceEvent>>,
    pub(crate) core: Arc<ComponentCore>,
    pub(crate) concurrency: Arc<Semaphore>,
    pub(crate) warmed: AtomicBool,
}

impl std::fmt::Debug for PocketCandleTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PocketCandleTts")
            .field("model_dir", &self.config.model_dir)
            .field("voice", &*self.config.current_voice())
            .field("max_concurrent", &self.config.max_concurrent)
            .finish_non_exhaustive()
    }
}

impl PocketCandleTts {
    #[instrument(skip_all, fields(provider = "pocket-candle", chars = text.len(), voice))]
    async fn run_stream(
        &self,
        text: &str,
        voice: &str,
        audio_tx: mpsc::Sender<AudioFrame>,
        cancel: CancellationToken,
    ) -> Result<(), TtsError> {
        let _permit = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            permit = self.concurrency.acquire() => {
                permit.map_err(|e| TtsError::InferenceFailed(format!("pocket-candle semaphore closed: {e}")))?
            }
        };

        let started = Instant::now();
        let engine = Arc::clone(&self.engine);
        let voices = Arc::clone(&self.voices);
        let cfg = self.config.clone();
        let text_owned = text.to_string();
        let voice_owned = voice.to_string();
        let cancel_clone = cancel.clone();

        let (total_samples, retries) = tokio::task::spawn_blocking(move || {
            let voice_state = voices.get_or_load(&engine, &cfg.voices_dir, &voice_owned)?;
            engine.synthesize_blocking(&text_owned, &voice_state, &cfg, &audio_tx, &cancel_clone)
        })
        .await
        .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle worker join: {e}")))??;

        if !cancel.is_cancelled() && total_samples == 0 {
            return Err(TtsError::InferenceFailed(
                "pocket-candle: synthesis produced zero audio samples".into(),
            ));
        }

        if total_samples > 0 {
            self.emit_timing(text, total_samples, started.elapsed(), voice, retries);
        }
        Ok(())
    }

    fn emit_timing(
        &self,
        text: &str,
        sample_count: usize,
        elapsed: Duration,
        voice: &str,
        retries: u32,
    ) {
        if let Some(ref tx) = self.audit_tx {
            let _ = tx.try_send(voice_pipes_sdk::tts::detail_event(
                "tts:pocket-candle",
                text.chars().count(),
                sample_count,
                SAMPLE_RATE,
                elapsed,
                serde_json::json!({
                    "voice": voice,
                    "provider": "pocket-candle",
                    "retries": retries,
                    "pipeline_mimi": self.config.pipeline_mimi,
                }),
            ));
        }
    }
}

#[async_trait]
impl TtsProvider for PocketCandleTts {
    async fn device_info(&self) -> Option<DeviceInfo> {
        let (runs_on, detail) = match self.engine.device {
            candle_core::Device::Cuda(_) => ("cuda", "pocket-candle on CUDA"),
            candle_core::Device::Metal(_) => ("metal", "pocket-candle on Metal"),
            candle_core::Device::Cpu => ("cpu", "pocket-candle on CPU"),
        };
        Some(DeviceInfo::static_(runs_on, detail))
    }

    #[instrument(skip_all, fields(provider = "pocket-candle"))]
    async fn warm_up(&self) -> Result<(), TtsError> {
        if self.warmed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let _permit = self.concurrency.acquire().await.map_err(|e| {
            TtsError::InferenceFailed(format!("pocket-candle semaphore closed: {e}"))
        })?;
        if self.warmed.load(Ordering::Relaxed) {
            return Ok(());
        }

        let engine = Arc::clone(&self.engine);
        let voices = Arc::clone(&self.voices);
        let mut cfg = self.config.clone();
        cfg.max_frames = cfg.max_frames.min(4);
        cfg.silent_start = None;
        let voice_name = (*self.config.current_voice()).clone();
        let (tx, mut rx) = mpsc::channel::<AudioFrame>(8);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let res = tokio::task::spawn_blocking(move || {
            let voice_state = voices.get_or_load(&engine, &cfg.voices_dir, &voice_name)?;
            engine.synthesize_blocking("Hi.", &voice_state, &cfg, &tx, &CancellationToken::new())
        })
        .await
        .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle warm-up join: {e}")))?;

        let _ = drain.await;
        res?;
        self.warmed.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let voice = (*self.config.current_voice()).clone();
        let stream = voice_pipes_sdk::tts::collect_stream(SAMPLE_RATE, |tx| {
            self.run_stream(text, &voice, tx, CancellationToken::new())
        });
        voice_pipes_sdk::tts::recorded(&self.core, stream).await
    }

    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        cancel: CancellationToken,
    ) -> Result<(), TtsError> {
        let voice = req
            .hints
            .and_then(|h| h.voice.clone())
            .unwrap_or_else(|| (*self.config.current_voice()).clone());
        let token = cancel.clone();
        voice_pipes_sdk::tts::recorded_unless_cancelled(
            &self.core,
            &token,
            self.run_stream(req.text, &voice, tx, cancel),
        )
        .await
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn name(&self) -> &'static str {
        "pocket-candle"
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}
