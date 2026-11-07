//! The `SttProvider` and `SttFactory` a pipeline sees.
//!
//! Everything here is what the SDK asks of a recogniser and nothing more: the
//! engine's plugin wrapper adapts it into the pipeline, the health registry,
//! the metrics and the probes. The model knows nothing of sessions; the pool
//! from `voice-pipes-onnx` lends one set of ONNX sessions per live stream,
//! the same way the built-in ONNX providers work.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use voice_pipes_onnx::{PooledSlot, SessionPool};
use voice_pipes_sdk::component::{
    ComponentCore, ComponentKind, CoreComponent, DeviceInfo, ModelIdentity,
};
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};
use voice_pipes_sdk::{
    AudioChunk, AudioFrame, ConfigError, InferenceMetrics, RecognitionRequest, SttCapability,
    SttConfig, SttError, SttFactory, SttProvider, TranscriptionResult, WarmUpScope, WizardChoice,
    WizardDeploy, WizardField,
};

use crate::engine::{Engine, EngineError, Sessions};
use crate::features::{SAMPLE_RATE, SAMPLE_RATE_HZ};
use crate::model::{CHUNK_MS, ModelConfig, ModelError, Tokens};

pub const PROVIDER_NAME: &str = "nemotron-3.5";
const DEFAULT_POOL_SIZE: usize = 2;
const DEFAULT_THREADS: usize = 4;
/// The locale prompt when the pipeline names none and the session sends
/// none. The model's own default is `auto`; a session should name its
/// locale, since `auto` costs accuracy on short utterances.
const DEFAULT_LOCALE: &str = "auto";
/// Silence appended to every utterance before the engine is flushed.
///
/// The model emits a token only once it has seen speech end: flushing the
/// partial last chunk with zeros (what the engine and the reference engine
/// do) is not enough, and a short utterance can decode to nothing while the
/// same audio with 100 ms of silence behind it decodes in full. On long
/// recordings the loss is the last word or so, which a WER over minutes of
/// audio does not show; on a one-word turn it is the turn. 400 ms is well
/// above the 100 ms that recovers the fixtures, at the cost of one or two
/// extra encoder chunks per utterance.
const TAIL_SILENCE_MS: usize = 400;

fn tail_silence() -> Vec<f32> {
    vec![0.0; SAMPLE_RATE * TAIL_SILENCE_MS / 1000]
}

/// The text without punctuation carried over from the utterance before.
///
/// The model decides a sentence's closing mark only once it hears what
/// follows: a question ended by silence comes back without its "?", and
/// the "?" is emitted when the next utterance's first chunk runs, where it
/// would open that utterance's transcript. More silence at the boundary
/// does not bring it forward (tried up to 1.2 s). The previous utterance
/// has already been committed, so the mark can only be dropped. Only the
/// marks a sentence ends with are stripped: an opening "¿" belongs to the
/// utterance it starts.
fn without_carried_punctuation(text: &str) -> &str {
    text.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '.' | '?' | '!' | ',' | ';' | ':' | '…')
    })
}

impl From<EngineError> for SttError {
    fn from(e: EngineError) -> Self {
        SttError::InferenceFailed(e.to_string())
    }
}

impl From<ModelError> for SttError {
    fn from(e: ModelError) -> Self {
        SttError::ModelLoadFailed(e.to_string())
    }
}

/// One session set, lent by the pool, as the engine's session access.
impl crate::engine::SessionsAccess for PooledSlot<Sessions> {
    fn sessions(&mut self) -> &mut Sessions {
        self.get_mut()
    }
}

pub struct Nemotron35Stt {
    pool: Arc<SessionPool<Sessions>>,
    cfg: Arc<ModelConfig>,
    tokens: Arc<Tokens>,
    default_prompt: i64,
    default_locale: String,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for Nemotron35Stt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nemotron35Stt")
            .field("pool_size", &self.pool.size())
            .field("default_locale", &self.default_locale)
            .finish_non_exhaustive()
    }
}

