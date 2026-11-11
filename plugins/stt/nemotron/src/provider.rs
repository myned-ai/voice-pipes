//! The `NemotronStt` recogniser: pooled ONNX sessions, buffered and streaming transcription.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, instrument};

use voice_pipes_onnx::SessionPool;
use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::stt::{
    AudioChunk, InferenceMetrics, RecognitionRequest, SttCapability, SttError, SttProvider,
    TranscriptionResult,
};

use crate::config::{
    DEFAULT_BLANK_ID, DEFAULT_MAX_TOKENS_PER_STEP, DEFAULT_POOL_SIZE, DEFAULT_THREADS,
};
use crate::streaming::{
    self, CacheTensors, DecoderState, ExportMetadata, NemotronSessions, StreamingEngine,
    transcribe_buffered,
};
use crate::vocab;

enum SessionBackend {
    Onnx(Box<NemotronSessions>),
    #[cfg(test)]
    Fake {
        delay: std::time::Duration,
        tokens: Vec<u32>,
    },
}

impl SessionBackend {
    fn transcribe_batch(
        &mut self,
        metadata: &ExportMetadata,
        samples: &[f32],
        blank_id: i32,
        max_tokens: usize,
    ) -> Result<Vec<u32>, SttError> {
        match self {
            Self::Onnx(sessions) => {
                transcribe_buffered(sessions, metadata, samples, blank_id, max_tokens)
            }
            #[cfg(test)]
            Self::Fake { delay, tokens } => {
                if !delay.is_zero() {
                    std::thread::sleep(*delay);
                }
                Ok(tokens.clone())
            }
        }
    }

    fn step_engine(
        &mut self,
        engine: &mut StreamingEngine,
        do_flush: bool,
    ) -> Result<Vec<u32>, SttError> {
        match self {
            Self::Onnx(sessions) => {
                let mut out = if engine.has_ready_chunk() {
                    engine.drain_ready_chunks(sessions)?
                } else {
                    Vec::new()
                };
                if do_flush {
                    let extra = engine.flush(sessions)?;
                    out.extend(extra);
                    engine.reset();
                }
                Ok(out)
            }
            #[cfg(test)]
            Self::Fake { delay, tokens } => {
                if !delay.is_zero() {
                    std::thread::sleep(*delay);
                }
                engine.chunks_processed += 1;
                engine.end_audio();
                if do_flush {
                    engine.reset();
                }
                Ok(tokens.clone())
            }
        }
    }
}

pub struct NemotronStt {
    pool: Arc<SessionPool<SessionBackend>>,
    metadata: Arc<ExportMetadata>,
    vocab: Arc<HashMap<u32, String>>,
    initial_cache: CacheTensors,
    initial_decoder_state: DecoderState,
    model_dir: PathBuf,
    threads: i16,
    blank_id: i32,
    max_tokens_per_step: usize,
    pool_size: usize,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for NemotronStt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NemotronStt")
            .field("model_dir", &self.model_dir)
            .field("threads", &self.threads)
            .field("blank_id", &self.blank_id)
            .field("max_tokens_per_step", &self.max_tokens_per_step)
            .field("pool_size", &self.pool_size)
            .finish_non_exhaustive()
    }
}

impl NemotronStt {
    pub fn new(model_dir: &Path) -> Result<Self, SttError> {
        Self::with_options(
            model_dir,
            DEFAULT_THREADS,
            DEFAULT_BLANK_ID,
            DEFAULT_MAX_TOKENS_PER_STEP,
            DEFAULT_POOL_SIZE,
        )
    }

