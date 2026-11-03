//! NVIDIA Parakeet TDT speech-to-text -- local ONNX inference.
//!
//! A voice-pipes recogniser plugin: the binary registers [`factory`] and
//! [`wizard_choices`]; `[stt] provider = "parakeet-tdt"` selects it. The
//! voice-pipes binary also registers it as the recogniser a pipeline
//! without `[stt]` falls back to.

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
mod preprocessing;
mod schema;
mod wizard;

pub use wizard::wizard_choices;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument};

use voice_pipes_onnx::SessionPool;
use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::audio::samples_in_ms;
use voice_pipes_sdk::component::{
    Component, ComponentCore, ComponentKind, CoreComponent, DeviceInfo,
};
use voice_pipes_sdk::options::as_f32;
use voice_pipes_sdk::stt::WarmUpScope;
use voice_pipes_sdk::{
    AudioChunk, ConfigError, InferenceMetrics, RecognitionRequest, SttCapability, SttConfig,
    SttError, SttFactory, SttProvider, TranscriptionResult, WordInfo,
};

use decoder::TdtDecoder;
use encoder::ParakeetEncoder;
use preprocessing::MelPreprocessor;

// ── Session pool ───────────────────────────────────────────────────

/// Threads for one operator in each of Parakeet's sessions.
const INTRA_THREADS: usize = 4;

/// A model output the code could not read (too few outputs, a negative
/// length), as an inference failure.
fn model_output(e: voice_pipes_onnx::ModelOutputError) -> SttError {
    SttError::InferenceFailed(e.to_string())
}

/// Open one of the model's graphs through the shared opener, on the device
/// `VP_ORT_EP` names (the CPU unless it asks for CUDA in a CUDA build).
fn open_session(path: &Path) -> Result<ort::session::Session, SttError> {
    let options = voice_pipes_onnx::SessionOptions::new(INTRA_THREADS)
        .with_target(voice_pipes_onnx::ExecutionTarget::from_env());
    voice_pipes_onnx::open_session(path, &options)
        .map_err(|e| SttError::ModelLoadFailed(e.to_string()))
}

/// Which export of the encoder and decoder graphs to load.
///
/// The exports share one directory layout and differ only in the file
/// names: the int8 export is `encoder-model.int8.onnx` and
/// `decoder_joint-model.int8.onnx`, the fp32 export `encoder-model.onnx`
/// (with its `encoder-model.onnx.data` weights beside it, which the runtime
/// finds on its own) and `decoder_joint-model.onnx`. The preprocessor and
/// vocabulary are the same files for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Precision {
    /// Dynamically quantised weights: a quarter of the size, and the only
    /// export the wizard downloads.
    Int8,
    /// Full-precision weights: what a fine-tune was evaluated at, where
    /// quantisation costs accuracy (several WER points on the Greek
    /// fine-tune) without buying speed on CPU.
    Fp32,
}

impl Precision {
    /// The option values `precision` accepts.
    const NAMES: [&'static str; 2] = ["int8", "fp32"];

    fn from_name(name: &str) -> Self {
        match name {
            "fp32" => Self::Fp32,
            _ => Self::Int8,
        }
    }

    fn encoder_file(self) -> &'static str {
        match self {
            Self::Int8 => "encoder-model.int8.onnx",
            Self::Fp32 => "encoder-model.onnx",
        }
    }

    fn decoder_file(self) -> &'static str {
        match self {
            Self::Int8 => "decoder_joint-model.int8.onnx",
            Self::Fp32 => "decoder_joint-model.onnx",
        }
    }
}

/// One complete set of ONNX sessions for the Parakeet pipeline.
struct SessionSet {
    preprocessor: MelPreprocessor,
    encoder: ParakeetEncoder,
    decoder: TdtDecoder,
}

impl SessionSet {
    fn new(model_dir: &Path, precision: Precision) -> Result<Self, SttError> {
        Ok(Self {
            preprocessor: MelPreprocessor::new(&model_dir.join("nemo128.onnx"))?,
            encoder: ParakeetEncoder::new(&model_dir.join(precision.encoder_file()))?,
            decoder: TdtDecoder::new(
                &model_dir.join(precision.decoder_file()),
                &model_dir.join("vocab.txt"),
            )?,
        })
    }
}

