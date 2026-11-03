//! Moonshine speech-to-text -- local ONNX inference.
//!
//! A voice-pipes recogniser plugin: the binary registers [`factory`] and
//! [`wizard_choices`]; `[stt] provider = "moonshine"` selects it.

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

mod decoder;
mod encoder;
mod wizard;

pub use wizard::wizard_choices;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument};

use voice_pipes_onnx::SessionPool;
use voice_pipes_sdk::audio::{i16_to_f32, samples_in_ms};
use voice_pipes_sdk::component::{
    Component, ComponentCore, ComponentKind, CoreComponent, DeviceInfo,
};
use voice_pipes_sdk::options::as_f32;
use voice_pipes_sdk::{
    AudioChunk, AudioFrame, BuildContext, BuildError, CancellationToken, ConfigError,
    InferenceMetrics, RecognitionRequest, SttCapability, SttConfig, SttError, SttFactory,
    SttProvider, TranscriptionResult, WarmUpScope,
};

use decoder::MoonshineDecoder;
use encoder::MoonshineEncoder;

/// Open one of the model's graphs through the shared opener: `threads` per
/// operator, on the device `VP_ORT_EP` names (the CPU unless it asks for
/// CUDA in a CUDA build).
fn open_session(
    path: &Path,
    threads: usize,
    what: &str,
) -> Result<ort::session::Session, SttError> {
    let options = voice_pipes_onnx::SessionOptions::new(threads)
        .with_target(voice_pipes_onnx::ExecutionTarget::from_env());
    voice_pipes_onnx::open_session(path, &options)
        .map_err(|e| SttError::ModelLoadFailed(format!("Moonshine {what}: {e}")))
}

// ── Session pool ───────────────────────────────────────────────────

/// One complete set of ONNX sessions for the Moonshine pipeline.
struct SessionSet {
    encoder: MoonshineEncoder,
    decoder: MoonshineDecoder,
}

impl SessionSet {
    fn new(model_dir: &Path, threads: usize) -> Result<Self, SttError> {
        let encoder_path = model_dir.join("encoder_model.onnx");
        let encoder_path_quant = model_dir.join("encoder_model_quantized.onnx");
        let final_encoder_path = if encoder_path_quant.exists() {
            encoder_path_quant
        } else if encoder_path.exists() {
            encoder_path
        } else {
            return Err(SttError::ModelLoadFailed(format!(
                "expected encoder_model.onnx or encoder_model_quantized.onnx in {}",
                model_dir.display()
            )));
        };

        // The merged export decodes with a key/value cache and gives the
        // same tokens; the plain one re-runs the whole prefix every step.
        let decoder_path_merged = model_dir.join("decoder_model_merged.onnx");
        let decoder_path = model_dir.join("decoder_model.onnx");
        let decoder_path_quant = model_dir.join("decoder_model_quantized.onnx");
        let final_decoder_path = if decoder_path_merged.exists() {
            decoder_path_merged
        } else if decoder_path_quant.exists() {
            decoder_path_quant
        } else if decoder_path.exists() {
            decoder_path
        } else {
            return Err(SttError::ModelLoadFailed(format!(
                "expected decoder_model_merged.onnx, decoder_model.onnx or \
                 decoder_model_quantized.onnx in {}",
                model_dir.display()
            )));
        };

        let tokenizer_path = model_dir.join("tokenizer.json");
        if !tokenizer_path.exists() {
            return Err(SttError::ModelLoadFailed(format!(
                "expected tokenizer.json in {}",
                model_dir.display()
            )));
        }

        Ok(Self {
            encoder: MoonshineEncoder::new(&final_encoder_path, threads)?,
            decoder: MoonshineDecoder::new(&final_decoder_path, &tokenizer_path, threads)?,
        })
    }

    /// Encode 16 kHz mono audio and decode it to text.
    fn transcribe(&mut self, samples: &[f32], max_new_tokens: usize) -> Result<String, SttError> {
        let enc_out = self.encoder.encode(samples)?;
        let tokens = self.decoder.decode(&enc_out, max_new_tokens)?;
        self.decoder.detokenize(&tokens)
    }
}

/// Build the bounded pool of `SessionSet`s used for concurrent inference.
fn build_pool(
    model_dir: &Path,
    pool_size: usize,
    threads: usize,
) -> Result<SessionPool<SessionSet>, SttError> {
    let mut sets = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        sets.push(SessionSet::new(model_dir, threads)?);
    }
    info!(pool_size, "Moonshine ONNX session pool created");
    SessionPool::new(sets, "Moonshine session sets")
        .map_err(|e| SttError::ConfigError(e.to_string()))
}