    pub fn with_options(
        model_dir: &Path,
        threads: i16,
        blank_id: i32,
        max_tokens_per_step: usize,
        pool_size: usize,
    ) -> Result<Self, SttError> {
        let core = Arc::new(ComponentCore::new("stt:nemotron", ComponentKind::Stt));
        if !model_dir.is_dir() {
            let err = SttError::ModelLoadFailed(format!(
                "model_dir does not exist: {}",
                model_dir.display()
            ));
            core.mark_unhealthy(format!("{err}"));
            return Err(err);
        }
        let metadata = match streaming::load_metadata(model_dir) {
            Ok(m) => m,
            Err(e) => {
                core.mark_unhealthy(format!("{e}"));
                return Err(e);
            }
        };
        let pool_size = pool_size.max(1);
        let mut raw_sets = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            match NemotronSessions::load(model_dir, threads) {
                Ok(s) => raw_sets.push(s),
                Err(e) => {
                    core.mark_unhealthy(format!("{e}"));
                    return Err(e);
                }
            }
        }
        let first_session = raw_sets.first().ok_or_else(|| {
            SttError::ConfigError("Nemotron session pool requires at least one session".into())
        })?;
        let initial_cache =
            match CacheTensors::new(&first_session.encoder, &metadata.cache_init_shapes) {
                Ok(c) => c,
                Err(e) => {
                    core.mark_unhealthy(format!("{e}"));
                    return Err(e);
                }
            };
        let initial_decoder_state = match DecoderState::new(&first_session.decoder, blank_id) {
            Ok(d) => d,
            Err(e) => {
                core.mark_unhealthy(format!("{e}"));
                return Err(e);
            }
        };
        let sets: Vec<SessionBackend> = raw_sets
            .into_iter()
            .map(|s| SessionBackend::Onnx(Box::new(s)))
            .collect();
        let vocab = match vocab::load_vocab(model_dir) {
            Ok(v) => v,
            Err(e) => {
                core.mark_unhealthy(format!("{e}"));
                return Err(e);
            }
        };
        info!(
            model_dir = %model_dir.display(),
            chunk = metadata.streaming_config.chunk,
            left = metadata.streaming_config.left,
            right = metadata.streaming_config.right,
            vocab_size = vocab.len(),
            pool_size,
            "Nemotron streaming STT loaded"
        );
        Ok(Self {
            pool: Arc::new(
                SessionPool::new(sets, "Nemotron session sets")
                    .map_err(|e| SttError::ConfigError(e.to_string()))?,
            ),
            metadata: Arc::new(metadata),
            vocab: Arc::new(vocab),
            initial_cache,
            initial_decoder_state,
            model_dir: model_dir.to_path_buf(),
            threads,
            blank_id,
            max_tokens_per_step,
            pool_size,
            core,
        })
    }

    async fn checkout_slot(
        &self,
        slot_wait: Option<std::time::Duration>,
    ) -> Result<voice_pipes_onnx::PooledSlot<SessionBackend>, SttError> {
        match slot_wait {
            Some(wait) => self.pool.checkout_within(wait).await,
            None => self.pool.checkout().await,
        }
        .map_err(SttError::busy)
    }
}

/// Number of chunk drains a `StreamingEngine` has committed to its cache.
/// Used as a cheap "did encoder run since last call" probe. Monotonic and
/// independent of buffer draining.
fn engine_chunk_progress(engine: &StreamingEngine) -> usize {
    engine.chunks_processed
}

/// The metrics of a run in `chunk_ms` chunks: a chunk is both the delay
/// floor and the window each step encodes.
fn build_metrics(
    audio_secs: f64,
    inference_wall: std::time::Duration,
    inference_count: u32,
    chunk_ms: u32,
) -> InferenceMetrics {
    InferenceMetrics::measured(audio_secs, inference_wall, inference_count)
        .with_delay_floor_ms(u64::from(chunk_ms))
        .with_streaming_window_ms(Some(chunk_ms))
}