/// Build the bounded pool of `SessionSet`s used for concurrent inference.
fn build_pool(
    model_dir: &Path,
    precision: Precision,
    pool_size: usize,
) -> Result<SessionPool<SessionSet>, SttError> {
    let mut sets = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        sets.push(SessionSet::new(model_dir, precision)?);
    }
    info!(pool_size, "ONNX session pool created");
    SessionPool::new(sets, "Parakeet session sets")
        .map_err(|e| SttError::ConfigError(e.to_string()))
}

// ── ParakeetStt provider ───────────────────────────────────────────

/// Default number of session sets in the pool.
const DEFAULT_POOL_SIZE: usize = 2;

/// The language reported when neither the config nor the request names
/// one: the v2 export the wizard downloads is English-only.
const DEFAULT_LANGUAGE: &str = "en-US";

/// Make the transcript read as the language's own text where the
/// tokenizer could not: Greek writes its question mark as `;`, but the
/// base vocabulary carries only the Latin `?`, so the Greek fine-tune
/// emits that and its model card asks for the swap.
fn localise(text: &mut String, words: &mut [WordInfo], language: &str) {
    let primary = language
        .split(['-', '_'])
        .next()
        .unwrap_or(language)
        .to_ascii_lowercase();
    if primary == "el" && text.contains('?') {
        *text = text.replace('?', ";");
        for w in words.iter_mut().filter(|w| w.word.contains('?')) {
            w.word = w.word.replace('?', ";");
        }
    }
}

/// Default cap on the audio window re-encoded per streaming inference
/// step (ms). `0.0` disables the cap (re-encodes the full utterance,
/// quadratic on long turns -- the historical behaviour).
///
/// Default chosen to comfortably exceed the 5.6 s history threshold the
/// Microsoft on-device streaming ASR paper (arXiv:2604.14493v2  5.2)
/// identifies as the accuracy-critical lookback window for cache-aware
/// transducers, while bounding CPU cost for long turns.
const DEFAULT_STREAMING_WINDOW_MS: f32 = 8000.0;

/// NVIDIA Parakeet TDT speech-to-text provider -- local ONNX inference.
///
/// Uses a pool of ONNX session sets for bounded-concurrency inference:
/// 1. Log-mel spectrogram preprocessor (nemo128.onnx)
/// 2. `FastConformer` encoder (encoder-model[.int8].onnx)
/// 3. TDT decoder + joint network (decoder_joint-model[.int8].onnx)
pub(crate) struct ParakeetStt {
    pool: Arc<SessionPool<SessionSet>>,
    /// The language reported on results when the request names none. The
    /// model has no language head: this is the operator's statement of
    /// what the export in `model_dir` was trained on.
    pub language: String,
    pub min_initial_buffer_ms: f32,
    pub min_inference_step_ms: f32,
    /// Bound on the audio window re-encoded per streaming inference step
    /// (ms). `0.0` = unbounded (the whole utterance is re-encoded every
    /// step, O(N²) over the utterance). Partials emitted
    /// while bounded reflect only the trailing window; the final pass at
    /// `is_last` always runs over the full utterance buffer.
    pub streaming_window_ms: f32,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for ParakeetStt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParakeetStt")
            .field("language", &self.language)
            .field("min_initial_buffer_ms", &self.min_initial_buffer_ms)
            .field("min_inference_step_ms", &self.min_inference_step_ms)
            .field("streaming_window_ms", &self.streaming_window_ms)
            .finish_non_exhaustive()
    }
}

impl ParakeetStt {
    /// Load the models from `model_dir`, `pool_size` sets of sessions for
    /// concurrent inference. The directory holds `nemo128.onnx`
    /// (preprocessor), the encoder and the decoder-and-joint graphs of the
    /// export `precision` names, and `vocab.txt` (BPE vocabulary).
    pub fn with_pool_size(
        model_dir: &Path,
        precision: Precision,
        pool_size: usize,
    ) -> Result<Self, SttError> {
        let core = Arc::new(ComponentCore::new("stt:parakeet", ComponentKind::Stt));
        let pool = match build_pool(model_dir, precision, pool_size) {
            Ok(p) => p,
            Err(e) => {
                core.mark_unhealthy(format!("model load failed: {e}"));
                return Err(e);
            }
        };
        info!(model_dir = %model_dir.display(), ?precision, pool_size, "Parakeet TDT loaded");
        Ok(Self {
            pool: Arc::new(pool),
            language: DEFAULT_LANGUAGE.into(),
            min_initial_buffer_ms: 800.0,
            min_inference_step_ms: 1000.0,
            streaming_window_ms: DEFAULT_STREAMING_WINDOW_MS,
            core,
        })
    }
}

