//! Bounded LRU cache of immutable [`VoiceState`] prefixes supporting both
//! precomputed `.safetensors` voice files and on-the-fly WAV cloning via Mimi encoder.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use candle_core::Tensor;
use parking_lot::RwLock;
use voice_pipes_sdk::TtsError;

use crate::config::{
    MAX_VOICE_SAFETENSORS_BYTES, MAX_VOICE_WAV_BYTES, SAMPLE_RATE, resolve_safe_voice_path,
};
use crate::model::PocketEngine;
use crate::model::kv_cache::{LayerKvPrefix, VoiceKvPrefix};
use crate::model::state::VoiceState;

/// Thread-safe bounded LRU cache of shared `Arc<VoiceState>` prefixes.
pub struct VoiceCache {
    max_entries: usize,
    inner: RwLock<CacheInner>,
}

struct CacheInner {
    map: HashMap<String, Arc<VoiceState>>,
    order: VecDeque<String>,
}

impl std::fmt::Debug for VoiceCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceCache")
            .field("max_entries", &self.max_entries)
            .finish_non_exhaustive()
    }
}

impl VoiceCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            inner: RwLock::new(CacheInner {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    /// Insert a pre-built voice state (used by warm-up and tests).
    pub fn insert(&self, state: Arc<VoiceState>) {
        let key = state.name.clone();
        let mut inner = self.inner.write();
        inner.order.retain(|k| k != &key);
        while inner.map.len() >= self.max_entries {
            if let Some(oldest) = inner.order.pop_front() {
                inner.map.remove(&oldest);
            } else {
                break;
            }
        }
        inner.order.push_back(key.clone());
        inner.map.insert(key, state);
    }

    pub fn get(&self, name: &str) -> Option<Arc<VoiceState>> {
        self.inner.read().map.get(name).cloned()
    }

    /// Resolve `voice_name` from cache or load safely from `voices_dir` (`.safetensors` or `.wav`).
    pub fn get_or_load(
        &self,
        engine: &PocketEngine,
        voices_dir: &Path,
        voice_name: &str,
    ) -> Result<Arc<VoiceState>, TtsError> {
        if let Some(hit) = self.get(voice_name) {
            return Ok(hit);
        }
        let path = resolve_safe_voice_path(voices_dir, voice_name)?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let loaded = match ext.as_str() {
            "safetensors" => load_safetensors_voice(engine, &path, voice_name)?,
            "wav" => load_wav_voice(engine, &path, voice_name)?,
            _ => {
                return Err(TtsError::ConfigError(format!(
                    "pocket-candle: unsupported voice file extension for {voice_name:?} (expected .safetensors or .wav)"
                )));
            }
        };
        let arc = Arc::new(loaded);
        self.insert(Arc::clone(&arc));
        Ok(arc)
    }
}

/// Load a precomputed `.safetensors` voice state (either raw Mimi embeddings or
/// pre-computed per-layer KV tensors) with strict size and shape validation.
fn load_safetensors_voice(
    engine: &PocketEngine,
    path: &Path,
    name: &str,
) -> Result<VoiceState, TtsError> {
    let meta = std::fs::metadata(path).map_err(|e| {
        TtsError::ConfigError(format!(
            "pocket-candle: cannot stat voice {}: {e}",
            path.display()
        ))
    })?;
    if meta.len() > MAX_VOICE_SAFETENSORS_BYTES {
        return Err(TtsError::ConfigError(format!(
            "pocket-candle: voice file {} exceeds {} byte limit",
            path.display(),
            MAX_VOICE_SAFETENSORS_BYTES
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| {
        TtsError::ConfigError(format!(
            "pocket-candle: cannot read voice {}: {e}",
            path.display()
        ))
    })?;
    let tensors = candle_core::safetensors::load_buffer(&bytes, &engine.device).map_err(|e| {
        TtsError::ConfigError(format!(
            "pocket-candle: invalid voice safetensors {}: {e}",
            path.display()
        ))
    })?;

    // Format A: Kyutai native precomputed voice cache (`transformer.layers.{i}.self_attn/cache`
    // of shape `[2, 1, T, H, D]`, axis 0: 0=K, 1=V) -> zero-copy Arc<VoiceKvPrefix>.
    if tensors.contains_key("transformer.layers.0.self_attn/cache") {
        return load_kyutai_kv_voice(engine, &tensors, name);
    }

    // Format B: generic per-layer KV cache (`layer_{i}.k`, `layer_{i}.v`).
    if tensors.contains_key("layer_0.k") {
        return load_generic_kv_voice(engine, &tensors, name);
    }

    // Format C: Kyutai audio prompt embeddings (`audio_prompt` of shape `[1, T, latent_dim]`).
    if let Some(prompt) = tensors.get("audio_prompt") {
        let dims = prompt.dims();
        if dims.len() != 3 || dims.get(2).copied() != Some(engine.arch.latent_dim) {
            return Err(TtsError::ConfigError(format!(
                "pocket-candle: voice {name:?} audio_prompt shape {dims:?} does not match latent_dim={}",
                engine.arch.latent_dim
            )));
        }
        let kv_prefix = engine
            .flow_lm
            .build_voice_prefix(prompt)
            .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle voice prefill: {e}")))?;
        return Ok(VoiceState {
            name: name.to_string(),
            fingerprint: engine.arch.fingerprint(),
            kv_prefix: Arc::new(kv_prefix),
        });
    }

    Err(TtsError::ConfigError(format!(
        "pocket-candle: voice {name:?} safetensors contains neither Kyutai KV cache, layer_0.k/v, nor audio_prompt"
    )))
}

fn load_kyutai_kv_voice(
    engine: &PocketEngine,
    tensors: &HashMap<String, Tensor>,
    name: &str,
) -> Result<VoiceState, TtsError> {
    let mut layers = Vec::with_capacity(engine.arch.num_layers);
    let expected_head_dim = engine.arch.d_model / engine.arch.num_heads.max(1);
    let mut prefix_len = 0usize;
    for i in 0..engine.arch.num_layers {
        let key = format!("transformer.layers.{i}.self_attn/cache");
        let Some(cache) = tensors.get(&key) else {
            return Err(TtsError::ConfigError(format!(
                "pocket-candle: voice {name:?} missing {key} (model expects {} layers)",
                engine.arch.num_layers
            )));
        };
        let dims = cache.dims();
        let ok = dims.len() == 5
            && dims.first().copied() == Some(2)
            && dims.get(1).copied() == Some(1)
            && dims.get(3).copied() == Some(engine.arch.num_heads)
            && dims.get(4).copied() == Some(expected_head_dim);
        if !ok {
            return Err(TtsError::ConfigError(format!(
                "pocket-candle: voice {name:?} {key} shape {dims:?} incompatible with model (heads={}, head_dim={expected_head_dim})",
                engine.arch.num_heads
            )));
        }
        let k = cache
            .get(0)
            .and_then(|t| t.transpose(1, 2))
            .and_then(|t| t.contiguous())
            .map_err(|e| TtsError::ConfigError(e.to_string()))?;
        let v = cache
            .get(1)
            .and_then(|t| t.transpose(1, 2))
            .and_then(|t| t.contiguous())
            .map_err(|e| TtsError::ConfigError(e.to_string()))?;
        prefix_len = k.dim(2).map_err(|e| TtsError::ConfigError(e.to_string()))?;
        layers.push(LayerKvPrefix { k, v });
    }
    Ok(VoiceState {
        name: name.to_string(),
        fingerprint: engine.arch.fingerprint(),
        kv_prefix: Arc::new(VoiceKvPrefix { layers, prefix_len }),
    })
}

fn load_generic_kv_voice(
    engine: &PocketEngine,
    tensors: &HashMap<String, Tensor>,
    name: &str,
) -> Result<VoiceState, TtsError> {
    let mut layers = Vec::with_capacity(engine.arch.num_layers);
    let expected_head_dim = engine.arch.d_model / engine.arch.num_heads.max(1);
    let mut prefix_len = 0usize;
    for i in 0..engine.arch.num_layers {
        let (Some(k), Some(v)) = (
            tensors.get(&format!("layer_{i}.k")),
            tensors.get(&format!("layer_{i}.v")),
        ) else {
            return Err(TtsError::ConfigError(format!(
                "pocket-candle: voice {name:?} missing layer_{i}.k/v (model expects {} layers)",
                engine.arch.num_layers
            )));
        };
        validate_kv_tensor(k, engine.arch.num_heads, expected_head_dim, name)?;
        validate_kv_tensor(v, engine.arch.num_heads, expected_head_dim, name)?;
        prefix_len = k.dim(2).map_err(|e| TtsError::ConfigError(e.to_string()))?;
        layers.push(LayerKvPrefix {
            k: k.clone(),
            v: v.clone(),
        });
    }
    Ok(VoiceState {
        name: name.to_string(),
        fingerprint: engine.arch.fingerprint(),
        kv_prefix: Arc::new(VoiceKvPrefix { layers, prefix_len }),
    })
}

fn validate_kv_tensor(
    t: &Tensor,
    expected_heads: usize,
    expected_head_dim: usize,
    voice_name: &str,
) -> Result<(), TtsError> {
    let dims = t.dims();
    let ok = dims.len() == 4
        && dims.first().copied() == Some(1)
        && dims.get(1).copied() == Some(expected_heads)
        && dims.get(3).copied() == Some(expected_head_dim);
    if !ok {
        return Err(TtsError::ConfigError(format!(
            "pocket-candle: voice {voice_name:?} KV tensor shape {dims:?} incompatible with model (heads={expected_heads}, head_dim={expected_head_dim})"
        )));
    }
    Ok(())
}

/// Load a reference WAV file (bounded by `MAX_VOICE_WAV_BYTES`), convert to mono 24 kHz `f32`,
/// and encode on the fly through Mimi encoder + `FlowLM` prefix prefill.
fn load_wav_voice(engine: &PocketEngine, path: &Path, name: &str) -> Result<VoiceState, TtsError> {
    let meta = std::fs::metadata(path).map_err(|e| {
        TtsError::ConfigError(format!(
            "pocket-candle: cannot stat WAV {}: {e}",
            path.display()
        ))
    })?;
    if meta.len() > MAX_VOICE_WAV_BYTES {
        return Err(TtsError::ConfigError(format!(
            "pocket-candle: WAV voice {} exceeds {} byte limit",
            path.display(),
            MAX_VOICE_WAV_BYTES
        )));
    }

    let reader = hound::WavReader::open(path).map_err(|e| {
        TtsError::ConfigError(format!(
            "pocket-candle: invalid WAV {}: {e}",
            path.display()
        ))
    })?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let raw_f32: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max_val = (1_i64 << spec.bits_per_sample.saturating_sub(1).min(30)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| (s as f32) / max_val)
                .collect()
        }
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(Result::ok)
            .collect(),
    };

    // Downmix to mono.
    let mut mono = Vec::with_capacity(raw_f32.len() / channels);
    for frame in raw_f32.chunks(channels) {
        let sum: f32 = frame.iter().copied().sum();
        mono.push(sum / (frame.len() as f32));
    }

    // Resample to 24 kHz if needed.
    let samples_24k = if spec.sample_rate == SAMPLE_RATE {
        mono
    } else {
        linear_resample(&mono, spec.sample_rate, SAMPLE_RATE)
    };
    engine.build_voice_state_from_audio(name.to_string(), &samples_24k)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "audio lengths are bounded by MAX_VOICE_WAV_BYTES and frac is in [0.0, 1.0)"
)]
fn linear_resample(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if input.is_empty() || src_rate == 0 || dst_rate == 0 {
        return Vec::new();
    }
    let ratio = f64::from(src_rate) / f64::from(dst_rate);
    let out_len = ((input.len() as f64) / ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = (i as f64) * ratio;
        let idx = (pos.floor() as usize).min(input.len().saturating_sub(1));
        let next = (idx + 1).min(input.len().saturating_sub(1));
        let frac = (pos - (idx as f64)) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(next).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}
