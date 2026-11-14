//! NeuTTS-Air TTS provider -- local GGUF Qwen2 + ONNX `NeuCodec` vocoder.
//!
//! NeuTTS-Air is an open-source (Apache 2.0) on-device speech language model
//! producing natural speech with instant zero-shot voice cloning capabilities.
//!
//! Output audio is natively 24 kHz, downsampled in-memory to Voice Pipes standard
//! 16 kHz mono PCM (`TARGET_SAMPLE_RATE = 16_000`).

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
mod resample;
mod schema;
mod wizard;

pub use wizard::wizard_choices;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument};

use engine::{NeuTtsEngine, TARGET_SAMPLE_RATE};
use voice_pipes_onnx::{EXECUTION_PROVIDER_VAR, ExecutionTarget, SessionPool};
use voice_pipes_sdk::component::{
    Component, ComponentCore, ComponentKind, CoreComponent, DeviceInfo,
};
use voice_pipes_sdk::{
    AudioFrame, BuildContext, BuildError, ConfigError, SynthesisRequest, TextChunkingConfig,
    TraceEvent, TtsConfig, TtsError, TtsFactory, TtsProvider,
};

/// Default pool size of vocoder engines.
const DEFAULT_POOL_SIZE: usize = 2;

/// Default intra-op threads per session.
const DEFAULT_THREADS: usize = 4;

/// Build the bounded session pool for `NeuCodec` vocoders.
fn build_vocoder_pool(
    vocoder_path: &Path,
    pool_size: usize,
    target: ExecutionTarget,
    threads: usize,
) -> Result<SessionPool<NeuTtsEngine>, TtsError> {
    let mut engines = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        engines.push(NeuTtsEngine::load_vocoder(vocoder_path, target, threads)?);
    }
    info!(pool_size, threads, "NeuTTS vocoder session pool created");
    SessionPool::new(engines, "NeuTTS vocoder engines")
        .map_err(|e| TtsError::ConfigError(e.to_string()))
}

/// NeuTTS-Air TTS provider.
pub(crate) struct NeuTtsAir {
    pool: Arc<SessionPool<NeuTtsEngine>>,
    model_path: PathBuf,
    vocoder_path: PathBuf,
    default_voice: String,
    #[expect(dead_code, reason = "staged for voice cloning reference audio")]
    reference_audio: Option<PathBuf>,
    #[expect(dead_code, reason = "staged for voice cloning reference transcript")]
    reference_text: Option<String>,
    #[expect(dead_code, reason = "staged for punctuation chunking")]
    text_chunking: TextChunkingConfig,
    audit_tx: Option<tokio::sync::mpsc::Sender<TraceEvent>>,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for NeuTtsAir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuTtsAir")
            .field("model_path", &self.model_path)
            .field("vocoder_path", &self.vocoder_path)
            .field("default_voice", &self.default_voice)
            .finish_non_exhaustive()
    }
}

impl NeuTtsAir {
    #[expect(clippy::too_many_arguments, reason = "internal constructor holding all validated fields")]
    pub(crate) fn load(
        model_path: &Path,
        vocoder_path: &Path,
        default_voice: &str,
        reference_audio: Option<PathBuf>,
        reference_text: Option<String>,
        pool_size: usize,
        threads: usize,
        text_chunking: TextChunkingConfig,
        target: ExecutionTarget,
    ) -> Result<Self, TtsError> {
        let core = Arc::new(ComponentCore::new("tts:neutts", ComponentKind::Tts));
        let pool = Arc::new(build_vocoder_pool(
            vocoder_path,
            pool_size,
            target,
            threads,
        )?);

        Ok(Self {
            pool,
            model_path: model_path.to_path_buf(),
            vocoder_path: vocoder_path.to_path_buf(),
            default_voice: default_voice.to_string(),
            reference_audio,
            reference_text,
            text_chunking,
            audit_tx: None,
            core,
        })
    }
}

