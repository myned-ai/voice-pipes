//! Unified `PocketEngine`: shared read-only weights (`FlowLM` + `Mimi`) and
//! pipelined concurrent streaming synthesis.

pub mod flow_lm;
pub mod kv_cache;
pub mod mimi;
pub mod state;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use voice_pipes_sdk::{AudioFrame, TtsError};

use crate::config::{PocketArchConfig, PocketCandleConfig, SAMPLE_RATE, SilentStartGuard};
use crate::model::flow_lm::FlowLmModel;
use crate::model::kv_cache::{DeltaKvCache, VoiceKvPrefix};
use crate::model::mimi::MimiCodec;
use crate::model::state::{RequestRng, VoiceState};
use crate::tokenizer::PocketTokenizer;

/// Shared, immutable Pocket-TTS engine (`Arc<PocketEngine>`).
///
/// All weights are loaded once and shared read-only across concurrent sessions;
/// each synthesis call allocates only a small per-request delta KV cache and
/// streaming Mimi conv state.
pub struct PocketEngine {
    pub(crate) arch: PocketArchConfig,
    pub(crate) flow_lm: FlowLmModel,
    pub(crate) mimi: MimiCodec,
    pub(crate) tokenizer: PocketTokenizer,
    pub(crate) device: Device,
    request_seq: AtomicU64,
}

impl std::fmt::Debug for PocketEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PocketEngine")
            .field("arch", &self.arch)
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

/// Outcome of one synthesis attempt (used by the silent-start guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptOutcome {
    Done { total_samples: usize, frames: usize },
    Cancelled { total_samples: usize, frames: usize },
    SilentStart { peak: i16 },
}

/// Peak tracker over the initial `window_samples` of decoded PCM.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SilentStartProbe {
    guard: SilentStartGuard,
    seen: usize,
    peak: i16,
    decided: bool,
}

impl SilentStartProbe {
    pub(crate) fn new(guard: SilentStartGuard) -> Self {
        Self {
            guard,
            seen: 0,
            peak: 0,
            decided: false,
        }
    }

    pub(crate) fn observe(&mut self, samples: &[i16]) {
        if self.decided || self.seen >= self.guard.window_samples {
            return;
        }
        let take = samples.len().min(self.guard.window_samples - self.seen);
        for &s in samples.iter().take(take) {
            self.peak = self.peak.max(s.saturating_abs());
        }
        self.seen += take;
        if self.peak >= self.guard.peak_threshold {
            self.decided = true;
        }
    }

    pub(crate) fn is_silent(&self) -> Option<i16> {
        (!self.decided
            && self.seen >= self.guard.window_samples
            && self.peak < self.guard.peak_threshold)
            .then_some(self.peak)
    }
}

impl PocketEngine {
    /// Load Pocket-TTS weights (`model.safetensors`, optional `config.json`, `tokenizer.json`)
    /// from `model_dir` using safe memory-mapped / buffered loading (`#![forbid(unsafe_code)]`).
    pub fn load_from_dir(model_dir: &Path, device: Device) -> Result<Self, TtsError> {
        let arch = load_arch_config(model_dir)?;
        let weights_path = find_weights_file(model_dir)?;
        let bytes = std::fs::read(&weights_path).map_err(|e| {
            TtsError::ModelLoadFailed(format!(
                "pocket-candle: failed to read {}: {e}",
                weights_path.display()
            ))
        })?;
        let vb =
            VarBuilder::from_buffered_safetensors(bytes, DType::F32, &device).map_err(|e| {
                TtsError::ModelLoadFailed(format!(
                    "pocket-candle: invalid safetensors in {}: {e}",
                    weights_path.display()
                ))
            })?;
        let flow_lm = FlowLmModel::load(arch.clone(), vb.pp("flow_lm"), &device)
            .map_err(|e| TtsError::ModelLoadFailed(format!("pocket-candle FlowLM load: {e}")))?;
        let mimi = MimiCodec::load(&arch, vb.pp("mimi"), &device)
            .map_err(|e| TtsError::ModelLoadFailed(format!("pocket-candle Mimi load: {e}")))?;
        let tokenizer = PocketTokenizer::from_model_dir(model_dir, arch.vocab_size)?;

        Ok(Self {
            arch,
            flow_lm,
            mimi,
            tokenizer,
            device,
            request_seq: AtomicU64::new(1),
        })
    }

