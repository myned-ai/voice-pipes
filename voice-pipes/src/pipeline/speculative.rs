use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::types::AudioChunk;
use crate::config::SpeculativeConfig;
use crate::text::{self, TextFragment, TextProcessor};
use crate::tts::TtsProvider;

/// The result of committing a turn.
#[derive(Debug)]
pub enum SpeculativeCommitResult {
    /// The speculation matched the committed transcript and was accepted.
    Accepted {
        rollbacks: u32,
        tokens_generated: u32,
        tokens_accepted: u32,
        /// The full accumulated assistant response text from the accepted
        /// speculation (buffered LLM fragments concatenated, matching the
        /// non-speculative bridge). Used as the turn's `full_text` in the EOU
        /// sentinel -- must NOT be confused with the user transcript.
        accepted_text: String,
        /// If `speculative_tts` is true, contains the buffered audio chunks
        /// and a receiver for subsequent audio chunks.
        audio_stream: Option<(Vec<AudioChunk>, mpsc::Receiver<AudioChunk>)>,
        /// If `speculative_tts` is false, contains the buffered text fragments
        /// and a receiver for subsequent text fragments.
        text_stream: Option<(Vec<TextFragment>, mpsc::Receiver<TextFragment>)>,
    },
    /// The speculation diverged from the committed transcript and was rejected.
    Rejected {
        rollbacks: u32,
        tokens_generated: u32,
    },
}

pub struct SpeculativeSessionManager {
    processors: Arc<Vec<Box<dyn TextProcessor>>>,
    tts: Arc<dyn TtsProvider>,
    config: SpeculativeConfig,
    pub(crate) state: Arc<Mutex<SpeculativeState>>,
}

impl std::fmt::Debug for SpeculativeSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpeculativeSessionManager")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

pub(crate) struct SpeculativeState {
    /// Baseline history snapshot taken at the start of the turn
    baseline_snapshot: Option<Vec<Option<Box<dyn std::any::Any + Send + Sync>>>>,
    /// Last partial text we triggered speculation for
    pub(crate) last_speculative_input: String,
    /// When the active speculative run was started
    last_run_time: Option<Instant>,
    /// Active cancellation token for LLM/TTS streaming
    active_cancel: Option<CancellationToken>,
    /// Buffered LLM text fragments from the active run
    pub(crate) buffered_fragments: Vec<TextFragment>,
    /// Active task handles
    active_llm_task: Option<tokio::task::JoinHandle<()>>,
    active_coordinator_task: Option<tokio::task::JoinHandle<()>>,
    /// Buffered audio chunks from the active run
    pub(crate) buffered_audio: Vec<AudioChunk>,
    /// Sender to forward subsequent fragments to when accepted
    outer_text_tx: Option<mpsc::Sender<TextFragment>>,
    /// Sender to forward subsequent audio chunks to when accepted
    outer_audio_tx: Option<mpsc::Sender<AudioChunk>>,
    /// Metrics: rollbacks count
    rollbacks: u32,
    /// Metrics: generated tokens count
    pub(crate) tokens_generated: u32,
    /// Metrics: accepted tokens count
    tokens_accepted: u32,
    /// Whether a turn is currently active/initialized
    turn_active: bool,
    /// Set by the coordinator when the speculative LLM and TTS
    /// have both finished for the active run. `commit()` must not install
    /// an outer sender after this point — nothing would ever drop it, and
    /// the consumer in `process_utterance` would wait on it forever.
    pub(crate) production_done: bool,
}

impl SpeculativeSessionManager {
    pub fn new(
        processors: Arc<Vec<Box<dyn TextProcessor>>>,
        tts: Arc<dyn TtsProvider>,
        config: SpeculativeConfig,
    ) -> Self {
        Self {
            processors,
            tts,
            config,
            state: Arc::new(Mutex::new(SpeculativeState {
                baseline_snapshot: None,
                last_speculative_input: String::new(),
                last_run_time: None,
                active_cancel: None,
                buffered_fragments: Vec::new(),
                active_llm_task: None,
                active_coordinator_task: None,
                buffered_audio: Vec::new(),
                outer_text_tx: None,
                outer_audio_tx: None,
                rollbacks: 0,
                tokens_generated: 0,
                tokens_accepted: 0,
                turn_active: false,
                production_done: false,
            })),
        }
    }

