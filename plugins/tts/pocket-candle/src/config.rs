//! Configuration, architecture metadata, and security bounds for Pocket-TTS Candle.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use voice_pipes_sdk::{ConfigError, TtsConfig};

/// Native Mimi output sample rate (24 kHz mono).
pub const SAMPLE_RATE: u32 = 24_000;

/// Native Mimi frame size: `24_000` Hz / 12.5 Hz = 1920 samples (80 ms).
pub(crate) const FRAME_SAMPLES: usize = 1920;

/// Default maximum concurrent synthesis requests per worker/server.
pub(crate) const DEFAULT_MAX_CONCURRENT: usize = 4;

/// Default LSD flow-matching steps per latent frame.
pub(crate) const DEFAULT_LSD_STEPS: usize = 1;

/// Default sampling temperature for flow-matching noise.
pub(crate) const DEFAULT_TEMPERATURE: f32 = 0.7;

/// Default EOS logit threshold (stop when EOS logit exceeds this value).
pub(crate) const DEFAULT_EOS_THRESHOLD: f32 = -4.0;

/// Default maximum generated audio frames per utterance (375 * 80ms = 30s).
pub(crate) const DEFAULT_MAX_FRAMES: usize = 375;

/// Default maximum input characters per synthesis request (`DoS` guard).
pub(crate) const DEFAULT_MAX_CHARS: usize = 2_000;

/// Default maximum cached voice states in the bounded LRU cache.
pub(crate) const DEFAULT_MAX_CACHED_VOICES: usize = 32;

/// Hard cap on a voice `.safetensors` file size (32 MiB).
pub(crate) const MAX_VOICE_SAFETENSORS_BYTES: u64 = 32 * 1024 * 1024;

/// Hard cap on a reference WAV voice-cloning file size (10 MiB).
pub(crate) const MAX_VOICE_WAV_BYTES: u64 = 10 * 1024 * 1024;

/// Default silent-start detection window (ms).
pub(crate) const DEFAULT_SILENT_START_MS: u64 = 1_200;

/// Default int16 peak below which the probe window counts as silent.
pub(crate) const DEFAULT_SILENT_START_PEAK: i16 = 500;

/// Default extra retry attempts after a silent start.
pub(crate) const DEFAULT_SILENT_START_RETRIES: u32 = 2;

/// Default fade-in ramp on frame 0 (ms) to prevent cold-start clicks.
pub(crate) const DEFAULT_FADE_IN_MS: u64 = 4;

/// Default voice name when none is configured.
pub(crate) const DEFAULT_VOICE: &str = "alba";

/// Structural hyperparameters of a Pocket-TTS checkpoint (`FlowLM` + `Mimi`).
///
/// Defaults match Kyutai's official 100M `kyutai/pocket-tts` checkpoint;
/// dimensions can also be overridden from a `config.json` in `model_dir` or
/// inferred directly from safetensors tensor shapes at load time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PocketArchConfig {
    /// `FlowLM` transformer hidden dimension (official 100M: 1024).
    pub d_model: usize,
    /// Number of `FlowLM` causal transformer layers (official 100M: 6).
    pub num_layers: usize,
    /// Number of attention heads in `FlowLM` (official 100M: 16).
    pub num_heads: usize,
    /// Feed-forward hidden dimension in `FlowLM` (official 100M: 4096).
    pub ffn_dim: usize,
    /// Continuous audio latent dimension from Mimi (official: 32).
    pub latent_dim: usize,
    /// Hidden width of the LSD `SimpleMLPAdaLN` flow head (official: 512).
    pub flow_dim: usize,
    /// Residual block depth of the LSD flow head (official: 6).
    pub flow_depth: usize,
    /// `RoPE` theta base frequency.
    pub rope_theta: u32,
    /// Mimi latent dimension (usually 512 internal, projected from/to `latent_dim`).
    pub mimi_dim: usize,
    /// Mimi `SEANet` upsampling/downsampling ratios (official: `[6, 5, 4]`, × 16 = 1920).
    pub mimi_ratios: Vec<usize>,
    /// Mimi depthwise upsample/downsample factor between 12.5 Hz and 200 Hz (official: 16).
    pub mimi_upsample_ratio: usize,
    /// Mimi `SEANet` base channel width (official: 64).
    pub mimi_base_channels: usize,
    /// Mimi local transformer layers (official: 2).
    pub mimi_transformer_layers: usize,
    /// Mimi local transformer attention heads (official: 8).
    pub mimi_transformer_heads: usize,
    /// Mimi local attention context window in frames (official: 250 = 20s, or 25 = 2s).
    pub mimi_context_frames: usize,
    /// `SentencePiece` vocabulary size (official: 4001 including pad/eos).
    pub vocab_size: usize,
}

