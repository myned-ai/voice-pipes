//! Piper VITS text-to-speech -- local ONNX inference, espeak-ng phonemes.
//!
//! A voice-pipes voice plugin: the binary registers [`factory`] and
//! [`wizard_choices`]; `[tts] provider = "piper"` selects it. The
//! voice-pipes binary also registers it as the voice a pipeline without
//! `[tts]` falls back to.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

mod engine;
mod model_config;
mod phonemizer;
mod schema;
mod wizard;

pub use wizard::wizard_choices;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument};

use engine::PiperEngine;
use model_config::PiperModelConfig;
use voice_pipes_onnx::SessionPool;
use voice_pipes_sdk::component::{
    Component, ComponentCore, ComponentKind, CoreComponent, DeviceInfo,
};
use voice_pipes_sdk::{
    AudioFrame, BuildContext, BuildError, ConfigError, SynthesisRequest, TextChunkingConfig,
    TraceEvent, TtsConfig, TtsError, TtsFactory, TtsProvider,
};

// ── Engine pool ──────────────────────────────────────────────

/// Default number of engine copies in the pool.
const DEFAULT_POOL_SIZE: usize = 2;

/// Default ONNX intra-op thread count per engine session.
const DEFAULT_THREADS: usize = 4;

/// Build the bounded pool of [`PiperEngine`]s used for concurrent inference.
fn build_pool(
    model_path: &Path,
    pool_size: usize,
    threads: usize,
) -> Result<SessionPool<PiperEngine>, TtsError> {
    let mut engines = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        engines.push(PiperEngine::load(model_path, threads)?);
    }
    info!(pool_size, threads, "Piper engine pool created");
    SessionPool::new(engines, "Piper engines").map_err(|e| TtsError::ConfigError(e.to_string()))
}

// ── Provider ─────────────────────────────────────────────────

/// Piper VITS TTS provider -- local ONNX inference with espeak-ng phonemization.
pub(crate) struct PiperTts {
    pool: Arc<SessionPool<PiperEngine>>,
    config: Arc<PiperModelConfig>,
    text_chunking: TextChunkingConfig,
    model_path: PathBuf,
    audit_tx: Option<tokio::sync::mpsc::Sender<TraceEvent>>,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for PiperTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiperTts")
            .field("model_path", &self.model_path)
            .finish_non_exhaustive()
    }
}

impl PiperTts {
    /// Load the voice at `model_path` (a `.onnx` with its `.onnx.json`
    /// config beside it), `pool_size` engines for concurrent synthesis.
    pub(crate) fn with_pool_size(
        model_path: &Path,
        pool_size: usize,
        threads: usize,
        text_chunking: TextChunkingConfig,
    ) -> Result<Self, TtsError> {
        let core = Arc::new(ComponentCore::new("tts:piper", ComponentKind::Tts));
        let config_path = model_path.with_extension("onnx.json");
        let config = match PiperModelConfig::load(&config_path) {
            Ok(c) => c,
            Err(e) => {
                core.mark_unhealthy(format!("config load failed: {e}"));
                return Err(e);
            }
        };
        let pool = match build_pool(model_path, pool_size, threads) {
            Ok(p) => p,
            Err(e) => {
                core.mark_unhealthy(format!("model load failed: {e}"));
                return Err(e);
            }
        };

        info!(
            model = %model_path.display(),
            sample_rate = config.audio.sample_rate,
            pool_size,
            threads,
            "Piper TTS loaded"
        );

        Ok(Self {
            pool: Arc::new(pool),
            config: Arc::new(config),
            text_chunking,
            model_path: model_path.to_path_buf(),
            audit_tx: None,
            core,
        })
    }