    /// Reset and prepare the manager for a new user turn, taking a snapshot of history.
    pub async fn start_turn(&self) {
        let mut state = self.state.lock().await;

        // Take snapshot of current stateful history in the processor chain
        let snapshot: Vec<Option<Box<dyn std::any::Any + Send + Sync>>> = self
            .processors
            .iter()
            .map(|p| p.snapshot_history())
            .collect();

        // Cancel any existing tasks
        if let Some(cancel) = state.active_cancel.take() {
            cancel.cancel();
        }
        if let Some(h) = state.active_llm_task.take() {
            h.abort();
        }
        if let Some(h) = state.active_coordinator_task.take() {
            h.abort();
        }

        state.baseline_snapshot = Some(snapshot);
        state.last_speculative_input.clear();
        state.last_run_time = None;
        state.buffered_fragments.clear();
        state.buffered_audio.clear();
        state.outer_text_tx = None;
        state.outer_audio_tx = None;
        state.rollbacks = 0;
        state.tokens_generated = 0;
        state.tokens_accepted = 0;
        state.turn_active = true;
        state.production_done = false;

        tracing::debug!("Speculative turn initialized; history snapshot taken");
    }

    /// Process a new partial STT transcript.
    pub async fn handle_partial(&self, text: &str) {
        let mut state = self.state.lock().await;
        if !state.turn_active {
            return;
        }

        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }

        if text == state.last_speculative_input {
            return;
        }

        // Apply throttle window
        if let Some(last_run) = state.last_run_time {
            let elapsed = last_run.elapsed();
            if elapsed < Duration::from_millis(self.config.min_partial_interval_ms) {
                tracing::debug!(
                    chars = text.chars().count(),
                    elapsed_ms = elapsed.as_millis(),
                    "Speculative execution throttled"
                );
                return;
            }
        }

        // Cancel previous active run
        if let Some(cancel) = state.active_cancel.take() {
            cancel.cancel();
            state.rollbacks += 1;
            tracing::info!(
                prev_chars = state.last_speculative_input.chars().count(),
                next_chars = text.chars().count(),
                "STT partial updated/diverged: cancelling active speculation and rolling back"
            );
        }
        if let Some(h) = state.active_llm_task.take() {
            h.abort();
        }
        if let Some(h) = state.active_coordinator_task.take() {
            h.abort();
        }

        // Restore history baseline
        if let Some(ref snapshot) = state.baseline_snapshot {
            for (p, snap) in self.processors.iter().zip(snapshot) {
                if let Some(s) = snap {
                    p.restore_history(s.as_ref());
                }
            }
        }

        // Prepare new run
        let cancel = CancellationToken::new();
        state.active_cancel = Some(cancel.clone());
        state.last_speculative_input.clone_from(&text);
        state.last_run_time = Some(Instant::now());
        state.buffered_fragments.clear();
        state.buffered_audio.clear();
        state.outer_text_tx = None;
        state.outer_audio_tx = None;
        state.production_done = false;

        let processors = Arc::clone(&self.processors);
        let text_input = text.clone();
        let cancel_llm = cancel.clone();

        let (llm_frag_tx, mut llm_frag_rx) = mpsc::channel::<TextFragment>(32);

        // Spawn LLM task
        let active_llm_task = tokio::spawn(async move {
            if cancel_llm.is_cancelled() {
                return;
            }
            let res =
                text::run_chain_stream(&processors, &text_input, llm_frag_tx, cancel_llm).await;
            if let Err(e) = res {
                tracing::warn!(error = %e, "Speculative LLM stream failed");
            }
        });
        state.active_llm_task = Some(active_llm_task);