#[async_trait]
impl SttProvider for NemotronStt {
    #[instrument(skip_all, fields(provider = "nemotron-streaming", samples = audio_frame.samples.len()))]
    async fn transcribe(&self, audio_frame: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = std::time::Instant::now();
        let raw_samples = audio_frame.samples.clone();
        let channels = audio_frame.channels;
        let sample_rate = audio_frame.sample_rate;

        let mut guard = match self.pool.checkout().await.map_err(SttError::busy) {
            Ok(g) => g,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };
        let metadata = Arc::clone(&self.metadata);
        let vocab = Arc::clone(&self.vocab);
        let blank_id = self.blank_id;
        let max_tokens = self.max_tokens_per_step;
        let chunk_frames = self.metadata.streaming_config.chunk;

        // The guard moves into the blocking task; its Drop returns the sessions
        // to the pool on every exit -- including a panic unwind -- so `?` on a
        // JoinError below cannot strand the sessions.
        let (_guard, result) = tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<TranscriptionResult, SttError> {
                let samples = voice_pipes_sdk::audio::mono_f32_at(
                    &raw_samples,
                    channels,
                    sample_rate,
                    16_000,
                )
                .map_err(|e| SttError::InvalidAudio(e.to_string()))?;
                let total_audio_secs = samples.len() as f64 / 16_000.0;
                let tokens = guard
                    .get_mut()
                    .transcribe_batch(&metadata, &samples, blank_id, max_tokens)?;
                let text = vocab::detokenize(&vocab, &tokens);
                let inference_metrics = build_metrics(
                    total_audio_secs,
                    start.elapsed(),
                    1,
                    u32::try_from(
                        chunk_frames * streaming::HOP_LENGTH * 1000 / streaming::SAMPLE_RATE,
                    )
                    .unwrap_or(u32::MAX),
                );
                Ok(TranscriptionResult::new(text, true)
                    .with_language(Some("en-US".into()))
                    .with_start_time(Some(std::time::Duration::ZERO))
                    .with_end_time(Some(std::time::Duration::from_secs_f64(total_audio_secs)))
                    .with_inference_metrics(Some(inference_metrics)))
            })();
            (guard, result)
        })
        .await
        .map_err(|e| {
            let err = SttError::InferenceFailed(format!("task join: {e}"));
            self.core.record_error(&err);
            err
        })?;

        // `_guard` drops here, returning the sessions to the pool.
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
        // Cache-aware streaming: the `StreamingEngine` owns the encoder
        // cache + decoder state and drains chunks as soon as enough audio
        // is buffered, so encoder work happens during speech and only the
        // right-context flush runs after `is_last`. One stream serves the
        // whole session: each `is_last` closes an utterance, the engine
        // state is reset, and the next utterance continues on the same
        // sessions with the next segment index.
        let metadata = Arc::clone(&self.metadata);
        let vocab = Arc::clone(&self.vocab);
        let blank_id = self.blank_id;
        let max_tokens = self.max_tokens_per_step;
        let chunk_frames = self.metadata.streaming_config.chunk;
        let chunk_ms =
            u32::try_from(chunk_frames * streaming::HOP_LENGTH * 1000 / streaming::SAMPLE_RATE)
                .unwrap_or(u32::MAX);
        let expected_sr = self.expected_sample_rate();

        let mut engine = StreamingEngine::from_templates(
            self.initial_cache.clone(),
            self.initial_decoder_state.clone(),
            &metadata,
            blank_id,
            max_tokens,
        );
        let slot_wait = request.slot_wait;

        let mut segment_index: u32 = 0;
        let mut all_tokens: Vec<u32> = Vec::new();
        let mut last_emitted_text = String::new();
        let mut total_samples: usize = 0;
        let mut total_inference_wall = std::time::Duration::ZERO;
        let mut chunks_run: u32 = 0;
        // Whether any final has gone out; a stream that closes without
        // ever having produced one still owes the caller an (empty) final.
        let mut emitted_final = false;

        loop {
            if cancel.is_cancelled() {
                break;
            }
            let recv_res = tokio::select! {
                () = cancel.cancelled() => None,
                r = audio_rx.recv() => r,
            };
            if recv_res.is_none() && cancel.is_cancelled() {
                break;
            }
            let recv = recv_res;
            let input_closed = recv.is_none();
            let (samples_to_feed, is_final) = if let Some(chunk) = recv {
                let mut samples: Vec<f32> = voice_pipes_sdk::audio::i16_to_f32(&chunk.samples);
                let mut is_last = chunk.is_last;
                // Take what is already queued, so a slow decode catches up, but
                // never past an utterance boundary: the chunks after `is_last`
                // belong to the next utterance, and reading on decodes them as
                // part of this one.
                for _ in 0..audio_rx.len() {
                    if is_last {
                        break;
                    }
                    let Ok(extra) = audio_rx.try_recv() else {
                        break;
                    };
                    samples.extend(voice_pipes_sdk::audio::i16_to_f32(&extra.samples));
                    is_last = extra.is_last;
                }
                (samples, is_last)
            } else {
                // Input closed. Audio fed since the last utterance
                // boundary is finalised; a boundary that already went
                // out leaves nothing to flush.
                if total_samples == 0 && emitted_final {
                    break;
                }
                (Vec::new(), true)
            };
            total_samples += samples_to_feed.len();
            if !samples_to_feed.is_empty() {
                engine.push_samples(&samples_to_feed);
            }

            let needs_onnx_step =
                engine.has_ready_chunk() || (is_final && engine.has_flushable_audio());
            if !needs_onnx_step && !is_final {
                continue;
            }

            let new_tokens = if needs_onnx_step {
                let mut slot = match self.checkout_slot(slot_wait).await {
                    Ok(s) => s,
                    Err(e) => {
                        self.core.record_error(&e);
                        return Err(e);
                    }
                };
                let infer_start = std::time::Instant::now();
                let do_flush = is_final;
                let (returned_engine, feed_result, ran_chunks) =
                    tokio::task::spawn_blocking(move || {
                        let before = engine_chunk_progress(&engine);
                        let combined = slot.get_mut().step_engine(&mut engine, do_flush);
                        let after = engine_chunk_progress(&engine);
                        let ran = u32::try_from(after.saturating_sub(before)).unwrap_or(u32::MAX);
                        (engine, combined, ran)
                    })
                    .await
                    .map_err(|e| {
                        let err = SttError::InferenceFailed(format!("task join: {e}"));
                        self.core.record_error(&err);
                        err
                    })?;
                let infer_elapsed = infer_start.elapsed();
                engine = returned_engine;
                total_inference_wall += infer_elapsed;
                chunks_run += ran_chunks;

                match feed_result {
                    Ok(t) => {
                        if !t.is_empty() || is_final {
                            self.core.record_ok(infer_elapsed);
                        }
                        t
                    }
                    Err(e) => {
                        self.core.record_error(&e);
                        return Err(e);
                    }
                }
            } else {
                engine.reset();
                Vec::new()
            };

            if !new_tokens.is_empty() {
                all_tokens.extend_from_slice(&new_tokens);
            }

            // Emit a partial whenever new tokens arrived, and a final on
            // is_final. Skip duplicate empty partials.
            let should_emit = is_final
                || (!new_tokens.is_empty() && {
                    // Re-detokenize because BPE may merge across chunk
                    // boundaries. Only emit if the rendered text changed.
                    let candidate = vocab::detokenize(&vocab, &all_tokens);
                    let changed = candidate != last_emitted_text;
                    if changed {
                        last_emitted_text = candidate;
                    }
                    changed
                });

            if should_emit {
                let text = if is_final {
                    let final_text = vocab::detokenize(&vocab, &all_tokens);
                    last_emitted_text.clone_from(&final_text);
                    final_text
                } else {
                    last_emitted_text.clone()
                };
                let total_audio_secs = total_samples as f64 / expected_sr as f64;
                let inference_metrics = build_metrics(
                    total_audio_secs,
                    total_inference_wall,
                    chunks_run.max(1),
                    chunk_ms,
                );
                let _ = tx
                    .send(
                        TranscriptionResult::new(text, is_final)
                            .with_language(Some("en-US".into()))
                            .with_segment_index(segment_index)
                            .with_start_time(Some(std::time::Duration::ZERO))
                            .with_end_time(Some(std::time::Duration::from_secs_f64(
                                total_audio_secs,
                            )))
                            .with_inference_metrics(Some(inference_metrics)),
                    )
                    .await;
            }

            if is_final {
                emitted_final = true;
                if input_closed {
                    break;
                }
                // Next utterance on the same stream.
                segment_index += 1;
                all_tokens.clear();
                last_emitted_text.clear();
                total_samples = 0;
                total_inference_wall = std::time::Duration::ZERO;
                chunks_run = 0;
            }
        }

        // `engine_guard` returns the sessions to the pool when it drops at the
        // end of this function (or on any early return above).
        Ok(())
    }

    fn concurrent_capacity(&self) -> Option<usize> {
        Some(self.pool.size())
    }

    fn expected_sample_rate(&self) -> u32 {
        16_000
    }

    fn name(&self) -> &'static str {
        "nemotron-streaming"
    }

    /// Nemotron now receives contiguous VAD trailing silence directly;
    /// adding an extra synthetic 500ms flush over-pads the input and
    /// causes tail hallucinations at the end of the utterance.
    fn needs_flush_silence(&self) -> bool {
        false
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        HashSet::from([SttCapability::Streaming])
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

#[cfg(test)]
#[path = "tests/provider.rs"]
mod tests;
