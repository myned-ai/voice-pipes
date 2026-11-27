//! `TtsFactory` for native Rust/Candle Pocket-TTS.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use candle_core::Device;
use tokio::sync::Semaphore;
use tracing::info;
use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, TtsConfig, TtsFactory, TtsProvider};

use crate::config::{
    DEFAULT_EOS_THRESHOLD, DEFAULT_FADE_IN_MS, DEFAULT_LSD_STEPS, DEFAULT_MAX_CACHED_VOICES,
    DEFAULT_MAX_CHARS, DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_FRAMES, DEFAULT_SILENT_START_MS,
    DEFAULT_SILENT_START_RETRIES, DEFAULT_TEMPERATURE, PocketCandleConfig, PocketCandleOptions,
    SAMPLE_RATE,
};
use crate::model::PocketEngine;
use crate::provider::PocketCandleTts;
use crate::voices::VoiceCache;

/// Factory for `provider = "pocket-candle"`.
#[derive(Debug)]
pub(crate) struct PocketCandleTtsFactory;

fn select_device() -> Device {
    #[cfg(feature = "cuda")]
    {
        if let Ok(dev) = Device::new_cuda(0) {
            return dev;
        }
    }
    Device::Cpu
}

impl TtsFactory for PocketCandleTtsFactory {
    fn output_sample_rate(&self, _cfg: &TtsConfig) -> Option<u32> {
        Some(SAMPLE_RATE)
    }

    fn name(&self) -> &'static str {
        "pocket-candle"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        if let Some(v) = cfg.voice.as_deref()
            && v.trim().is_empty()
        {
            errors.push(ConfigError::new(
                "voice",
                "pocket-candle: voice must not be empty",
            ));
        }
        if let Err(problems) = PocketCandleOptions::read(cfg) {
            errors.extend(problems);
        }
        errors
    }

    fn build(
        &self,
        cfg: &TtsConfig,
        ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let mut config = PocketCandleConfig::from_tts_config(cfg)?;
        config.voice = voice_pipes_sdk::tts::adopt_voice(ctx.params, config.voice);

        let device = select_device();
        let engine = Arc::new(PocketEngine::load_from_dir(&config.model_dir, device)?);
        let voices = Arc::new(VoiceCache::new(config.max_cached_voices));
        let max_concurrent = config.max_concurrent;

        info!(
            model_dir = %config.model_dir.display(),
            voices_dir = %config.voices_dir.display(),
            voice = %*config.current_voice(),
            max_concurrent,
            lsd_steps = config.lsd_steps,
            pipeline_mimi = config.pipeline_mimi,
            "Pocket-TTS Candle provider initialized"
        );

        Ok(Box::new(PocketCandleTts {
            engine,
            voices,
            config,
            audit_tx: ctx.audit_tx.clone(),
            core: Arc::new(ComponentCore::new("tts:pocket-candle", ComponentKind::Tts)),
            concurrency: Arc::new(Semaphore::new(max_concurrent)),
            warmed: AtomicBool::new(false),
        }))
    }

    fn provider_schema(&self, cfg: &TtsConfig) -> ProviderSchema {
        let mut s = ProviderSchema::default();
        push_identity_params(&mut s, cfg);
        push_tuning_params(&mut s, cfg);
        s.passthrough_options(
            &cfg.options,
            &[
                "max_concurrent",
                "lsd_steps",
                "first_frame_lsd_steps",
                "temperature",
                "eos_threshold",
                "max_frames",
                "max_chars",
                "max_cached_voices",
                "silent_start_ms",
                "silent_start_peak",
                "silent_start_retries",
                "fade_in_ms",
                "pipeline_mimi",
                "seed",
            ],
            "pocket-candle-specific option.",
        );
        let _ = (
            DEFAULT_TEMPERATURE,
            DEFAULT_EOS_THRESHOLD,
            DEFAULT_MAX_CHARS,
            DEFAULT_MAX_CACHED_VOICES,
            DEFAULT_FADE_IN_MS,
        );
        s
    }
}

fn push_identity_params(s: &mut ProviderSchema, cfg: &TtsConfig) {
    s.push(
        ParamDescriptor::static_str(
            "provider",
            "Provider",
            "The TTS framework or service used for speech synthesis.",
        ),
        serde_json::json!(cfg.provider),
    );
    if let Some(dir) = cfg
        .options
        .get("model_dir")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| cfg.model.as_ref().map(|p| p.display().to_string()))
    {
        s.push(
            ParamDescriptor::static_str(
                "model_dir",
                "Model directory",
                "Local directory containing model.safetensors and tokenizer.json.",
            ),
            serde_json::json!(dir),
        );
    }
    if let Some(v) = &cfg.voice {
        s.push(
            ParamDescriptor::mutable_str(
                "voice",
                "Voice",
                "Voice identifier (.safetensors or .wav under voices_dir).",
            ),
            serde_json::json!(v),
        );
    }
}

fn push_tuning_params(s: &mut ProviderSchema, cfg: &TtsConfig) {
    s.push(
        ParamDescriptor::static_int_display(
            "max_concurrent",
            "Max concurrent syntheses",
            "Maximum simultaneous synthesis streams sharing the immutable Candle weights.",
        ),
        cfg.options
            .get("max_concurrent")
            .cloned()
            .unwrap_or(serde_json::json!(DEFAULT_MAX_CONCURRENT)),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "lsd_steps",
            "LSD flow steps",
            "Euler integration steps per 80 ms latent frame.",
        ),
        cfg.options
            .get("lsd_steps")
            .cloned()
            .unwrap_or(serde_json::json!(DEFAULT_LSD_STEPS)),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "max_frames",
            "Max frames",
            "Maximum 80 ms audio frames per utterance (DoS bound).",
        ),
        cfg.options
            .get("max_frames")
            .cloned()
            .unwrap_or(serde_json::json!(DEFAULT_MAX_FRAMES)),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "silent_start_ms",
            "Silent-start window (ms)",
            "Early silent-start detection window in milliseconds (0 disables).",
        ),
        cfg.options
            .get("silent_start_ms")
            .cloned()
            .unwrap_or(serde_json::json!(DEFAULT_SILENT_START_MS)),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "silent_start_retries",
            "Silent-start retries",
            "Extra attempts with a fresh seed when silent start trips.",
        ),
        cfg.options
            .get("silent_start_retries")
            .cloned()
            .unwrap_or(serde_json::json!(DEFAULT_SILENT_START_RETRIES)),
    );
}