    /// Construct a tiny deterministic in-memory engine for fast unit and conformance tests.
    #[cfg(test)]
    pub(crate) fn tiny_for_tests() -> Result<Self, TtsError> {
        let device = Device::Cpu;
        let arch = PocketArchConfig::tiny_for_tests();
        let vb = VarBuilder::zeros(DType::F32, &device);
        let flow_lm = FlowLmModel::load(arch.clone(), vb.pp("flow_lm"), &device)
            .map_err(|e| TtsError::ModelLoadFailed(format!("test FlowLM: {e}")))?;
        let mimi = MimiCodec::load(&arch, vb.pp("mimi"), &device)
            .map_err(|e| TtsError::ModelLoadFailed(format!("test Mimi: {e}")))?;
        let tokenizer = PocketTokenizer::for_tests(arch.vocab_size);
        Ok(Self {
            arch,
            flow_lm,
            mimi,
            tokenizer,
            device,
            request_seq: AtomicU64::new(1),
        })
    }

    /// Build a zero-copy [`VoiceState`] from a 24 kHz mono reference waveform via Mimi encoder.
    pub fn build_voice_state_from_audio(
        &self,
        name: String,
        samples_24k: &[f32],
    ) -> Result<VoiceState, TtsError> {
        let embeddings = self
            .mimi
            .encode_audio(samples_24k)
            .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle Mimi encode: {e}")))?;
        let kv_prefix = self
            .flow_lm
            .build_voice_prefix(&embeddings)
            .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle voice prefill: {e}")))?;
        Ok(VoiceState {
            name,
            fingerprint: self.arch.fingerprint(),
            kv_prefix: Arc::new(kv_prefix),
        })
    }

    /// Build an empty voice state (for testing or unconditioned generation).
    #[cfg(test)]
    pub fn empty_voice_state(&self, name: String) -> VoiceState {
        VoiceState {
            name,
            fingerprint: self.arch.fingerprint(),
            kv_prefix: Arc::new(VoiceKvPrefix::empty(self.arch.num_layers)),
        }
    }