impl Nemotron35Stt {
    pub fn load(
        model_dir: &Path,
        pool_size: usize,
        threads: usize,
        locale: &str,
    ) -> Result<Self, SttError> {
        let core = Arc::new(ComponentCore::new("stt:nemotron-3.5", ComponentKind::Stt));
        let loaded = (|| -> Result<Self, SttError> {
            let cfg = ModelConfig::load(model_dir)?;
            cfg.validate_files(model_dir)?;
            let tokens = Tokens::load(model_dir, &cfg)?;
            let default_prompt = cfg.prompt_for(locale).ok_or_else(|| {
                SttError::ConfigError(format!("the model has no locale {locale:?}"))
            })?;
            let mut sets = Vec::with_capacity(pool_size);
            for _ in 0..pool_size {
                sets.push(Sessions::open(model_dir, &cfg, threads)?);
            }
            tracing::info!(
                model_dir = %model_dir.display(),
                pool_size,
                threads,
                chunk_ms = CHUNK_MS,
                locale,
                "Nemotron 3.5 loaded"
            );
            Ok(Self {
                pool: Arc::new(
                    SessionPool::new(sets, "Nemotron 3.5 session sets")
                        .map_err(|e| SttError::ConfigError(e.to_string()))?,
                ),
                cfg: Arc::new(cfg),
                tokens: Arc::new(tokens),
                default_prompt,
                default_locale: locale.to_string(),
                core: Arc::clone(&core),
            })
        })();
        if let Err(e) = &loaded {
            core.mark_unhealthy(format!("model load failed: {e}"));
        }
        loaded
    }

    /// The prompt id for a session: its requested locale when the model has
    /// it, else the pipeline's default. A locale the model lacks is logged
    /// and falls back rather than failing the session, since the caller's
    /// language is a hint, not a contract.
    fn prompt_for_session(&self, requested: Option<&str>) -> (i64, String) {
        let Some(l) = requested else {
            return (self.default_prompt, self.default_locale.clone());
        };
        if let Some(id) = self.cfg.prompt_for(l) {
            (id, l.to_string())
        } else {
            tracing::warn!(locale = l, default = %self.default_locale, "unknown locale; using the default");
            (self.default_prompt, self.default_locale.clone())
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one result from one decode step; each argument is a different measurement of it"
    )]
    fn result(
        &self,
        tokens: &[u32],
        tags: &[u32],
        chunks_run: u32,
        is_final: bool,
        segment_index: u32,
        locale: &str,
        speech_secs: f64,
        total_audio_secs: f64,
        inference_wall: Duration,
    ) -> TranscriptionResult {
        let (text, _) = self.tokens.decode(tokens);
        let text = without_carried_punctuation(&text).to_string();
        // Under `auto` the model's first tag is its detection; under a fixed
        // locale it is an echo. Either way it is what the model believed.
        let language = tags
            .first()
            .and_then(|&id| self.tokens.locale_of(id))
            .map(str::to_string)
            .or_else(|| (locale != DEFAULT_LOCALE).then(|| locale.to_string()));
        let inference_metrics = is_final.then(|| {
            InferenceMetrics::measured(total_audio_secs, inference_wall, chunks_run)
                // The chunk plus the lookahead it was exported with, in audio time.
                .with_delay_floor_ms(u64::from(CHUNK_MS))
                .with_streaming_window_ms(Some(CHUNK_MS))
        });
        TranscriptionResult::new(text, is_final)
            .with_language(language)
            .with_segment_index(segment_index)
            .with_start_time(Some(Duration::ZERO))
            .with_end_time(Some(Duration::from_secs_f64(speech_secs)))
            .with_inference_metrics(inference_metrics)
    }
}

use voice_pipes_sdk::audio::i16_to_f32 as to_f32;