#[async_trait]
impl SttProvider for ParakeetStt {
    #[instrument(skip_all, fields(provider = "parakeet-tdt", samples = audio_frame.samples.len()))]
    async fn transcribe(&self, audio_frame: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = std::time::Instant::now();
        let mut pooled = self.pool.checkout().await.map_err(SttError::busy)?;

        let raw_samples = audio_frame.samples.clone();
        let channels = audio_frame.channels;
        let sample_rate = audio_frame.sample_rate;
        let language = self.language.clone();

        let (_pooled, result) = tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<TranscriptionResult, SttError> {
                let set = pooled.get_mut();
                let samples =
                    voice_pipes_sdk::audio::mono_f32_at(&raw_samples, channels, sample_rate, 16000)
                        .map_err(|e| SttError::InvalidAudio(e.to_string()))?;

                let mel = set.preprocessor.process(&samples)?;
                let encoder_out = set.encoder.encode(
                    &mel.features,
                    mel.mel_bins,
                    mel.time_steps,
                    &mel.features_lens,
                )?;
                let tokens = set.decoder.decode(&encoder_out)?;
                let total_audio_secs = samples.len() as f64 / 16000.0;
                let (mut text, mut words) = set
                    .decoder
                    .detokenize_with_timestamps(&tokens, total_audio_secs);
                localise(&mut text, &mut words, &language);

                let end_time = std::time::Duration::from_secs_f64(total_audio_secs);

                info!(
                    text_len = text.len(),
                    token_count = tokens.len(),
                    word_count = words.len(),
                    "transcription complete"
                );

                // Batch: the whole utterance at once, so no streaming
                // window and no partial-emission delay.
                let inference_metrics =
                    InferenceMetrics::measured(total_audio_secs, start.elapsed(), 1);

                Ok(TranscriptionResult::new(text, true)
                    .with_language(Some(language))
                    .with_start_time(Some(std::time::Duration::ZERO))
                    .with_end_time(Some(end_time))
                    .with_words(words)
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
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        let mut pooled = match request.slot_wait {
            Some(wait) => self.pool.checkout_within(wait).await,
            None => self.pool.checkout().await,
        }
        .map_err(SttError::busy)?;

        // The request's language over the configured one, as a label: the
        // model transcribes whatever it was trained on either way.
        let language = request
            .language
            .clone()
            .unwrap_or_else(|| self.language.clone());

        // Incremental f32 buffer -- we convert new i16 chunks on arrival
        // instead of reconverting the entire buffer each inference pass.
        let mut f32_buffer: Vec<f32> = Vec::new();
        let mut last_emitted_text = String::new();
        let mut last_inference_len: usize = 0;
        let expected_sr = self.expected_sample_rate();
        let min_initial_buffer = samples_in_ms(expected_sr, self.min_initial_buffer_ms);
        let min_inference_step = samples_in_ms(expected_sr, self.min_inference_step_ms);
        // Bounded re-encode window.
        // 0 -> unbounded (whole utterance re-encoded each step, O(N²)).
        let window_samples: usize = if self.streaming_window_ms > 0.0 {
            samples_in_ms(expected_sr, self.streaming_window_ms)
        } else {
            0
        };
        let streaming_window_ms_meta: Option<u32> = (self.streaming_window_ms > 0.0).then(|| {
            u32::try_from(samples_in_ms(1000, self.streaming_window_ms)).unwrap_or(u32::MAX)
        });

        // The latest in-progress decode that found words, kept in case the
        // final decode of the whole utterance comes back empty.
        let mut earlier: Option<EarlierWords> = None;

        // Per-utterance accumulators for the streaming-inference metrics
        // attached to the final result.
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
            // Incrementally convert only new samples to f32
            extend_f32_buffer(&mut f32_buffer, &chunk.samples);
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
                extend_f32_buffer(&mut f32_buffer, &extra_chunk.samples);
                is_final = extra_chunk.is_last;
            }

            // Bound memory: drop the oldest samples past the cap so a
            // runaway utterance / stuck VAD can't grow the buffer without bound.
            let max_buffer_samples = samples_in_ms(expected_sr, MAX_STREAM_BUFFER_SECS * 1000.0);
            let dropped = trim_buffer_to_cap(&mut f32_buffer, max_buffer_samples);
            if dropped > 0 {
                last_inference_len = last_inference_len.saturating_sub(dropped);
                tracing::warn!(
                    dropped_samples = dropped,
                    cap_secs = MAX_STREAM_BUFFER_SECS,
                    "Parakeet streaming buffer hit the cap; dropping oldest audio"
                );
            }

            if f32_buffer.len() < min_initial_buffer && !is_final {
                continue;
            }

            if !is_final && (f32_buffer.len() - last_inference_len) < min_inference_step {
                continue;
            }

            last_inference_len = f32_buffer.len();

            // Choose the slice to re-encode.
            //   * `is_final` -> always run over the full buffer so the canonical
            //     transcript matches batch quality.
            //   * Otherwise, if a window cap is set and the buffer exceeds it,
            //     only re-encode the trailing window (paper  6.1: small cache-
            //     aware chunks beat re-encoding the whole utterance every step).
            let samples: Vec<f32> =
                if is_final || window_samples == 0 || f32_buffer.len() <= window_samples {
                    f32_buffer.clone()
                } else {
                    f32_buffer
                        .get(f32_buffer.len() - window_samples..)
                        .unwrap_or_default()
                        .to_vec()
                };

            let covered_whole_buffer = samples.len() == f32_buffer.len();
            let inference_start = std::time::Instant::now();
            let (returned_pooled, result) = tokio::task::spawn_blocking(move || {
                let result = decode_samples(pooled.get_mut(), &samples);
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

            let (mut text, mut words) = match result {
                Ok(v) => {
                    self.core.record_ok(inference_elapsed);
                    v
                }
                Err(e) => {
                    self.core.record_error(&e);
                    return Err(e);
                }
            };

            if !is_final && !text.trim().is_empty() {
                earlier = Some(EarlierWords {
                    text: text.clone(),
                    words: words.clone(),
                    buffer_len: f32_buffer.len(),
                    covered_whole_buffer,
                });
            }
            if is_final {
                let rescued = match rescue_plan(&text, earlier.as_ref()) {
                    Rescue::Keep => None,
                    Rescue::UseEarlier => earlier.take().map(|e| (e.text, e.words)),
                    Rescue::Redecode(len) => {
                        let prefix = f32_buffer
                            .get(..len.min(f32_buffer.len()))
                            .unwrap_or_default()
                            .to_vec();
                        let t_rescue = std::time::Instant::now();
                        let (returned_pooled, result) = tokio::task::spawn_blocking(move || {
                            let result = decode_samples(pooled.get_mut(), &prefix);
                            (pooled, result)
                        })
                        .await
                        .map_err(|e| SttError::InferenceFailed(format!("task join: {e}")))?;
                        total_inference_wall += t_rescue.elapsed();
                        inference_count += 1;
                        pooled = returned_pooled;
                        result.ok().filter(|(t, _)| !t.trim().is_empty())
                    }
                };
                if let Some((t, w)) = rescued {
                    tracing::warn!(
                        audio_ms = f32_buffer.len() * 1000 / expected_sr as usize,
                        words = w.len(),
                        "Parakeet final decode came back empty after an earlier decode \
                         of the same utterance found words; using those"
                    );
                    self.core.extras_with(|x| {
                        let n = x
                            .get("empty_final_rescues")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        x.insert("empty_final_rescues".into(), (n + 1).into());
                    });
                    text = t;
                    words = w;
                }
            }
            localise(&mut text, &mut words, &language);

            if text != last_emitted_text || is_final {
                last_emitted_text = text.clone();
                let inference_metrics = is_final.then(|| {
                    let total_audio_secs = f32_buffer.len() as f64 / expected_sr as f64;
                    let m = InferenceMetrics::measured(
                        total_audio_secs,
                        total_inference_wall,
                        inference_count,
                    )
                    .with_streaming_window_ms(streaming_window_ms_meta);
                    // A bounded window is re-decoded once per step, so it is
                    // the delay floor; an unbounded one has none to report.
                    match streaming_window_ms_meta {
                        Some(window) => m.with_delay_floor_ms(u64::from(window)),
                        None => m,
                    }
                });
                let _ = tx
                    .send(
                        TranscriptionResult::new(text, is_final)
                            .with_language(Some(language.clone()))
                            .with_segment_index(segment_index)
                            .with_start_time(Some(std::time::Duration::ZERO))
                            .with_words(words)
                            .with_inference_metrics(inference_metrics),
                    )
                    .await;
            }

            if is_final {
                f32_buffer.clear();
                last_emitted_text.clear();
                earlier = None;
                last_inference_len = 0;
                total_inference_wall = std::time::Duration::ZERO;
                inference_count = 0;
                segment_index += 1;
            }
        }

        Ok(())
    }

    /// Run one short decode so the first caller is not the one who pays for
    /// the runtime's lazy start-up.
    ///
    /// The ONNX sessions are built at load, but the runtime still does work
    /// on the first run of each graph -- allocating arenas, choosing kernels,
    /// warming caches -- and it landed on the first caller after a worker
    /// started as three to six seconds of endpointing. Later sessions on a
    /// warm worker do not pay it.
    ///
    /// This holds a session set for the length of a decode, so it must run
    /// where no caller can want that set: once at worker start, before the
    /// transport opens, which [`warm_up_scope`](Self::warm_up_scope) asks
    /// for. Run per session it collided with the caller's first utterance
    /// on a pool of one, which then failed for want of a set -- worse than
    /// the slow turn it was meant to remove.
    ///
    /// The frame is a second of silence: it runs every stage, and silence
    /// keeps the decoder's work to a minimum. The result is thrown away.
    async fn warm_up(&self) -> Result<(), SttError> {
        let silence = AudioFrame {
            samples: vec![0i16; 16_000],
            sample_rate: 16_000,
            channels: 1,
        };
        self.transcribe(&silence).await.map(|_| ())
    }

    fn warm_up_scope(&self) -> WarmUpScope {
        WarmUpScope::PerProcess
    }

    fn concurrent_capacity(&self) -> Option<usize> {
        Some(self.pool.size())
    }

    fn expected_sample_rate(&self) -> u32 {
        16000
    }

    fn name(&self) -> &'static str {
        "parakeet-tdt"
    }

    /// Where the sessions run: `open_session` picks the execution target
    /// from the same environment, so the report is what was built.
    async fn device_info(&self) -> Option<DeviceInfo> {
        let (runs_on, detail) = voice_pipes_onnx::ExecutionTarget::from_env().device_report();
        Some(DeviceInfo::config(runs_on, detail))
    }

    /// TDT/RNN-T decoder mangles the final token when it sees a
    /// digital-zero cliff (observed: "Yes please" -> "Yes pletti....",
    /// laughter -> fluent nonsense). VAD already supplies real trailing
    /// silence, so the synthetic flush is unnecessary here.
    fn needs_flush_silence(&self) -> bool {
        false
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        HashSet::from([SttCapability::InterimResults, SttCapability::WordTimestamps])
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(std::sync::Arc::clone(
            &self.core,
        ))))
    }
}