// ── MoonshineStt provider ───────────────────────────────────────────

const DEFAULT_POOL_SIZE: usize = 2;
const DEFAULT_THREADS: usize = 4;
const DEFAULT_MAX_NEW_TOKENS: usize = 256;

/// The longest stretch of audio one streamed utterance keeps, in seconds.
///
/// Moonshine is a batch seq2seq model that re-encodes the whole buffer each
/// step, so an unbounded buffer is both an OOM risk and O(N²) CPU on a
/// runaway utterance or a stuck VAD. A generous sliding window bounds the
/// per-step encode by the cap rather than the utterance length.
const MAX_STREAM_BUFFER_SECS: f32 = 120.0;

/// Useful Sensors Moonshine speech-to-text provider -- local ONNX inference.
pub(crate) struct MoonshineStt {
    pool: Arc<SessionPool<SessionSet>>,
    min_initial_buffer_ms: f32,
    min_inference_step_ms: f32,
    max_new_tokens: usize,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for MoonshineStt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoonshineStt")
            .field("min_initial_buffer_ms", &self.min_initial_buffer_ms)
            .field("min_inference_step_ms", &self.min_inference_step_ms)
            .field("max_new_tokens", &self.max_new_tokens)
            .finish_non_exhaustive()
    }
}

impl MoonshineStt {
    /// Load Moonshine models with a specific pool size and threads.
    pub(crate) fn with_options(
        model_dir: &Path,
        pool_size: usize,
        threads: usize,
    ) -> Result<Self, SttError> {
        let core = Arc::new(ComponentCore::new("stt:moonshine", ComponentKind::Stt));
        let pool = match build_pool(model_dir, pool_size, threads) {
            Ok(p) => p,
            Err(e) => {
                core.mark_unhealthy(format!("model load failed: {e}"));
                return Err(e);
            }
        };
        info!(model_dir = %model_dir.display(), pool_size, threads, "Moonshine STT loaded");
        Ok(Self {
            pool: Arc::new(pool),
            min_initial_buffer_ms: 800.0,
            min_inference_step_ms: 1000.0,
            max_new_tokens: DEFAULT_MAX_NEW_TOKENS,
            core,
        })
    }
}

#[async_trait]
impl SttProvider for MoonshineStt {
    async fn device_info(&self) -> Option<DeviceInfo> {
        let (runs_on, detail) = voice_pipes_onnx::ExecutionTarget::from_env().device_report();
        Some(DeviceInfo::config(runs_on, detail))
    }

    /// ONNX Runtime pays its start-up on each session's first run: the
    /// first utterance of a worker took longer to transcribe than it took
    /// to say. Every set in the pool is warmed, each held until all are
    /// done so the next checkout reaches a cold one, and once per process
    /// (see [`WarmUpScope`]), before any caller can contend for a set. The
    /// audio is a second of silence; the decoder runs a few steps
    /// regardless, so its cached path warms too.
    async fn warm_up(&self) -> Result<(), SttError> {
        let mut held = Vec::with_capacity(self.pool.size());
        for _ in 0..self.pool.size() {
            held.push(self.pool.checkout().await.map_err(SttError::busy)?);
        }
        tokio::task::spawn_blocking(move || -> Result<(), SttError> {
            let silence = vec![0.0_f32; 16_000];
            for set in &mut held {
                let enc_out = set.encoder.encode(&silence)?;
                set.decoder.warm_up(&enc_out)?;
            }
            Ok(())
        })
        .await
        .map_err(|e| SttError::InferenceFailed(format!("warm-up task join: {e}")))?
    }

    fn warm_up_scope(&self) -> WarmUpScope {
        WarmUpScope::PerProcess
    }