#[async_trait]
impl SttProvider for Nemotron35Stt {
    async fn transcribe(&self, audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = Instant::now();
        if audio.sample_rate != SAMPLE_RATE_HZ || audio.channels != 1 {
            return Err(SttError::InvalidAudio(format!(
                "expected {SAMPLE_RATE} Hz mono, got {} Hz {} ch",
                audio.sample_rate, audio.channels
            )));
        }
        let slot = self.pool.checkout().await.map_err(SttError::busy)?;
        let pcm = to_f32(&audio.samples);
        let pcm_len = pcm.len();
        let cfg = Arc::clone(&self.cfg);
        let tags = self.tokens.locale_tag_ids();
        let prompt = self.default_prompt;
        let run = tokio::task::spawn_blocking(
            move || -> Result<(Engine<PooledSlot<Sessions>>, usize), EngineError> {
                let mut engine = Engine::new(slot, &cfg, prompt, tags)?;
                engine.accept(&pcm)?;
                let tail = tail_silence();
                let tail_len = tail.len();
                engine.accept(&tail)?;
                let pad = engine.finish()?;
                Ok((engine, tail_len + pad))
            },
        )
        .await
        .map_err(|e| SttError::InferenceFailed(format!("task join: {e}")))?;
        match run {
            Ok((engine, extra_samples)) => {
                let wall = start.elapsed();
                self.core.record_ok(wall);
                let speech_secs = pcm_len as f64 / SAMPLE_RATE as f64;
                let total_audio_secs = (pcm_len + extra_samples) as f64 / SAMPLE_RATE as f64;
                Ok(self.result(
                    engine.tokens(),
                    engine.locale_tags_seen(),
                    engine.chunks_run(),
                    true,
                    0,
                    &self.default_locale,
                    speech_secs,
                    total_audio_secs,
                    wall,
                ))
            }
            Err(e) => {
                self.core.record_error(&e);
                Err(e.into())
            }
        }
    }

    async fn transcribe_stream(
        &self,
        request: RecognitionRequest,
        mut audio_rx: tokio::sync::mpsc::Receiver<AudioChunk>,
        tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        // One set of sessions and one engine for the whole stream; the
        // engine owns the slot and moves into each blocking run and back,
        // so a panic there drops it and returns the sessions to the pool.
        // The engine is never reset between utterances: its caches carry
        // each utterance into the next as acoustic context, and only the
        // token stream is cut at a boundary.
        let slot = match request.slot_wait {
            Some(wait) => self.pool.checkout_within(wait).await,
            None => self.pool.checkout().await,
        }
        .map_err(SttError::busy)?;
        let (prompt, locale) = self.prompt_for_session(request.language.as_deref());
        let tags = self.tokens.locale_tag_ids();
        let cfg = Arc::clone(&self.cfg);
        let mut engine = Some(Engine::new(slot, &cfg, prompt, tags.clone()).map_err(|e| {
            self.core.record_error(&e);
            SttError::from(e)
        })?);
        let mut segment_index: u32 = 0;
        let mut last_emitted = String::new();
        let mut audio_samples: usize = 0;
        let mut utterance_chunks_run: u32 = 0;
        let mut inference_wall = Duration::ZERO;
        let mut emitted_final = false;

        loop {
            let recv = tokio::select! {
                () = cancel.cancelled() => break,
                r = audio_rx.recv() => r,
            };
            let (mut pcm, is_final) = if let Some(chunk) = recv {
                // Take what is already queued, so a slow decode catches up,
                // but only that: a sender that refills the queue as fast as
                // it drains would otherwise hand over a whole utterance in
                // one run and no interim result would come out. Never past
                // an utterance boundary: the chunk after `is_last` belongs
                // to the next utterance and its own state.
                let mut pcm = to_f32(&chunk.samples);
                let mut is_last = chunk.is_last;
                for _ in 0..audio_rx.len() {
                    if is_last {
                        break;
                    }
                    let Ok(extra) = audio_rx.try_recv() else {
                        break;
                    };
                    pcm.extend(to_f32(&extra.samples));
                    is_last = extra.is_last;
                }
                (pcm, is_last)
            } else {
                // Input closed: finalise what is buffered, unless a boundary
                // already went out and nothing came after it.
                if audio_samples == 0 && emitted_final {
                    break;
                }
                (Vec::new(), true)
            };
            audio_samples += pcm.len();
            let Some(mut eng) = engine.take() else {
                return Err(SttError::InferenceFailed(
                    "the stream lost its engine between runs".into(),
                ));
            };
            let before_chunks = eng.chunks_run();
            let t0 = Instant::now();
            let (eng, res) = tokio::task::spawn_blocking(move || {
                let res = (|| -> Result<Option<(Vec<u32>, Vec<u32>, usize)>, EngineError> {
                    if !pcm.is_empty() {
                        eng.accept(&pcm)?;
                    }
                    pcm.clear();
                    if is_final {
                        // Silence behind the speech is what lets the model
                        // emit its last word; then the utterance closes on
                        // a chunk boundary and its tokens come out.
                        let tail = tail_silence();
                        let tail_len = tail.len();
                        eng.accept(&tail)?;
                        let (tokens, tags, pad) = eng.end_utterance()?;
                        return Ok(Some((tokens, tags, tail_len + pad)));
                    }
                    Ok(None)
                })();
                (eng, res)
            })
            .await
            .map_err(|e| SttError::InferenceFailed(format!("task join: {e}")))?;
            inference_wall += t0.elapsed();
            let after_chunks = eng.chunks_run();
            utterance_chunks_run += after_chunks.saturating_sub(before_chunks);
            let closed = match res {
                Ok(closed) => closed,
                Err(e) => {
                    self.core.record_error(&e);
                    return Err(e.into());
                }
            };
            let speech_secs = audio_samples as f64 / SAMPLE_RATE as f64;
            let (tokens, tags, extra_samples): (&[u32], &[u32], usize) = match &closed {
                Some((t, g, extra)) => (t, g, *extra),
                None => (eng.tokens(), eng.locale_tags_seen(), 0),
            };
            let total_audio_secs = (audio_samples + extra_samples) as f64 / SAMPLE_RATE as f64;
            let (text, _) = self.tokens.decode(tokens);
            // A carried mark alone is no interim worth sending.
            let text = without_carried_punctuation(&text).to_string();
            if is_final || text != last_emitted {
                last_emitted = text;
                let r = self.result(
                    tokens,
                    tags,
                    utterance_chunks_run.max(1),
                    is_final,
                    segment_index,
                    &locale,
                    speech_secs,
                    total_audio_secs,
                    inference_wall,
                );
                if tx.send(r).await.is_err() {
                    return Ok(());
                }
            }
            if is_final {
                self.core.record_ok(inference_wall);
                emitted_final = true;
                segment_index += 1;
                last_emitted.clear();
                audio_samples = 0;
                utterance_chunks_run = 0;
                inference_wall = Duration::ZERO;
                engine = Some(eng);
                if recv_closed(&audio_rx) {
                    break;
                }
            } else {
                engine = Some(eng);
            }
        }
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        SAMPLE_RATE_HZ
    }

    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// One decode of silence, so the runtime's lazy start-up is paid here.
    async fn warm_up(&self) -> Result<(), SttError> {
        let silence = AudioFrame {
            samples: vec![0i16; SAMPLE_RATE],
            sample_rate: SAMPLE_RATE_HZ,
            channels: 1,
        };
        self.transcribe(&silence).await.map(|_| ())
    }