impl Default for PocketArchConfig {
    fn default() -> Self {
        Self {
            d_model: 1024,
            num_layers: 6,
            num_heads: 16,
            ffn_dim: 4096,
            latent_dim: 32,
            flow_dim: 512,
            flow_depth: 6,
            rope_theta: 10_000,
            mimi_dim: 512,
            mimi_ratios: vec![6, 5, 4],
            mimi_upsample_ratio: 16,
            mimi_base_channels: 64,
            mimi_transformer_layers: 2,
            mimi_transformer_heads: 8,
            mimi_context_frames: 250,
            vocab_size: 4001,
        }
    }
}

impl PocketArchConfig {
    /// Compact architectural fingerprint used to validate cached `.safetensors` voice states.
    pub fn fingerprint(&self) -> VoiceFingerprint {
        VoiceFingerprint {
            d_model: self.d_model,
            num_layers: self.num_layers,
            num_heads: self.num_heads,
            latent_dim: self.latent_dim,
        }
    }

    /// Tiny architecture used by fast in-memory unit & conformance tests (no weights on disk).
    #[cfg(test)]
    pub(crate) fn tiny_for_tests() -> Self {
        Self {
            d_model: 32,
            num_layers: 2,
            num_heads: 4,
            ffn_dim: 64,
            latent_dim: 8,
            flow_dim: 32,
            flow_depth: 2,
            rope_theta: 10_000,
            mimi_dim: 32,
            mimi_ratios: vec![6, 5, 4],
            mimi_upsample_ratio: 16,
            mimi_base_channels: 4,
            mimi_transformer_layers: 1,
            mimi_transformer_heads: 4,
            mimi_context_frames: 32,
            vocab_size: 128,
        }
    }
}

/// Structural compatibility key for precomputed voice states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceFingerprint {
    pub d_model: usize,
    pub num_layers: usize,
    pub num_heads: usize,
    pub latent_dim: usize,
}

/// Silent-start guard configuration. `None` disables the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SilentStartGuard {
    /// Number of 24 kHz samples inspected before triggering a silent-start retry.
    pub window_samples: usize,
    /// Peak int16 amplitude required within `window_samples` to count as speech.
    pub peak_threshold: i16,
    /// Extra synthesis attempts after the first silent attempt.
    pub retries: u32,
}

/// Parsed runtime configuration for [`crate::provider::PocketCandleTts`].
#[derive(Debug, Clone)]
pub struct PocketCandleConfig {
    pub model_dir: PathBuf,
    pub voices_dir: PathBuf,
    pub voice: voice_pipes_sdk::tts::VoiceCell,
    pub max_concurrent: usize,
    pub lsd_steps: usize,
    pub first_frame_lsd_steps: usize,
    pub temperature: f32,
    pub eos_threshold: f32,
    pub max_frames: usize,
    pub max_chars: usize,
    pub max_cached_voices: usize,
    pub silent_start: Option<SilentStartGuard>,
    pub fade_in_samples: usize,
    pub pipeline_mimi: bool,
    pub seed: Option<u64>,
}

/// Raw options parsed from `[tts]` / `[tts.options]`.
pub(crate) struct PocketCandleOptions {
    pub model_dir: Option<PathBuf>,
    pub voices_dir: Option<PathBuf>,
    pub max_concurrent: usize,
    pub lsd_steps: usize,
    pub first_frame_lsd_steps: usize,
    pub temperature: f32,
    pub eos_threshold: f32,
    pub max_frames: usize,
    pub max_chars: usize,
    pub max_cached_voices: usize,
    pub silent_start_ms: u64,
    pub silent_start_peak: i16,
    pub silent_start_retries: u32,
    pub fade_in_ms: u64,
    pub pipeline_mimi: bool,
    pub seed: Option<u64>,
}

impl PocketCandleOptions {
    pub(crate) fn read(cfg: &TtsConfig) -> Result<Self, Vec<ConfigError>> {
        let mut o = cfg.options();
        let model_dir = o.string("model_dir").map(PathBuf::from);
        let voices_dir = o.string("voices_dir").map(PathBuf::from);
        let max_concurrent = o.usize_in("max_concurrent", 1..=64, DEFAULT_MAX_CONCURRENT);
        let lsd_steps = o.usize_in("lsd_steps", 1..=16, DEFAULT_LSD_STEPS);
        let first_frame_lsd_steps = o.usize_in("first_frame_lsd_steps", 1..=16, lsd_steps);
        let temperature = voice_pipes_sdk::options::as_f32(o.f64_in(
            "temperature",
            0.0..=2.0,
            f64::from(DEFAULT_TEMPERATURE),
        ));
        let eos_threshold = voice_pipes_sdk::options::as_f32(o.f64_in(
            "eos_threshold",
            -20.0..=10.0,
            f64::from(DEFAULT_EOS_THRESHOLD),
        ));
        let max_frames = o.usize_in("max_frames", 8..=3_750, DEFAULT_MAX_FRAMES);
        let max_chars = o.usize_in("max_chars", 16..=20_000, DEFAULT_MAX_CHARS);
        let max_cached_voices = o.usize_in("max_cached_voices", 1..=256, DEFAULT_MAX_CACHED_VOICES);
        let silent_start_ms = o.u64_in("silent_start_ms", 0..=10_000, DEFAULT_SILENT_START_MS);
        let silent_start_peak = o
            .optional_u64_in("silent_start_peak", 1..=i16::MAX.unsigned_abs().into())
            .and_then(|v| i16::try_from(v).ok())
            .unwrap_or(DEFAULT_SILENT_START_PEAK);
        let silent_start_retries =
            o.u32_in("silent_start_retries", 0..=10, DEFAULT_SILENT_START_RETRIES);
        let fade_in_ms = o.u64_in("fade_in_ms", 0..=50, DEFAULT_FADE_IN_MS);
        let pipeline_mimi = o.bool("pipeline_mimi", true);
        let seed = o.optional_u64_in("seed", 0..=u64::MAX);
        o.finish()?;
        Ok(Self {
            model_dir,
            voices_dir,
            max_concurrent,
            lsd_steps,
            first_frame_lsd_steps,
            temperature,
            eos_threshold,
            max_frames,
            max_chars,
            max_cached_voices,
            silent_start_ms,
            silent_start_peak,
            silent_start_retries,
            fade_in_ms,
            pipeline_mimi,
            seed,
        })
    }
}