#[async_trait]
impl TtsProvider for NeuTtsAir {
    fn name(&self) -> &'static str {
        "neutts"
    }

    fn sample_rate(&self) -> u32 {
        TARGET_SAMPLE_RATE
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }

    async fn device_info(&self) -> Option<DeviceInfo> {
        Some(DeviceInfo::static_("cpu", "on-device CPU"))
    }

    #[instrument(skip_all, fields(text_len = text.len()))]
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let req = SynthesisRequest::new(text);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let cancel = tokio_util::sync::CancellationToken::new();

        self.synthesize_stream(req, tx, cancel).await?;

        let mut all_samples = Vec::new();
        while let Some(frame) = rx.recv().await {
            all_samples.extend_from_slice(&frame.samples);
        }

        if all_samples.is_empty() {
            return Err(TtsError::InferenceFailed(
                "NeuTTS synthesized no audio samples".into(),
            ));
        }

        Ok(AudioFrame {
            samples: all_samples,
            sample_rate: TARGET_SAMPLE_RATE,
            channels: 1,
        })
    }

    #[instrument(skip_all, fields(text_len = req.text.len()))]
    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        if cancel.is_cancelled() {
            return Ok(());
        }

        let start = std::time::Instant::now();
        let pool = Arc::clone(&self.pool);
        let text = req.text.to_string();

        // Check out a vocoder slot from the pool
        let mut slot = pool
            .checkout()
            .await
            .map_err(|e| TtsError::busy(format!("vocoder pool checkout failed: {e}")))?;

        // Phonemize via voice_pipes_sdk::phonemize runner
        let phonemes = voice_pipes_sdk::phonemize::run_espeak_with_language(&text, "en-us")?;

        if cancel.is_cancelled() {
            return Ok(());
        }

        // Placeholder token synthesis until full GGUF Qwen2 weights are invoked
        let tokens: Vec<i64> = phonemes.chars().map(|c| (c as i64) % 1024).collect();

        if !tokens.is_empty() {
            let samples = slot.decode_tokens(&tokens)?;
            if !samples.is_empty() && !cancel.is_cancelled() {
                let frame = AudioFrame {
                    samples,
                    sample_rate: TARGET_SAMPLE_RATE,
                    channels: 1,
                };
                let _ = tx.send(frame).await;
            }
        }

        self.core.record_ok(start.elapsed());
        Ok(())
    }
}

// ── Factory ──────────────────────────────────────────────────

/// Factory for the NeuTTS-Air provider.
#[derive(Debug)]
pub(crate) struct NeuTtsFactory;

impl TtsFactory for NeuTtsFactory {
    fn name(&self) -> &'static str {
        "neutts"
    }

    fn aliases(&self) -> &[&str] {
        &["neutts-air", "neuphonic"]
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        if cfg.model.as_deref().is_none_or(|p| p.as_os_str().is_empty()) {
            errors.push(ConfigError::new(
                "model",
                "model path required for NeuTTS-Air (path to GGUF file)",
            ));
        }
        if !cfg.options.contains_key("vocoder") {
            errors.push(ConfigError::new(
                "vocoder",
                "vocoder path required for NeuTTS-Air (path to neucodec_decoder.onnx)",
            ));
        }
        errors
    }

    fn build(
        &self,
        cfg: &TtsConfig,
        ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, BuildError> {
        let model_path = cfg
            .model
            .as_deref()
            .ok_or_else(|| BuildError::Config(vec![ConfigError::new("model", "model path required for NeuTTS")]))?;

        let options = NeuTtsOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let vocoder_path = Path::new(&options.vocoder);
        let default_voice = cfg.voice.as_deref().unwrap_or("default");

        let mut tts = NeuTtsAir::load(
            model_path,
            vocoder_path,
            default_voice,
            options.reference_audio,
            options.reference_text,
            options.pool_size,
            options.threads,
            ctx.text_chunking.clone(),
            options.target,
        )
        .map_err(|e| BuildError::failed(e.to_string()))?;

        tts.audit_tx.clone_from(&ctx.audit_tx);
        Ok(Box::new(tts))
    }

    fn provider_schema(&self, cfg: &TtsConfig) -> voice_pipes_sdk::ProviderSchema {
        schema::schema(cfg)
    }

    fn output_sample_rate(&self, _cfg: &TtsConfig) -> Option<u32> {
        Some(TARGET_SAMPLE_RATE)
    }
}

#[derive(Debug)]
struct NeuTtsOptions {
    vocoder: String,
    reference_audio: Option<PathBuf>,
    reference_text: Option<String>,
    pool_size: usize,
    threads: usize,
    target: ExecutionTarget,
}

impl NeuTtsOptions {
    fn read(cfg: &TtsConfig) -> Result<Self, Vec<ConfigError>> {
        let mut o = cfg.options();
        let vocoder = o.string("vocoder");
        if vocoder.is_none() {
            o.error("vocoder", "vocoder path required for NeuTTS-Air");
        }

        let ref_audio = o.string("reference_audio").map(PathBuf::from);
        let ref_text = o.string("reference_text");
        let pool_size = o.usize_in("pool_size", 1..=64, DEFAULT_POOL_SIZE);
        let default_threads = voice_pipes_onnx::intra_threads_from_env(DEFAULT_THREADS);
        let threads = o.usize_in("threads", 0..=128, default_threads);

        let device_option = o
            .value("execution_provider")
            .is_some()
            .then(|| o.one_of("execution_provider", &["cpu", "cuda", "CPU", "CUDA"], "cpu"));
        let env = std::env::var(EXECUTION_PROVIDER_VAR).ok();

        o.finish()?;

        let target = if device_option.as_deref() == Some("cuda")
            || env.as_deref() == Some("cuda")
        {
            ExecutionTarget::Cuda
        } else {
            ExecutionTarget::Cpu
        };

        Ok(Self {
            vocoder: vocoder.unwrap_or_default(),
            reference_audio: ref_audio,
            reference_text: ref_text,
            pool_size,
            threads,
            target,
        })
    }
}

/// Factory registration entry point.
pub fn factory() -> Box<dyn TtsFactory> {
    Box::new(NeuTtsFactory)
}

#[cfg(test)]
#[path = "tests/neutts.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
