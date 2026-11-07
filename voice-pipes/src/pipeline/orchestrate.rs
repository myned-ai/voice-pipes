use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::speculative::{SpeculativeCommitResult, SpeculativeSessionManager};
use crate::config::OutputChunkingConfig;
use crate::text::{self, TextFragment, TextProcessor};
use crate::transport::{ErrorCode, ErrorComponent, ErrorSeverity, ServerControl, ServerMessage};
use crate::tts::TtsProvider;

use super::events::PipelineEvent;
use super::extras::{LlmExtras, SttExtras, TtsExtras};
use super::metrics::{TurnMetricsSnapshot, TurnRecorder};
use super::post_audio::{self, AudioPostProcessor};
use super::tts::tts_task;
use super::types::{AudioChunk, AudioLedger, Utterance, UtteranceSource};

/// Base backoff for the first LLM retry attempt, in milliseconds.
const LLM_RETRY_BASE_BACKOFF_MS: u64 = 100;
/// Cap applied to the exponential backoff between attempts.
const LLM_RETRY_MAX_BACKOFF_MS: u64 = 1_000;
/// The longest rate-limit wait a turn sits through before retrying; a
/// service asking for more fails the turn, since the user is waiting for
/// the reply.
const LLM_RETRY_MAX_RATE_LIMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The fragment whose audio is going out, counted until the next caption
/// starts another, then recorded in the [`AudioLedger`] so a barge-in can
/// tell how much of each fragment was heard.
#[derive(Default)]
struct FragmentTally {
    text: Option<String>,
    spoken_text: Option<String>,
    idx: u32,
    text_offset: usize,
    samples: u64,
    rate: u32,
    word_timings: Option<Vec<super::types::WordTiming>>,
    /// Where the next fragment's text starts in the whole reply.
    next_offset: usize,
}

impl FragmentTally {
    /// Begin counting the fragment `caption` announces.
    fn start(&mut self, caption: &super::types::CaptionMeta, sample_rate: u32) {
        self.text = Some(caption.text.clone());
        self.spoken_text.clone_from(&caption.spoken_text);
        self.idx = caption.fragment_idx;
        self.text_offset = self.next_offset;
        self.next_offset += caption.text.len();
        self.samples = 0;
        self.rate = sample_rate;
        self.word_timings.clone_from(&caption.word_timings);
    }

    /// Record the fragment being counted, if any, in `ledger`.
    fn flush(&mut self, ledger: &parking_lot::Mutex<AudioLedger>) {
        if let Some(text) = self.text.take() {
            ledger.lock().push(
                text,
                self.spoken_text.take(),
                self.text_offset,
                self.idx,
                self.samples,
                self.rate,
                self.word_timings.take(),
            );
        }
    }
}