impl PocketCandleConfig {
    pub fn from_tts_config(cfg: &TtsConfig) -> Result<Self, anyhow::Error> {
        let opts = PocketCandleOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let model_dir = opts
            .model_dir
            .or_else(|| cfg.model.clone())
            .unwrap_or_else(|| PathBuf::from("models/pocket-tts"));
        let voices_dir = opts.voices_dir.unwrap_or_else(|| model_dir.join("voices"));
        let voice = cfg
            .voice
            .clone()
            .unwrap_or_else(|| DEFAULT_VOICE.to_string());

        let silent_start = (opts.silent_start_ms > 0).then(|| SilentStartGuard {
            window_samples: usize::try_from(u64::from(SAMPLE_RATE) * opts.silent_start_ms / 1000)
                .unwrap_or(usize::MAX),
            peak_threshold: opts.silent_start_peak,
            retries: opts.silent_start_retries,
        });
        let fade_in_samples =
            usize::try_from(u64::from(SAMPLE_RATE) * opts.fade_in_ms / 1000).unwrap_or(0);

        Ok(Self {
            model_dir,
            voices_dir,
            voice: voice_pipes_sdk::tts::voice_cell(voice),
            max_concurrent: opts.max_concurrent,
            lsd_steps: opts.lsd_steps,
            first_frame_lsd_steps: opts.first_frame_lsd_steps,
            temperature: opts.temperature,
            eos_threshold: opts.eos_threshold,
            max_frames: opts.max_frames,
            max_chars: opts.max_chars,
            max_cached_voices: opts.max_cached_voices,
            silent_start,
            fade_in_samples,
            pipeline_mimi: opts.pipeline_mimi,
            seed: opts.seed,
        })
    }

    pub(crate) fn current_voice(&self) -> std::sync::Arc<String> {
        self.voice.load_full()
    }
}

/// Validate that `name` is a safe single-segment voice identifier or relative path
/// inside `root`, rejecting URL schemes, null bytes, and path traversal (`..`).
pub(crate) fn resolve_safe_voice_path(
    root: &Path,
    voice_name: &str,
) -> Result<PathBuf, voice_pipes_sdk::TtsError> {
    let trimmed = voice_name.trim();
    if trimmed.is_empty()
        || trimmed.contains("://")
        || trimmed.contains('\0')
        || trimmed.contains("..")
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
    {
        return Err(voice_pipes_sdk::TtsError::ConfigError(format!(
            "pocket-candle: invalid or disallowed voice identifier {trimmed:?}"
        )));
    }

    let has_known_ext = Path::new(trimmed).extension().is_some_and(|ext| {
        ext.eq_ignore_ascii_case("safetensors") || ext.eq_ignore_ascii_case("wav")
    });
    let candidate_safetensors = if has_known_ext {
        root.join(trimmed)
    } else {
        let st = root.join(format!("{trimmed}.safetensors"));
        if st.exists() {
            st
        } else {
            root.join(format!("{trimmed}.wav"))
        }
    };

    let canonical_root = root.canonicalize().map_err(|e| {
        voice_pipes_sdk::TtsError::ConfigError(format!(
            "pocket-candle: voices_dir {} is not readable: {e}",
            root.display()
        ))
    })?;
    let canonical_target = candidate_safetensors.canonicalize().map_err(|e| {
        voice_pipes_sdk::TtsError::ConfigError(format!(
            "pocket-candle: voice {trimmed:?} not found under {}: {e}",
            root.display()
        ))
    })?;

    if !canonical_target.starts_with(&canonical_root) {
        return Err(voice_pipes_sdk::TtsError::ConfigError(format!(
            "pocket-candle: voice path escapes voices_dir for {trimmed:?}"
        )));
    }
    Ok(canonical_target)
}