    async fn synthesize_inner(
        &self,
        text: &str,
        t0: std::time::Instant,
    ) -> Result<AudioFrame, TtsError> {
        // Phonemize: text -> IPA -> phoneme IDs
        let phoneme_ids = {
            let text = text.to_string();
            let config = Arc::clone(&self.config);
            tokio::task::spawn_blocking(move || phonemizer::phonemize(&text, &config))
                .await
                .map_err(|e| TtsError::PhonemizationFailed(format!("task join: {e}")))?
        }?;
        if phoneme_ids.is_empty() {
            return Ok(AudioFrame {
                samples: Vec::new(),
                sample_rate: self.config.audio.sample_rate,
                channels: self.config.audio.channels,
            });
        }

        // Inference: phoneme IDs -> PCM samples (via pool checkout)
        let mut pooled = self.pool.checkout().await.map_err(TtsError::busy)?;
        let config = Arc::clone(&self.config);
        let samples =
            tokio::task::spawn_blocking(move || pooled.get_mut().infer(&phoneme_ids, &config))
                .await
                .map_err(|e| TtsError::InferenceFailed(format!("task join: {e}")))?;
        let samples = samples?;

        let audio_duration_s = seconds(samples.len(), self.config.audio.sample_rate);
        tracing::Span::current().record("audio_duration_s", audio_duration_s);

        if let Some(tx) = &self.audit_tx {
            let _ = tx.try_send(TraceEvent::new(
                "tts:piper",
                "tts_usage",
                serde_json::json!({
                    "chars": text.len(),
                    "audio_duration_s": audio_duration_s,
                    "sample_rate": self.config.audio.sample_rate,
                }),
            ));
            let _ = tx.try_send(voice_pipes_sdk::tts::detail_event(
                "tts:piper",
                text.len(),
                samples.len(),
                self.config.audio.sample_rate,
                t0.elapsed(),
                serde_json::json!({}),
            ));
        }

        Ok(AudioFrame {
            samples,
            sample_rate: self.config.audio.sample_rate,
            channels: self.config.audio.channels,
        })
    }
}

#[async_trait]
impl TtsProvider for PiperTts {
    #[instrument(skip_all, fields(provider = "piper", text_len = text.len()))]
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let t0 = std::time::Instant::now();
        let res = self.synthesize_inner(text, t0).await.and_then(|frame| {
            if frame.samples.is_empty() {
                Err(no_audio())
            } else {
                Ok(frame)
            }
        });
        match &res {
            Ok(_) => self.core.record_ok(t0.elapsed()),
            Err(e) => self.core.record_error(e),
        }
        res
    }

    fn sample_rate(&self) -> u32 {
        self.config.audio.sample_rate
    }

    fn name(&self) -> &'static str {
        "piper"
    }

    /// Where the voice runs: the engine opens its session on the execution
    /// target this same environment names, so the report is what was built.
    async fn device_info(&self) -> Option<DeviceInfo> {
        let (runs_on, detail) = voice_pipes_onnx::ExecutionTarget::from_env().device_report();
        Some(DeviceInfo::config(runs_on, detail))
    }

    /// Synthesize a tiny throwaway phrase to load the voice model into
    /// memory and amortise the ~1.3 s cold-start tax off the first user
    /// turn.  Result is discarded; only side effects (model warmed) matter.
    async fn warm_up(&self) -> Result<(), TtsError> {
        // A word, not punctuation: text with no phonemes skips the model.
        let _ = self
            .synthesize_inner("Hello.", std::time::Instant::now())
            .await?;
        Ok(())
    }

    #[instrument(skip_all, fields(provider = "piper", text_len = req.text.len()))]
    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let mut buffer = req.text;
        // A sentence with nothing to speak is skipped; a request where no
        // sentence had any is an error, as a whole-text call is.
        let mut spoke = false;
        while !buffer.is_empty() {
            if cancel.is_cancelled() {
                return Ok(());
            }
            // Re-use the LLM text's sentence-splitting logic to break larger
            // blocks into synthesized chunks.
            let (sentence, rest) = voice_pipes_sdk::sentence::split_first_sentence(
                buffer,
                self.text_chunking.min_soft_split_chars,
                self.text_chunking.min_soft_split_words,
                true,
            )
            .unwrap_or((buffer, ""));
            let sentence = sentence.trim();
            if !sentence.is_empty() {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let t_sentence = std::time::Instant::now();
                let frame = match self.synthesize_inner(sentence, t_sentence).await {
                    Ok(frame) if frame.samples.is_empty() => {
                        buffer = rest;
                        continue;
                    }
                    Ok(frame) => {
                        self.core.record_ok(t_sentence.elapsed());
                        frame
                    }
                    Err(e) => {
                        self.core.record_error(&e);
                        return Err(e);
                    }
                };
                spoke = true;

                // Break into fixed-size chunks (e.g. ~4096 samples == ~186ms @ 22kHz)
                // for smoother downstream streaming and cancellation checks.
                for chunk_samples in frame.samples.chunks(4096) {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    let chunk_frame = AudioFrame {
                        samples: chunk_samples.to_vec(),
                        sample_rate: frame.sample_rate,
                        channels: frame.channels,
                    };
                    if tx.send(chunk_frame).await.is_err() {
                        tracing::debug!("piper synthesize_stream downstream closed");
                        return Ok(());
                    }
                }
            }
            buffer = rest;
        }
        if !spoke {
            let e = no_audio();
            self.core.record_error(&e);
            return Err(e);
        }
        Ok(())
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(std::sync::Arc::clone(
            &self.core,
        ))))
    }
}