// ── Factory ──────────────────────────────────────────────────

/// The factory the binary registers for `[stt] provider = "parakeet-tdt"`.
pub fn factory() -> Box<dyn SttFactory> {
    Box::new(ParakeetFactory)
}

/// Factory for the Parakeet TDT STT provider.
#[derive(Debug)]
struct ParakeetFactory;

impl SttFactory for ParakeetFactory {
    fn name(&self) -> &'static str {
        "parakeet-tdt"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError> {
        ParakeetOptions::read(cfg).err().unwrap_or_default()
    }

    fn build(
        &self,
        cfg: &SttConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, voice_pipes_sdk::BuildError> {
        let default_dir = PathBuf::from("models/parakeet-tdt");
        let model_dir = cfg.model_dir.as_deref().unwrap_or(&default_dir);
        let options = ParakeetOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;
        let mut parakeet =
            ParakeetStt::with_pool_size(model_dir, options.precision, options.pool_size)?;
        if let Some(language) = options.language {
            parakeet.language = language;
        }
        if let Some(ms) = options.initial_buffer_ms {
            parakeet.min_initial_buffer_ms = as_f32(ms);
        }
        if let Some(ms) = options.inference_step_ms {
            parakeet.min_inference_step_ms = as_f32(ms);
        }
        if let Some(ms) = options.streaming_window_ms {
            parakeet.streaming_window_ms = as_f32(ms);
        }

        Ok(Box::new(parakeet))
    }

    fn provider_schema(&self, cfg: &SttConfig) -> voice_pipes_sdk::schema::ProviderSchema {
        schema::schema(cfg)
    }
}

/// The settings Parakeet reads from its option table.
struct ParakeetOptions {
    precision: Precision,
    language: Option<String>,
    pool_size: usize,
    initial_buffer_ms: Option<f64>,
    inference_step_ms: Option<f64>,
    streaming_window_ms: Option<f64>,
}

impl ParakeetOptions {
    fn read(cfg: &SttConfig) -> Result<Self, Vec<ConfigError>> {
        let mut o = cfg.options();
        let options = Self {
            precision: Precision::from_name(&o.one_of("precision", &Precision::NAMES, "int8")),
            language: o.string("language").filter(|l| !l.trim().is_empty()),
            pool_size: o.usize_in("pool_size", 1..=64, DEFAULT_POOL_SIZE),
            initial_buffer_ms: o.optional_f64_in("initial_buffer_ms", 0.0..=60_000.0),
            inference_step_ms: o.optional_f64_in("inference_step_ms", 1.0..=60_000.0),
            streaming_window_ms: o.optional_f64_in("streaming_window_ms", 1.0..=600_000.0),
        };
        o.finish()?;
        Ok(options)
    }
}

/// Incrementally convert i16 samples to f32 and append to the buffer.
#[inline]
/// Mel, encoder and greedy decode of one buffer, to text and word timings.
fn decode_samples(
    set: &mut SessionSet,
    samples: &[f32],
) -> Result<(String, Vec<WordInfo>), SttError> {
    let mel = set.preprocessor.process(samples)?;
    let encoder_out = set.encoder.encode(
        &mel.features,
        mel.mel_bins,
        mel.time_steps,
        &mel.features_lens,
    )?;
    let tokens = set.decoder.decode(&encoder_out)?;
    let total_audio_secs = samples.len() as f64 / 16000.0;
    Ok(set
        .decoder
        .detokenize_with_timestamps(&tokens, total_audio_secs))
}

/// What an in-progress decode of the current utterance found.
///
/// Greedy TDT decoding can lose a whole utterance on one near-tie: when
/// blank narrowly beats the first word, the prediction network never leaves
/// its start state, and from there mid-sentence frames read as "not yet" all
/// the way to the end. What tips that tie can be as little as how much quiet
/// trails the speech, so the final decode (the whole utterance, with its
/// trailing silence) can come back empty after an in-progress decode of the
/// same audio found every word. Those words are the better answer.
struct EarlierWords {
    text: String,
    words: Vec<WordInfo>,
    /// Samples in the buffer when the decode ran.
    buffer_len: usize,
    /// Whether that decode saw the whole buffer, not only its trailing
    /// window; only then are its words the decode of `..buffer_len`.
    covered_whole_buffer: bool,
}

/// What to do with a final decode, given the latest in-progress decode that
/// found words.
#[derive(Debug, PartialEq, Eq)]
enum Rescue {
    /// The final has words, or nothing ever did: it stands.
    Keep,
    /// The final is empty; the earlier decode covered its whole buffer, so
    /// its words are the decode of that prefix.
    UseEarlier,
    /// The final is empty; the earlier decode saw only a trailing window,
    /// so decode the prefix it came from, of this many samples.
    Redecode(usize),
}

fn rescue_plan(final_text: &str, earlier: Option<&EarlierWords>) -> Rescue {
    match earlier {
        Some(e) if final_text.trim().is_empty() => {
            if e.covered_whole_buffer {
                Rescue::UseEarlier
            } else {
                Rescue::Redecode(e.buffer_len)
            }
        }
        _ => Rescue::Keep,
    }
}

fn extend_f32_buffer(buf: &mut Vec<f32>, samples: &[i16]) {
    buf.reserve(samples.len());
    for &s in samples {
        buf.push(s as f32 / 32768.0);
    }
}

/// Hard cap on the streaming sample buffer as a multiple of the re-encode
/// window. The window bounds *compute*, but the buffer is otherwise
/// kept whole for the final pass and grows without bound on a runaway
/// utterance or a VAD that never emits `is_last`. This bound is generous enough
/// that no realistic single utterance is truncated, while stopping an OOM.
const MAX_STREAM_BUFFER_SECS: f32 = 120.0;

/// Drop the oldest samples so `buf` never exceeds `max`, keeping the trailing
/// window. Returns the number dropped so the caller can fix up buffer-relative
/// indices. `max == 0` disables the cap.
fn trim_buffer_to_cap(buf: &mut Vec<f32>, max: usize) -> usize {
    if max == 0 || buf.len() <= max {
        return 0;
    }
    let drop = buf.len() - max;
    buf.drain(..drop);
    drop
}

#[cfg(test)]
#[path = "tests/parakeet.rs"]
mod tests;