        // Set up speculative coordinator task
        let state_clone = Arc::clone(&self.state);
        let cancel_coordinator = cancel.clone();
        let tts = Arc::clone(&self.tts);
        let config_tts = self.config.speculative_tts;

        let active_coordinator_task = tokio::spawn(async move {
            let mut tts_tx: Option<mpsc::Sender<TextFragment>> = None;
            let mut audio_rx_opt: Option<mpsc::Receiver<AudioChunk>> = None;
            let mut tts_handle: Option<tokio::task::JoinHandle<()>> = None;

            if config_tts {
                let (t_tx, t_rx) = mpsc::channel::<TextFragment>(32);
                let (a_tx, a_rx) = mpsc::channel::<AudioChunk>(32);
                tts_tx = Some(t_tx);
                audio_rx_opt = Some(a_rx);

                let tts_provider = Arc::clone(&tts);
                let tts_session_dummy = Arc::new(tokio::sync::Mutex::new(None));
                let cancel_tts = cancel_coordinator.clone();
                let error_reported_dummy = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (event_dummy_tx, _) = mpsc::channel::<crate::transport::ServerMessage>(32);

                let handle = tokio::spawn(async move {
                    super::tts::tts_task(
                        t_rx,
                        a_tx,
                        tts_provider,
                        tts_session_dummy,
                        None,
                        None,
                        cancel_tts,
                        0,
                        None,
                        event_dummy_tx,
                        error_reported_dummy,
                    )
                    .await;
                });
                tts_handle = Some(handle);
            }

            let mut llm_done = false;
            loop {
                if cancel_coordinator.is_cancelled() {
                    break;
                }
                if llm_done && audio_rx_opt.is_none() {
                    break;
                }

                tokio::select! {
                    biased;
                    () = cancel_coordinator.cancelled() => break,

                    frag_opt = llm_frag_rx.recv(), if !llm_done => {
                        if let Some(frag) = frag_opt {
                            let tokens = frag.token_count.unwrap_or_else(|| {
                                frag.text.split_whitespace().count() as u32
                            });
                            // Never hold the state mutex across the
                            // bounded outer send — once that channel fills
                            // (the consumer drains at playback pace) every
                            // other manager call would queue behind it.
                            let outer = {
                                let mut s = state_clone.lock().await;
                                s.tokens_generated += tokens;
                                s.buffered_fragments.push(frag.clone());
                                s.outer_text_tx.clone()
                            };
                            if let Some(tx) = outer {
                                let _ = tx.send(frag.clone()).await;
                            }
                            if let Some(ref tx) = tts_tx {
                                let _ = tx.send(frag).await;
                            }
                        } else {
                            llm_done = true;
                            drop(tts_tx.take()); // Signal TTS that LLM is done
                        }
                    }

                    audio_opt = async {
                        if let Some(ref mut rx) = audio_rx_opt {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    }, if audio_rx_opt.is_some() => {
                        match audio_opt {
                            Some(chunk) => {
                                let outer = {
                                    let mut s = state_clone.lock().await;
                                    s.buffered_audio.push(chunk.clone());
                                    s.outer_audio_tx.clone()
                                };
                                if let Some(tx) = outer {
                                    let _ = tx.send(chunk).await;
                                }
                            }
                            None => {
                                audio_rx_opt = None;
                            }
                        }
                    }
                }
            }

            if let Some(h) = tts_handle {
                let _ = h.await;
            }
            // Production is over. Drop the outer senders so an
            // accepted consumer sees end-of-stream, and remember it so a
            // later `commit()` does not hand out a sender nobody will drop.
            let mut s = state_clone.lock().await;
            s.production_done = true;
            s.outer_text_tx = None;
            s.outer_audio_tx = None;
        });

        state.active_coordinator_task = Some(active_coordinator_task);
    }