    #[instrument(skip_all, fields(provider = "moonshine", samples = audio_frame.samples.len()))]
    async fn transcribe(&self, audio_frame: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = std::time::Instant::now();
        let mut pooled = self.pool.checkout().await.map_err(SttError::busy)?;

        let raw_samples = audio_frame.samples.clone();
        let channels = audio_frame.channels;
        let sample_rate = audio_frame.sample_rate;
        let max_new_tokens = self.max_new_tokens;

        let (_pooled, result) = tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<TranscriptionResult, SttError> {
                let samples =
                    voice_pipes_sdk::audio::mono_f32_at(&raw_samples, channels, sample_rate, 16000)
                        .map_err(|e| SttError::InvalidAudio(e.to_string()))?;

                let text = pooled.transcribe(&samples, max_new_tokens)?;

                let total_audio_secs = samples.len() as f64 / 16000.0;
                let end_time = std::time::Duration::from_secs_f64(total_audio_secs);

                let inference_metrics =
                    InferenceMetrics::measured(total_audio_secs, start.elapsed(), 1);

                Ok(TranscriptionResult::new(text, true)
                    .with_language(Some("en".into()))
                    .with_start_time(Some(std::time::Duration::ZERO))
                    .with_end_time(Some(end_time))
                    .with_inference_metrics(Some(inference_metrics)))
            })();
            (pooled, result)
        })
        .await
        .map_err(|e| {
            let err = SttError::InferenceFailed(format!("task join: {e}"));
            self.core.record_error(&err);
            err
        })?;

        match &result {
            Ok(_) => self.core.record_ok(start.elapsed()),
            Err(e) => self.core.record_error(e),
        }
        result
    }

    async fn transcribe_stream(
        &self,
        request: RecognitionRequest,
        mut audio_rx: tokio::sync::mpsc::Receiver<AudioChunk>,
        tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
        cancel: CancellationToken,
    ) -> Result<(), SttError> {
        let mut pooled = match request.slot_wait {
            Some(wait) => self.pool.checkout_within(wait).await,
            None => self.pool.checkout().await,
        }
        .map_err(SttError::busy)?;

        let mut f32_buffer: Vec<f32> = Vec::new();
        let mut last_emitted_text = String::new();
        let mut last_inference_len: usize = 0;
        let expected_sr = self.expected_sample_rate();

        let min_initial_buffer = samples_in_ms(expected_sr, self.min_initial_buffer_ms);
        let min_inference_step = samples_in_ms(expected_sr, self.min_inference_step_ms);
        let max_buffer_samples = samples_in_ms(expected_sr, MAX_STREAM_BUFFER_SECS * 1000.0);
        let max_new_tokens = self.max_new_tokens;

        let mut total_inference_wall = std::time::Duration::ZERO;
        let mut inference_count: u32 = 0;
        // One stream serves the whole session; each utterance gets the
        // next segment index.
        let mut segment_index: u32 = 0;

        while !cancel.is_cancelled() {
            let chunk = tokio::select! {
                () = cancel.cancelled() => break,
                c = audio_rx.recv() => match c {
                    Some(ch) => ch,
                    None => break,
                }
            };

            f32_buffer.extend(i16_to_f32(&chunk.samples));
            let mut is_final = chunk.is_last;

            // Take what is already queued, so a slow decode catches up, but
            // never past an utterance boundary: the chunks after `is_last`
            // belong to the next utterance, and reading on decodes them as
            // part of this one.
            for _ in 0..audio_rx.len() {
                if is_final {
                    break;
                }
                let Ok(extra_chunk) = audio_rx.try_recv() else {
                    break;
                };
                f32_buffer.extend(i16_to_f32(&extra_chunk.samples));
                is_final = extra_chunk.is_last;
            }

            if f32_buffer.len() > max_buffer_samples {
                let drop = f32_buffer.len() - max_buffer_samples;
                f32_buffer.drain(..drop);
                last_inference_len = last_inference_len.saturating_sub(drop);
                tracing::warn!(
                    dropped_samples = drop,
                    cap_secs = MAX_STREAM_BUFFER_SECS,
                    "Moonshine streaming buffer hit the cap; dropping oldest audio"
                );
            }

            if f32_buffer.len() < min_initial_buffer && !is_final {
                continue;
            }

            if !is_final && (f32_buffer.len() - last_inference_len) < min_inference_step {
                continue;
            }

            last_inference_len = f32_buffer.len();

            let samples = f32_buffer.clone();
            let inference_start = std::time::Instant::now();

            let (returned_pooled, result) = tokio::task::spawn_blocking(move || {
                let result = pooled.transcribe(&samples, max_new_tokens);
                (pooled, result)
            })
            .await
            .map_err(|e| {
                let err = SttError::InferenceFailed(format!("task join: {e}"));
                self.core.record_error(&err);
                err
            })?;

            pooled = returned_pooled;
            let inference_elapsed = inference_start.elapsed();
            total_inference_wall += inference_elapsed;
            inference_count = inference_count.saturating_add(1);

            let text = match result {
                Ok(t) => {
                    self.core.record_ok(inference_elapsed);
                    t
                }
                Err(e) => {
                    self.core.record_error(&e);
                    return Err(e);
                }
            };

            let text = settle_final_text(text, &last_emitted_text, is_final);

            if text != last_emitted_text || is_final {
                last_emitted_text = text.clone();
                let inference_metrics = is_final.then(|| {
                    let total_audio_secs = f32_buffer.len() as f64 / f64::from(expected_sr);
                    InferenceMetrics::measured(
                        total_audio_secs,
                        total_inference_wall,
                        inference_count,
                    )
                });

                let _ = tx
                    .send(
                        TranscriptionResult::new(text, is_final)
                            .with_language(Some("en".into()))
                            .with_segment_index(segment_index)
                            .with_start_time(Some(std::time::Duration::ZERO))
                            .with_inference_metrics(inference_metrics),
                    )
                    .await;
            }

            if is_final {
                f32_buffer.clear();
                last_emitted_text.clear();
                last_inference_len = 0;
                total_inference_wall = std::time::Duration::ZERO;
                inference_count = 0;
                segment_index += 1;
            }
        }

        Ok(())
    }

    fn concurrent_capacity(&self) -> Option<usize> {
        Some(self.pool.size())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "moonshine"
    }

    fn needs_flush_silence(&self) -> bool {
        // Moonshine can handle variable length, no synthetic flush silence needed
        false
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        HashSet::from([SttCapability::InterimResults])
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

// ── Factory ──────────────────────────────────────────────────

/// The factory the binary registers for `[stt] provider = "moonshine"`.
pub fn factory() -> Box<dyn SttFactory> {
    Box::new(MoonshineFactory)
}

/// Factory for the Moonshine STT provider.
#[derive(Debug)]
struct MoonshineFactory;

impl SttFactory for MoonshineFactory {
    fn name(&self) -> &'static str {
        "moonshine"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError> {
        MoonshineOptions::read(cfg).err().unwrap_or_default()
    }

    fn build(
        &self,
        cfg: &SttConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, BuildError> {
        let default_dir = PathBuf::from("models/moonshine");
        let model_dir = cfg.model_dir.as_deref().unwrap_or(&default_dir);

        let options = MoonshineOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let mut provider =
            MoonshineStt::with_options(model_dir, options.pool_size, options.threads)?;
        if let Some(ms) = options.initial_buffer_ms {
            provider.min_initial_buffer_ms = as_f32(ms);
        }
        if let Some(ms) = options.inference_step_ms {
            provider.min_inference_step_ms = as_f32(ms);
        }
        if let Some(t) = options.max_new_tokens {
            provider.max_new_tokens = t;
        }

        Ok(Box::new(provider))
    }
}

/// The settings Moonshine reads from its option table.
struct MoonshineOptions {
    pool_size: usize,
    threads: usize,
    initial_buffer_ms: Option<f64>,
    inference_step_ms: Option<f64>,
    max_new_tokens: Option<usize>,
}

impl MoonshineOptions {
    fn read(cfg: &SttConfig) -> Result<Self, Vec<ConfigError>> {
        let mut o = cfg.options();
        let options = Self {
            pool_size: o.usize_in("pool_size", 1..=64, DEFAULT_POOL_SIZE),
            // 0 leaves the count to ONNX Runtime.
            threads: o.usize_in("threads", 0..=256, DEFAULT_THREADS),
            initial_buffer_ms: o.optional_f64_in("initial_buffer_ms", 0.0..=60_000.0),
            inference_step_ms: o.optional_f64_in("inference_step_ms", 1.0..=60_000.0),
            max_new_tokens: o
                .optional_u64_in("max_new_tokens", 1..=4_096)
                .and_then(|n| usize::try_from(n).ok()),
        };
        o.finish()?;
        Ok(options)
    }
}

/// The text a pass stands for, given what the passes before it produced.
///
/// The final pass re-decodes the whole buffer, which by then ends in the
/// silence that closed the utterance. This model can answer that with nothing
/// at all, having transcribed the same speech a moment earlier. Taken at face
/// value the empty final replaced the interim, the utterance was discarded as
/// empty, and a caller who had been heard got no reply. Words the model has
/// already produced for this audio outrank a later pass that produced none.
fn settle_final_text(text: String, last_emitted: &str, is_final: bool) -> String {
    if is_final && text.trim().is_empty() && !last_emitted.trim().is_empty() {
        tracing::warn!(
            kept_chars = last_emitted.chars().count(),
            "Moonshine final pass returned no text; keeping the last interim result"
        );
        return last_emitted.to_string();
    }
    text
}

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;
