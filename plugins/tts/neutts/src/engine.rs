//! NeuTTS-Air engine: GGUF Qwen2 token generation via `llama-cpp-2` and ONNX `NeuCodec` decoding.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use ort::session::Session;
use ort::value::Tensor as OrtTensor;
use tracing::instrument;

use voice_pipes_onnx::{open_session, ExecutionTarget, SessionOptions};
use voice_pipes_sdk::audio::f32_to_i16;
use voice_pipes_sdk::TtsError;

use crate::voices::{JO_REFERENCE_CODES, JO_REFERENCE_PHONEMES};

/// Native sampling rate of `NeuCodec` (24 kHz).
pub const NATIVE_SAMPLE_RATE: u32 = 24_000;

/// Number of 24 kHz samples per acoustic token (20 ms at 24 kHz).
pub const HOP_LENGTH: usize = 480;

/// Streaming vocoder constants matching upstream `neuphonic/neutts-air`.
const STREAMING_OVERLAP_FRAMES: usize = 1;
const STREAMING_FRAMES_PER_CHUNK: usize = 25;
const STREAMING_LOOKFORWARD: usize = 5;
const STREAMING_LOOKBACK: usize = 50;
const STREAMING_STRIDE_SAMPLES: usize = STREAMING_FRAMES_PER_CHUNK * HOP_LENGTH;

pub const IM_END_TOKEN: u32 = 151_643;
pub const END_OF_TEXT_TOKEN: u32 = 151_645;
pub const SPEECH_GENERATION_END: u32 = 151_670;
pub const SPEECH_CODE_OFFSET: u32 = 151_671;
pub const SPEECH_CODE_MAX: u32 = 217_206;

static BACKEND: OnceLock<Arc<LlamaBackend>> = OnceLock::new();
static INIT_MUTEX: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Safely initialize and retrieve the shared `llama.cpp` backend instance.
fn get_or_init_backend() -> Result<Arc<LlamaBackend>, TtsError> {
    if let Some(backend) = BACKEND.get() {
        return Ok(Arc::clone(backend));
    }
    let _guard = INIT_MUTEX.lock();
    if let Some(backend) = BACKEND.get() {
        return Ok(Arc::clone(backend));
    }
    let mut backend = LlamaBackend::init()
        .map_err(|e| TtsError::ModelLoadFailed(format!("LlamaBackend init failed: {e}")))?;
    backend.void_logs();
    let arc = Arc::new(backend);
    let _ = BACKEND.set(Arc::clone(&arc));
    Ok(arc)
}

/// Linear overlap-add reconstruction matching upstream `neutts.py::_linear_overlap_add`.
pub(crate) fn linear_overlap_add(frames: &[Vec<f32>], stride: usize) -> Vec<f32> {
    if frames.is_empty() {
        return Vec::new();
    }
    let mut total_size = 0usize;
    for (i, frame) in frames.iter().enumerate() {
        let frame_end = stride.saturating_mul(i).saturating_add(frame.len());
        if frame_end > total_size {
            total_size = frame_end;
        }
    }
    if total_size == 0 {
        return Vec::new();
    }

    let mut out = vec![0.0_f32; total_size];
    let mut sum_weight = vec![0.0_f32; total_size];

    let mut offset = 0usize;
    for frame in frames {
        let frame_len = frame.len();
        let denom = (frame_len.saturating_add(1)) as f32;
        for (j, &sample) in frame.iter().enumerate() {
            let t = (j.saturating_add(1)) as f32 / denom;
            let weight = 0.5_f32 - (t - 0.5_f32).abs();
            let idx = offset.saturating_add(j);
            if let (Some(o), Some(sw)) = (out.get_mut(idx), sum_weight.get_mut(idx)) {
                *o += weight * sample;
                *sw += weight;
            }
        }
        offset = offset.saturating_add(stride);
    }

    for (o, &sw) in out.iter_mut().zip(sum_weight.iter()) {
        if sw > 0.0 {
            *o /= sw;
        }
    }
    out
}

/// NeuTTS-Air synthesis engine holding the GGUF Qwen2 model and the `NeuCodec` vocoder session.
pub struct NeuTtsEngine {
    pub(crate) backend: Arc<LlamaBackend>,
    pub(crate) model: Arc<LlamaModel>,
    pub(crate) vocoder_session: Session,
    pub(crate) threads: usize,
}