    /// Commit the turn, deciding whether to accept or reject the speculation.
    pub async fn commit(&self, final_text: &str) -> SpeculativeCommitResult {
        let mut state = self.state.lock().await;
        state.turn_active = false;

        let final_normalized = normalize_text(final_text);
        let speculative_normalized = normalize_text(&state.last_speculative_input);

        let is_match = !final_normalized.is_empty()
            && !speculative_normalized.is_empty()
            && final_normalized == speculative_normalized;

        let rollbacks = state.rollbacks;
        let tokens_generated = state.tokens_generated;

        if is_match {
            tracing::info!(
                final_chars = final_text.chars().count(),
                speculative_chars = state.last_speculative_input.chars().count(),
                "Speculative execution accepted!"
            );

            let accepted_tokens: u32 = state
                .buffered_fragments
                .iter()
                .map(|f| {
                    f.token_count
                        .unwrap_or_else(|| f.text.split_whitespace().count() as u32)
                })
                .sum();
            state.tokens_accepted = accepted_tokens;

            // Full assistant response text, concatenated the same way the
            // non-speculative bridge accumulates it (`push_str`, no separators).
            let accepted_text: String = state
                .buffered_fragments
                .iter()
                .map(|f| f.text.as_str())
                .collect();

            // When the coordinator already finished, the buffered
            // items are the whole response; the returned receiver must
            // yield `None` right after them, so its sender is dropped here
            // instead of being parked in `state` with nothing to drop it.
            let production_done = state.production_done;
            let audio_stream = self.config.speculative_tts.then(|| {
                let buffered = std::mem::take(&mut state.buffered_audio);
                let (tx, rx) = mpsc::channel::<AudioChunk>(32);
                if !production_done {
                    state.outer_audio_tx = Some(tx);
                }
                (buffered, rx)
            });

            let text_stream = (!self.config.speculative_tts).then(|| {
                let buffered = std::mem::take(&mut state.buffered_fragments);
                let (tx, rx) = mpsc::channel::<TextFragment>(32);
                if !production_done {
                    state.outer_text_tx = Some(tx);
                }
                (buffered, rx)
            });

            SpeculativeCommitResult::Accepted {
                rollbacks,
                tokens_generated,
                tokens_accepted: accepted_tokens,
                accepted_text,
                audio_stream,
                text_stream,
            }
        } else {
            tracing::info!(
                final_chars = final_text.chars().count(),
                speculative_chars = state.last_speculative_input.chars().count(),
                "Speculative execution rejected (mismatch)"
            );

            // Cancel tasks
            if let Some(cancel) = state.active_cancel.take() {
                cancel.cancel();
            }
            if let Some(h) = state.active_llm_task.take() {
                h.abort();
            }
            if let Some(h) = state.active_coordinator_task.take() {
                h.abort();
            }

            // Restore history baseline to clear out speculation mutations
            if let Some(ref snapshot) = state.baseline_snapshot {
                for (p, snap) in self.processors.iter().zip(snapshot) {
                    if let Some(s) = snap {
                        p.restore_history(s.as_ref());
                    }
                }
            }

            // Mismatch counts as an additional rollback
            let final_rollbacks = rollbacks + 1;
            state.rollbacks = final_rollbacks;

            SpeculativeCommitResult::Rejected {
                rollbacks: final_rollbacks,
                tokens_generated,
            }
        }
    }

    /// Cancel the current speculative run immediately.
    pub async fn cancel(&self) {
        let mut state = self.state.lock().await;
        state.turn_active = false;

        if let Some(cancel) = state.active_cancel.take() {
            cancel.cancel();
        }
        if let Some(h) = state.active_llm_task.take() {
            h.abort();
        }
        if let Some(h) = state.active_coordinator_task.take() {
            h.abort();
        }

        // Restore baseline history state
        if let Some(ref snapshot) = state.baseline_snapshot {
            for (p, snap) in self.processors.iter().zip(snapshot) {
                if let Some(s) = snap {
                    p.restore_history(s.as_ref());
                }
            }
        }
    }
}

/// Helper to normalize transcripts for fuzzy comparison
fn normalize_text(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "tests/speculative.rs"]
mod tests;
