//! Kokoro TTS provider -- local ONNX inference with espeak-ng phonemization.
//!
//! Kokoro-82M is a `StyleTTS 2` model producing 24 kHz speech. It supports
//! multiple voices from a single voice-embedding file and per-request
//! overrides (voice, language, speed) via [`SynthesisHints`].
//!
//! A plugin on the SDK: the binary registers [`factory`] and configs select
//! it with `provider = "kokoro"`. It needs `espeak-ng` on `PATH` at run time.

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
mod tokenizer;
mod voices;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument};

use engine::KokoroEngine;
use tokenizer::KokoroVocab;
use voice_pipes_onnx::{EXECUTION_PROVIDER_VAR, ExecutionTarget, SessionPool};
use voice_pipes_sdk::component::{
    Component, ComponentCore, ComponentKind, CoreComponent, DeviceInfo,
};
use voice_pipes_sdk::{
    AudioFrame, ConfigError, SynthesisHints, SynthesisRequest, TextChunkingConfig, TraceEvent,
    TtsConfig, TtsError, TtsProvider, WizardChoice, WizardDeploy, WizardDownload, WizardField,
    WizardLayout,
};
use voices::KokoroVoices;

/// Default speed when no override is given.
const DEFAULT_SPEED: f32 = 1.0;

/// Default number of engine copies in the pool.
const DEFAULT_POOL_SIZE: usize = 2;

/// Default ONNX intra-op thread count per engine session.
const DEFAULT_THREADS: usize = 4;

/// Chunk size for streaming output (~170 ms at 24 kHz).
const STREAM_CHUNK_SAMPLES: usize = 4096;

/// Kokoro voice prefix -> espeak-ng language code.
///
/// American English (`af`/`am`) and any unknown prefix map to `en-us`.
fn language_for_voice(voice: &str) -> &'static str {
    match voice.get(..2) {
        Some("bf" | "bm") => "en-gb",
        Some("jf" | "jm") => "ja",
        Some("zf" | "zm") => "cmn",
        Some("ef" | "em") => "es",
        Some("ff" | "fm") => "fr",
        Some("hf" | "hm") => "hi",
        Some("if" | "im") => "it",
        Some("pf" | "pm") => "pt-br",
        _ => "en-us",
    }
}

/// The error for text that produced no audio, which would otherwise pass
/// as a spoken reply the caller never hears.
fn no_audio() -> TtsError {
    TtsError::InferenceFailed("kokoro returned no audio".into())
}

// ── Engine pool ──────────────────────────────────────────────

/// Build the bounded pool of [`KokoroEngine`]s used for concurrent inference.
fn build_pool(
    model_path: &Path,
    pool_size: usize,
    target: ExecutionTarget,
    threads: usize,
) -> Result<SessionPool<KokoroEngine>, TtsError> {
    let mut engines = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        engines.push(KokoroEngine::load(model_path, target, threads)?);
    }
    info!(pool_size, threads, "Kokoro engine pool created");
    SessionPool::new(engines, "Kokoro engines").map_err(|e| TtsError::ConfigError(e.to_string()))
}

// ── Provider ─────────────────────────────────────────────────

/// Configuration for the Kokoro TTS provider.
#[derive(Debug)]
#[expect(
    clippy::struct_field_names,
    reason = "each field is the default a request's own setting overrides"
)]
pub(crate) struct KokoroConfig {
    /// Default voice name (e.g. `"af_heart"`).
    pub default_voice: String,
    /// Default language code override. When `None`, derived from the voice prefix.
    pub default_language: Option<String>,
    /// Default speed multiplier (0.5-2.0).
    pub default_speed: f32,
}

/// Kokoro-82M TTS provider -- local ONNX inference with multi-voice support.
pub struct KokoroTts {
    pool: Arc<SessionPool<KokoroEngine>>,
    voices: Arc<KokoroVoices>,
    vocab: Arc<KokoroVocab>,
    config: KokoroConfig,
    text_chunking: TextChunkingConfig,
    model_path: PathBuf,
    audit_tx: Option<tokio::sync::mpsc::Sender<TraceEvent>>,
    core: Arc<ComponentCore>,
    device: Device,
}

/// Where the sessions were opened to run, and the setting that asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Device {
    target: ExecutionTarget,
    setting: &'static str,
}

/// The option that picks Kokoro's device ahead of the process-wide one.
const DEVICE_OPTION: &str = "execution_provider";