    /// Holds a pool set while it warms, so once at worker start, not per
    /// session against a caller's first utterance.
    fn warm_up_scope(&self) -> WarmUpScope {
        WarmUpScope::PerProcess
    }

    fn concurrent_capacity(&self) -> Option<usize> {
        Some(self.pool.size())
    }

    async fn model_identity(&self) -> Option<ModelIdentity> {
        Some(
            ModelIdentity::from_config(self.cfg.base_model.clone())
                .with_version(Some(format!("onnx fp16, {CHUNK_MS} ms chunk"))),
        )
    }

    async fn device_info(&self) -> Option<DeviceInfo> {
        let (runs_on, detail) = voice_pipes_onnx::ExecutionTarget::from_env().device_report();
        Some(DeviceInfo::config(runs_on, detail))
    }

    fn needs_flush_silence(&self) -> bool {
        // The final chunk is zero-padded by the engine itself.
        false
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        [
            SttCapability::Streaming,
            SttCapability::InterimResults,
            SttCapability::LanguageDetection,
        ]
        .into_iter()
        .collect()
    }

    fn as_component(&self) -> Option<Arc<dyn voice_pipes_sdk::Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

/// Whether the audio channel has closed, without taking a message from it.
fn recv_closed(rx: &tokio::sync::mpsc::Receiver<AudioChunk>) -> bool {
    rx.is_closed() && rx.is_empty()
}

// ── Factory ─────────────────────────────────────────────────

#[derive(Debug)]
pub struct Nemotron35Factory;

/// The settings Nemotron 3.5 reads from its option table.
struct N35Options {
    language: Option<String>,
    pool_size: usize,
    threads: usize,
}

impl N35Options {
    fn read(cfg: &SttConfig) -> Result<Self, Vec<ConfigError>> {
        let mut o = cfg.options();
        let options = Self {
            language: o.string("language"),
            // Session sets loaded, at least one.
            pool_size: o.usize_in("pool_size", 1..=64, DEFAULT_POOL_SIZE),
            // Threads per ONNX session, 0 for ONNX Runtime's own choice.
            threads: o.usize_in("threads", 0..=256, DEFAULT_THREADS),
        };
        o.finish()?;
        Ok(options)
    }
}

fn model_dir_of(cfg: &SttConfig) -> Result<PathBuf, ConfigError> {
    cfg.model_dir.clone().ok_or_else(|| {
        ConfigError::new(
            "model_dir",
            format!("{PROVIDER_NAME} needs the directory holding the ONNX export"),
        )
    })
}

impl SttFactory for Nemotron35Factory {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn aliases(&self) -> &[&str] {
        &["nemotron35", "nemotron-3-5"]
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError> {
        let (language, mut errs) = match N35Options::read(cfg) {
            Ok(options) => (options.language, Vec::new()),
            Err(problems) => (None, problems),
        };
        let dir = match model_dir_of(cfg) {
            Ok(d) => d,
            Err(e) => {
                errs.push(e);
                return errs;
            }
        };
        match ModelConfig::load(&dir) {
            Err(e) => errs.push(ConfigError::new("model_dir", e.to_string())),
            Ok(model) => {
                if let Err(e) = model.validate_files(&dir) {
                    errs.push(ConfigError::new("model_dir", e.to_string()));
                }
                if let Some(l) = language.as_deref()
                    && model.prompt_for(l).is_none()
                {
                    errs.push(ConfigError::new(
                        "language",
                        format!("the model has no locale {l:?}; use a tag such as hr-HR, or auto"),
                    ));
                }
            }
        }
        errs
    }