/// The error for text that produced no audio, which would otherwise pass
/// as a spoken reply the caller never hears.
fn no_audio() -> TtsError {
    TtsError::InferenceFailed("piper returned no audio".into())
}

/// The length in seconds of `samples` at `rate`, for the audit trail.
#[expect(
    clippy::cast_precision_loss,
    reason = "a sample count and a rate, far inside f32's exact range"
)]
fn seconds(samples: usize, rate: u32) -> f32 {
    samples as f32 / rate as f32
}

// ── Factory ──────────────────────────────────────────────────

/// The factory the binary registers for `[tts] provider = "piper"`.
pub fn factory() -> Box<dyn TtsFactory> {
    Box::new(PiperFactory)
}

/// Factory for the Piper VITS TTS provider.
#[derive(Debug)]
struct PiperFactory;

/// Piper's options (`pool_size` and `threads`); any other key is refused.
fn piper_options(cfg: &TtsConfig) -> Result<(usize, usize), Vec<ConfigError>> {
    let mut o = cfg.options();
    let pool_size = o.usize_in("pool_size", 1..=128, DEFAULT_POOL_SIZE);
    let default_threads = voice_pipes_onnx::intra_threads_from_env(DEFAULT_THREADS);
    let threads = o.usize_in("threads", 0..=256, default_threads);
    o.finish()?;
    Ok((pool_size, threads))
}

impl TtsFactory for PiperFactory {
    /// From the model's `.onnx.json`, which states the rate the voice was
    /// trained at.
    fn output_sample_rate(&self, cfg: &TtsConfig) -> Option<u32> {
        let model = cfg.model.as_deref()?;
        PiperModelConfig::load(&model.with_extension("onnx.json"))
            .ok()
            .map(|c| c.audio.sample_rate)
    }

    fn name(&self) -> &'static str {
        "piper"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<ConfigError> {
        let mut errs = vec![];
        if cfg.model.is_none() {
            errs.push(ConfigError::new(
                "tts.model",
                "model path required for piper",
            ));
        }
        if let Err(problems) = piper_options(cfg) {
            errs.extend(problems);
        }
        errs
    }

    fn build(
        &self,
        cfg: &TtsConfig,
        ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let model_path = cfg.model.as_ref().ok_or_else(|| {
            BuildError::failed("[tts] provider = \"piper\" requires a model path (set tts.model)")
        })?;
        let (pool_size, threads) = piper_options(cfg).map_err(voice_pipes_sdk::options::refused)?;

        let mut tts =
            PiperTts::with_pool_size(model_path, pool_size, threads, ctx.text_chunking.clone())?;
        tts.audit_tx.clone_from(&ctx.audit_tx);
        Ok(Box::new(tts))
    }

    fn provider_schema(&self, cfg: &TtsConfig) -> voice_pipes_sdk::schema::ProviderSchema {
        schema::schema(cfg)
    }
}
