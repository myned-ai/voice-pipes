use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::audit::streaming::AuditEvent;
use crate::pipeline::events::{
    HistoryModification, InterruptionTrigger, PipelineEvent, TruncationMethod,
};
use crate::pipeline::{
    self, AudioChunk, AudioLedger, PipelineTurn, SharedTurnState, TurnMetricsSnapshot, TurnState,
    Utterance, UtteranceSource, get_turn_state,
};
use crate::transport::{
    MessageRole, PipelineMessage, ServerControl, ServerMessage, SessionConfig, SessionMode,
};

use super::turn::{self, TurnTracker};
use super::utils::{handle_inject_audio, handle_tts_only, inject_text_utterance};
use crate::transport::serve::TransportState;
use crate::transport::serve::gateway::engine_driver::EngineDriver;
use crate::transport::serve::gateway::state_machine::TurnTrigger;

impl EngineDriver for ClassicSessionContext<'_> {
    fn cancel_turn(&mut self) -> impl Future<Output = anyhow::Result<()>> {
        if let Some(cancel) = self.tracker.current_cancel.take() {
            cancel.cancel();
        }
        std::future::ready(Ok(()))
    }

    fn resolve_played_text(&self, utterance_id: u32, offset_ms: u32) -> Option<String> {
        self.audio_ledgers
            .get(&utterance_id)
            .and_then(|ledger| ledger.lock().text_at_offset(offset_ms as u64))
    }
}

pub struct ClassicSessionContext<'a> {
    pub session_id: &'a str,
    pub tracker: &'a mut TurnTracker,
    pub suppress_audio: &'a Arc<AtomicBool>,
    pub out_tx: &'a mpsc::Sender<ServerMessage>,
    /// Off-path audio annotators for this session.
    /// Teed from `handle_audio_output`, cancelled on every `ClearAudio`,
    /// and asked for per-turn metrics at turn end. Empty = no-op.
    pub annotators: &'a crate::pipeline::AnnotatorHubs,
    /// The session's turn detector, shared with the STT task.
    /// Teed the outbound audio alongside `annotators` so a detector that
    /// reads both sides of the conversation can see what the agent is
    /// playing. `None` when the session runs without a cascade STT task.
    pub turn_detector: Option<&'a Arc<dyn crate::stt::turn::TurnDetector>>,
    /// Continuous two-sided recorder for this session, when configured.
    /// Fed the user's audio before the VAD gates it and the agent's audio
    /// as it goes out, so the recording keeps the silence and the relative
    /// timing that the per-utterance artefacts drop.
    pub session_recorder: Option<&'a crate::audit::SessionRecorder>,
    pub audio_tx: &'a mpsc::Sender<AudioChunk>,
    pub metrics_rx: &'a mut mpsc::Receiver<TurnMetricsSnapshot>,
    pub proc_audio_rx: &'a mut mpsc::Receiver<AudioChunk>,
    pub turn_state: &'a SharedTurnState,
    pub session_processors: &'a Arc<Vec<Box<dyn crate::text::TextProcessor>>>,
    pub streaming_audit: &'a Option<crate::audit::StreamingAuditSink>,
    pub pe_tx: &'a tokio::sync::broadcast::Sender<PipelineEvent>,
    pub stt_name: &'a str,
    pub tts_name: &'a str,
    pub client_sample_rate: u32,
    pub negotiated_output: Option<u32>,
    pub audio_ledgers:
        &'a mut std::collections::HashMap<u32, std::sync::Arc<parking_lot::Mutex<AudioLedger>>>,
    pub barge_in_sent_at: &'a mut Option<std::time::Instant>,
    pub tts_session: Arc<tokio::sync::Mutex<Option<Box<dyn crate::tts::TtsSession>>>>,
    pub spec_manager: Option<Arc<pipeline::SpeculativeSessionManager>>,
    pub sanitize_cfg: Option<voice_pipes_sdk::SanitizeConfig>,
    /// Per-session client-tool allowlist.
    /// On cascade the driving LLM surfaces calls as
    /// [`AgentEvent::ClientToolCall`](crate::agent::AgentEvent); the
    /// name is validated here before the command is routed to the
    /// client — the pipeline never executes a tool.
    /// Where link signals go (see `transport::adaptive`). Cloneable and
    /// non-blocking, so reporting can never stall the audio path.
    pub observations: crate::transport::adaptive::ObservationSink,
    pub client_tools: &'a crate::agent::client_tools::ClientToolIndex,
    /// In-flight client tool calls awaiting a `ClientToolResult`
    /// (bounded: hard cap + TTL; session-local).
    pub client_calls: &'a mut crate::agent::client_tools::ClientToolCalls,
}