impl Device {
    /// The `execution_provider` option when it is set, otherwise
    /// `VP_ORT_EP` (`env`), the setting every other local model follows.
    fn resolve(option: Option<&str>, env: Option<&str>) -> Self {
        match option {
            Some(value) => Self {
                target: ExecutionTarget::parse(Some(value)),
                setting: DEVICE_OPTION,
            },
            None => Self {
                target: ExecutionTarget::parse(env),
                setting: EXECUTION_PROVIDER_VAR,
            },
        }
    }

    /// Where a session the shared opener opened on `target` runs (`"cpu"`
    /// or `"cuda"`) and, when CUDA was asked for, which setting asked and,
    /// in a build without CUDA, why it runs on the CPU.
    fn report(self) -> (&'static str, Option<String>) {
        let (runs_on, why) = self.target.effective();
        let detail = match (self.target, why) {
            (ExecutionTarget::Cpu, _) => None,
            (_, Some(why)) => Some(format!("{}=cuda: {why}", self.setting)),
            (_, None) => Some(format!("{}=cuda", self.setting)),
        };
        (runs_on.as_str(), detail)
    }
}

impl std::fmt::Debug for KokoroTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KokoroTts")
            .field("config", &self.config)
            .field("model_path", &self.model_path)
            .finish_non_exhaustive()
    }
}

impl KokoroTts {
    /// Create a new Kokoro TTS provider with the default pool size of 1
    /// (backward-compatible).
    ///
    /// - `model_path`: path to the `.onnx` model file.
    /// - `voices_path`: path to the voices NPZ file (`voices-v1.0.bin`).
    /// - `default_voice`: initial voice to use when no hint is provided.
    pub fn new(
        model_path: &Path,
        voices_path: &Path,
        default_voice: &str,
        text_chunking: TextChunkingConfig,
        target: ExecutionTarget,
    ) -> Result<Self, TtsError> {
        Self::with_pool_size(
            model_path,
            voices_path,
            default_voice,
            DEFAULT_POOL_SIZE,
            text_chunking,
            target,
        )
    }

    /// Create a new Kokoro TTS provider with a specific engine pool size for
    /// concurrent inference, its sessions opened on `target` by the shared
    /// opener (a CUDA target in a build without CUDA runs on the CPU, and
    /// the device report says so).
    pub fn with_pool_size(
        model_path: &Path,
        voices_path: &Path,
        default_voice: &str,
        pool_size: usize,
        text_chunking: TextChunkingConfig,
        target: ExecutionTarget,
    ) -> Result<Self, TtsError> {
        let device = Device {
            target,
            setting: EXECUTION_PROVIDER_VAR,
        };
        let threads = voice_pipes_onnx::intra_threads_from_env(DEFAULT_THREADS);
        Self::load(
            model_path,
            voices_path,
            default_voice,
            pool_size,
            threads,
            text_chunking,
            device,
        )
    }

    /// Load the model and voices, `pool_size` sessions on `device`.
    fn load(
        model_path: &Path,
        voices_path: &Path,
        default_voice: &str,
        pool_size: usize,
        threads: usize,
        text_chunking: TextChunkingConfig,
        device: Device,
    ) -> Result<Self, TtsError> {
        let core = Arc::new(ComponentCore::new("tts:kokoro", ComponentKind::Tts));
        // The opener runs a session where `effective()` says, or fails to
        // open it, so the report states what the pool below actually uses.
        core.set_extra(
            "requested_provider",
            serde_json::json!(device.target.as_str()),
        );
        core.set_extra("active_provider", serde_json::json!(device.report().0));

        let pool = match build_pool(model_path, pool_size, device.target, threads) {
            Ok(p) => p,
            Err(e) => {
                core.mark_unhealthy(format!("model load failed: {e}"));
                return Err(e);
            }
        };
        let voices = match KokoroVoices::load(voices_path) {
            Ok(v) => v,
            Err(e) => {
                core.mark_unhealthy(format!("voices load failed: {e}"));
                return Err(e);
            }
        };
        let vocab = KokoroVocab::load_bundled()?;

        // Validate that the default voice exists.
        voices.get_style(default_voice, 0)?;

        info!(
            model = %model_path.display(),
            voices = %voices_path.display(),
            default_voice,
            voice_count = voices.list_voices().len(),
            style_dim = voices.style_dim(),
            pool_size,
            device = device.report().0,
            "Kokoro TTS loaded"
        );

        Ok(Self {
            pool: Arc::new(pool),
            voices: Arc::new(voices),
            vocab: Arc::new(vocab),
            config: KokoroConfig {
                default_voice: default_voice.to_string(),
                default_language: None,
                default_speed: DEFAULT_SPEED,
            },
            text_chunking,
            model_path: model_path.to_path_buf(),
            audit_tx: None,
            core,
            device,
        })
    }