    /// What the graph editor and `inspect` show. Everything is static: the
    /// model directory and the pool are fixed at load, and the locale
    /// changes per session through the request, not through the editor.
    fn provider_schema(&self, cfg: &SttConfig) -> ProviderSchema {
        let o = &cfg.options;
        let mut s = ProviderSchema::default();
        s.push(
            ParamDescriptor::static_str(
                "provider",
                "Provider",
                "The STT framework or service used for transcription.",
            ),
            cfg.provider.clone(),
        );
        s.push(
            ParamDescriptor::static_path(
                "model_dir",
                "Model directory",
                "Directory holding the Nemotron 3.5 ONNX export (config, tokens, \
                 decoder, joiner, the two 320 ms encoders and their weights).",
            ),
            cfg.model_dir.as_ref().map_or(serde_json::Value::Null, |p| {
                serde_json::json!(p.display().to_string())
            }),
        );
        s.push(
            ParamDescriptor::static_str(
                "language",
                "Default locale",
                "BCP-47 locale the model decodes with when the session names none \
                 (e.g. hr-HR, en-US); `auto` detects, at a cost on short utterances.",
            ),
            o.get("language")
                .cloned()
                .unwrap_or_else(|| serde_json::json!(DEFAULT_LOCALE)),
        );
        s.push(
            ParamDescriptor::static_int_display(
                "pool_size",
                "Session pool",
                "ONNX session sets loaded at start, one per concurrent session; \
                 each is a full copy of the model in memory.",
            ),
            o.get("pool_size")
                .cloned()
                .unwrap_or_else(|| serde_json::json!(DEFAULT_POOL_SIZE)),
        );
        s.push(
            ParamDescriptor::static_int_display(
                "threads",
                "Threads",
                "Intra-op threads per ONNX session; 0 lets ONNX Runtime choose.",
            ),
            o.get("threads")
                .cloned()
                .unwrap_or_else(|| serde_json::json!(DEFAULT_THREADS)),
        );
        s.passthrough_options(
            o,
            &["language", "pool_size", "threads"],
            "Nemotron 3.5 provider-specific option.",
        );
        s
    }

