//! NeuTTS-Air TTS provider -- local GGUF Qwen2 + ONNX `NeuCodec` vocoder.
//!
//! NeuTTS-Air is an open-source (Apache 2.0) on-device speech language model
//! producing natural speech with instant zero-shot voice cloning capabilities.
//!
//! Output audio is natively 24 kHz; the pipeline engine automatically resamples
//! chunk-by-chunk to the pipeline standard (16 kHz) via `StreamingResampler`.

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
mod schema;
mod voices;
mod wizard;

pub use wizard::wizard_choices;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument};

use engine::{NATIVE_SAMPLE_RATE, NeuTtsEngine};
use voice_pipes_onnx::{EXECUTION_PROVIDER_VAR, ExecutionTarget, SessionPool};
use voice_pipes_sdk::component::{
    Component, ComponentCore, ComponentKind, CoreComponent, DeviceInfo,
};
use voice_pipes_sdk::{
    AudioFrame, BuildContext, BuildError, ConfigError, SynthesisRequest, TextChunkingConfig,
    TraceEvent, TtsConfig, TtsError, TtsFactory, TtsProvider,
};

/// Default pool size of `NeuTTS` engines (1 slot minimizes CPU memory and cold-start time).
const DEFAULT_POOL_SIZE: usize = 1;

/// Default intra-op threads per session (fallback when parallelism query fails).
const DEFAULT_THREADS: usize = 4;

/// Resolve default thread count matching upstream `_n_perf_cores()` (physical core count, clamped to 4..=8).
fn resolve_default_threads() -> usize {
    let perf_cores = std::thread::available_parallelism()
        .map_or(DEFAULT_THREADS, |n| (n.get() / 2).clamp(4, 8));
    voice_pipes_onnx::intra_threads_from_env(perf_cores)
}