    /// Synthesize with explicit voice, language, and speed.
    async fn synthesize_inner(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        speed: f32,
    ) -> Result<AudioFrame, TtsError> {
        let t0 = std::time::Instant::now();

        // G2P: text -> IPA -> token IDs. This shells out to espeak
        // (`Command::output()`), which blocks — run it on a blocking thread so
        // it doesn't stall the async runtime (Piper already does this).
        let tokens = {
            let text = text.to_string();
            let language = language.to_string();
            let vocab = Arc::clone(&self.vocab);
            tokio::task::spawn_blocking(move || tokenizer::text_to_tokens(&text, &language, &vocab))
                .await
                .map_err(|e| TtsError::InferenceFailed(format!("g2p task join: {e}")))??
        };
        if tokens.is_empty() {
            return Ok(AudioFrame {
                samples: vec![],
                sample_rate: engine::SAMPLE_RATE,
                channels: 1,
            });
        }

        // Look up voice style embedding for this token count
        let style = self.voices.get_style(voice, tokens.len())?.to_vec();

        // ONNX inference via pool checkout
        let mut pooled = self.pool.checkout().await.map_err(TtsError::busy)?;
        let samples =
            tokio::task::spawn_blocking(move || pooled.get_mut().infer(&tokens, &style, speed))
                .await
                .map_err(|e| TtsError::InferenceFailed(format!("task join: {e}")))?;
        let samples = samples?;

        let audio_duration_s = samples.len() as f32 / engine::SAMPLE_RATE as f32;

        if let Some(tx) = &self.audit_tx {
            let _ = tx.try_send(TraceEvent::new(
                "tts:kokoro",
                "tts_usage",
                serde_json::json!({
                    "chars": text.len(),
                    "audio_duration_s": audio_duration_s,
                    "sample_rate": engine::SAMPLE_RATE,
                    "voice": voice,
                }),
            ));
            let _ = tx.try_send(voice_pipes_sdk::tts::detail_event(
                "tts:kokoro",
                text.len(),
                samples.len(),
                engine::SAMPLE_RATE,
                t0.elapsed(),
                serde_json::json!({ "voice": voice }),
            ));
        }

        Ok(AudioFrame {
            samples,
            sample_rate: engine::SAMPLE_RATE,
            channels: 1,
        })
    }

    /// Resolve effective voice, language, and speed from hints + defaults.
    fn resolve_params(&self, hints: Option<&SynthesisHints>) -> (String, String, f32) {
        let voice = hints
            .and_then(|h| h.voice.as_deref())
            .unwrap_or(&self.config.default_voice)
            .to_string();

        let language = hints
            .and_then(|h| h.language.as_deref())
            .or(self.config.default_language.as_deref())
            .unwrap_or_else(|| language_for_voice(&voice))
            .to_string();

        let speed = hints
            .and_then(|h| h.speed)
            .unwrap_or(self.config.default_speed);

        (voice, language, speed)
    }
}