    /// Blocking synthesis entry point (run inside `tokio::task::spawn_blocking`),
    /// with optional `FlowLM` <-> Mimi pipelining, per-step cancellation, and early
    /// silent-start retry.
    pub fn synthesize_blocking(
        self: &Arc<Self>,
        text: &str,
        voice: &VoiceState,
        cfg: &PocketCandleConfig,
        audio_tx: &mpsc::Sender<AudioFrame>,
        cancel: &CancellationToken,
    ) -> Result<(usize, u32), TtsError> {
        if voice.fingerprint != self.arch.fingerprint() {
            return Err(TtsError::ConfigError(format!(
                "pocket-candle: voice {:?} fingerprint {:?} does not match model {:?}",
                voice.name,
                voice.fingerprint,
                self.arch.fingerprint()
            )));
        }

        let token_ids = self.tokenizer.encode(text, cfg.max_chars)?;
        let req_seq = self.request_seq.fetch_add(1, Ordering::Relaxed);
        let head_dim = self.arch.d_model / self.arch.num_heads.max(1);
        let max_delta = token_ids
            .len()
            .saturating_add(cfg.max_frames)
            .saturating_add(4);
        let mut delta_cache = DeltaKvCache::new(
            self.arch.num_layers,
            self.arch.num_heads,
            max_delta,
            head_dim,
            &self.device,
        )
        .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle KV alloc: {e}")))?;

        let attempts = 1 + cfg.silent_start.map_or(0, |g| g.retries);
        for attempt in 0..attempts {
            if cancel.is_cancelled() {
                return Ok((0, attempt));
            }
            delta_cache.reset();
            let guard = if attempt + 1 < attempts {
                cfg.silent_start
            } else {
                None
            };
            let mut rng = RequestRng::for_request(cfg.seed, req_seq, attempt);
            let outcome = if cfg.pipeline_mimi {
                self.run_attempt_pipelined(
                    &token_ids,
                    &voice.kv_prefix,
                    &mut delta_cache,
                    cfg,
                    guard,
                    &mut rng,
                    audio_tx,
                    cancel,
                )?
            } else {
                self.run_attempt_sequential(
                    &token_ids,
                    &voice.kv_prefix,
                    &mut delta_cache,
                    cfg,
                    guard,
                    &mut rng,
                    audio_tx,
                    cancel,
                )?
            };

            match outcome {
                AttemptOutcome::Done { total_samples, .. }
                | AttemptOutcome::Cancelled { total_samples, .. } => {
                    return Ok((total_samples, attempt));
                }
                AttemptOutcome::SilentStart { peak } => {
                    warn!(
                        attempt = attempt + 1,
                        attempts,
                        peak,
                        "pocket-candle silent start detected; retrying with fresh RNG seed"
                    );
                }
            }
        }
        Ok((0, attempts.saturating_sub(1)))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "internal hot-loop attempt runner"
    )]
    fn run_attempt_sequential(
        &self,
        token_ids: &[u32],
        voice_prefix: &VoiceKvPrefix,
        delta_cache: &mut DeltaKvCache,
        cfg: &PocketCandleConfig,
        guard: Option<SilentStartGuard>,
        rng: &mut RequestRng,
        audio_tx: &mpsc::Sender<AudioFrame>,
        cancel: &CancellationToken,
    ) -> Result<AttemptOutcome, TtsError> {
        self.flow_lm
            .prefill_text(token_ids, voice_prefix, delta_cache)
            .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle text prefill: {e}")))?;

        let mut mimi_state = self
            .mimi
            .init_decoder_state(cfg.max_frames)
            .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle Mimi init: {e}")))?;
        let mut probe = guard.map(SilentStartProbe::new);
        let mut prev_latent: Option<Tensor> = None;
        let mut eos_step: Option<usize> = None;
        let mut total_samples = 0usize;
        let fade_out = cfg.fade_in_samples.max(DEFAULT_FADE_OUT_SAMPLES);

        for step_idx in 0..cfg.max_frames {
            if cancel.is_cancelled() {
                return Ok(AttemptOutcome::Cancelled {
                    total_samples,
                    frames: step_idx,
                });
            }
            let lsd = if step_idx == 0 {
                cfg.first_frame_lsd_steps
            } else {
                cfg.lsd_steps
            };
            let (next_norm, denorm, eos_logit) = self
                .flow_lm
                .step(
                    prev_latent.as_ref(),
                    voice_prefix,
                    delta_cache,
                    lsd,
                    cfg.temperature,
                    rng,
                )
                .map_err(|e| {
                    TtsError::InferenceFailed(format!("pocket-candle FlowLM step: {e}"))
                })?;
            prev_latent = Some(next_norm);

            if eos_step.is_none() && step_idx > 0 && eos_logit > cfg.eos_threshold {
                eos_step = Some(step_idx);
            }
            let is_last = step_idx + 1 >= cfg.max_frames
                || eos_step.is_some_and(|e| step_idx >= e + DEFAULT_FRAMES_AFTER_EOS);

            let mut pcm = self
                .mimi
                .decode_frame(&denorm, &mut mimi_state, cfg.fade_in_samples)
                .map_err(|e| {
                    TtsError::InferenceFailed(format!("pocket-candle Mimi decode: {e}"))
                })?;
            if is_last {
                apply_fade_out(&mut pcm, fade_out);
            }
            total_samples = total_samples.saturating_add(pcm.len());

            if let Some(p) = probe.as_mut() {
                p.observe(&pcm);
                if let Some(peak) = p.is_silent() {
                    return Ok(AttemptOutcome::SilentStart { peak });
                }
            }

            let frame = AudioFrame {
                samples: pcm,
                sample_rate: SAMPLE_RATE,
                channels: 1,
            };
            if audio_tx.blocking_send(frame).is_err() {
                return Ok(AttemptOutcome::Cancelled {
                    total_samples,
                    frames: step_idx + 1,
                });
            }
            if is_last {
                return Ok(AttemptOutcome::Done {
                    total_samples,
                    frames: step_idx + 1,
                });
            }
        }
        Ok(AttemptOutcome::Done {
            total_samples,
            frames: cfg.max_frames,
        })
    }

    /// Pipelined `FlowLM` <-> Mimi execution: `FlowLM` produces latent `t+1` concurrently
    /// while Mimi decodes latent `t` on a scoped worker thread over a 2-slot channel.
    #[expect(
        clippy::too_many_arguments,
        reason = "internal hot-loop pipelined runner"
    )]
    fn run_attempt_pipelined(
        &self,
        token_ids: &[u32],
        voice_prefix: &VoiceKvPrefix,
        delta_cache: &mut DeltaKvCache,
        cfg: &PocketCandleConfig,
        guard: Option<SilentStartGuard>,
        rng: &mut RequestRng,
        audio_tx: &mpsc::Sender<AudioFrame>,
        cancel: &CancellationToken,
    ) -> Result<AttemptOutcome, TtsError> {
        self.flow_lm
            .prefill_text(token_ids, voice_prefix, delta_cache)
            .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle text prefill: {e}")))?;

        let (lat_tx, lat_rx) = std::sync::mpsc::sync_channel::<(Tensor, bool)>(2);
        let attempt_cancel = cancel.child_token();
        let mimi_cancel = attempt_cancel.clone();
        let fade_in = cfg.fade_in_samples;
        let fade_out = fade_in.max(DEFAULT_FADE_OUT_SAMPLES);

        let max_frames = cfg.max_frames;
        std::thread::scope(|scope| {
            let mimi_handle = scope.spawn(move || {
                self.run_mimi_consumer(
                    &lat_rx,
                    max_frames,
                    fade_in,
                    fade_out,
                    guard,
                    audio_tx,
                    &mimi_cancel,
                )
            });

            let mut prev_latent: Option<Tensor> = None;
            let mut eos_step: Option<usize> = None;
            for step_idx in 0..cfg.max_frames {
                if attempt_cancel.is_cancelled() {
                    break;
                }
                let lsd = if step_idx == 0 {
                    cfg.first_frame_lsd_steps
                } else {
                    cfg.lsd_steps
                };
                let (next_norm, denorm, eos_logit) = self
                    .flow_lm
                    .step(
                        prev_latent.as_ref(),
                        voice_prefix,
                        delta_cache,
                        lsd,
                        cfg.temperature,
                        rng,
                    )
                    .map_err(|e| {
                        TtsError::InferenceFailed(format!("pocket-candle FlowLM step: {e}"))
                    })?;
                prev_latent = Some(next_norm);

                if eos_step.is_none() && step_idx > 0 && eos_logit > cfg.eos_threshold {
                    eos_step = Some(step_idx);
                }
                let is_last = step_idx + 1 >= cfg.max_frames
                    || eos_step.is_some_and(|e| step_idx >= e + DEFAULT_FRAMES_AFTER_EOS);

                if lat_tx.send((denorm, is_last)).is_err() || is_last {
                    break;
                }
            }
            drop(lat_tx);

            mimi_handle.join().map_err(|_panic| {
                TtsError::InferenceFailed("pocket-candle Mimi worker thread panicked".into())
            })?
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "internal pipelined Mimi consumer loop"
    )]
    fn run_mimi_consumer(
        &self,
        lat_rx: &std::sync::mpsc::Receiver<(Tensor, bool)>,
        max_frames: usize,
        fade_in: usize,
        fade_out: usize,
        guard: Option<SilentStartGuard>,
        audio_tx: &mpsc::Sender<AudioFrame>,
        mimi_cancel: &CancellationToken,
    ) -> Result<AttemptOutcome, TtsError> {
        let mut mimi_state = self
            .mimi
            .init_decoder_state(max_frames)
            .map_err(|e| TtsError::InferenceFailed(format!("pocket-candle Mimi init: {e}")))?;
        let mut probe = guard.map(SilentStartProbe::new);
        let mut total_samples = 0usize;
        let mut frames = 0usize;

        while let Ok((denorm, is_last)) = lat_rx.recv() {
            if mimi_cancel.is_cancelled() {
                return Ok(AttemptOutcome::Cancelled {
                    total_samples,
                    frames,
                });
            }
            let mut pcm = self
                .mimi
                .decode_frame(&denorm, &mut mimi_state, fade_in)
                .map_err(|e| {
                    TtsError::InferenceFailed(format!("pocket-candle Mimi decode: {e}"))
                })?;
            if is_last {
                apply_fade_out(&mut pcm, fade_out);
            }
            total_samples = total_samples.saturating_add(pcm.len());
            frames = frames.saturating_add(1);

            if let Some(p) = probe.as_mut() {
                p.observe(&pcm);
                if let Some(peak) = p.is_silent() {
                    mimi_cancel.cancel();
                    return Ok(AttemptOutcome::SilentStart { peak });
                }
            }

            let frame = AudioFrame {
                samples: pcm,
                sample_rate: SAMPLE_RATE,
                channels: 1,
            };
            if audio_tx.blocking_send(frame).is_err() {
                mimi_cancel.cancel();
                return Ok(AttemptOutcome::Cancelled {
                    total_samples,
                    frames,
                });
            }
        }
        Ok(AttemptOutcome::Done {
            total_samples,
            frames,
        })
    }
}