/// Process a single utterance through LLM -> TTS, streaming audio chunks
/// to `audio_tx`.  Returns timing metrics.  Returns when generation is
/// complete or when `cancel` is triggered (user interruption).
#[expect(
    clippy::type_complexity,
    reason = "session-handle type is spelled out where it is threaded; an alias would relocate, not simplify"
)]
pub async fn process_utterance(
    utterance: &Utterance,
    audio_tx: mpsc::Sender<AudioChunk>,
    event_tx: mpsc::Sender<ServerMessage>,
    processors: Arc<Vec<Box<dyn TextProcessor>>>,
    post_processors: Arc<Vec<Box<dyn AudioPostProcessor>>>,
    tts: Arc<dyn TtsProvider>,
    tts_session: Option<Arc<tokio::sync::Mutex<Option<Box<dyn crate::tts::TtsSession>>>>>,
    output_rate: Option<u32>,
    output_chunking: Option<OutputChunkingConfig>,
    cancel: CancellationToken,
    audit: Option<crate::audit::StreamingAuditSink>,
    // Optional broadcast sender for structured pipeline events.
    pe_tx: Option<tokio::sync::broadcast::Sender<PipelineEvent>>,
    ledger: Arc<parking_lot::Mutex<AudioLedger>>,
    // Shared cold-start flag for the owning TTS provider handle.  The first
    // successful synthesis swaps this from `true` to `false`; subsequent
    // calls observe the flag value at entry.  Pass an Arc with `false` to opt
    // out of cold-start tracking (e.g. live mode).
    tts_cold_start: Arc<AtomicBool>,
    spec_manager: Option<Arc<SpeculativeSessionManager>>,
    sanitize_cfg: Option<voice_pipes_sdk::SanitizeConfig>,
) -> TurnMetricsSnapshot {
    let process_start = std::time::Instant::now();
    let (frag_tx, mut frag_rx) = mpsc::channel::<TextFragment>(16);
    let (tts_frag_tx, tts_frag_rx) = mpsc::channel::<TextFragment>(16);
    let (tts_audio_tx, mut tts_audio_rx) = mpsc::channel::<AudioChunk>(32);

    let cancel_llm = cancel.clone();
    let cancel_tts = cancel.clone();
    let cancel_fwd = cancel.clone();
    let processors2 = std::sync::Arc::clone(&processors);
    let text = utterance.text.clone();
    let utterance_id = utterance.utterance_id;

    // Audit: LLM input
    if let Some(ref a) = audit {
        a.record(crate::audit::AuditEvent::LlmInput {
            utterance_id,
            text: utterance.text.clone(),
        });
    }
    if let Some(ref tx) = pe_tx {
        let _ = tx.send(PipelineEvent::LlmStarted { utterance_id });
    }

    // Shared flag to prevent cascading errors from reaching the client.
    let error_reported = Arc::new(AtomicBool::new(false));

    // Resolve the LLM (last-in-chain) error policy once, outside the spawned
    // task, so the closure doesn't need to hold the processors Arc just for
    // policy lookup.
    let llm_component = processors.last().and_then(|p| p.as_component());
    let llm_error_action = crate::component::classify_streaming_error(
        llm_component
            .as_deref()
            .map(|c| c as &dyn crate::component::Component),
        "text",
    );
    // Distil into (suppress, max_retries) so the spawned task only carries
    // primitives and doesn't need to re-pattern-match the enum.
    let (llm_suppress, llm_max_retries) = match llm_error_action {
        crate::component::ErrorAction::Suppress => (true, 0u8),
        crate::component::ErrorAction::Retry { max_attempts } => (false, max_attempts),
        crate::component::ErrorAction::Propagate => (false, 0u8),
    };

    let llm_provider_name = processors
        .last()
        .map_or_else(|| "unknown".to_string(), |p| p.name().to_string());

    let mut spec_rollbacks = None;
    let mut spec_tokens_generated = None;
    let mut spec_tokens_accepted = None;
    let mut spec_efficiency = None;

    let commit_res = if let Some(ref manager) = spec_manager {
        Some(manager.commit(&utterance.text).await)
    } else {
        None
    };

    let mut is_accepted = false;
    let mut opt_audio_stream = None;
    let mut opt_text_stream = None;
    // The accepted speculation's assistant response text (NOT the user
    // transcript) -- becomes this turn's full_text in the EOU sentinel.
    let mut accepted_full_text = String::new();

    if let Some(res) = commit_res {
        match res {
            SpeculativeCommitResult::Accepted {
                rollbacks,
                tokens_generated,
                tokens_accepted,
                accepted_text,
                audio_stream,
                text_stream,
            } => {
                is_accepted = true;
                spec_rollbacks = Some(rollbacks);
                spec_tokens_generated = Some(tokens_generated);
                spec_tokens_accepted = Some(tokens_accepted);
                if tokens_generated > 0 {
                    spec_efficiency =
                        Some((tokens_accepted as f32 / tokens_generated as f32) * 100.0);
                }
                accepted_full_text = accepted_text;
                opt_audio_stream = audio_stream;
                opt_text_stream = text_stream;
            }
            SpeculativeCommitResult::Rejected {
                rollbacks,
                tokens_generated,
            } => {
                spec_rollbacks = Some(rollbacks);
                spec_tokens_generated = Some(tokens_generated);
            }
        }
    }

    let mut llm_handle;
    let bridge_handle;
    let tts_handle;

    // Bridge -> snapshot channel carrying `(canonical_ttft, perceived_ttft)`.
    let (ttft_tx, ttft_rx) =
        tokio::sync::oneshot::channel::<(std::time::Duration, std::time::Duration)>();
    let (llm_done_tx, llm_done_rx) =
        tokio::sync::oneshot::channel::<(std::time::Duration, u32, Option<u32>)>();
    // Notify the audio forwarding loop when the first LLM fragment is sent to TTS.
    let (first_frag_tx, first_frag_rx) = tokio::sync::oneshot::channel::<std::time::Instant>();
    // Channel to carry the full accumulated LLM text to the EOU sentinel
    let (accumulated_text_tx, mut accumulated_text_rx) = tokio::sync::oneshot::channel::<String>();
    let commit_start = utterance.committed_at.unwrap_or(process_start);
    let tts_skipped = is_accepted && opt_audio_stream.is_some();

    if is_accepted {
        // full_text is the assistant's generated response, not the user's
        // transcript. Speculative audio/text was produced before the utterance
        // id was known, so it carries a placeholder id (0); stamp the real id
        // onto every forwarded chunk below.
        if !accepted_full_text.is_empty() {
            let _ = accumulated_text_tx.send(accepted_full_text);
        }
        drop((llm_done_tx, ttft_tx));
        let _ = first_frag_tx.send(std::time::Instant::now());

        llm_handle = tokio::spawn(async {});

        if let Some((buffered_audio, mut subsequent_audio_rx)) = opt_audio_stream {
            bridge_handle = tokio::spawn(async {});
            let tts_audio_tx_clone = tts_audio_tx.clone();
            // The forwarder's clone must be the only sender. With
            // the original alive until this function returns, the forward
            // loop below could never observe `None` and every accepted
            // speculative turn would park here forever (no TurnMetrics, leaked
            // task + ledger per turn).
            drop(tts_audio_tx);
            let cancel_fwd_clone = cancel.clone();
            tts_handle = tokio::spawn(async move {
                for mut chunk in buffered_audio {
                    chunk.utterance_id = utterance_id;
                    if tts_audio_tx_clone.send(chunk).await.is_err() {
                        return;
                    }
                }
                while let Some(mut chunk) = subsequent_audio_rx.recv().await {
                    if cancel_fwd_clone.is_cancelled() {
                        break;
                    }
                    chunk.utterance_id = utterance_id;
                    if tts_audio_tx_clone.send(chunk).await.is_err() {
                        break;
                    }
                }
            });
        } else if let Some((buffered_fragments, mut subsequent_fragments_rx)) = opt_text_stream {
            let tts_frag_tx_clone = tts_frag_tx.clone();
            // Same as the audio path — the forwarder's clone must
            // be the last sender so `tts_task` sees the fragment stream end.
            drop(tts_frag_tx);
            let cancel_fwd_clone = cancel.clone();
            // The forwarder stands in for the bridge in this path; keep its
            // handle so teardown aborts it with the others.
            bridge_handle = tokio::spawn(async move {
                for frag in buffered_fragments {
                    if tts_frag_tx_clone.send(frag).await.is_err() {
                        return;
                    }
                }
                while let Some(frag) = subsequent_fragments_rx.recv().await {
                    if cancel_fwd_clone.is_cancelled() {
                        break;
                    }
                    if tts_frag_tx_clone.send(frag).await.is_err() {
                        break;
                    }
                }
            });

            let tts_event_tx = event_tx.clone();
            let tts_session_task =
                tts_session.unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(None)));
            let cancel_tts = cancel.clone();
            let error_reported = Arc::new(AtomicBool::new(false));
            let sanitize_arg = sanitize_cfg.clone();
            tts_handle = tokio::spawn(tts_task(
                tts_frag_rx,
                tts_audio_tx,
                std::sync::Arc::clone(&tts),
                tts_session_task,
                output_rate,
                output_chunking,
                cancel_tts,
                utterance_id,
                audit.clone(),
                tts_event_tx,
                error_reported,
                sanitize_arg,
            ));
        } else {
            bridge_handle = tokio::spawn(async {});
            tts_handle = tokio::spawn(async {});
        }
    } else {
        // Spawn LLM (text chain) for this utterance.
        let llm_error_reported = std::sync::Arc::clone(&error_reported);
        let llm_event_tx = event_tx.clone();
        let utterance_source = utterance.source;
        llm_handle = tokio::spawn(async move {
            if cancel_llm.is_cancelled() {
                return;
            }
            // Agent `Say` turn: the text IS the assistant output — skip
            // the LLM chain and hand it straight to the bridge/TTS. The
            // bridge still accumulates it, so audit, EOU full_text, and
            // LlmCompleted all carry the spoken text.
            if utterance_source == UtteranceSource::AgentSay {
                let _ = frag_tx
                    .send(TextFragment::new(text).with_is_end_of_response(true))
                    .await;
                return;
            }
            let result = run_llm_chain_with_retry(
                processors2,
                text,
                frag_tx,
                cancel_llm,
                llm_max_retries,
                utterance_source == UtteranceSource::AgentGenerate,
            )
            .await;
            let Err(e) = result else { return };
            tracing::error!(error = %e, "text processing failed");
            if llm_suppress {
                return;
            }
            if !llm_error_reported.swap(true, Ordering::SeqCst) {
                let err_msg = ServerMessage::Control(ServerControl::Error {
                    code: ErrorCode::LlmRetriesExhausted,
                    message: format!("Language model failed: {e}"),
                    severity: ErrorSeverity::Error,
                    turn_id: None,
                    component: Some(ErrorComponent::Llm),
                });
                match llm_event_tx.send(err_msg).await {
                    Ok(()) => tracing::info!("pipeline error sent to client"),
                    Err(se) => {
                        tracing::warn!(error = %se, "failed to send pipeline error to client -- channel closed");
                    }
                }
            }
        });

        // Bridge: intercept LLM fragments to accumulate full output for audit,
        // then forward to TTS.
        let bridge_start = std::time::Instant::now();
        let llm_provider_name_for_bridge = llm_provider_name.clone();
        let audit_bridge = audit.clone();
        let cancel_bridge = cancel.clone();
        let bridge_pe_tx = pe_tx.clone();
        let sanitize_cfg_bridge = sanitize_cfg.clone().unwrap_or_else(|| {
            voice_pipes_sdk::SanitizeConfig::from_tts(
                None,
                tts.name(),
                tts.supported_inline_tags(),
            )
        });
        bridge_handle = tokio::spawn(async move {
            let mut accumulated = String::new();
            let llm_start = std::time::Instant::now();
            let mut ttft_tx = Some(ttft_tx);
            let mut first_frag_tx = Some(first_frag_tx);
            let mut accumulated_text_tx = Some(accumulated_text_tx);
            let mut real_token_count: Option<u32> = None;
            loop {
                tokio::select! {
                    biased;
                    () = cancel_bridge.cancelled() => break,
                    frag = frag_rx.recv() => match frag {
                        Some(f) => {
                            if let Some(tx) = ttft_tx.take() {
                                let perceived = bridge_start.elapsed();
                                let canonical = f
                                    .first_token_ms
                                    .map_or(perceived, |ms| std::time::Duration::from_secs_f64(ms / 1000.0));
                                let _ = tx.send((canonical, perceived));
                                if let Some(ref tx) = bridge_pe_tx {
                                    let _ = tx.send(PipelineEvent::LlmFirstToken {
                                        utterance_id,
                                        elapsed_ms: perceived.as_millis() as u64,
                                    });
                                }
                                if let Some(tx) = first_frag_tx.take() {
                                    let _ = tx.send(std::time::Instant::now());
                                }
                            }
                            accumulated.push_str(&f.text);
                            if f.token_count.is_some() {
                                real_token_count = f.token_count;
                            }
                            // The response text is complete at the final
                            // fragment; hand it to the audio loop now, before
                            // the fragment reaches TTS. The LLM stream may
                            // stay open a while longer (usage parsing, history
                            // bookkeeping), and the end-of-utterance sentinel
                            // must not wait for that.
                            if f.is_end_of_response
                                && !accumulated.is_empty()
                                && let Some(tx) = accumulated_text_tx.take()
                            {
                                let _ = tx.send(accumulated.clone());
                            }
                            if tts_frag_tx.send(f).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
            let llm_generation = llm_start.elapsed();
            let token_estimate = accumulated.split_whitespace().count() as u32;
            let _ = llm_done_tx.send((llm_generation, token_estimate, real_token_count));
            if let Some(ref a) = audit_bridge {
                let dual = voice_pipes_sdk::sanitize_dual_track(&accumulated, &sanitize_cfg_bridge);
                a.record(crate::audit::AuditEvent::LlmOutput {
                    utterance_id,
                    text: accumulated.clone(),
                    spoken_text: Some(dual.spoken),
                    tts_directives: dual.directives,
                    stripped_directives: dual.stripped,
                    latency: llm_generation,
                    is_partial: cancel_bridge.is_cancelled(),
                });
                let elapsed_ms = llm_generation.as_secs_f64() * 1000.0;
                a.record(crate::audit::AuditEvent::Trace(
                    crate::audit::TraceEvent::new(
                        format!("llm:{llm_provider_name_for_bridge}"),
                        "llm_timing",
                        serde_json::json!({
                            "elapsed_ms": (elapsed_ms * 10.0).round() / 10.0,
                            "tokens": token_estimate,
                            "provider": &llm_provider_name_for_bridge,
                        }),
                    ),
                ));
            }
            // Streams that end without a flagged final fragment deliver the
            // text when the channel closes.
            if !accumulated.is_empty()
                && let Some(tx) = accumulated_text_tx.take()
            {
                let _ = tx.send(accumulated.clone());
            }
            if let Some(ref tx) = bridge_pe_tx {
                let _ = tx.send(PipelineEvent::LlmCompleted {
                    utterance_id,
                    text_len: accumulated.len(),
                    elapsed_ms: llm_generation.as_millis() as u64,
                    is_partial: cancel_bridge.is_cancelled(),
                });
            }
        });

        // Spawn TTS for this utterance.
        let tts_event_tx = event_tx.clone();
        let tts_session_task =
            tts_session.unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(None)));
        tts_handle = tokio::spawn(tts_task(
            tts_frag_rx,
            tts_audio_tx,
            std::sync::Arc::clone(&tts),
            tts_session_task,
            output_rate,
            output_chunking,
            cancel_tts,
            utterance_id,
            audit.clone(),
            tts_event_tx,
            error_reported,
            sanitize_cfg,
        ));
    }

    // Forward TTS audio to the caller's audio_tx, respecting cancellation.
    // Capture TTFA (time to first audio) on the first non-empty chunk,
    // and TTS TTFB (pipeline-internal, from first LLM fragment to first audio).
    let mut ttfa: Option<std::time::Duration> = None;
    let mut tts_ttfb: Option<std::time::Duration> = None;
    let mut tts_synthesis: Option<std::time::Duration> = None;
    let mut first_frag_rx = Some(first_frag_rx);
    let mut first_frag_instant: Option<std::time::Instant> = None;
    let mut first_tts_audio_instant: Option<std::time::Instant> = None;
    let mut first_emit_instant: Option<std::time::Instant> = None;
    // Aggregate audio sample count + native rate so the runner can compute
    // realtime-factor and audio-duration without re-walking the pipeline.
    let mut tts_total_samples: u64 = 0;
    let mut tts_observed_rate: Option<u32> = None;
    let mut tts_observed_channels: u16 = 1;
    // Cold-start: snapshot before the first synthesis, swap atomically once
    // we observe a real audio chunk.  We keep a local copy so the metric we
    // report reflects "this turn was the cold one", not "the provider is
    // currently warm" (which is always the case after rep 1).
    let cold_start_was_set = tts_cold_start.load(Ordering::SeqCst);
    let mut cold_start_observed = false;

    // AudioLedger: accumulate text<->audio mapping per-fragment (provided by runner).
    let mut fragment = FragmentTally::default();

    if let Some(ref tx) = pe_tx {
        let _ = tx.send(PipelineEvent::TtsStarted { utterance_id });
    }
    // Streaming-phase throughput (`output` component) and per-processor
    // chain stats (`audio_post:<name>`).
    let mut output_throughput = crate::pipeline::OutputThroughput::default();
    let mut chain_stats = crate::pipeline::ChainStats::default();
    loop {
        tokio::select! {
            biased;
            () = cancel_fwd.cancelled() => break,
            // Receive the instant the first LLM fragment was forwarded to TTS.
            frag_instant = async {
                if let Some(ref mut rx) = first_frag_rx {
                    rx.await.ok()
                } else {
                    std::future::pending::<Option<std::time::Instant>>().await
                }
            }, if first_frag_rx.is_some() => {
                first_frag_instant = frag_instant;
                first_frag_rx = None;
            },
            chunk = tts_audio_rx.recv() => match chunk {
                Some(mut c) => {
                    if ttfa.is_none() && !c.samples.is_empty() {
                        if first_frag_instant.is_none()
                            && let Some(mut rx) = first_frag_rx.take()
                        {
                            first_frag_instant = rx.try_recv().ok();
                        }
                        let now = std::time::Instant::now();
                        first_tts_audio_instant = Some(now);
                        ttfa = Some(now.saturating_duration_since(process_start));
                        // TTS TTFB = time from first LLM fragment to first audio sample
                        // (when TTS actually ran).
                        if !tts_skipped
                            && let Some(frag_t) = first_frag_instant
                        {
                            tts_ttfb = Some(now.saturating_duration_since(frag_t));
                        }
                        if let Some(ref tx) = pe_tx {
                            let _ = tx.send(PipelineEvent::TtsFirstAudio {
                                utterance_id,
                                elapsed_ms: now.saturating_duration_since(process_start).as_millis() as u64,
                            });
                        }
                        // First real audio: consume the cold-start flag.
                        // `swap` returns the previous value; we record whether
                        // *this* call observed cold = true.
                        cold_start_observed = tts_cold_start.swap(false, Ordering::SeqCst);
                    }
                    // Track running sample total + native sample rate so we can
                    // derive audio-duration / RTF after the loop.
                    if !c.samples.is_empty() {
                        tts_total_samples += c.samples.len() as u64;
                        if tts_observed_rate.is_none() && c.sample_rate > 0 {
                            tts_observed_rate = Some(c.sample_rate);
                        }
                        if c.channels > 0 {
                            tts_observed_channels = c.channels;
                        }
                        if c.sample_rate > 0 {
                            let chunk_ms = (c.samples.len() as u64
                                / u64::from(c.channels.max(1)))
                                * 1000
                                / u64::from(c.sample_rate);
                            output_throughput.on_source(chunk_ms);
                        }
                    }

                    // ── AudioLedger bookkeeping ─────────────────────
                    // When a chunk carries CaptionMeta, it marks the start
                    // of a new fragment.  Flush the previous fragment's
                    // accumulated samples into the ledger.
                    if let Some(ref caption) = c.caption {
                        fragment.flush(&ledger);
                        fragment.start(caption, c.sample_rate);
                    }
                    fragment.samples += c.samples.len() as u64;

                    // Flush final fragment on end-of-utterance sentinel.
                    if c.is_end_of_utterance {
                        // Stamp total TTS synthesis duration: sentinel chunk synth_duration or active elapsed.
                        if !tts_skipped {
                            tts_synthesis = c.synth_duration.or_else(|| first_frag_instant.map(|t| t.elapsed()));
                        }
                        fragment.flush(&ledger);
                    }

                    // On the EOU sentinel, embed the full LLM text directly in
                    // the chunk so it travels through the same ordered pipeline
                    // as the audio -- no separate async task, no race possible.
                    // The client will emit a synthetic text event from this field
                    // in the same JS call-stack tick as the EOU audio event.
                    if c.is_end_of_utterance {
                        let text_result = tokio::time::timeout(
                            std::time::Duration::from_millis(500),
                            &mut accumulated_text_rx,
                        ).await;
                        if let Ok(Ok(text)) = text_result {
                            c.full_text = Some(text);
                        }
                    }

                    // Run post-TTS processors.  Failures are logged and skipped
                    // inside `run_chain` -- the chunk is never dropped.
                    if !post_processors.is_empty() {
                        post_audio::run_chain(&post_processors, &mut c, &event_tx, &mut chain_stats)
                            .await;
                    }

                    let has_audio = !c.samples.is_empty();
                    tokio::select! {
                        biased;
                        () = cancel_fwd.cancelled() => break,
                        res = audio_tx.send(c) => {
                            if res.is_err() {
                                break;
                            }
                            if has_audio {
                                output_throughput.on_emit();
                                first_emit_instant.get_or_insert_with(std::time::Instant::now);
                            }
                        }
                    }
                }
                None => break,
            },
        }
    }

    // No post-loop text dispatch needed -- full_text is embedded directly in
    // the EOU sentinel above.  If the loop exited early (cancellation), the
    // chunk was never sent so there is nothing to confirm on the client.

    // Push remaining partial fragment into ledger on cancellation.
    fragment.flush(&ledger);

    // Ensure spawned tasks finish. Give `llm_handle` a brief window to finish
    // recording partial assistant history upon cooperative cancellation before
    // force-aborting it.
    bridge_handle.abort();
    tts_handle.abort();
    if cancel.is_cancelled() {
        if tokio::time::timeout(std::time::Duration::from_millis(250), &mut llm_handle)
            .await
            .is_err()
        {
            llm_handle.abort();
            let _ = llm_handle.await;
        }
    } else {
        let _ = llm_handle.await;
    }
    let _ = bridge_handle.await;
    let _ = tts_handle.await;
    if let Some(ref tx) = pe_tx {
        let _ = tx.send(PipelineEvent::TtsCompleted { utterance_id });
    }

    // Collect LLM generation metrics from the bridge.
    let (llm_generation, llm_token_estimate, llm_token_count) = match llm_done_rx.await.ok() {
        Some((d, est, count)) => (Some(d), Some(est), count),
        None => (None, None, None),
    };

    // ── Derive TTS-side aggregates ─────────────────────────
    let tts_audio_duration = match (tts_total_samples, tts_observed_rate) {
        (samples, Some(rate)) if samples > 0 && rate > 0 => {
            let secs = voice_pipes_sdk::audio::audio_duration_seconds(
                samples,
                rate,
                tts_observed_channels,
            );
            Some(std::time::Duration::from_secs_f64(secs))
        }
        _ => None,
    };
    let tts_rtf = match (tts_synthesis, tts_audio_duration) {
        (Some(synth), Some(audio)) if audio.as_secs_f64() > 0.0 => {
            Some((synth.as_secs_f64() / audio.as_secs_f64()) as f32)
        }
        _ => None,
    };
    let tts_rtfx = match (tts_synthesis, tts_audio_duration) {
        (Some(synth), Some(audio)) if synth.as_secs_f64() > 0.0 => {
            Some((audio.as_secs_f64() / synth.as_secs_f64()) as f32)
        }
        _ => None,
    };
    // Characters actually fed to TTS (the agent's reply, from the
    // ledger), not the user's transcript: RTF-per-character and cost
    // analysis in the admin BFF are computed against this count.
    let tts_chars = u32::try_from(
        ledger
            .lock()
            .entries
            .iter()
            .map(|e| e.text.chars().count())
            .sum::<usize>(),
    )
    .ok();
    let tts_provider_name = Some(tts.name().to_string());
    // "No audio produced" detection: only meaningful when the turn was not
    // cancelled (cancellation is the user's fault, not the TTS engine's).
    let tts_error = (tts_total_samples == 0 && !cancel.is_cancelled())
        .then(|| "tts produced no audio".to_string());
    // If we never produced audio, the cold flag was not consumed -- preserve
    // it so the next turn gets a chance to be the cold one.  Restore by
    // re-storing the snapshot we took at entry.
    if !cold_start_observed && cold_start_was_set && tts_total_samples == 0 {
        tts_cold_start.store(true, Ordering::SeqCst);
    }

    let ttft = ttft_rx.await.ok();
    let (ttft_canonical, ttft_perceived) = match ttft {
        Some((c, p)) => (Some(c), Some(p)),
        None => (None, None),
    };
    let process_total = commit_start.elapsed();

    // Build the snapshot via TurnRecorder anchored at `commit_start`.
    let recorder = TurnRecorder::new_at(commit_start);

    // STT span -- work happened upstream of process_utterance so the span
    // is synthesised at the snapshot-building stage with its measured window.
    // Agent-initiated turns have no user speech at all, so no STT span.
    if !utterance.source.is_agent() {
        let mut stt_span = recorder.begin("stt", "stt");
        let stt_inference = utterance.inference_metrics.as_ref();
        let stt_proc_ms = utterance.stt_latency.map(|d| d.as_millis() as u64);
        stt_span.extras_typed(&SttExtras {
            processing_ms: stt_proc_ms,
            endpointing_ms: utterance.endpointing_latency.map(|d| d.as_millis() as u64),
            stt_audio_duration_ms: stt_inference.and_then(|m| m.audio_duration_ms),
            stt_rtfx: stt_inference.and_then(|m| m.rtfx),
            stt_algorithmic_delay_ms: stt_inference.and_then(|m| m.algorithmic_delay_ms),
            stt_effective_latency_ms: stt_inference.and_then(|m| m.effective_latency_ms),
            stt_inference_count: stt_inference.and_then(|m| m.inference_count),
            stt_streaming_window_ms: stt_inference.and_then(|m| m.streaming_window_ms),
        });
        stt_span.finish_with_window(0, stt_proc_ms);
    }

    // Telescoping critical-path stages (`t_commit -> t_first_frag -> t_first_tts_audio -> t_first_emit`).
    use super::extras::round_tenths_ms;
    let llm_skip_reason = if is_accepted {
        Some("speculative_accept".to_string())
    } else if utterance.source == UtteranceSource::AgentSay {
        Some("agent_say".to_string())
    } else {
        None
    };
    let llm_ran = llm_skip_reason.is_none();
    let llm_wait_ms = llm_ran
        .then(|| {
            first_frag_instant.map(|f| round_tenths_ms(f.saturating_duration_since(commit_start)))
        })
        .flatten();
    let tts_skip_reason = tts_skipped.then(|| "speculative_accept".to_string());
    let tts_stage_anchor = if llm_ran {
        first_frag_instant
    } else {
        Some(commit_start)
    };
    let tts_wait_ms = (!tts_skipped)
        .then(|| {
            tts_stage_anchor
                .zip(first_tts_audio_instant)
                .map(|(f, a)| round_tenths_ms(a.saturating_duration_since(f)))
        })
        .flatten();
    let output_stage_anchor = if tts_skipped {
        Some(commit_start)
    } else {
        first_tts_audio_instant
    };
    let output_wait_ms = output_stage_anchor
        .zip(first_emit_instant)
        .map(|(a, e)| round_tenths_ms(e.saturating_duration_since(a)));
    let commit_to_first_audio_ms =
        first_emit_instant.map(|e| round_tenths_ms(e.saturating_duration_since(commit_start)));

    // LLM span.
    let mut llm_span = recorder.begin("llm", "llm");
    llm_span.set_provider(llm_provider_name.clone());
    llm_span.extras_typed(&LlmExtras {
        ttft_ms: llm_ran
            .then_some(ttft_canonical)
            .flatten()
            .map(|d| d.as_millis() as u64),
        generation_ms: llm_ran
            .then_some(llm_generation)
            .flatten()
            .map(|d| d.as_millis() as u64),
        token_estimate: llm_ran.then_some(llm_token_estimate).flatten(),
        token_count: llm_ran.then_some(llm_token_count).flatten(),
        llm_perceived_ttft_ms: llm_ran
            .then_some(ttft_perceived)
            .flatten()
            .map(|d| d.as_millis() as u64),
        llm_wait_ms,
        stage_skipped_reason: llm_skip_reason,
        speculative_rollbacks: spec_rollbacks,
        speculative_tokens_generated: spec_tokens_generated,
        speculative_tokens_accepted: spec_tokens_accepted,
        speculative_efficiency: spec_efficiency,
    });
    if llm_ran {
        let llm_start_ms = process_start
            .saturating_duration_since(commit_start)
            .as_millis() as u64;
        let llm_end_ms = llm_generation.map(|d| llm_start_ms + d.as_millis() as u64);
        llm_span.finish_with_window(llm_start_ms, llm_end_ms);
    } else {
        llm_span.finish_with_window(0, None);
    }

    // TTS span.
    let mut tts_span = recorder.begin("tts", "tts");
    if let Some(name) = tts_provider_name.as_ref() {
        tts_span.set_provider(name.clone());
    }
    tts_span.set_cold_start(cold_start_observed);
    if let Some(err) = tts_error.as_ref() {
        tts_span.set_error(err.clone());
    }
    tts_span.extras_typed(&TtsExtras {
        ttfb_ms: (!tts_skipped)
            .then(|| tts_ttfb.map(|d| d.as_millis() as u64))
            .flatten(),
        tts_wait_ms,
        output_wait_ms,
        commit_to_first_audio_ms,
        stage_skipped_reason: tts_skip_reason,
        synthesis_ms: (!tts_skipped)
            .then(|| tts_synthesis.map(|d| d.as_millis() as u64))
            .flatten(),
        audio_duration_ms: tts_audio_duration.map(|d| d.as_millis() as u64),
        sample_rate: tts_observed_rate,
        chars: tts_chars,
        rtf: tts_rtf,
        rtfx: tts_rtfx,
    });
    if !tts_skipped && let Some(frag_t) = tts_stage_anchor {
        let tts_start_ms = frag_t.saturating_duration_since(commit_start).as_millis() as u64;
        let tts_end_ms = tts_synthesis.map(|d| tts_start_ms + d.as_millis() as u64);
        tts_span.finish_with_window(tts_start_ms, tts_end_ms);
    } else {
        tts_span.finish_with_window(0, None);
    }

    let mut snapshot = recorder.snapshot(utterance_id, cancel.is_cancelled(), None);
    snapshot.total_ms = process_total.as_millis() as u64;
    // Streaming-phase throughput + post-chain stats (interrupted turns
    // included: the span delivered so far).
    snapshot
        .components
        .extend(output_throughput.finish_at(Some(commit_start), commit_to_first_audio_ms));
    snapshot
        .components
        .extend(chain_stats.components_at(Some(commit_start)));
    if utterance.source.is_agent() {
        snapshot.initiator = crate::pipeline::metrics::TurnInitiator::Agent;
    }
    snapshot
}

/// Run the LLM (text) chain with first-token-gated retry.
///
/// Mirrors [`super::tts::spawn_synthesis_task`] for text streaming:
/// each retry attempt writes into a per-attempt inner channel; we relay
/// fragments to `frag_tx` as soon as they arrive, flipping the
/// `token_seen` gate on the first one.  When the chain returns a transient
/// `Err` before any token has crossed the gate (and `max_retries > 0` and
/// the turn has not been cancelled), the call is replayed after a jittered
/// exponential backoff (from 100ms, capped at 1s), or after the wait a
/// rate limit names; a rate limit asking for more than
/// `LLM_RETRY_MAX_RATE_LIMIT_WAIT` fails the turn instead.  Once a token has
/// been forwarded any later error propagates as-is so the client never
/// sees duplicated output.
///
/// Returns `Ok(())` on chain success, the latest error on terminal
/// failure (retries exhausted or token already forwarded), or `Ok(())`
/// when cancelled mid-attempt or when the downstream `frag_tx` was
/// dropped.
pub(super) async fn run_llm_chain_with_retry(
    processors: Arc<Vec<Box<dyn TextProcessor>>>,
    text: String,
    frag_tx: mpsc::Sender<TextFragment>,
    cancel: CancellationToken,
    max_retries: u8,
    // Agent `Generate` turn: `text` is injected context, not user
    // input — the chain runs via `process_context_stream` so
    // history-keeping processors don't record a synthetic user turn.
    context_mode: bool,
) -> Result<(), text::TextProcessorError> {
    let mut attempts_remaining = max_retries;
    let mut token_seen = false;

    loop {
        let (inner_tx, mut inner_rx) = mpsc::channel::<TextFragment>(16);
        let processors_attempt = std::sync::Arc::clone(&processors);
        let text_attempt = text.clone();
        let cancel_attempt = cancel.clone();
        let chain_handle = tokio::spawn(async move {
            if context_mode {
                text::run_chain_context_stream(
                    &processors_attempt,
                    &text_attempt,
                    inner_tx,
                    cancel_attempt,
                )
                .await
            } else {
                text::run_chain_stream(&processors_attempt, &text_attempt, inner_tx, cancel_attempt)
                    .await
            }
        });

        // Relay fragments until the inner channel closes.
        while let Some(frag) = inner_rx.recv().await {
            token_seen = true;
            if frag_tx.send(frag).await.is_err() {
                if cancel.is_cancelled() {
                    // Let `chain_handle` finish its cooperative cancellation path
                    // (e.g. `ConversationMemoryWrapper` appending partial text to
                    // history) rather than aborting it mid-commit.
                    drop(inner_rx);
                    break;
                }
                chain_handle.abort();
                return Ok(());
            }
        }

        let chain_result = match chain_handle.await {
            Ok(r) => r,
            Err(je) if je.is_panic() => Err(text::TextProcessorError::new(
                "text-chain",
                format!("LLM task panicked: {je}"),
            )),
            Err(je) => Err(text::TextProcessorError::new("text-chain", je.to_string())),
        };

        match chain_result {
            Ok(()) => return Ok(()),
            Err(e) => {
                // Only a failure that may pass is worth another attempt; a
                // refused request or a bad configuration fails again.
                // A rate limit longer than a spoken turn can wait fails the
                // turn now rather than holding the reply.
                let waits_too_long = e
                    .retry_after()
                    .is_some_and(|wait| wait > LLM_RETRY_MAX_RATE_LIMIT_WAIT);
                if !token_seen
                    && e.is_transient()
                    && !waits_too_long
                    && attempts_remaining > 0
                    && !cancel.is_cancelled()
                {
                    let attempt = u32::from(max_retries - attempts_remaining + 1);
                    let backoff = voice_pipes_sdk::reconnect::backoff_with_jitter(
                        std::time::Duration::from_millis(LLM_RETRY_BASE_BACKOFF_MS),
                        std::time::Duration::from_millis(LLM_RETRY_MAX_BACKOFF_MS),
                        attempt,
                    );
                    // A rate limit is waited out for as long as the service
                    // asked, up to the cap above.
                    let delay = e.retry_after().map_or(backoff, |wait| wait.max(backoff));
                    tracing::warn!(
                        error = %e,
                        attempt,
                        of = max_retries,
                        delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                        "LLM call failed before any token; retrying after backoff"
                    );
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = cancel.cancelled() => return Ok(()),
                    }
                    attempts_remaining -= 1;
                    continue;
                }
                if waits_too_long && !token_seen {
                    tracing::warn!(
                        error = %e,
                        "LLM rate limit asks for a longer wait than a turn allows; the turn fails"
                    );
                }
                if token_seen && attempts_remaining > 0 {
                    tracing::warn!(
                        error = %e,
                        "LLM call failed after a token was already forwarded; retry skipped to avoid duplicated output"
                    );
                }
                return Err(e);
            }
        }
    }
}