#[async_trait]
impl TtsProvider for KokoroTts {
    #[instrument(skip_all, fields(provider = "kokoro", text_len = text.len()))]
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let t0 = std::time::Instant::now();
        let (voice, language, speed) = self.resolve_params(None);
        let res = self
            .synthesize_inner(text, &voice, &language, speed)
            .await
            .and_then(|frame| {
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

    #[instrument(skip_all, fields(provider = "kokoro", text_len = req.text.len()))]
    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let text = req.text;
        let hints = req.hints;
        let (voice, language, speed) = self.resolve_params(hints);

        let mut buffer = text;
        // A sentence with nothing to speak is skipped; a request where no
        // sentence had any is an error, as a whole-text call is.
        let mut spoke = false;
        while !buffer.is_empty() {
            if cancel.is_cancelled() {
                return Ok(());
            }
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
                let t_chunk = std::time::Instant::now();
                let frame = match self
                    .synthesize_inner(sentence, &voice, &language, speed)
                    .await
                {
                    Ok(f) if f.samples.is_empty() => {
                        buffer = rest;
                        continue;
                    }
                    Ok(f) => {
                        self.core.record_ok(t_chunk.elapsed());
                        f
                    }
                    Err(e) => {
                        self.core.record_error(&e);
                        return Err(e);
                    }
                };
                spoke = true;

                for chunk in frame.samples.chunks(STREAM_CHUNK_SAMPLES) {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    let chunk_frame = AudioFrame {
                        samples: chunk.to_vec(),
                        sample_rate: frame.sample_rate,
                        channels: frame.channels,
                    };
                    if tx.send(chunk_frame).await.is_err() {
                        tracing::debug!("kokoro synthesize_stream downstream closed");
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

    fn sample_rate(&self) -> u32 {
        engine::SAMPLE_RATE
    }

    fn name(&self) -> &'static str {
        "kokoro"
    }

    fn supported_inline_tags(&self) -> &'static [&'static str] {
        &["/ipa/"]
    }

    async fn device_info(&self) -> Option<DeviceInfo> {
        let (runs_on, detail) = self.device.report();
        Some(DeviceInfo::config(runs_on, detail))
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

/// Factory for the Kokoro TTS provider.
#[derive(Debug)]
pub(crate) struct KokoroFactory;

impl voice_pipes_sdk::TtsFactory for KokoroFactory {
    fn output_sample_rate(&self, _cfg: &voice_pipes_sdk::TtsConfig) -> Option<u32> {
        Some(engine::SAMPLE_RATE)
    }

    fn name(&self) -> &'static str {
        "kokoro"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<ConfigError> {
        let mut errs = vec![];
        if cfg.model.is_none() {
            errs.push(ConfigError::new(
                "tts.model",
                "model path required for kokoro",
            ));
        }
        if let Err(problems) = KokoroOptions::read(cfg) {
            errs.extend(problems);
        }
        errs
    }

    fn build(
        &self,
        cfg: &TtsConfig,
        ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let audit_tx = ctx.audit_tx.clone();
        let text_chunking = &ctx.text_chunking;
        let model_path = cfg
            .model
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("tts.model required for kokoro"))?;
        let KokoroOptions {
            voices_path,
            pool_size,
            threads,
            device,
        } = KokoroOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let default_voice = cfg.voice.as_deref().unwrap_or("af_heart");

        let mut tts = KokoroTts::load(
            model_path,
            Path::new(&voices_path),
            default_voice,
            pool_size,
            threads,
            text_chunking.clone(),
            device,
        )?;
        tts.audit_tx = audit_tx;
        Ok(Box::new(tts))
    }

    fn provider_schema(&self, cfg: &TtsConfig) -> voice_pipes_sdk::ProviderSchema {
        schema::schema(cfg)
    }
}

/// The settings Kokoro reads from its option table.
struct KokoroOptions {
    voices_path: String,
    pool_size: usize,
    threads: usize,
    device: Device,
}

impl KokoroOptions {
    fn read(cfg: &TtsConfig) -> Result<Self, Vec<ConfigError>> {
        let mut o = cfg.options();
        let voices = o.string("voices");
        let voices_path = voices.or_else(|| o.string("voices_path"));
        if voices_path.is_none() {
            o.error("voices", "voices path required for kokoro");
        }
        let pool_size = o.usize_in("pool_size", 1..=128, DEFAULT_POOL_SIZE);
        let default_threads = voice_pipes_onnx::intra_threads_from_env(DEFAULT_THREADS);
        let threads = o.usize_in("threads", 0..=256, default_threads);
        let device_option = o
            .value(DEVICE_OPTION)
            .is_some()
            .then(|| o.one_of(DEVICE_OPTION, &["cpu", "cuda", "CPU", "CUDA"], "cpu"));
        let env = std::env::var(EXECUTION_PROVIDER_VAR).ok();
        o.finish()?;
        Ok(Self {
            voices_path: voices_path.unwrap_or_default(),
            pool_size,
            threads,
            device: Device::resolve(device_option.as_deref(), env.as_deref()),
        })
    }
}

/// Where the wizard fetches the model and the voice embeddings from.
const MODEL_RELEASE: &str =
    "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0";

/// What a binary registers to make `provider = "kokoro"` available:
/// `voice_pipes::tts::register_tts_factory(voice_pipes_tts_kokoro::factory)`.
pub fn factory() -> Box<dyn voice_pipes_sdk::TtsFactory> {
    Box::new(KokoroFactory)
}

/// How this provider offers itself in the setup wizard.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    vec![
        WizardChoice::new(
            "Kokoro (Local ONNX)",
            WizardDeploy::Download {
                layout: WizardLayout::File,
                files: vec![
                    WizardDownload::new(
                        "kokoro-v1.0.onnx",
                        &format!("{MODEL_RELEASE}/kokoro-v1.0.onnx"),
                    ),
                    WizardDownload::new(
                        "voices-v1.0.bin",
                        &format!("{MODEL_RELEASE}/voices-v1.0.bin"),
                    ),
                ],
            },
        )
        .with_feature(Some("tts-kokoro".into()))
        .with_fields(vec![
            WizardField::path("model", "Kokoro ONNX model path", "models/kokoro-v1.0.onnx"),
            WizardField::fixed("voices", "models/voices-v1.0.bin"),
        ])
        .with_menu_rank(20),
    ]
}

#[cfg(test)]
#[path = "tests/kokoro.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
