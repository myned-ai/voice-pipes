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
    self, ExportMetadata, NemotronSessions, StreamingEngine, transcribe_buffered,
};
use crate::vocab;

pub struct NemotronStt {
    pool: Arc<SessionPool<NemotronSessions>>,
    metadata: Arc<ExportMetadata>,
    vocab: Arc<HashMap<u32, String>>,
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
        let mut sets = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            match NemotronSessions::load(model_dir, threads) {
                Ok(s) => sets.push(s),
                Err(e) => {
                    core.mark_unhealthy(format!("{e}"));
                    return Err(e);
                }
            }
        }
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
            model_dir: model_dir.to_path_buf(),
            threads,
            blank_id,
            max_tokens_per_step,
            pool_size,
            core,
        })
    }
}

/// A streaming engine built on a pooled session set. The engine owns the
/// sessions while it lives; on drop they go back to the pool, keeping the
/// provider usable even if a streaming inference step panics.
/// Wait for a free session set and wrap it in a streaming engine. The
/// engine holds the pooled slot, so the sessions go back to the pool when it
/// drops, and when it fails to build.
///
/// A free function, not a method: the pool type lives in `voice-pipes-onnx`
/// and a foreign type takes no inherent impl.
async fn checkout_engine(
    pool: &Arc<SessionPool<NemotronSessions>>,
    metadata: &ExportMetadata,
    blank_id: i32,
    max_tokens_per_step: usize,
    slot_wait: Option<std::time::Duration>,
) -> Result<StreamingEngine, SttError> {
    let slot = match slot_wait {
        Some(wait) => pool.checkout_within(wait).await,
        None => pool.checkout().await,
    }
    .map_err(SttError::busy)?;
    StreamingEngine::new(slot, metadata, blank_id, max_tokens_per_step)
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
                let tokens = transcribe_buffered(
                    guard.get_mut(),
                    &metadata,
                    &samples,
                    blank_id,
                    max_tokens,
                )?;
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

        // The engine owns the sessions; the guard returns them to the pool on
        // drop (loop exit, early `?`, or a panic unwind in the blocking task).
        // The engine moves in/out of `spawn_blocking` on every recv batch.
        let mut engine_guard =
            match checkout_engine(&self.pool, &metadata, blank_id, max_tokens, request.slot_wait).await {
                Ok(e) => e,
                Err(e) => {
                    self.core.record_error(&e);
                    return Err(e);
                }
            };

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

            // Run encoder/decoder work on a blocking thread. The guard moves
            // in and back out so the engine stays inside it throughout -- a
            // panic in the blocking task drops the guard there and returns the
            // sessions to the pool.
            let infer_start = std::time::Instant::now();
            let do_flush = is_final;
            let reset_metadata = Arc::clone(&metadata);
            let (returned_guard, feed_result, ran_chunks) =
                tokio::task::spawn_blocking(move || {
                    let engine = &mut engine_guard;
                    // Track how many chunk drains actually executed so
                    // `inference_count` reflects encoder work, not loop
                    // iterations.
                    let before = engine_chunk_progress(engine);
                    let mut combined: Result<Vec<u32>, SttError> = if samples_to_feed.is_empty() {
                        Ok(Vec::new())
                    } else {
                        engine.feed_samples(&samples_to_feed)
                    };
                    if do_flush && combined.is_ok() {
                        match engine.flush() {
                            Ok(extra) => {
                                if let Ok(ref mut v) = combined {
                                    v.extend(extra);
                                }
                            }
                            Err(e) => combined = Err(e),
                        }
                        // The utterance is done: start the next one from a
                        // clean encoder cache and decoder state.
                        if combined.is_ok()
                            && let Err(e) = engine.reset(&reset_metadata)
                        {
                            combined = Err(e);
                        }
                    }
                    let after = engine_chunk_progress(engine);
                    let ran = u32::try_from(after.saturating_sub(before)).unwrap_or(u32::MAX);
                    (engine_guard, combined, ran)
                })
                .await
                .map_err(|e| {
                    let err = SttError::InferenceFailed(format!("task join: {e}"));
                    self.core.record_error(&err);
                    err
                })?;
            let infer_elapsed = infer_start.elapsed();
            engine_guard = returned_guard;
            total_inference_wall += infer_elapsed;
            chunks_run += ran_chunks;

            let new_tokens = match feed_result {
                Ok(t) => {
                    if !t.is_empty() || is_final {
                        self.core.record_ok(infer_elapsed);
                    }
                    t
                }
                Err(e) => {
                    self.core.record_error(&e);
                    // `engine_guard` drops on return, restoring the sessions.
                    return Err(e);
                }
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