impl std::fmt::Debug for NeuTtsEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuTtsEngine")
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl NeuTtsEngine {
    /// Load the NeuTTS-Air GGUF SLM and `NeuCodec` ONNX vocoder.
    ///
    /// Both the Qwen2 backbone and the ONNX `NeuCodec` decoder are pinned to CPU
    /// (`with_n_gpu_layers(0)`, `with_devices(&[])`, `ExecutionTarget::Cpu`), matching
    /// upstream `neuphonic/neutts-air` and avoiding CUDA kernel/VRAM contention with
    /// GPU LLM sidecars or unsupported Blackwell `sm_120` cubins.
    pub fn load(
        model_path: &Path,
        vocoder_path: &Path,
        _target: ExecutionTarget,
        threads: usize,
    ) -> Result<Self, TtsError> {
        let backend = get_or_init_backend()?;

        let options = SessionOptions::new(threads).with_target(ExecutionTarget::Cpu);
        let vocoder_session = open_session(vocoder_path, &options)
            .map_err(|e| TtsError::ModelLoadFailed(format!("NeuCodec load failed: {e}")))?;

        let model_params = LlamaModelParams::default()
            .with_n_gpu_layers(0)
            .with_devices(&[])
            .map_err(|e| TtsError::ModelLoadFailed(format!("set CPU-only devices failed: {e}")))?;
        let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
            .map_err(|e| TtsError::ModelLoadFailed(format!("load Qwen2 model failed: {e}")))?;

        Ok(Self {
            backend,
            model: Arc::new(model),
            vocoder_session,
            threads,
        })
    }

    /// Decode discrete acoustic tokens into raw 24 kHz f32 audio samples.
    #[instrument(skip_all, fields(token_count = tokens.len()))]
    pub fn decode_codes_24k(&mut self, tokens: &[i32]) -> Result<Vec<f32>, TtsError> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let len = tokens.len();
        let tokens_tensor = OrtTensor::from_array(([1, 1, len], tokens.to_vec()))
            .map_err(|e| TtsError::InferenceFailed(format!("tokens tensor: {e}")))?;

        let outputs = self
            .vocoder_session
            .run(ort::inputs!["codes" => tokens_tensor])
            .map_err(|e| TtsError::InferenceFailed(format!("NeuCodec vocoder run: {e}")))?;

        let output = outputs
            .values()
            .next()
            .ok_or_else(|| TtsError::InferenceFailed("NeuCodec returned no output".into()))?;

        let audio_tensor = output
            .try_extract_tensor::<f32>()
            .map_err(|e| TtsError::InferenceFailed(format!("NeuCodec extract audio: {e}")))?;

        Ok(audio_tensor.1.to_vec())
    }

    /// Decode discrete acoustic tokens into 24 kHz i16 audio frames.
    #[allow(dead_code, reason = "batch decode helper for tests and offline synthesis")]
    #[instrument(skip_all, fields(token_count = tokens.len()))]
    pub fn decode_tokens(&mut self, tokens: &[i32]) -> Result<Vec<i16>, TtsError> {
        let raw_24k = self.decode_codes_24k(tokens)?;
        Ok(f32_to_i16(&raw_24k))
    }

    pub(crate) fn build_prompt_tokens(&self, input_phonemes: &str) -> Result<Vec<u32>, TtsError> {
        use std::fmt::Write as _;
        let mut codes_str = String::with_capacity(JO_REFERENCE_CODES.len().saturating_mul(16));
        for &c in JO_REFERENCE_CODES {
            let _ = write!(codes_str, "<|speech_{c}|>");
        }
        let trimmed = input_phonemes
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let clean_phonemes =
            if trimmed.ends_with('.') || trimmed.ends_with('?') || trimmed.ends_with('!') {
                trimmed
            } else {
                format!("{trimmed}.")
            };
        let prompt_str = format!(
            "user: Convert the text to speech:<|TEXT_PROMPT_START|>{JO_REFERENCE_PHONEMES} {clean_phonemes}<|TEXT_PROMPT_END|>\nassistant:<|SPEECH_GENERATION_START|>{codes_str}"
        );
        let tokens = self
            .model
            .vocab()
            .tokenize(prompt_str.as_bytes(), false, true);
        tokens
            .into_iter()
            .map(|tok| {
                u32::try_from(tok.0)
                    .map_err(|e| TtsError::InferenceFailed(format!("token overflow: {e}")))
            })
            .collect()
    }

    /// Run prompt prefill through `llama.cpp` and sample the initial generated token.
    pub(crate) fn run_prefill(
        ctx: &mut LlamaContext<'_>,
        sampler: &mut LlamaSampler,
        prompt_tokens: &[u32],
    ) -> Result<LlamaToken, TtsError> {
        let mut batch = LlamaBatch::new(2048, 1);
        let prompt_len = prompt_tokens.len();
        let last_idx = prompt_len.saturating_sub(1);

        for (i, &tok) in prompt_tokens.iter().enumerate() {
            let is_last = i == last_idx;
            let pos = i32::try_from(i)
                .map_err(|e| TtsError::InferenceFailed(format!("pos overflow: {e}")))?;
            let tok_i32 = i32::try_from(tok)
                .map_err(|e| TtsError::InferenceFailed(format!("token overflow: {e}")))?;
            batch
                .add(LlamaToken(tok_i32), pos, &[0], is_last)
                .map_err(|e| TtsError::InferenceFailed(format!("batch add prefill: {e}")))?;
        }

        ctx.decode(&mut batch)
            .map_err(|e| TtsError::InferenceFailed(format!("prefill decode: {e}")))?;

        Ok(sampler.sample(ctx, -1))
    }

    /// Decode a streaming window from `token_cache`, overlap-add into `audio_cache`, and send 24 kHz frames.
    fn flush_streaming_chunk(
        &mut self,
        token_cache: &[i32],
        audio_cache: &mut Vec<Vec<f32>>,
        n_decoded_tokens: usize,
        n_decoded_samples: &mut usize,
        is_final: bool,
        tx: &tokio::sync::mpsc::Sender<voice_pipes_sdk::AudioFrame>,
    ) -> Result<bool, TtsError> {
        let new_samples_24k = if is_final {
            let remaining_tokens = token_cache.len().saturating_sub(n_decoded_tokens);
            if remaining_tokens == 0 {
                return Ok(true);
            }
            let lookback_total = STREAMING_LOOKBACK
                .saturating_add(STREAMING_OVERLAP_FRAMES)
                .saturating_add(remaining_tokens);
            let tokens_start = token_cache.len().saturating_sub(lookback_total);
            let sample_start = n_decoded_tokens
                .saturating_sub(tokens_start)
                .saturating_mul(HOP_LENGTH);

            let Some(curr_codes) = token_cache.get(tokens_start..) else {
                return Ok(true);
            };
            let recon = self.decode_codes_24k(curr_codes)?;
            let start_clamped = sample_start.min(recon.len());
            let Some(slice) = recon.get(start_clamped..) else {
                return Ok(true);
            };
            audio_cache.push(slice.to_vec());

            let processed = linear_overlap_add(audio_cache, STREAMING_STRIDE_SAMPLES);
            let prev_end = (*n_decoded_samples).min(processed.len());
            *n_decoded_samples = processed.len();
            processed.get(prev_end..).unwrap_or(&[]).to_vec()
        } else {
            let lookback_offset = STREAMING_LOOKBACK.saturating_add(STREAMING_OVERLAP_FRAMES);
            let tokens_start = n_decoded_tokens.saturating_sub(lookback_offset);
            let tokens_end = n_decoded_tokens
                .saturating_add(STREAMING_FRAMES_PER_CHUNK)
                .saturating_add(STREAMING_LOOKFORWARD)
                .saturating_add(STREAMING_OVERLAP_FRAMES)
                .min(token_cache.len());
            let sample_start = n_decoded_tokens
                .saturating_sub(tokens_start)
                .saturating_mul(HOP_LENGTH);
            let window_frames =
                STREAMING_FRAMES_PER_CHUNK.saturating_add(2 * STREAMING_OVERLAP_FRAMES);
            let sample_end = sample_start.saturating_add(window_frames.saturating_mul(HOP_LENGTH));

            let Some(curr_codes) = token_cache.get(tokens_start..tokens_end) else {
                return Ok(true);
            };
            let recon = self.decode_codes_24k(curr_codes)?;
            let end_clamped = sample_end.min(recon.len());
            let start_clamped = sample_start.min(end_clamped);
            let Some(slice) = recon.get(start_clamped..end_clamped) else {
                return Ok(true);
            };
            audio_cache.push(slice.to_vec());

            let processed = linear_overlap_add(audio_cache, STREAMING_STRIDE_SAMPLES);
            let new_end = audio_cache
                .len()
                .saturating_mul(STREAMING_STRIDE_SAMPLES)
                .min(processed.len());
            let prev_end = (*n_decoded_samples).min(new_end);
            *n_decoded_samples = new_end;
            processed.get(prev_end..new_end).unwrap_or(&[]).to_vec()
        };

        if new_samples_24k.is_empty() {
            return Ok(true);
        }

        let pcm_24k = f32_to_i16(&new_samples_24k);
        for chunk in pcm_24k.chunks(1200) {
            let frame = voice_pipes_sdk::AudioFrame {
                samples: chunk.to_vec(),
                sample_rate: NATIVE_SAMPLE_RATE,
                channels: 1,
            };
            if tx.blocking_send(frame).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Build the anti-looping speech token sampler for `NeuTTS-Air` GGUF inference.
    fn build_sampler(model: &LlamaModel) -> LlamaSampler {
        let n_vocab = model.n_vocab();
        LlamaSampler::chain_simple([
            LlamaSampler::penalties(n_vocab, 24, 1.0, 1.0, 0.2),
            LlamaSampler::dry(
                model,
                1.2,
                1.75,
                2,
                48,
                std::iter::empty::<&str>(),
            ),
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.90, 1),
            LlamaSampler::min_p(0.10, 1),
            LlamaSampler::temp(0.65),
            LlamaSampler::dist(42),
        ])
    }

    /// Autoregressively synthesize speech from input phonemes, streaming audio frames over `tx`.
    #[instrument(skip_all, fields(phonemes_len = input_phonemes.len()))]
    pub fn synthesize(
        &mut self,
        input_phonemes: &str,
        cancel: &tokio_util::sync::CancellationToken,
        tx: &tokio::sync::mpsc::Sender<voice_pipes_sdk::AudioFrame>,
    ) -> Result<(), TtsError> {
        if cancel.is_cancelled() || tx.is_closed() {
            return Ok(());
        }

        let prompt_tokens = self.build_prompt_tokens(input_phonemes)?;
        let prompt_len = prompt_tokens.len();

        let n_threads = self.threads.max(1);
        let threads_i32 = i32::try_from(n_threads).unwrap_or(4);
        let batch_threads_i32 = i32::try_from(
            std::thread::available_parallelism()
                .map_or(n_threads, std::num::NonZero::get)
                .max(n_threads),
        )
        .unwrap_or(threads_i32);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(2048))
            .with_n_batch(2048)
            .with_n_ubatch(512)
            .with_n_threads(threads_i32)
            .with_n_threads_batch(batch_threads_i32)
            .with_offload_kqv(false)
            .with_op_offload(false);

        let model = Arc::clone(&self.model);
        let mut ctx = model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| TtsError::InferenceFailed(format!("create context: {e}")))?;

        let mut sampler = Self::build_sampler(&model);

        let mut next_token = Self::run_prefill(&mut ctx, &mut sampler, &prompt_tokens)?;
        let max_new_tokens = (input_phonemes.len().saturating_mul(8))
            .clamp(60, 1800)
            .min(2048usize.saturating_sub(prompt_len));

        let mut token_cache: Vec<i32> = Vec::with_capacity(JO_REFERENCE_CODES.len() + max_new_tokens);
        token_cache.extend_from_slice(JO_REFERENCE_CODES);
        let mut n_decoded_tokens = JO_REFERENCE_CODES.len();
        let mut n_decoded_samples = 0usize;
        let mut audio_cache: Vec<Vec<f32>> = Vec::new();

        let mut step_batch = LlamaBatch::new(1, 1);
        for step in 0..max_new_tokens {
            if cancel.is_cancelled() || tx.is_closed() {
                break;
            }

            let tok_u32 = u32::try_from(next_token.0).unwrap_or(0);
            if tok_u32 == SPEECH_GENERATION_END
                || tok_u32 == IM_END_TOKEN
                || tok_u32 == END_OF_TEXT_TOKEN
            {
                break;
            }

            if (SPEECH_CODE_OFFSET..=SPEECH_CODE_MAX).contains(&tok_u32) {
                let code_u32 = tok_u32.saturating_sub(SPEECH_CODE_OFFSET);
                if let Ok(code) = i32::try_from(code_u32) {
                    token_cache.push(code);
                }
            }

            if token_cache.len().saturating_sub(n_decoded_tokens)
                >= STREAMING_FRAMES_PER_CHUNK + STREAMING_LOOKFORWARD
            {
                if !self.flush_streaming_chunk(
                    &token_cache,
                    &mut audio_cache,
                    n_decoded_tokens,
                    &mut n_decoded_samples,
                    false,
                    tx,
                )? {
                    return Ok(());
                }
                n_decoded_tokens = n_decoded_tokens.saturating_add(STREAMING_FRAMES_PER_CHUNK);
            }

            step_batch.clear();
            let pos_usize = prompt_len.saturating_add(step);
            let pos = i32::try_from(pos_usize)
                .map_err(|e| TtsError::InferenceFailed(format!("pos overflow: {e}")))?;
            step_batch
                .add(next_token, pos, &[0], true)
                .map_err(|e| TtsError::InferenceFailed(format!("batch add step: {e}")))?;

            ctx.decode(&mut step_batch)
                .map_err(|e| TtsError::InferenceFailed(format!("step decode: {e}")))?;

            next_token = sampler.sample(&ctx, -1);
        }

        if !cancel.is_cancelled() && !tx.is_closed() && token_cache.len() > n_decoded_tokens {
            let _ = self.flush_streaming_chunk(
                &token_cache,
                &mut audio_cache,
                n_decoded_tokens,
                &mut n_decoded_samples,
                true,
                tx,
            )?;
        }

        Ok(())
    }
}