/// Check whether the current buffer ends with a common title/abbreviation before a period.
fn ends_with_abbreviation(buf: &str) -> bool {
    let last_word = buf
        .rsplit(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("");
    matches!(
        last_word.to_ascii_lowercase().as_str(),
        "mr" | "mrs" | "ms" | "dr" | "prof" | "sr" | "jr" | "st" | "vs" | "etc"
    )
}

/// Phonemize text via `espeak-ng` while preserving clause punctuation (`;`, `:`, `,`, `.`, `!`, `?`),
/// matching Python `phonemizer`'s `EspeakBackend(preserve_punctuation=True, with_stress=True)`
/// used by upstream `neuphonic/neutts-air`, while keeping numbers (`3.14`, `1,500`, `2:30`)
/// and abbreviations (`Dr.`, `Mr.`) intact in a single `espeak-ng` context.
pub(crate) fn phonemize_preserving_punctuation(text: &str, lang: &str) -> Result<String, TtsError> {
    let chars: Vec<char> = text
        .chars()
        .map(|c| match c {
            '‘' | '’' => '\'',
            '“' | '”' => '"',
            other => other,
        })
        .collect();

    let mut clauses: Vec<(String, Option<char>)> = Vec::new();
    let mut current = String::new();

    for (i, &ch) in chars.iter().enumerate() {
        if matches!(ch, ';' | ':' | ',' | '.' | '!' | '?') {
            let prev_digit = i > 0 && chars.get(i.saturating_sub(1)).is_some_and(char::is_ascii_digit);
            let next_digit = chars.get(i.saturating_add(1)).is_some_and(char::is_ascii_digit);
            let in_number = matches!(ch, '.' | ',' | ':') && prev_digit && next_digit;
            let is_abbrev = ch == '.' && ends_with_abbreviation(&current);

            if in_number || is_abbrev {
                current.push(ch);
            } else if current.chars().any(char::is_alphanumeric) {
                clauses.push((std::mem::take(&mut current), Some(ch)));
            } else if let Some(last) = clauses.last_mut() {
                last.1 = Some(ch);
                current.clear();
            } else {
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    if current.chars().any(char::is_alphanumeric) {
        clauses.push((current, None));
    }

    if clauses.is_empty() {
        return Ok(String::new());
    }

    let mut out_parts = Vec::with_capacity(clauses.len());
    let last_idx = clauses.len().saturating_sub(1);
    for (idx, (clause, punct)) in clauses.into_iter().enumerate() {
        let raw_ipa = voice_pipes_sdk::phonemize::run_espeak_with_language(clause.trim(), lang)?;
        let clean_ipa = raw_ipa.split_whitespace().collect::<Vec<_>>().join(" ");
        if clean_ipa.is_empty() {
            continue;
        }
        let suffix = match punct {
            Some(p) => p,
            None if idx == last_idx => '.',
            None => ',',
        };
        out_parts.push(format!("{clean_ipa}{suffix}"));
    }

    Ok(out_parts.join(" "))
}

/// Build the bounded session pool for `NeuTTS` engines.
fn build_engine_pool(
    model_path: &Path,
    vocoder_path: &Path,
    pool_size: usize,
    target: ExecutionTarget,
    threads: usize,
) -> Result<SessionPool<NeuTtsEngine>, TtsError> {
    let mut engines = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        engines.push(NeuTtsEngine::load(model_path, vocoder_path, target, threads)?);
    }
    info!(pool_size, threads, "NeuTTS session pool created");
    SessionPool::new(engines, "NeuTTS engines")
        .map_err(|e| TtsError::ConfigError(e.to_string()))
}

/// NeuTTS-Air TTS provider.
pub(crate) struct NeuTtsAir {
    pool: Arc<SessionPool<NeuTtsEngine>>,
    model_path: PathBuf,
    vocoder_path: PathBuf,
    default_voice: String,
    sentence_chunking: bool,
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
            .field("sentence_chunking", &self.sentence_chunking)
            .finish_non_exhaustive()
    }
}

impl NeuTtsAir {
    #[expect(clippy::too_many_arguments, reason = "internal constructor holding all validated fields")]
    pub(crate) fn load(
        model_path: &Path,
        vocoder_path: &Path,
        default_voice: &str,
        sentence_chunking: bool,
        reference_audio: Option<PathBuf>,
        reference_text: Option<String>,
        pool_size: usize,
        threads: usize,
        text_chunking: TextChunkingConfig,
        target: ExecutionTarget,
    ) -> Result<Self, TtsError> {
        let core = Arc::new(ComponentCore::new("tts:neutts", ComponentKind::Tts));
        let pool = Arc::new(build_engine_pool(
            model_path,
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
            sentence_chunking,
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
        NATIVE_SAMPLE_RATE
    }

    fn sentence_chunking(&self) -> bool {
        self.sentence_chunking
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
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let cancel = tokio_util::sync::CancellationToken::new();

        let stream_fut = self.synthesize_stream(req, tx, cancel);
        let collect_fut = async {
            let mut all_samples = Vec::new();
            while let Some(frame) = rx.recv().await {
                all_samples.extend_from_slice(&frame.samples);
            }
            all_samples
        };

        let (stream_res, all_samples) = tokio::join!(stream_fut, collect_fut);
        stream_res?;

        if all_samples.is_empty() {
            return Err(TtsError::InferenceFailed(
                "NeuTTS synthesized no audio samples".into(),
            ));
        }

        Ok(AudioFrame {
            samples: all_samples,
            sample_rate: NATIVE_SAMPLE_RATE,
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
        let text = req.text.to_string();

        // 1. Run phonemization first (does not hold an engine slot!)
        let text_clone = text.clone();
        let phonemes = tokio::task::spawn_blocking(move || {
            phonemize_preserving_punctuation(&text_clone, "en-us")
        })
        .await
        .map_err(|e| TtsError::PhonemizationFailed(format!("phonemize join error: {e}")))?
        ?;

        if cancel.is_cancelled() || phonemes.trim().is_empty() {
            return Ok(());
        }

        // 2. Check out an engine slot (allow up to 30s wait on CPU for preceding sentences)
        let pool = Arc::clone(&self.pool);
        let mut slot = pool
            .checkout_within(std::time::Duration::from_secs(30))
            .await
            .map_err(|e| TtsError::busy(format!("engine pool checkout failed: {e}")))?;

        let cancel_clone = cancel.clone();
        let tx_clone = tx.clone();
        tokio::task::spawn_blocking(move || {
            slot.synthesize(&phonemes, &cancel_clone, &tx_clone)
        })
        .await
        .map_err(|e| TtsError::InferenceFailed(format!("synthesis join error: {e}")))?
        ?;

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
        if let Err(opt_errs) = NeuTtsOptions::read(cfg) {
            errors.extend(opt_errs);
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
            cfg.sentence_chunking,
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
        Some(NATIVE_SAMPLE_RATE)
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
        let default_threads = resolve_default_threads();
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