    fn build(
        &self,
        cfg: &SttConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let dir = model_dir_of(cfg).map_err(|e| anyhow::anyhow!("{e}"))?;
        let N35Options {
            language,
            pool_size,
            threads,
        } = N35Options::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let locale = language.as_deref().unwrap_or(DEFAULT_LOCALE);
        let provider = Nemotron35Stt::load(&dir, pool_size, threads, locale)?;
        Ok(Box::new(provider))
    }
}

/// What a binary registers: `voice_pipes::stt::register_stt_factory(factory)`.
///
/// The plugin does not call the engine itself — that would make it depend
/// on the engine crate, which is what a plugin exists not to do. The binary
/// that composes the two does the registering.
pub fn factory() -> Box<dyn SttFactory> {
    Box::new(Nemotron35Factory)
}

/// The setup wizard's entry: where the export lives and which locale a
/// session assumes when it names none.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    vec![
        WizardChoice::new(
            "Nemotron 3.5 (Local ONNX plugin, 40 languages incl. Croatian)",
            WizardDeploy::Registry {
                key: "nemotron-3.5".into(),
            },
        )
        .with_feature(Some("stt-nemotron35".into()))
        .with_fields(vec![
            WizardField::path(
                "model_dir",
                "Nemotron 3.5 model directory path",
                "models/nemotron-3.5",
            ),
            WizardField::text(
                "language",
                "Default locale (BCP-47 such as hr-HR, en-US, or auto)",
                DEFAULT_LOCALE,
            ),
        ])
        .with_notes(vec![
            "Nemotron 3.5 runs locally via ONNX Runtime (10 files, ~1.3 GB download from Hugging Face).".into(),
            "Can also be pre-fetched with `python scripts/download_nemotron35.py`.".into(),
            "For English only, the English Nemotron is the more accurate choice.".into(),
        ])
        .with_menu_rank(20),
    ]
}

fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<HashMap<String, (Vec<i64>, Vec<f32>)>>();
}

#[cfg(test)]
mod tests {
    use super::without_carried_punctuation;

    /// The live shape: the previous question's "?" and the two spaces the
    /// decoder puts after it open the next utterance.
    #[test]
    fn a_closing_mark_carried_from_the_previous_utterance_is_dropped() {
        assert_eq!(
            without_carried_punctuation("?  Can you take me to the settings page"),
            "Can you take me to the settings page"
        );
        assert_eq!(without_carried_punctuation("?"), "");
        assert_eq!(without_carried_punctuation(". , hello"), "hello");
    }

    #[test]
    fn an_opening_mark_and_a_mark_inside_the_text_stay() {
        assert_eq!(
            without_carried_punctuation("¿Qué hora es?"),
            "¿Qué hora es?"
        );
        assert_eq!(without_carried_punctuation("Yes, I do."), "Yes, I do.");
        assert_eq!(without_carried_punctuation(""), "");
    }

    #[test]
    fn inference_metrics_computes_effective_latency_and_uses_total_audio_time() {
        use crate::model::CHUNK_MS;
        use std::time::Duration;
        use voice_pipes_sdk::InferenceMetrics;

        // 1.0s speech + 0.4s tail silence = 1.4s total processed in 350ms wall time
        let total_audio_secs = 1.4;
        let inference_wall = Duration::from_millis(350);
        let chunks_run = 4;
        let m = InferenceMetrics::measured(total_audio_secs, inference_wall, chunks_run)
            .with_delay_floor_ms(u64::from(CHUNK_MS))
            .with_streaming_window_ms(Some(CHUNK_MS));

        assert_eq!(m.audio_duration_ms, Some(1400));
        assert_eq!(m.inference_count, Some(4));
        assert_eq!(m.algorithmic_delay_ms, Some(u64::from(CHUNK_MS)));
        assert_eq!(m.streaming_window_ms, Some(CHUNK_MS));
        assert!(m.rtfx.is_some());
        let rtfx = m.rtfx.unwrap();
        // 1.4 / 0.35 = 4.0
        assert!((rtfx - 4.0).abs() < 1e-4);
        // effective latency = delay * (1 + 1/rtfx) = 320 * (1 + 0.25) = 400
        assert_eq!(m.effective_latency_ms, Some(400));
    }
}