impl ClassicSessionContext<'_> {
    pub async fn handle_audio(&mut self, mut chunk: AudioChunk, session_mode: SessionMode) -> bool {
        if !session_mode.accepts_audio() {
            if self.tracker.audio_chunk_count == 0 {
                warn!(
                    session_id = %self.session_id,
                    mode = ?session_mode,
                    "audio received but session mode does not accept audio -- ignoring"
                );
            }
            self.tracker.audio_chunk_count += 1;
            return true;
        }
        self.tracker.audio_chunk_count += 1;
        chunk.sample_rate = self.client_sample_rate;
        if self.tracker.audio_chunk_count <= 3 {
            info!(
                session_id = %self.session_id,
                chunk_num = self.tracker.audio_chunk_count,
                samples = chunk.samples.len(),
                sample_rate = chunk.sample_rate,
                "audio chunk diagnostic"
            );
        }

        // TtsSession user audio tee point.
        //
        // `try_lock`, never `lock().await`. The stateful synth task
        // holds this mutex for the whole `synthesize_turn`, so awaiting it
        // here would park the session loop (VAD, STT, barge-in, pongs) for
        // the length of the agent's reply. The tee is best-effort; a frame
        // skipped while synthesis holds the lock is the lesser harm.
        if let Ok(mut s_lock) = self.tts_session.try_lock()
            && let Some(s) = s_lock.as_mut()
        {
            let frame = crate::audio::AudioFrame {
                samples: chunk.samples.clone(),
                sample_rate: chunk.sample_rate,
                channels: chunk.channels,
            };
            if let Err(e) = s.push_user_audio(&frame).await {
                warn!(session_id = %self.session_id, error = %e, "failed to push user audio to stateful TTS session");
            }
        }

        // Continuous recorder tee, before the VAD: what it drops is exactly
        // what a turn-taking model needs to see.
        if let Some(recorder) = self.session_recorder {
            recorder.record(
                crate::audit::SessionAudioSide::User,
                &chunk.samples,
                chunk.sample_rate,
                chunk.channels,
            );
        }

        // Never wait for room: this runs in the session loop, and a speech
        // path that has stopped draining would otherwise stop Cancel,
        // barge-in and pings with it. A full path drops the chunk instead,
        // warning once per backlog.
        match self.audio_tx.try_send(chunk) {
            Ok(()) => {
                if self.tracker.mic_backlogged {
                    self.tracker.mic_backlogged = false;
                    info!(
                        session_id = %self.session_id,
                        dropped_total = self.tracker.mic_chunks_dropped,
                        "speech path caught up; microphone audio flows again"
                    );
                }
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.tracker.mic_chunks_dropped += 1;
                if !self.tracker.mic_backlogged {
                    self.tracker.mic_backlogged = true;
                    warn!(
                        session_id = %self.session_id,
                        "speech path is full; dropping microphone audio until it drains"
                    );
                }
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                warn!(session_id = %self.session_id, "STT audio channel closed");
                false
            }
        }
    }

    pub async fn handle_cancel(&mut self) {
        debug!(session_id = %self.session_id, "client requested cancel (barge-in)");
        self.client_interrupt("client_cancel").await;
    }

    /// Tear down the active turn on a client-originated interrupt (an
    /// explicit `Cancel`, or typed text arriving mid-turn).
    ///
    /// Three situations are told apart by the tracker:
    /// - No active turn: there is nothing to cancel and the turn machine
    ///   does not move. The client may still hold buffered audio from an
    ///   earlier turn, so it is asked to flush, and that is all.
    /// - Active turn still producing: the pipeline is cancelled, its
    ///   metrics collected once and emitted as interrupted, and the turn
    ///   moves to Draining.
    /// - Active turn already complete on the server (metrics emitted, only
    ///   the client-side tail is still playing): the tail is cut with
    ///   `ClearAudio` and an `Interrupted` event, but no second metrics
    ///   snapshot is produced and the aggregate is not touched.
    async fn client_interrupt(&mut self, reason: &str) {
        let utterance_id = self.tracker.current_utterance_id;
        if self.tracker.current_cancel.is_none() {
            debug!(
                session_id = %self.session_id,
                reason,
                "interrupt with no active turn: asking the client to flush, state unchanged"
            );
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::ClearAudio {
                    utterance_id,
                    reason: reason.to_string(),
                }))
                .await;
            return;
        }
        let already_complete = self.tracker.metrics_emitted;

        self.suppress_audio.store(true, Ordering::Release);
        let _ = self.cancel_turn().await;
        if let Some(ref manager) = self.spec_manager {
            manager.cancel().await;
        }

        // Cancel stateful TTS session on client cancel
        {
            let mut s_lock = self.tts_session.lock().await;
            if let Some(s) = s_lock.as_mut()
                && let Err(e) = s.cancel().await
            {
                warn!(session_id = %self.session_id, error = %e, "failed to cancel stateful TTS session during client cancel");
            }
        }
        // Recorded once, by the session's event recorder (`session_events`).
        let _ = self.pe_tx.send(PipelineEvent::Interrupted {
            utterance_id,
            trigger: InterruptionTrigger::ClientCancel,
            played_ms: None,
        });
        self.annotators.cancel(utterance_id);
        let _ = self
            .out_tx
            .send(ServerMessage::Control(ServerControl::ClearAudio {
                utterance_id,
                reason: reason.to_string(),
            }))
            .await;
        *self.barge_in_sent_at = Some(std::time::Instant::now());
        if !already_complete {
            let mut metrics = turn::collect_metrics(self.metrics_rx, true).await;
            metrics
                .components
                .extend(self.annotators.turn_components(utterance_id));
            turn::emit_turn_metrics(
                self.tracker,
                true,
                &metrics,
                self.streaming_audit,
                self.out_tx,
                self.pe_tx,
                self.stt_name,
                self.tts_name,
            )
            .await;
        }
        // When no audio was synthesized yet (or when a typed text interrupt is
        // about to immediately advance to the next utterance before a
        // `PlaybackReport` can arrive), truncate any partial assistant output
        // recorded in LLM history immediately so the next turn does not assume
        // the user heard it.
        if self.synthesized_duration_ms(utterance_id).is_none() || reason == "text_interrupt" {
            self.truncate_unheard_turn(utterance_id);
        }
        turn::transition_to_draining(
            self.session_id,
            self.tracker,
            self.turn_state,
            self.out_tx,
            self.proc_audio_rx,
        )
        .await;
        self.suppress_audio.store(false, Ordering::Release);
    }

    pub async fn handle_ping(&mut self, timestamp_ms: u64) {
        crate::transport::serve::gateway::shared_handlers::handle_ping(self.out_tx, timestamp_ms)
            .await;
    }

    pub async fn handle_playback_complete(&mut self, utterance_id: u32) {
        if let Some(reason) = self.tracker.stale_playback_reason(utterance_id) {
            warn!(
                session_id = %self.session_id,
                utterance_id,
                current_utterance_id = self.tracker.current_utterance_id,
                last_finalized = ?self.tracker.last_finalized_utterance_id,
                reason,
                "ignoring stale PlaybackComplete from client"
            );
            return;
        }
        let current_state = get_turn_state(self.turn_state);
        match current_state {
            // The pipeline is still producing this turn: a playback-complete
            // now (a manual client control, or a client that misjudged its
            // queue) would abandon the remaining audio and leave the turn's
            // metrics to be picked up by the next turn.
            TurnState::Speaking if !self.tracker.eou_sent => {
                warn!(
                    session_id = %self.session_id,
                    utterance_id,
                    "ignoring PlaybackComplete: the turn is still producing audio"
                );
            }
            TurnState::Speaking | TurnState::Draining => {
                debug!(
                    session_id = %self.session_id,
                    utterance_id,
                    prior_state = ?current_state,
                    "received playback complete from client"
                );
                self.tracker.mark_playback_finalized(utterance_id);
                // The turn is over: drop its (already-finished) cancel
                // token. Leaving it in place kept `current_cancel`
                // `Some(..)` until the NEXT turn started, so every
                // "no active turn" check (idle re-engagement arming,
                // queued agent-speech drain) stayed false forever after
                // the first completed turn.
                self.tracker.current_cancel = None;
                turn::transition(
                    self.session_id,
                    self.turn_state,
                    self.out_tx,
                    TurnTrigger::PlaybackFinalized,
                )
                .await;
                if let Some(ref manager) = self.spec_manager {
                    manager.start_turn().await;
                }
            }
            TurnState::Listening | TurnState::Processing => {
                warn!(
                    session_id = %self.session_id,
                    utterance_id,
                    state = ?current_state,
                    "ignoring PlaybackComplete: pipeline is not in a playback state"
                );
            }
        }
    }

    /// Total audio (ms) actually synthesized for a turn, per its ledger.
    /// `None` when no ledger exists or nothing was produced. Used to clamp a
    /// client's reported playback offset — the user cannot have heard more
    /// audio than was ever produced.
    fn synthesized_duration_ms(&self, utterance_id: u32) -> Option<u64> {
        self.audio_ledgers
            .get(&utterance_id)
            .map(|ledger| ledger.lock().total_duration_ms)
            .filter(|&ms| ms > 0)
    }

    /// Mark any assistant reply committed during `utterance_id` as completely
    /// unheard (`""`) in conversation history when a turn is cancelled before
    /// client playback can report an offset.
    fn truncate_unheard_turn(&self, utterance_id: u32) {
        for proc in self.session_processors.iter() {
            if let Some(original) = proc.truncate_history("") {
                info!(
                    session_id = %self.session_id,
                    utterance_id,
                    original_len = original.len(),
                    "truncated unheard assistant turn from conversation history"
                );
                let _ = self.pe_tx.send(PipelineEvent::HistoryModified {
                    modification: HistoryModification {
                        utterance_id,
                        original_text: original,
                        retained_text: String::new(),
                        method: TruncationMethod::PlaybackReport,
                        played_ms: Some(0),
                    },
                });
                break;
            }
        }
    }

    /// Store client-reported playback health (underruns / inserted silence)
    /// on the tracker for the turn record and write the `NNN_playback.json`
    /// capture artifact.
    pub fn handle_playback_quality(
        &mut self,
        utterance_id: u32,
        health: crate::api::types::PlaybackHealth,
    ) {
        debug!(
            session_id = %self.session_id,
            utterance_id,
            underruns = health.underrun_count,
            total_silence_ms = health.total_silence_ms,
            "received playback quality from client"
        );
        if let Some(audit) = self.streaming_audit {
            audit.record(crate::audit::AuditEvent::PlaybackHealth {
                utterance_id,
                health: health.clone(),
            });
        }
        // S2 for the adaptive controller: the client's own account of whether
        // audio arrived fast enough to play.
        self.observations
            .observe(turn::playback_observation(&health));
        self.tracker.playback_health = Some(health);
    }

    pub async fn handle_playback_report(
        &mut self,
        utterance_id: u32,
        reported_offset_ms: u32,
        last_played_text: Option<String>,
    ) {
        if let Some(reason) = self.tracker.stale_playback_reason(utterance_id) {
            warn!(
                session_id = %self.session_id,
                utterance_id,
                current_utterance_id = self.tracker.current_utterance_id,
                last_finalized = ?self.tracker.last_finalized_utterance_id,
                offset_ms = reported_offset_ms,
                reason,
                "ignoring stale PlaybackReport from client"
            );
            return;
        }

        // Guard against a client reporting more audio "played" than was
        // ever synthesized for this turn (buffered/scheduled or silent-underrun
        // audio miscounted as heard). Clamp to the synthesized duration so the
        // history truncation and barge-in telemetry can never over-claim what
        // the user actually heard.
        let last_played_offset_ms = match self.synthesized_duration_ms(utterance_id) {
            Some(max_ms) if u64::from(reported_offset_ms) > max_ms => {
                warn!(
                    session_id = %self.session_id,
                    utterance_id,
                    reported_offset_ms,
                    synthesized_ms = max_ms,
                    "PlaybackReport offset exceeds synthesized audio; clamping (client over-reported played audio)"
                );
                max_ms as u32
            }
            _ => reported_offset_ms,
        };

        debug!(
            session_id = %self.session_id,
            utterance_id,
            offset_ms = last_played_offset_ms,
            "received playback report from client"
        );

        let played_text = last_played_text
            .filter(|t| !t.is_empty())
            .or_else(|| self.resolve_played_text(utterance_id, last_played_offset_ms))
            .unwrap_or_default();

        for proc in self.session_processors.iter() {
            if let Some(original) = proc.truncate_history(&played_text) {
                info!(
                    session_id = %self.session_id,
                    utterance_id,
                    original_len = original.len(),
                    retained_len = played_text.len(),
                    "truncated conversation history via playback report"
                );
                let _ = self.pe_tx.send(PipelineEvent::HistoryModified {
                    modification: HistoryModification {
                        utterance_id,
                        original_text: original,
                        retained_text: played_text.clone(),
                        method: TruncationMethod::PlaybackReport,
                        played_ms: Some(last_played_offset_ms),
                    },
                });
                let _ = self
                    .out_tx
                    .send(ServerMessage::Control(ServerControl::TextTruncated {
                        utterance_id,
                        final_text: played_text.clone(),
                    }))
                    .await;
                break;
            }
        }

        let mut barge_in_rtt_ms = None;
        if let Some(sent_at) = self.barge_in_sent_at.take() {
            let rtt_ms = sent_at.elapsed().as_millis() as u64;
            barge_in_rtt_ms = Some(rtt_ms);
            debug!(
                session_id = %self.session_id,
                utterance_id,
                rtt_ms,
                "barge-in RTT measured"
            );
            if let Some(audit) = self.streaming_audit {
                audit.record(crate::audit::AuditEvent::Trace(
                    crate::audit::TraceEvent::new(
                        "transport",
                        "barge_in_rtt",
                        serde_json::json!({
                            "utterance_id": utterance_id,
                            "rtt_ms": rtt_ms,
                            "last_played_offset_ms": last_played_offset_ms,
                        }),
                    ),
                ));
            }
        }

        // Recorded once, by the session's event recorder (`session_events`).
        let _ = self.pe_tx.send(PipelineEvent::BargeInResolved {
            utterance_id,
            trigger: InterruptionTrigger::ServerVad,
            played_ms: Some(last_played_offset_ms),
            retained_text: played_text,
            barge_in_rtt_ms,
        });
    }

    pub fn handle_timing_report(
        &mut self,
        utterance_id: u32,
        event: crate::transport::TimingEventKind,
        client_timestamp_ms: u64,
    ) {
        let event_name = event.as_str().to_string();
        crate::transport::serve::gateway::shared_handlers::emit_client_timing(
            self.session_id,
            self.pe_tx,
            self.streaming_audit,
            utterance_id,
            &event_name,
            client_timestamp_ms,
        );
        if utterance_id == self.tracker.current_utterance_id {
            self.tracker
                .client_timings
                .push(crate::api::types::ClientTimingEntry {
                    event: event_name,
                    client_timestamp_ms,
                });
        }
    }

    pub async fn handle_pipeline_message(
        &mut self,
        pipeline_msg: PipelineMessage,
        text_utt_tx: &mpsc::Sender<Utterance>,
    ) -> bool {
        match pipeline_msg.kind.as_str() {
            "text" => {
                let text = pipeline_msg.text.clone().unwrap_or_default();
                if text.is_empty() {
                    debug!(session_id = %self.session_id, "ignoring empty text message");
                } else {
                    info!(
                        session_id = %self.session_id,
                        message_id = %pipeline_msg.message_id,
                        "text input via pipeline message"
                    );
                    if self.tracker.current_cancel.is_some() {
                        info!(
                            session_id = %self.session_id,
                            utterance_id = self.tracker.current_utterance_id,
                            "text message interruption: cancelling current turn"
                        );
                        self.client_interrupt("text_interrupt").await;
                    }
                    let _ = self
                        .out_tx
                        .send(ServerMessage::Message(PipelineMessage::text(
                            &text,
                            MessageRole::User,
                        )))
                        .await;

                    inject_text_utterance(&text, self.tracker, text_utt_tx).await;
                }
            }
            other => {
                debug!(
                    session_id = %self.session_id,
                    kind = other,
                    "unhandled pipeline message kind"
                );
            }
        }
        true
    }

    /// A `configure` arriving after the handshake is **ignored**, not applied.
    ///
    /// `client_sample_rate` is bound once from the negotiated config when the
    /// runner starts and never reassigned, and the STT provider carries that
    /// rate into its own upstream handshake -- honouring a mid-session change
    /// would mean tearing down and reopening that stream, along with the VAD
    /// windows and endpointing timers calibrated on it. Every comparable
    /// protocol fixes the input format at setup for the same reason (WebRTC
    /// renegotiates through SDP; Deepgram and `AssemblyAI` take the rate as a
    /// connection parameter). If an adaptive uplink rate lands it gets a
    /// purpose-built frame with utterance-boundary semantics rather than this
    /// general-purpose bag.
    ///
    /// Warn rather than info: a client that believes it changed the rate and
    /// did not will send audio the server silently misinterprets, and there is
    /// no other symptom to find it by. The realtime engine already warns on
    /// the same message.
    pub fn handle_configure(&mut self, cfg: SessionConfig) {
        warn!(
            session_id = %self.session_id,
            requested_sample_rate = cfg.sample_rate,
            negotiated_sample_rate = self.client_sample_rate,
            "ignoring mid-session `configure`: audio format is fixed at the handshake"
        );
    }

    pub fn handle_inject_audio(
        &mut self,
        source: String,
        scenario_id: Option<String>,
        experiment_id: Option<String>,
        speed: f32,
    ) {
        handle_inject_audio(
            self.session_id,
            source,
            scenario_id,
            experiment_id,
            speed,
            self.audio_tx,
            self.out_tx,
        );
    }

    pub fn handle_tts_only_synthesize(
        &mut self,
        id: String,
        text: String,
        language: Option<String>,
        voice: Option<String>,
        tts_cold_start: &Arc<AtomicBool>,
        state: &TransportState,
    ) {
        let tts_for_tts_only = Arc::clone(
            state
                .tts
                .as_ref()
                .expect("classic runner handles TtsOnlySynthesize with a local TTS provider"),
        );
        handle_tts_only(
            self.session_id.to_string(),
            id,
            text,
            language,
            voice,
            tts_for_tts_only,
            Arc::clone(tts_cold_start),
            state.config.pipeline.audit.captures_dir.clone(),
            self.out_tx.clone(),
        );
    }

    pub async fn handle_server_barge_in(&mut self) {
        self.tracker.current_cancel = None;
        let barge_in_start = std::time::Instant::now();
        let current_state = get_turn_state(self.turn_state);
        let is_early = current_state == TurnState::Processing;
        // The server-side turn already finished and reported its metrics;
        // the user is talking over the tail the client is still playing.
        // Only that tail is cut: no second metrics snapshot, no second
        // aggregate entry.
        let already_complete = self.tracker.metrics_emitted;
        let trigger = if is_early {
            InterruptionTrigger::EarlyBarge
        } else {
            InterruptionTrigger::ServerVad
        };
        let played_ms_opt = is_early.then_some(0);

        // Cancel stateful TTS session on server barge-in
        {
            let mut s_lock = self.tts_session.lock().await;
            if let Some(s) = s_lock.as_mut()
                && let Err(e) = s.cancel().await
            {
                warn!(session_id = %self.session_id, error = %e, "failed to cancel stateful TTS session during server barge-in");
            }
        }

        info!(
            session_id = %self.session_id,
            utterance_id = self.tracker.current_utterance_id,
            is_early,
            "server-side barge-in: utterance interrupted"
        );
        self.suppress_audio.store(true, Ordering::Release);
        self.annotators.cancel(self.tracker.current_utterance_id);
        if let Some(ref manager) = self.spec_manager {
            manager.cancel().await;
        }
        debug!(session_id = %self.session_id, "barge-in: suppress_audio = true");
        // Recorded once, by the session's event recorder (`session_events`).
        let _ = self.pe_tx.send(PipelineEvent::Interrupted {
            utterance_id: self.tracker.current_utterance_id,
            trigger: trigger.clone(),
            played_ms: played_ms_opt,
        });

        if !is_early {
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::ClearAudio {
                    utterance_id: self.tracker.current_utterance_id,
                    reason: "server_vad".to_string(),
                }))
                .await;
            *self.barge_in_sent_at = Some(std::time::Instant::now());
            debug!(
                session_id = %self.session_id,
                elapsed_ms = barge_in_start.elapsed().as_millis() as u64,
                "barge-in: ClearAudio sent to client"
            );
        }

        if !already_complete {
            let mut metrics = turn::collect_metrics(self.metrics_rx, true).await;
            metrics.components.extend(
                self.annotators
                    .turn_components(self.tracker.current_utterance_id),
            );
            turn::emit_turn_metrics(
                self.tracker,
                true,
                &metrics,
                self.streaming_audit,
                self.out_tx,
                self.pe_tx,
                self.stt_name,
                self.tts_name,
            )
            .await;
        }
        if is_early {
            // `collect_metrics` above awaited `process_utterance` (which
            // awaited `llm_handle`), so any partial LLM output for this turn
            // has now been committed to history. Mark it as unheard (`""`)
            // before the next turn begins.
            self.truncate_unheard_turn(self.tracker.current_utterance_id);
            let _ = self.pe_tx.send(PipelineEvent::BargeInResolved {
                utterance_id: self.tracker.current_utterance_id,
                trigger: trigger.clone(),
                played_ms: Some(0),
                retained_text: String::new(),
                barge_in_rtt_ms: None,
            });
        }
        turn::transition_to_draining(
            self.session_id,
            self.tracker,
            self.turn_state,
            self.out_tx,
            self.proc_audio_rx,
        )
        .await;
        if is_early {
            let utt_id = turn::transition_early_barge_to_listening(
                self.session_id,
                self.tracker,
                self.turn_state,
                self.out_tx,
                self.proc_audio_rx,
            )
            .await;
            if let Some(ref manager) = self.spec_manager {
                manager.start_turn().await;
            }
            debug!(
                session_id = %self.session_id,
                utterance_id = utt_id,
                "early barge-in: short-circuited Draining -> Listening (no client handshake possible)"
            );
        }
        self.suppress_audio.store(false, Ordering::Release);
        debug!(
            session_id = %self.session_id,
            total_ms = barge_in_start.elapsed().as_millis() as u64,
            "barge-in: complete, suppress_audio = false"
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "per-turn plumbing handed from the session loop; a param struct would just relocate the list"
    )]
    pub async fn handle_utterance(
        &mut self,
        utterance: Utterance,
        cancel_watch_tx: &tokio::sync::watch::Sender<CancellationToken>,
        proc_audio_tx: &mpsc::Sender<AudioChunk>,
        session_post_processors: &Arc<Vec<Box<dyn crate::pipeline::AudioPostProcessor>>>,
        session_output_chunking: &Option<crate::config::OutputChunkingConfig>,
        metrics_tx: &mpsc::Sender<TurnMetricsSnapshot>,
        tts_cold_start: &Arc<std::sync::atomic::AtomicBool>,
        state: &TransportState,
    ) {
        // Turn boundary: commit user turn on VAD end-of-speech.
        // Agent-initiated turns have no user turn to commit.
        if !utterance.source.is_agent() {
            let mut s_lock = self.tts_session.lock().await;
            if let Some(s) = s_lock.as_mut()
                && let Err(e) = s.commit_user_turn().await
            {
                warn!(session_id = %self.session_id, error = %e, "failed to commit user turn to stateful TTS session");
            }
        }

        info!(
            session_id = %self.session_id,
            utterance_id = utterance.utterance_id,
            chars = utterance.text.chars().count(),
            "processing utterance"
        );
        let _ = self.pe_tx.send(PipelineEvent::UtteranceReceived {
            utterance_id: utterance.utterance_id,
            text: utterance.text.clone(),
            source: format!("{:?}", utterance.source),
        });
        if utterance.source == UtteranceSource::Audio {
            let _ = self.pe_tx.send(PipelineEvent::SttCompleted {
                utterance_id: utterance.utterance_id,
                elapsed_ms: utterance.stt_latency.map(|d| d.as_millis() as u64),
            });
        }

        // Echo the user's words back as a chat bubble. Only for real
        // user speech: typed text is echoed on injection, and agent
        // turns have no user message at all.
        if utterance.source == UtteranceSource::Audio {
            let _ = self
                .out_tx
                .send(ServerMessage::Message(PipelineMessage {
                    kind: "text".into(),
                    text: Some(utterance.text.clone()),
                    payload: None,
                    binary: None,
                    role: MessageRole::User,
                    turn_id: None,
                    message_id: format!("stt-{}", utterance.utterance_id),
                }))
                .await;
        }

        let trigger = if utterance.source.is_agent() {
            TurnTrigger::AgentTurnStarted
        } else {
            TurnTrigger::UtteranceCommitted
        };
        turn::transition(self.session_id, self.turn_state, self.out_tx, trigger).await;

        // A snapshot from an abandoned previous turn must not be mistaken
        // for this one's.
        turn::drain_stale_metrics(self.session_id, self.metrics_rx);
        let pipe_turn = PipelineTurn::new();
        self.tracker.start_turn_at(
            utterance.utterance_id,
            pipe_turn.cancel.clone(),
            utterance.committed_at,
        );
        let _ = cancel_watch_tx.send(pipe_turn.cancel.clone());

        let tx = proc_audio_tx.clone();
        let procs = Arc::clone(self.session_processors);
        let post_procs = Arc::clone(session_post_processors);
        let tts = Arc::clone(
            state
                .tts
                .as_ref()
                .expect("classic runner processes utterances with a local TTS provider"),
        );
        let output_rate = self.negotiated_output;
        let output_chunking = session_output_chunking.clone();
        let cancel = pipe_turn.cancel.clone();
        let aggression_audit = self.streaming_audit.clone();
        let m_tx = metrics_tx.clone();
        let event_tx_utt = self.out_tx.clone();
        let pe_tx_utt = Some(self.pe_tx.clone());
        let cold_start_arg = std::sync::Arc::clone(tts_cold_start);

        let ledger = std::sync::Arc::new(parking_lot::Mutex::new(AudioLedger::default()));
        self.audio_ledgers
            .insert(utterance.utterance_id, std::sync::Arc::clone(&ledger));
        while self.audio_ledgers.len() > 8 {
            if let Some(&oldest) = self.audio_ledgers.keys().min() {
                if oldest == self.tracker.current_utterance_id {
                    break;
                }
                self.audio_ledgers.remove(&oldest);
            }
        }

        let tts_session_arg = Arc::clone(&self.tts_session);
        let spec_manager_arg = self.spec_manager.clone();
        let sanitize_cfg_arg = self.sanitize_cfg.clone();
        tokio::spawn(async move {
            let metrics = pipeline::process_utterance(
                &utterance,
                tx,
                event_tx_utt,
                procs,
                post_procs,
                tts,
                Some(tts_session_arg),
                output_rate,
                output_chunking,
                cancel,
                aggression_audit,
                pe_tx_utt,
                std::sync::Arc::clone(&ledger),
                cold_start_arg,
                spec_manager_arg,
                sanitize_cfg_arg,
            )
            .await;
            let _ = m_tx.send(metrics).await;
        });
    }

    /// Apply one [`AgentEvent`](crate::agent::AgentEvent) on the runner
    /// task (single-writer turn-state rule). Speech events (`Say` /
    /// `Generate` / `CompleteTool`) synthesize a full agent-initiated
    /// turn via [`handle_utterance`](Self::handle_utterance) when the
    /// session is idle; otherwise they queue (`WhenIdle`), preempt
    /// (`Interrupt` — cancel now, speak once Listening), or record
    /// context only (`Silent`). `Message` forwards to the client with
    /// no turn at all.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::ref_option,
        reason = "event dispatcher mirroring handle_utterance's borrowed-args \
                  shape; the loop passes the same locals to both"
    )]
    pub async fn handle_agent_event(
        &mut self,
        event: crate::agent::AgentEvent,
        pending_agent: &mut std::collections::VecDeque<crate::agent::AgentEvent>,
        cancel_watch_tx: &tokio::sync::watch::Sender<CancellationToken>,
        proc_audio_tx: &mpsc::Sender<AudioChunk>,
        session_post_processors: &Arc<Vec<Box<dyn crate::pipeline::AudioPostProcessor>>>,
        session_output_chunking: &Option<crate::config::OutputChunkingConfig>,
        metrics_tx: &mpsc::Sender<TurnMetricsSnapshot>,
        tts_cold_start: &Arc<std::sync::atomic::AtomicBool>,
        state: &TransportState,
    ) {
        use crate::agent::{AgentEvent, Scheduling};

        // Normalize the speech-bearing variants to (text, source,
        // scheduling); handle the rest inline.
        let (text, source, scheduling) = match event {
            AgentEvent::Message {
                message,
                record_in_history,
            } => {
                // Out-of-turn client message: no turn, no TTS, no
                // state change.
                let history_repr = record_in_history.then(|| {
                    message
                        .text
                        .clone()
                        .or_else(|| {
                            message
                                .payload
                                .as_ref()
                                .map(std::string::ToString::to_string)
                        })
                        .unwrap_or_default()
                });
                let _ = self.out_tx.send(ServerMessage::Message(message)).await;
                if let Some(repr) = history_repr
                    && !repr.is_empty()
                {
                    let note = format!("A non-spoken message was shown to the user: {repr}");
                    for p in self.session_processors.iter() {
                        p.inject_context(&note).await;
                    }
                }
                return;
            }
            AgentEvent::Cancel => {
                if self.tracker.current_cancel.is_some() {
                    self.handle_cancel().await;
                }
                return;
            }
            #[cfg(feature = "realtime")]
            AgentEvent::ToolDispatchDone { name, .. } => {
                warn!(
                    session_id = %self.session_id,
                    tool = %name,
                    "ToolDispatchDone on a classic session -- dropped (realtime-runner internal)"
                );
                return;
            }
            AgentEvent::ClientToolCall {
                call_id,
                name,
                arguments,
            } => {
                self.handle_client_tool_call(call_id, name, arguments).await;
                return;
            }
            AgentEvent::Say { text, scheduling } => (text, UtteranceSource::AgentSay, scheduling),
            AgentEvent::Generate {
                context,
                scheduling,
            } => (context, UtteranceSource::AgentGenerate, scheduling),
            AgentEvent::CompleteTool {
                call_id,
                result,
                scheduling,
            } => (
                format!(
                    "The background task for tool call {call_id} has completed. \
                     Result: {result}. Tell the user about this result."
                ),
                UtteranceSource::AgentGenerate,
                scheduling,
            ),
        };

        // Silent: record in conversation context only — never spoken.
        if scheduling == Scheduling::Silent {
            for p in self.session_processors.iter() {
                p.inject_context(&text).await;
            }
            return;
        }

        let idle = self.tracker.current_cancel.is_none()
            && get_turn_state(self.turn_state) == TurnState::Listening;
        if !idle {
            match scheduling {
                Scheduling::Interrupt => {
                    // Tear the active turn down now; the event re-queues
                    // and fires as soon as the state machine returns to
                    // Listening (drain arm in the session loop).
                    self.handle_cancel().await;
                    crate::agent::queue_pending(
                        pending_agent,
                        match source {
                            UtteranceSource::AgentSay => AgentEvent::Say {
                                text,
                                scheduling: Scheduling::WhenIdle,
                            },
                            _ => AgentEvent::Generate {
                                context: text,
                                scheduling: Scheduling::WhenIdle,
                            },
                        },
                        true,
                        self.session_id,
                    );
                }
                _ => {
                    crate::agent::queue_pending(
                        pending_agent,
                        match source {
                            UtteranceSource::AgentSay => AgentEvent::Say { text, scheduling },
                            _ => AgentEvent::Generate {
                                context: text,
                                scheduling,
                            },
                        },
                        false,
                        self.session_id,
                    );
                }
            }
            return;
        }

        // Idle: open the agent turn through the normal utterance path so
        // ledger, metrics, captions, ClearAudio, and barge-in come free.
        if source == UtteranceSource::AgentSay {
            // The text IS the assistant output; record it in history at
            // generation time (playback truncation adjusts later, same
            // as LLM-generated turns).
            for p in self.session_processors.iter() {
                p.append_assistant_message(&text).await;
            }
        }
        let utterance = Utterance {
            text,
            utterance_id: self.tracker.next_text_id(),
            confidence: None,
            stt_latency: None,
            source,
            endpointing_latency: None,
            inference_metrics: None,
            committed_at: Some(std::time::Instant::now()),
        };
        self.handle_utterance(
            utterance,
            cancel_watch_tx,
            proc_audio_tx,
            session_post_processors,
            session_output_chunking,
            metrics_tx,
            tts_cold_start,
            state,
        )
        .await;
    }

    /// Route a client-side tool call surfaced by the driving LLM: validate against the allowlist,
    /// register for result correlation, and send the `trigger_action`
    /// command to the client. No turn, no TTS, no state change — the
    /// LLM turn ends prose-free on its own (`finish_reason:
    /// "tool_calls"` carries no text), so silence is the default.
    /// Failures are injected as silent context so the model doesn't
    /// assume the action happened.
    async fn handle_client_tool_call(
        &mut self,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    ) {
        use crate::agent::client_tools;

        let Some(tool) = self.client_tools.lookup(&name) else {
            warn!(
                session_id = %self.session_id,
                call_id = %call_id,
                tool = %name,
                "client tool call references unknown tool -- rejected"
            );
            let note =
                format!("The tool call '{name}' failed: no such client-side tool is available.");
            for p in self.session_processors.iter() {
                p.inject_context(&note).await;
            }
            return;
        };
        match self
            .client_calls
            .try_register(&call_id, &name, tool.speak_on_result)
        {
            Ok(()) => {
                info!(
                    session_id = %self.session_id,
                    call_id = %call_id,
                    tool = %name,
                    "routing client tool call to the client"
                );
                // Session-details record: which tool was called, with what.
                if let Some(audit) = self.streaming_audit {
                    audit.record(AuditEvent::Trace(crate::audit::TraceEvent::new(
                        "client_tools",
                        "client_tool_call",
                        client_tools::call_audit_payload(
                            &call_id,
                            &name,
                            &arguments,
                            tool.speak_on_result,
                            None,
                        ),
                    )));
                }
                // The command must not be dropped under backpressure, so
                // this is a blocking send on the session's own queue.
                let msg = client_tools::trigger_action_message(
                    &call_id,
                    &name,
                    arguments,
                    tool.speak_on_result,
                );
                let _ = self.out_tx.send(ServerMessage::Message(msg)).await;
            }
            Err(e) => {
                warn!(
                    session_id = %self.session_id,
                    call_id = %call_id,
                    tool = %name,
                    error = %e,
                    "client tool call rejected"
                );
                let note = format!("The client-side tool call '{name}' was rejected: {e}.");
                for p in self.session_processors.iter() {
                    p.inject_context(&note).await;
                }
            }
        }
    }

    /// Resolve a client-reported tool result against the in-flight map.
    /// Returns the agent event to feed back through
    /// [`handle_agent_event`](Self::handle_agent_event): `Generate` +
    /// `Silent` (context-only injection) by default, `Generate` +
    /// `WhenIdle` (a spoken confirmation turn) when the tool or the
    /// client opted into speech. `None` for interim phases and
    /// unknown/expired/duplicate ids.
    pub fn resolve_client_tool_result(
        &mut self,
        call_id: &str,
        phase: crate::transport::ClientToolPhase,
        result: Option<&serde_json::Value>,
        speak_requested: bool,
    ) -> Option<crate::agent::AgentEvent> {
        use crate::agent::client_tools;
        // Peek before resolving — a terminal result removes the entry.
        let name = self.client_calls.peek(call_id).map(|m| m.name.clone());
        let disposition = client_tools::resolve_result(
            self.client_calls,
            call_id,
            phase,
            result,
            speak_requested,
        );
        // Session-details record: how the call resolved. Accepted
        // resolutions AND interim acks (Started/Progress of a known call)
        // are recorded — the ack timestamp lets the diagnostics separate
        // "long-running client action by design" from "client never
        // responded". Dropped duplicates/unknown ids stay log-only.
        let spoken = match &disposition {
            client_tools::ResultDisposition::Context { speak, .. } => Some(*speak),
            client_tools::ResultDisposition::Acked => Some(false),
            client_tools::ResultDisposition::Ignore => None,
        };
        if let Some(spoken) = spoken
            && let Some(audit) = self.streaming_audit
        {
            audit.record(AuditEvent::Trace(crate::audit::TraceEvent::new(
                "client_tools",
                "client_tool_result",
                client_tools::result_audit_payload(
                    call_id,
                    name.as_deref(),
                    phase,
                    result,
                    spoken,
                    speak_requested,
                ),
            )));
        }
        client_tools::disposition_event(disposition)
    }

    pub async fn handle_audio_output(
        &mut self,
        c: AudioChunk,
        client_config: &Option<SessionConfig>,
        state: &TransportState,
    ) {
        let was_cancelled = self.tracker.current_cancel.is_none();
        if was_cancelled {
            return;
        }

        if !c.samples.is_empty() && get_turn_state(self.turn_state) != TurnState::Speaking {
            turn::transition(
                self.session_id,
                self.turn_state,
                self.out_tx,
                TurnTrigger::AudioOutputStarted,
            )
            .await;
        }

        self.tracker.record_ttfb(!c.samples.is_empty());

        if let Some(ref caption) = c.caption {
            let _ = self.pe_tx.send(PipelineEvent::CaptionEmitted {
                utterance_id: self.tracker.current_utterance_id,
                fragment_idx: caption.fragment_idx,
                text: caption.text.clone(),
            });
        }

        let is_end = c.is_end_of_utterance;
        if !c.samples.is_empty() || is_end {
            // Tee for off-path annotators: the chunk has already been through
            // the inline chain, so annotators see mutated samples. Never blocks.
            self.annotators.offer(&c);
            // Same tee for the turn detector's agent channel: a detector that
            // predicts from both sides needs what the client will hear. The
            // built-in rule detectors ignore it.
            if let Some(detector) = self.turn_detector {
                detector.feed_agent_audio(&c.samples, c.sample_rate);
            }
            if let Some(recorder) = self.session_recorder {
                recorder.record(
                    crate::audit::SessionAudioSide::Agent,
                    &c.samples,
                    c.sample_rate,
                    c.channels,
                );
            }
            let _ = self.out_tx.send(ServerMessage::Audio(c)).await;
        }

        if is_end {
            self.tracker.eou_sent = true;
            let was_cancelled = self.tracker.current_cancel.is_none();
            if !was_cancelled {
                let mut pipe_metrics = turn::collect_metrics(self.metrics_rx, true).await;
                pipe_metrics.components.extend(
                    self.annotators
                        .turn_components(self.tracker.current_utterance_id),
                );
                let total_ms = self.tracker.total_latency().as_millis() as u64;
                let _ = self.pe_tx.send(PipelineEvent::TurnCompleted {
                    utterance_id: self.tracker.current_utterance_id,
                    total_ms,
                });
                let snapshot = turn::emit_turn_metrics(
                    self.tracker,
                    false,
                    &pipe_metrics,
                    self.streaming_audit,
                    self.out_tx,
                    self.pe_tx,
                    self.stt_name,
                    self.tts_name,
                )
                .await;
                if let Some(ref tx) = state.turn_complete_tx {
                    let tags = client_config
                        .as_ref()
                        .map(|c| c.tags.clone())
                        .unwrap_or_default();
                    let record = turn::build_turn_record(
                        self.session_id,
                        &snapshot,
                        self.tracker,
                        self.stt_name,
                        self.tts_name,
                        self.client_sample_rate,
                        self.negotiated_output,
                        &state.config.pipeline.name,
                        tags,
                    );
                    turn::emit_record(tx, record, "turn");
                }
            }

            turn::transition_to_draining(
                self.session_id,
                self.tracker,
                self.turn_state,
                self.out_tx,
                self.proc_audio_rx,
            )
            .await;
        }
    }
}