/// Extra 80 ms Mimi frames synthesized after EOS triggers so short utterances
/// ("Hello!") finish their final phoneme naturally before fading out.
const DEFAULT_FRAMES_AFTER_EOS: usize = 2;

/// 8 ms raised-cosine fade-out at 24 kHz (192 samples) on the final frame so
/// consecutive sentence fragments join cleanly at zero without a step click.
const DEFAULT_FADE_OUT_SAMPLES: usize = 192;

#[expect(
    clippy::cast_possible_truncation,
    reason = "value is explicitly clamped to i16 range before cast"
)]
fn apply_fade_out(pcm: &mut [i16], fade_samples: usize) {
    let n = fade_samples.min(pcm.len());
    if n == 0 {
        return;
    }
    let start = pcm.len().saturating_sub(n);
    let denom = n as f32;
    if let Some(tail) = pcm.get_mut(start..) {
        for (idx, sample) in tail.iter_mut().enumerate() {
            let frac = ((idx + 1) as f32) / denom;
            let gain = f32::midpoint(1.0, (std::f32::consts::PI * frac).cos());
            *sample = (f32::from(*sample) * gain).round().clamp(-32768.0, 32767.0) as i16;
        }
    }
}

fn load_arch_config(model_dir: &Path) -> Result<PocketArchConfig, TtsError> {
    let cfg_path = model_dir.join("config.json");
    if !cfg_path.exists() {
        return Ok(PocketArchConfig::default());
    }
    let raw = std::fs::read_to_string(&cfg_path).map_err(|e| {
        TtsError::ModelLoadFailed(format!(
            "pocket-candle: failed to read {}: {e}",
            cfg_path.display()
        ))
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        TtsError::ModelLoadFailed(format!(
            "pocket-candle: invalid config.json at {}: {e}",
            cfg_path.display()
        ))
    })
}

fn find_weights_file(model_dir: &Path) -> Result<std::path::PathBuf, TtsError> {
    for candidate in [
        "model.safetensors",
        "tts_b6369a24.safetensors",
        "pocket_tts.safetensors",
    ] {
        let p = model_dir.join(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(TtsError::ModelLoadFailed(format!(
        "pocket-candle: no model.safetensors found in {}",
        model_dir.display()
    )))
}

#[cfg(test)]
mod fade_tests {
    use super::apply_fade_out;

    #[test]
    fn fade_out_ramps_tail_smoothly_to_zero() {
        let mut pcm = vec![10_000i16; 300];
        apply_fade_out(&mut pcm, 192);
        assert_eq!(pcm[0], 10_000, "head before fade window untouched");
        assert_eq!(
            pcm[107], 10_000,
            "sample right before fade window untouched"
        );
        assert!(pcm[200] < 6_000 && pcm[200] > 4_000, "midpoint ~50%");
        assert_eq!(*pcm.last().unwrap(), 0, "final sample lands exactly at 0");
    }
}
