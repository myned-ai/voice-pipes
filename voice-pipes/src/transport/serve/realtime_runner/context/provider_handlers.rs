//! Provider-event handlers for [`RealtimeSessionContext`].

#[allow(
    clippy::wildcard_imports,
    reason = "handler submodules deliberately share the context module's import surface"
)]
use super::*;

use std::collections::VecDeque;

use crate::agent::{AgentEvent, Scheduling};
use crate::realtime::{ToolError, ToolOutcome};
use crate::transport::{ClientControl, ClientMessage};
use voice_pipes_sdk::secrets::Secret;

use super::super::turn::Resumption;

/// Consecutive provider closes (each within the rapid-close window of
/// the previous — see the runner's `Closed` arm) after which reconnects
/// stop even for closes the classifier calls transient. A healthy
/// reconnect survives longer than the window; only a permanent
/// condition re-closes this fast repeatedly.
pub(crate) const MAX_RAPID_PROVIDER_CLOSES: u32 = 3;

/// Reconnects one session may perform over its whole life. A provider
/// that keeps closing with a transient-looking reason, slowly enough to
/// stay outside the rapid-close window, would otherwise be reconnected
/// forever, each attempt a fresh billed conversation.
pub(crate) const MAX_SESSION_RECONNECTS: u32 = 10;

/// Back-off before a reconnect attempt: doubles with each close in the
/// current rapid streak, capped so a recovering provider is retried
/// within a few seconds.
pub(crate) fn reconnect_backoff(rapid_closes: u32) -> std::time::Duration {
    let exponent = rapid_closes.saturating_sub(1).min(4);
    std::time::Duration::from_secs(1u64 << exponent)
}

/// Run `work` (a reconnect, with its back-off) while still serving the
/// client: pings are answered, so the client does not take the wait for a
/// dead connection, and microphone audio is dropped, since there is no
/// provider to send it to. `None` when the client leaves or the worker shuts
/// down first; the work is then abandoned.
pub(crate) async fn while_serving_client<T>(
    in_rx: &mut mpsc::Receiver<ClientMessage>,
    out_tx: &mpsc::Sender<ServerMessage>,
    shutdown: &tokio_util::sync::CancellationToken,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::pin!(work);
    let mut dropped_audio = 0u64;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return None,
            out = &mut work => {
                if dropped_audio > 0 {
                    debug!(chunks = dropped_audio, "microphone audio dropped while reconnecting");
                }
                return Some(out);
            }
            msg = in_rx.recv() => match msg {
                None => return None,
                Some(ClientMessage::Control(ClientControl::Ping { timestamp_ms })) => {
                    crate::transport::serve::gateway::shared_handlers::handle_ping(out_tx, timestamp_ms)
                        .await;
                }
                Some(ClientMessage::Audio(_)) => dropped_audio += 1,
                Some(_) => debug!("client message ignored while the provider reconnects"),
            },
        }
    }
}

impl RealtimeSessionContext<'_> {
    /// Run an outbound audio chunk through the shared post-audio chain, then
    /// forward it to the transport. Both the provider audio-delta path and
    /// the end-of-utterance sentinel go through here so `audio_meta.extras`
    /// is populated identically to the cascade engine
    /// (see [`crate::pipeline::post_audio::run_chain`]).
    async fn emit_audio_chunk(&mut self, mut chunk: AudioChunk) {
        if !self.post_processors.is_empty() {
            let processors = self.post_processors;
            let out_tx = self.out_tx;
            // Per-turn chain stats live on the turn; the EOU sentinel after
            // `finish()` has no turn and is not worth accounting for.
            let mut scratch = crate::pipeline::ChainStats::default();
            let stats = match self.current_turn.as_mut() {
                Some(turn) => &mut turn.chain,
                None => &mut scratch,
            };
            crate::pipeline::post_audio::run_chain(processors, &mut chunk, out_tx, stats).await;
        }
        // Tee for off-path annotators (after the inline chain). Never blocks.
        self.annotators.offer(&chunk);
        if let Some(recorder) = self.session_recorder {
            recorder.record(
                crate::audit::SessionAudioSide::Agent,
                &chunk.samples,
                chunk.sample_rate,
                chunk.channels,
            );
        }
        let has_audio = !chunk.samples.is_empty();
        let _ = self.out_tx.send(ServerMessage::Audio(chunk)).await;
        if has_audio && let Some(turn) = self.current_turn.as_mut() {
            turn.output.on_emit();
        }
    }

    pub async fn handle_provider_audio_delta(
        &mut self,
        samples: Vec<i16>,
        sample_rate: u32,
        alignment: Option<Vec<crate::pipeline::WordTiming>>,
    ) -> Result<(), anyhow::Error> {
        if *self.ignoring_stray_deltas {
            return Ok(());
        }
        *self.audio_chunks_downlinked += 1;
        if self.current_turn.is_none() {
            self.flush_pending_user_transcript().await?;
            // First audio of a new response: whatever flush the gate was
            // armed for is over, and this audio must reach the client.
            self.suppress_audio.release();
        }
        self.ensure_turn(true);
        let turn = self
            .current_turn
            .as_mut()
            .expect("turn created above in this handler");

        // `sample_rate` is provider-supplied; a zero would panic
        // this task (integer division) and end the session.
        let duration_ms = if sample_rate == 0 {
            warn!(
                session_id = %self.session_id,
                "provider audio delta with sample_rate 0; counting no duration"
            );
            0
        } else {
            (samples.len() as u64 * 1000) / u64::from(sample_rate)
        };
        // Pre-chain arrival stamp for `source_rtf` (the provider's own pace).
        turn.output.on_source(duration_ms);
        *self.session_audio_emitted_ms = self.session_audio_emitted_ms.saturating_add(duration_ms);
        // Word-exact alignment: extend
        // the turn's aligned-word ledger and build this chunk's caption
        // BEFORE the samples are counted, so offsets anchor at the
        // chunk's audio start. The ledger drives word-exact barge-in
        // truncation; the caption gives clients real word timings via
        // the same contract the cascade TTS path uses.
        let caption = alignment.map(|words| build_realtime_audio_caption(turn, words, duration_ms));
        // The ONLY place `ttfb_ms` is stamped: the first audio delta with
        // samples. Gemini's opening output transcription arrives 0.3–1.2 s
        // before this and must not stamp it.
        if turn.ttfb_ms.is_none() && !samples.is_empty() {
            let elapsed = turn.ttfb_base().elapsed().as_millis() as u64;
            turn.ttfb_ms = Some(elapsed);
            let _ = self.pe_tx.send(PipelineEvent::TtsFirstAudio {
                utterance_id: turn.utterance_id,
                elapsed_ms: elapsed,
            });
        }
        if !turn.speaking_announced {
            turn.speaking_announced = true;
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Speaking,
                }))
                .await;
        }
        turn.audio_samples = turn.audio_samples.saturating_add(samples.len() as u64);
        turn.output_sample_rate = Some(sample_rate);
        // The model is audibly speaking again: transcripts are real spoken
        // content from here on — release anything withheld and disarm the
        // post-function-call phantom guard.
        *self.phantom_transcript_guard = false;
        let withheld = std::mem::take(&mut turn.withheld_transcript);
        let utterance_id = turn.utterance_id;
        // Always advance the fragment counter (captions carry it even
        // when no audit sink is attached).
        let fragment_idx = turn.output_fragment_idx;
        turn.output_fragment_idx = turn.output_fragment_idx.saturating_add(1);
        if let Some(audit) = self.streaming_audit {
            audit.record(AuditEvent::TtsFragment {
                utterance_id,
                fragment_idx,
                text: String::new(),
                spoken_text: None,
                audio: crate::audio::AudioFrame {
                    samples: samples.clone(),
                    sample_rate,
                    channels: 1,
                },
            });
        }
        for msg in withheld {
            let _ = self.out_tx.send(ServerMessage::Message(msg)).await;
        }
        self.emit_audio_chunk(AudioChunk {
            samples,
            sample_rate,
            channels: 1,
            is_end_of_utterance: false,
            caption,
            utterance_id,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await;
        Ok(())
    }

    pub async fn handle_provider_text_delta(&mut self, text: String) -> Result<(), anyhow::Error> {
        if *self.ignoring_stray_deltas {
            return Ok(());
        }
        if self.current_turn.is_none() {
            self.flush_pending_user_transcript().await?;
        }
        // In text mode (e.g. Gemini without ResponseStarted), we must adopt
        // the pending transcript so we don't hold onto it indefinitely and
        // stay in Processing state.
        self.ensure_turn(true);
        let turn = self
            .current_turn
            .as_mut()
            .expect("turn created above in this handler");

        // First transcript delta: a diagnostic "the model started" instant,
        // NOT the TTFB — that waits for audio. The state transition stays
        // here so clients keep seeing `speaking` at the first response
        // content, exactly as before.
        if turn.first_text_ms.is_none() {
            turn.first_text_ms = Some(turn.ttfb_base().elapsed().as_millis() as u64);
        }
        if !turn.speaking_announced {
            turn.speaking_announced = true;
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Speaking,
                }))
                .await;
        }

        let audio_ms_now = turn.estimated_text_ms;
        let turn_audio_ms = audio_ms_now.saturating_sub(turn.start_audio_ms);

        // Advance virtual offset based on character length at a natural speaking rate (15 chars/second)
        let char_count = text.chars().count() as u64;
        let delta_duration_ms = (char_count * 1000) / 15;
        turn.estimated_text_ms = turn.estimated_text_ms.saturating_add(delta_duration_ms);

        turn.assistant_text.push_str(&text);
        turn.text_timeline
            .push((audio_ms_now, turn.assistant_text.len()));
        let utterance_id = turn.utterance_id;
        let delta_idx = turn.text_delta_idx;
        turn.text_delta_idx = turn.text_delta_idx.saturating_add(1);
        if let Some(audit) = self.streaming_audit {
            audit.record(AuditEvent::AssistantTranscriptDelta {
                utterance_id,
                delta_idx,
                text: text.clone(),
                audio_ms: turn_audio_ms,
            });
        }
        let payload = Some(serde_json::json!({
            "audio_ms": turn_audio_ms,
        }));
        let msg = PipelineMessage {
            kind: "text".into(),
            text: Some(text),
            payload,
            binary: None,
            role: MessageRole::Assistant,
            turn_id: Some(format!("rt-{utterance_id}")),
            message_id: format!("rt-asst-{utterance_id}-{delta_idx}"),
        };
        // Transcript reaches the client only once the turn is audibly
        // speaking. Until then it is withheld: flushed on the first audio
        // delta, at turn end for a genuine text-only turn, or dropped for
        // a zero-audio tool-call turn (Gemini transcribes the model's
        // unspoken "planned speech" around function calls — text the user
        // never heard must not appear in the chat).
        let turn = self
            .current_turn
            .as_mut()
            .expect("turn created above in this handler");
        if turn.audio_samples == 0 {
            turn.withheld_transcript.push(msg);
        } else {
            let _ = self.out_tx.send(ServerMessage::Message(msg)).await;
        }
        Ok(())
    }

    pub async fn handle_provider_input_transcript(
        &mut self,
        text: String,
        is_final: bool,
    ) -> Result<(), anyhow::Error> {
        // Late duplicate finals: OpenAI's transcription.completed often
        // arrives AFTER a fast (tool-call) turn already consumed and
        // flushed the delta-accumulated transcript. By then the buffer
        // is empty and the per-turn flushed flag has been reset, so
        // without this memory the re-delivery would surface the same
        // user bubble twice under a fresh utterance id. A genuinely
        // repeated sentence is safe: `InputSpeechStarted` clears the
        // memory when the user speaks again.
        if is_final {
            let buffered_empty = match &*self.current_turn {
                Some(turn) => turn
                    .user_transcript
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty(),
                None => self
                    .pending_user_transcript
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty(),
            };
            if buffered_empty
                && self
                    .last_flushed_user_transcript
                    .as_deref()
                    .is_some_and(|last| last == text)
            {
                debug!(
                    session_id = %self.session_id,
                    "dropping provider re-delivery of an already-flushed user transcript"
                );
                return Ok(());
            }
        }
        // New speech ends a silent tool-call leg the provider never closed
        // (a bare toolCall without turnComplete): its words are the user's
        // next utterance and must not merge into the leg's transcript.
        if self
            .current_turn
            .as_ref()
            .is_some_and(|t| t.opened_by_tool_call && t.audio_samples == 0)
        {
            self.handle_provider_response_completed().await?;
        }
        // Anchor the TTFB clock at end-of-user-speech. Providers that emit
        // no explicit `InputSpeechStopped` (Gemini Live) stream input
        // transcription deltas right before the model's audio output; the
        // last delta seen while no assistant turn is active is the closest
        // proxy we have for "user stopped talking". The anchor is drained
        // into the next turn's `ttfb_anchor` when that turn is created.
        // A proxy only ever replaces a weaker anchor (none, the client's
        // own speech-end, or an earlier proxy): a provider speech-stopped
        // event or a text submission is the better instant and stays.
        if self.current_turn.is_none()
            && matches!(
                self.request_anchor.map(|a| a.source),
                None | Some(AnchorSource::ClientSpeechEnd | AnchorSource::InputTranscriptProxy)
            )
        {
            *self.request_anchor = Some(RequestAnchor::now(AnchorSource::InputTranscriptProxy));
        }
        let transcript_ref = if let Some(turn) = &mut *self.current_turn {
            &mut turn.user_transcript
        } else {
            &mut *self.pending_user_transcript
        };

        if is_final {
            let current = transcript_ref.get_or_insert_with(String::new);
            if current.is_empty() {
                current.push_str(&text);
                *self.pending_user_transcript_flushed = false;
            } else if !text.is_empty() && !current.ends_with(&text) {
                *current = text;
                *self.pending_user_transcript_flushed = false;
            }
            self.flush_pending_user_transcript().await?;
        } else {
            let current = transcript_ref.get_or_insert_with(String::new);
            current.push_str(&text);
            *self.pending_user_transcript_flushed = false;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "linear turn-finalisation sequence (withheld flush, audit, EOU sentinel, persistence, state)"
    )]
    pub async fn handle_provider_response_completed(&mut self) -> Result<(), anyhow::Error> {
        if *self.ignoring_stray_deltas {
            *self.ignoring_stray_deltas = false;
        }
        let Some(mut turn) = self.current_turn.take() else {
            // A tool-call leg that produced no audio never opened a turn:
            // Gemini 3.x closes the call's own turn right after `toolCall`,
            // and a silent client tool gets no follow-up at all. The client
            // was put in Processing when the user finished, so hand it back
            // Listening here or it waits for a turn that never comes.
            if *self.phantom_transcript_guard {
                debug!(
                    session_id = %self.session_id,
                    "realtime: tool-call leg completed without a turn; back to listening"
                );
                let state = if self.pending_user_transcript.is_some() {
                    PipelineState::Processing
                } else {
                    PipelineState::Listening
                };
                let _ = self
                    .out_tx
                    .send(ServerMessage::Control(ServerControl::StateChanged {
                        state,
                    }))
                    .await;
            } else {
                debug!(
                    session_id = %self.session_id,
                    "realtime: ResponseCompleted arrived but current_turn is already None (likely completed or interrupted)"
                );
            }
            return Ok(());
        };

        // Withheld transcript resolution (see handle_provider_text_delta):
        // a genuine text-only turn flushes it now; a zero-audio turn in the
        // shadow of a function call drops it — the model never spoke those
        // words, so they must not appear in the client chat or the EOU
        // full_text. The session guard covers calls that arrived before the
        // turn existed and Gemini's duplicate follow-up responses (which
        // carry no function call of their own).
        let withheld = std::mem::take(&mut turn.withheld_transcript);
        let phantom_transcript =
            (turn.saw_function_call || *self.phantom_transcript_guard) && turn.audio_samples == 0;
        if phantom_transcript {
            if !turn.assistant_text.is_empty() {
                debug!(
                    session_id = %self.session_id,
                    utterance_id = turn.utterance_id,
                    chars = turn.assistant_text.chars().count(),
                    "dropping unspoken transcript of a zero-audio tool-call turn"
                );
            }
        } else {
            for msg in withheld {
                let _ = self.out_tx.send(ServerMessage::Message(msg)).await;
            }
        }
        // A silent tool-call leg whose result will be spoken hands its
        // end-of-speech anchor to that spoken follow-up, the reply the user
        // is actually waiting for. Its TTFB then spans the tool round-trip,
        // which the report re-anchors at the tool result.
        if turn.saw_function_call
            && turn.audio_samples == 0
            && self.request_anchor.is_none()
            && self.client_calls.any_spoken_pending()
        {
            *self.request_anchor = turn.ttfb_anchor;
        }

        {
            let user_transcript = turn.user_transcript.clone();
            // Capture what a late barge-in would need BEFORE `finish`
            // consumes the turn. Zero-audio turns (tool-call legs,
            // text-only responses) have nothing playing — no tail.
            let tail_fields = (turn.audio_emitted_ms() > 0).then(|| {
                (
                    turn.utterance_id,
                    turn.assistant_text.clone(),
                    turn.text_timeline.clone(),
                    turn.audio_emitted_ms(),
                    turn.item_id.clone(),
                    turn.start_audio_ms,
                    turn.aligned_words.clone(),
                )
            });
            let mut snapshot = turn.finish(false, *self.input_audio_ms_acc, self.provider_label);
            if let Some(audit) = self.streaming_audit {
                if !snapshot
                    .llm_response_text
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
                {
                    audit.record(AuditEvent::LlmOutput {
                        utterance_id: snapshot.utterance_id,
                        text: snapshot.llm_response_text.clone().unwrap_or_default(),
                        spoken_text: None,
                        tts_directives: Vec::new(),
                        stripped_directives: Vec::new(),
                        latency: std::time::Duration::from_millis(snapshot.total_ms),
                        is_partial: false,
                    });
                }
                audit.record(AuditEvent::UtteranceDone {
                    utterance_id: snapshot.utterance_id,
                    interrupted: false,
                    terminated: false,
                    stt_provider: self.provider_label.to_string(),
                    tts_provider: self.provider_label.to_string(),
                });
            }
            // End-of-utterance sentinel: no audio, but it still runs through
            // the post-audio chain so a stateful processor gets its
            // `end_of_utterance` reset signal -- exactly as the cascade EOU
            // chunk does.
            self.emit_audio_chunk(AudioChunk {
                samples: Vec::new(),
                sample_rate: snapshot
                    .components
                    .first()
                    .and_then(|c| {
                        c.extras
                            .get("sample_rate")
                            .and_then(serde_json::Value::as_u64)
                    })
                    .unwrap_or(self.output_sample_rate as u64) as u32,
                channels: 1,
                is_end_of_utterance: true,
                caption: None,
                utterance_id: snapshot.utterance_id,
                // A phantom transcript must not reach the client via the
                // EOU sentinel either (the SDK renders full_text as the
                // authoritative agent bubble). It stays on the snapshot
                // for persistence — the session-details view still shows
                // what the model generated, marked by audio_duration 0.
                full_text: if phantom_transcript {
                    None
                } else {
                    snapshot.llm_response_text.clone()
                },
                extras: None,
                last_speech_at: None,
                synth_duration: None,
            })
            .await;
            let _ = self.pe_tx.send(PipelineEvent::TtsCompleted {
                utterance_id: snapshot.utterance_id,
            });
            let _ = self.pe_tx.send(PipelineEvent::TurnCompleted {
                utterance_id: snapshot.utterance_id,
                total_ms: snapshot.total_ms,
            });
            // Off-path annotator metrics ride the same snapshot.
            snapshot
                .components
                .extend(self.annotators.turn_components(snapshot.utterance_id));
            // Client-facing `turn_metrics` control frame — cascade sessions
            // send this from `serve::turn::emit_turn_complete`; realtime
            // clients (SDK, benchmark collector) rely on the same frame to
            // demarcate the turn and read server-side latency.
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::TurnMetrics {
                    snapshot: snapshot.clone(),
                }))
                .await;
            let _ = self.pe_tx.send(PipelineEvent::TurnMetricsReady {
                utterance_id: snapshot.utterance_id,
                metrics: snapshot.clone(),
            });

            if let Some(audit) = self.streaming_audit
                && !self.pending_input_audio.is_empty()
            {
                let samples = std::mem::take(self.pending_input_audio);
                audit.record(AuditEvent::MicRaw {
                    utterance_id: snapshot.utterance_id,
                    audio: crate::audio::AudioFrame {
                        samples,
                        sample_rate: *self.pending_input_rate,
                        channels: 1,
                    },
                });
            } else {
                self.pending_input_audio.clear();
            }
            // The provider is done, but the client keeps playing for the
            // audio's full duration — arm the playback tail so a
            // `speech_started` in that window is honoured as a barge-in
            // instead of silently dropped (`current_turn` is `None` from
            // here on).
            *self.playback_tail = tail_fields.map(
                |(
                    utterance_id,
                    full_text,
                    text_timeline,
                    audio_emitted_ms,
                    item_id,
                    start_audio_ms,
                    aligned_words,
                )| {
                    PlaybackTail::new(PendingInterruption {
                        utterance_id,
                        full_text,
                        text_timeline,
                        audio_emitted_ms,
                        item_id,
                        start_audio_ms,
                        aligned_words,
                        snapshot: snapshot.clone(),
                        user_transcript: user_transcript.clone(),
                    })
                },
            );
            (self.persist_turn)(snapshot, user_transcript);
            *self.pending_user_transcript_flushed = false;
            *self.input_audio_ms_acc = 0;
            if self.pending_user_transcript.is_some() {
                let _ = self
                    .out_tx
                    .send(ServerMessage::Control(ServerControl::StateChanged {
                        state: PipelineState::Processing,
                    }))
                    .await;
            } else {
                let _ = self
                    .out_tx
                    .send(ServerMessage::Control(ServerControl::StateChanged {
                        state: PipelineState::Listening,
                    }))
                    .await;
            }
        }
        Ok(())
    }

    /// The provider reports a cancelled response. When a turn is still open
    /// it is flushed like a barge-in (`ClearAudio`, interrupted record) and
    /// the audio gate armed for it. When no turn is open — the client
    /// already cancelled and flushed, and this is the provider's late
    /// acknowledgement — nothing is sent and the gate is left alone: arming
    /// it here, after the client's `PlaybackComplete` had already released
    /// it, left every later response muted.
    pub async fn handle_provider_response_cancelled(&mut self) -> Result<(), anyhow::Error> {
        if *self.ignoring_stray_deltas {
            *self.ignoring_stray_deltas = false;
        }
        if let Some(f) = flush_turn_as_interrupted(
            self.current_turn,
            self.input_audio_ms_acc,
            self.provider_label,
            InterruptionTrigger::ServerVad,
            "provider_cancel",
            self.out_tx,
            self.pe_tx,
            self.streaming_audit.as_ref(),
            self.annotators,
        )
        .await
        {
            self.suppress_audio.arm(f.snapshot.utterance_id);
            if let Some(audit) = self.streaming_audit {
                let samples = std::mem::take(self.pending_input_audio);
                audit.record(AuditEvent::MicRaw {
                    utterance_id: f.snapshot.utterance_id,
                    audio: crate::audio::AudioFrame {
                        samples,
                        sample_rate: *self.pending_input_rate,
                        channels: 1,
                    },
                });
            } else {
                self.pending_input_audio.clear();
            }
            (self.persist_turn)(f.snapshot, f.user_transcript);
            *self.pending_interruption = Some(f.pending);
        }
        if self.pending_user_transcript.is_some() {
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Processing,
                }))
                .await;
        } else {
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Listening,
                }))
                .await;
        }
        Ok(())
    }

    /// Completes immediately; it keeps the async fallible signature every
    /// provider handler shares, so the runner's dispatch stays uniform.
    pub fn handle_provider_response_started(
        &mut self,
    ) -> impl Future<Output = Result<(), anyhow::Error>> {
        *self.ignoring_stray_deltas = false;
        // A new response supersedes any flush still awaiting the client's
        // acknowledgement; its audio must not be dropped.
        self.suppress_audio.release();
        self.flush_open_turn(false);
        self.ensure_turn(true);
        std::future::ready(Ok(()))
    }

    /// Create the assistant turn if none is open, draining the pending
    /// request anchor into it (see [`Self::attach_anchor`]). Providers
    /// without `ResponseStarted` (Gemini Live) reach this from the first
    /// delta; `adopt_transcript` moves the buffered user transcript onto
    /// the turn.
    pub(super) fn ensure_turn(&mut self, adopt_transcript: bool) {
        if self.current_turn.is_some() {
            return;
        }
        let id = *self.next_utterance_id;
        *self.next_utterance_id = self.next_utterance_id.saturating_add(1);
        let mut turn = RealtimeTurn::new(id, *self.session_audio_emitted_ms);
        if adopt_transcript {
            turn.user_transcript = self.pending_user_transcript.take();
        }
        self.attach_anchor(&mut turn);
        *self.current_turn = Some(turn);
    }

    /// Drain the request anchor and vp's observer stamp into a new turn.
    /// Provider anchors (and text submits) win; the observer stamp upgrades
    /// a client-only anchor, replaces a transcript proxy that arrived with
    /// the reply, or stands in when there is none
    /// (`ttfb_anchor_source = "vp_vad"`), and rides along on every spoken
    /// turn so `finish()` can report `provider_endpointing_ms`.
    fn attach_anchor(&mut self, turn: &mut RealtimeTurn) {
        let mut anchor = self.request_anchor.take();
        let now = Instant::now();
        let fresh_stamp = (*self.vp_speech_end).filter(|s| {
            now.saturating_duration_since(s.instant) <= super::super::turn::VP_SPEECH_END_MAX_AGE
        });
        let is_text = anchor.is_some_and(|a| a.source == AnchorSource::TextSubmit);
        // A transcript proxy that landed right as the model started
        // answering says nothing about when the user stopped: Gemini 3.x
        // sends the whole input transcript in the same burst as the first
        // reply, so the "endpointing" it implies is the entire wait and TTFB
        // comes out as ~0. vp's own stamp is the end of speech then.
        let proxy_is_reply_time = anchor.is_some_and(|a| a.is_reply_time_proxy(now));
        if let Some(stamp) = fresh_stamp
            && !is_text
        {
            if !anchor.is_some_and(|a| a.source.is_provider()) || proxy_is_reply_time {
                anchor = Some(RequestAnchor::at(stamp, AnchorSource::VpVad));
            }
            turn.vp_speech_end = Some(stamp);
            *self.vp_speech_end = None;
        }
        turn.ttfb_anchor = anchor;
    }

    /// vp's uplink observer saw the user's speech end. Belongs to the open
    /// turn when it precedes that turn's own provider anchor (the provider
    /// was faster than our hangover); otherwise it waits for the next turn.
    pub(super) fn note_vp_speech_end(&mut self, mut stamp: super::super::turn::SpeechEndStamp) {
        if let Some(turn) = self.current_turn.as_mut()
            && turn.vp_speech_end.is_none()
            && turn
                .ttfb_anchor
                .is_some_and(|a| a.source.is_provider() && stamp.instant <= a.instant)
        {
            turn.vp_speech_end = Some(stamp);
            return;
        }
        // A pause longer than the observer's hangover splits one utterance
        // into two segments: the newer stamp replaces the parked one, but
        // its input-audio stats pool both.
        if let Some(parked) = *self.vp_speech_end {
            stamp.absorb_earlier(&parked);
        }
        *self.vp_speech_end = Some(stamp);
    }

    /// Finish and persist a turn that is still open, marking it
    /// interrupted. No-op when no turn is in flight.
    ///
    /// Called with `terminated = false` when a new `ResponseStarted`
    /// supersedes an unfinished turn, and with `terminated = true` from
    /// the runner's shutdown path so a turn in flight at teardown is
    /// not lost. The latter matters most for providers whose completion
    /// boundary is *inferred* from the next turn's events (ElevenLabs):
    /// their final turn — and with it a short single-exchange session —
    /// would otherwise never reach the session store.
    pub fn flush_open_turn(&mut self, terminated: bool) {
        let Some(prev) = self.current_turn.take() else {
            return;
        };
        let prev_user_transcript = prev.user_transcript.clone();
        let snapshot = prev.finish(true, *self.input_audio_ms_acc, self.provider_label);
        let _ = self.pe_tx.send(PipelineEvent::TurnMetricsReady {
            utterance_id: snapshot.utterance_id,
            metrics: snapshot.clone(),
        });
        if let Some(audit) = self.streaming_audit {
            let samples = std::mem::take(self.pending_input_audio);
            audit.record(AuditEvent::MicRaw {
                utterance_id: snapshot.utterance_id,
                audio: crate::audio::AudioFrame {
                    samples,
                    sample_rate: *self.pending_input_rate,
                    channels: 1,
                },
            });
            audit.record(AuditEvent::UtteranceDone {
                utterance_id: snapshot.utterance_id,
                interrupted: true,
                terminated,
                stt_provider: self.provider_label.to_string(),
                tts_provider: self.provider_label.to_string(),
            });
        } else {
            self.pending_input_audio.clear();
        }
        (self.persist_turn)(snapshot, prev_user_transcript);
        *self.pending_user_transcript_flushed = false;
        *self.input_audio_ms_acc = 0;
    }

    /// Record that the provider reported end-of-user-speech
    /// (`InputSpeechStopped`). Captures the TTFB anchor so the next
    /// assistant turn measures time-to-first-byte from this instant.
    pub fn note_user_speech_end(&mut self) {
        if self.current_turn.is_none() {
            *self.request_anchor = Some(RequestAnchor::now(AnchorSource::ProviderSpeechStopped));
        }
    }

    pub async fn handle_provider_input_speech_started(&mut self) -> Result<(), anyhow::Error> {
        // New user speech: an identical final transcript from here on is
        // a genuine repeat, not a provider re-delivery.
        *self.last_flushed_user_transcript = None;
        if self.current_turn.is_some() {
            if let Some(f) = flush_turn_as_interrupted(
                self.current_turn,
                self.input_audio_ms_acc,
                self.provider_label,
                InterruptionTrigger::ServerVad,
                "server_vad",
                self.out_tx,
                self.pe_tx,
                self.streaming_audit.as_ref(),
                self.annotators,
            )
            .await
            {
                self.suppress_audio.arm(f.snapshot.utterance_id);
                if let Some(audit) = self.streaming_audit {
                    let samples = std::mem::take(self.pending_input_audio);
                    audit.record(AuditEvent::MicRaw {
                        utterance_id: f.snapshot.utterance_id,
                        audio: crate::audio::AudioFrame {
                            samples,
                            sample_rate: *self.pending_input_rate,
                            channels: 1,
                        },
                    });
                } else {
                    self.pending_input_audio.clear();
                }
                (self.persist_turn)(f.snapshot, f.user_transcript);
                *self.pending_interruption = Some(f.pending);
                *self.ignoring_stray_deltas = true;
            }
        } else if let Some(tail) = self.playback_tail.take() {
            // The provider already completed the response, but the client
            // is still playing its audio (S2S generation runs far ahead of
            // realtime). Treat speech over that tail as a barge-in: stop
            // client playback and hand the tail to the existing
            // `PendingInterruption` → `PlaybackReport` flow, which
            // truncates the transcript, re-persists the turn as
            // interrupted, and prunes the provider-side history.
            if tail.is_live() {
                let utterance_id = tail.utterance_id();
                debug!(
                    session_id = %self.session_id,
                    utterance_id,
                    "realtime: speech_started during post-completion playback tail — barge-in"
                );
                self.suppress_audio.arm(utterance_id);
                self.annotators.cancel(utterance_id);
                let _ = self
                    .out_tx
                    .send(ServerMessage::Control(ServerControl::ClearAudio {
                        utterance_id,
                        reason: "server_vad".to_string(),
                    }))
                    .await;
                let interrupted_event = PipelineEvent::Interrupted {
                    utterance_id,
                    trigger: InterruptionTrigger::ServerVad,
                    played_ms: None,
                };
                // Recorded once, by the session's event recorder
                // (`session_events`).
                let _ = self.pe_tx.send(interrupted_event);
                *self.pending_interruption = Some(tail.pending);
            }
        }
        let _ = self
            .out_tx
            .send(ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Processing,
            }))
            .await;
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "all provider handlers share the fallible signature so the runner dispatch stays uniform"
    )]
    pub fn handle_provider_item_added(&mut self, item_id: String) -> Result<(), anyhow::Error> {
        if *self.ignoring_stray_deltas {
            return Ok(());
        }
        self.ensure_turn(false);
        let turn = self
            .current_turn
            .as_mut()
            .expect("turn created above in this handler");
        if turn.item_id.is_none() {
            turn.item_id = Some(item_id);
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::unnecessary_wraps,
        clippy::needless_pass_by_value,
        reason = "mirrors RealtimeEvent::ResponseUsage field-for-field; all provider handlers share the fallible signature"
    )]
    pub fn handle_provider_usage(
        &mut self,
        response_id: Option<String>,
        total_tokens: u32,
        input_tokens: u32,
        output_tokens: u32,
        input_text_tokens: u32,
        input_audio_tokens: u32,
        input_cached_tokens: u32,
        output_text_tokens: u32,
        output_audio_tokens: u32,
    ) -> Result<(), anyhow::Error> {
        let utterance_id = self
            .current_turn
            .as_ref()
            .map_or(self.next_utterance_id.saturating_sub(1), |t| t.utterance_id);
        let elapsed_ms = self
            .current_turn
            .as_ref()
            .map_or(0.0, |t| t.started_at.elapsed().as_secs_f64() * 1000.0);
        let payload = serde_json::json!({
            "response_id": response_id,
            "provider": self.provider_label,
            "model": self.provider_model,
            "total_tokens": total_tokens,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "input_token_details": {
                "text_tokens": input_text_tokens,
                "audio_tokens": input_audio_tokens,
                "cached_tokens": input_cached_tokens,
            },
            "output_token_details": {
                "text_tokens": output_text_tokens,
                "audio_tokens": output_audio_tokens,
            },
        });
        if let Some(audit) = self.streaming_audit.as_ref() {
            audit.record(AuditEvent::RealtimeUsage {
                utterance_id,
                payload: payload.clone(),
            });
            let elapsed_ms_rounded = (elapsed_ms * 10.0).round() / 10.0;
            let tps = (elapsed_ms > 0.0 && output_tokens > 0).then(|| {
                ((f64::from(output_tokens) / (elapsed_ms / 1000.0)) * 10.0).round() / 10.0
            });
            let llm_detail_payload = serde_json::json!({
                "provider": self.provider_label,
                "model": self.provider_model,
                "stream": true,
                "elapsed_ms": elapsed_ms_rounded,
                "total_ms": elapsed_ms_rounded,
                "generate_ms": elapsed_ms_rounded,
                "tokens_per_sec": tps,
                "usage": {
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": total_tokens,
                    "prompt_tokens_details": {
                        "text_tokens": input_text_tokens,
                        "audio_tokens": input_audio_tokens,
                        "cached_tokens": input_cached_tokens,
                    },
                    "completion_tokens_details": {
                        "text_tokens": output_text_tokens,
                        "audio_tokens": output_audio_tokens,
                    },
                },
                "utterance_id": utterance_id,
            });
            audit.record(AuditEvent::Trace(crate::audit::TraceEvent {
                timestamp: std::time::SystemTime::now(),
                component: format!("llm:realtime:{}", self.provider_label),
                event_type: "llm_detail".to_string(),
                payload: llm_detail_payload,
            }));
        }
        debug!(
            session_id = %self.session_id,
            utterance_id,
            total_tokens,
            input_tokens,
            output_tokens,
            input_audio_tokens,
            output_audio_tokens,
            "realtime usage recorded"
        );
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "all provider handlers share the fallible signature so the runner dispatch stays uniform"
    )]
    pub fn handle_provider_rate_limits(
        &mut self,
        items: &[RateLimitInfo],
    ) -> Result<(), anyhow::Error> {
        for item in items {
            debug!(
                session_id = %self.session_id,
                provider = %self.provider_label,
                name = %item.name,
                limit = item.limit,
                remaining = item.remaining,
                reset_seconds = item.reset_seconds,
                "realtime rate limit"
            );
        }
        if let Some(audit) = self.streaming_audit.as_ref() {
            let serialised: Vec<serde_json::Value> = items
                .iter()
                .map(|it| {
                    serde_json::json!({
                        "name": it.name,
                        "limit": it.limit,
                        "remaining": it.remaining,
                        "reset_seconds": it.reset_seconds,
                    })
                })
                .collect();
            audit.record(AuditEvent::Trace(crate::audit::TraceEvent {
                event_type: "realtime_rate_limits".to_string(),
                timestamp: std::time::SystemTime::now(),
                component: format!("realtime:{}", self.provider_label),
                payload: serde_json::json!({
                    "provider": self.provider_label,
                    "rate_limits": serialised,
                }),
            }));
        }
        Ok(())
    }

    /// Dispatch a provider function call WITHOUT blocking the select
    /// loop: the handler runs on a spawned task and its outcome comes
    /// back as [`AgentEvent::ToolDispatchDone`] on the agent channel.
    /// Audio forwarding and barge-in keep working while the tool runs.
    ///
    /// Client-side tools are branched off
    /// FIRST, before the server `ToolRegistry` lookup: the provider's
    /// `call_id` is only in scope here (a `ToolHandler` never sees it),
    /// and the client must receive it to correlate its result. The
    /// branch runs inline — it only emits a message, registers the
    /// call, and acks — so there is no task spawn per call.
    pub async fn handle_provider_function_call(
        &mut self,
        call_id: String,
        name: String,
        arguments: String,
        tool_registry: &ToolRegistry,
        agent_tx: &mpsc::Sender<AgentEvent>,
    ) {
        // The user's words triggered this call: flush their buffered
        // transcription FIRST so the chat shows question → command in
        // speaking order. A silent client-tool turn produces no audio,
        // so without this the transcript would only flush on some later
        // turn — after the trigger_action card. Idempotent; harmless
        // when nothing is pending.
        let _ = self.flush_pending_user_transcript().await;
        // Gemini can emit a bare toolCall with no response turn at all
        // (3.x does for every call). The call is the model's answer, so it
        // opens the turn: the turn claims the parked transcript (otherwise
        // the user's NEXT utterance would append onto it, and its audit
        // record would take this one's utterance id), the call is
        // attributed to it, and its anchor is settled here, where a
        // transcript proxy that arrived with the call yields to vp's own
        // end-of-speech stamp.
        if self.current_turn.is_none() {
            self.ensure_turn(true);
            if let Some(turn) = self.current_turn.as_mut() {
                turn.opened_by_tool_call = true;
            }
        }
        // Mark the phantom-transcript window: from this call until the
        // model audibly speaks again, zero-audio transcripts are unspoken
        // planned speech and must not reach the client — see
        // handle_provider_text_delta / handle_provider_response_completed.
        // Both the turn flag and the session guard are set: the call can
        // arrive before any turn exists, and Gemini's duplicate follow-up
        // responses arrive in later turns with no call of their own.
        *self.phantom_transcript_guard = true;
        if let Some(turn) = self.current_turn.as_mut() {
            turn.saw_function_call = true;
        }
        if let Some(tool) = self.client_tools.lookup(&name) {
            self.dispatch_client_tool(call_id, &name, &arguments, tool.speak_on_result)
                .await;
            return;
        }
        if let Some(handler) = tool_registry.lookup(&name) {
            debug!(
                session_id = %self.session_id,
                call_id = %call_id,
                tool = %name,
                "dispatching realtime tool call (non-blocking)"
            );
            // Tool transparency: the pipeline sees the provider's
            // FunctionCall, so it emits the activity fact to the client.
            // Best-effort — a full client queue must not stall dispatch.
            let args_json = serde_json::from_str::<serde_json::Value>(&arguments)
                .unwrap_or_else(|_not_json| serde_json::Value::String(arguments.clone()));
            let _ = self
                .out_tx
                .try_send(ServerMessage::Message(PipelineMessage::tool_call(
                    &name, args_json,
                )));
            let tx = agent_tx.clone();
            tokio::spawn(async move {
                let outcome = handler.invoke(&arguments).await;
                let _ = tx
                    .send(AgentEvent::ToolDispatchDone {
                        call_id,
                        name,
                        outcome,
                    })
                    .await;
            });
        } else {
            warn!(
                session_id = %self.session_id,
                call_id = %call_id,
                tool = %name,
                args_bytes = arguments.len(),
                "realtime function call references unknown tool"
            );
            // Routed through the same completion path as real
            // dispatches so the error output gets the same
            // post-output response kick. try_send: the receiver is
            // this very loop, so a blocking send could deadlock on
            // a full channel.
            let err = ToolError::new(format!("tool '{name}' is not registered"));
            if agent_tx
                .try_send(AgentEvent::ToolDispatchDone {
                    call_id,
                    name,
                    outcome: Err(err),
                })
                .is_err()
            {
                warn!(
                    session_id = %self.session_id,
                    "agent channel full; dropping unknown-tool error reply"
                );
            }
        }
    }

    /// Route a client-side tool call: emit the `trigger_action` command
    /// to the client, register the call for result correlation, and ack
    /// the provider with the (only) function output for this `call_id` —
    /// silently, i.e. WITHOUT a `response.create` kick (a client
    /// command must not produce narration; speech happens only on an
    /// opt-in result).
    async fn dispatch_client_tool(
        &mut self,
        call_id: String,
        name: &str,
        arguments: &str,
        speak_on_result: bool,
    ) {
        use crate::agent::client_tools;

        let args_json = serde_json::from_str::<serde_json::Value>(arguments)
            .unwrap_or_else(|_not_json| serde_json::Value::String(arguments.to_string()));
        let ack = match self
            .client_calls
            .try_register(&call_id, name, speak_on_result)
        {
            Ok(()) => {
                debug!(
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
                            name,
                            &args_json,
                            speak_on_result,
                            self.current_turn.as_ref().map(|t| t.utterance_id),
                        ),
                    )));
                }
                // The command itself must not be dropped under backpressure
                // (unlike the best-effort `tool_call` transparency message),
                // so this is a blocking send on the session's own queue.
                let msg = client_tools::trigger_action_message(
                    &call_id,
                    name,
                    args_json,
                    speak_on_result,
                );
                let _ = self.out_tx.send(ServerMessage::Message(msg)).await;
                client_tools::dispatch_ack()
            }
            Err(e) => {
                warn!(
                    session_id = %self.session_id,
                    call_id = %call_id,
                    tool = %name,
                    error = %e,
                    "client tool call rejected"
                );
                client_tools::overloaded_ack()
            }
        };
        // One output per call_id: the ack satisfies the provider,
        // silently. On OpenAI silence = no `response.create` kick; on
        // Gemini (which auto-continues after every toolResponse) the
        // `silent` flag maps to `scheduling: SILENT`.
        if let Err(e) = self
            .client
            .send_function_output(&call_id, name, &ack, true)
            .await
        {
            warn!(
                session_id = %self.session_id,
                call_id = %call_id,
                tool = %name,
                error = %e,
                "client tool ack send_function_output failed"
            );
        }
    }

    /// Resolve a client-reported tool result against the in-flight map.
    /// Returns the agent event to feed through
    /// [`handle_agent_event`](Self::handle_agent_event) (silent context
    /// injection, or a spoken confirmation when opted in), or `None`
    /// for interim phases and unknown/expired/duplicate ids.
    pub fn resolve_client_tool_result(
        &mut self,
        call_id: &str,
        phase: crate::transport::ClientToolPhase,
        result: Option<&serde_json::Value>,
        speak_requested: bool,
    ) -> Option<AgentEvent> {
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

    /// Apply one [`AgentEvent`] on the runner task (single-writer rule:
    /// only the select loop mutates session state).
    ///
    /// `responses` tracks whether a provider response is currently
    /// streaming (and which); `kick_pending` defers a `response.create`
    /// until the response completes; `pending_agent` holds `WhenIdle`
    /// events queued behind the active response. An `Interrupt` cancels
    /// the active response and requests a replacement in one go, and tells
    /// the tracker so the cancelled response's terminal event is not
    /// mistaken for the replacement finishing.
    #[expect(
        clippy::too_many_lines,
        reason = "single exhaustive dispatcher over the AgentEvent enum; \
                  splitting per-variant helpers would scatter the \
                  scheduling policy"
    )]
    pub async fn handle_agent_event(
        &mut self,
        event: AgentEvent,
        responses: &mut ResponseTracker,
        kick_pending: &mut bool,
        pending_agent: &mut VecDeque<AgentEvent>,
    ) -> Result<(), anyhow::Error> {
        match event {
            AgentEvent::ToolDispatchDone {
                call_id,
                name,
                outcome,
            } => {
                let reply = match outcome {
                    Ok(ToolOutcome::Done(output)) => output,
                    // The ack goes back as the (only) function output;
                    // the real result arrives later as `CompleteTool`
                    // and is injected as a context message.
                    Ok(ToolOutcome::Pending { ack }) => ack,
                    Err(e) => {
                        warn!(
                            session_id = %self.session_id,
                            call_id = %call_id,
                            tool = %name,
                            error = %e,
                            "realtime tool returned error"
                        );
                        serde_json::json!({ "error": e.message }).to_string()
                    }
                };
                if let Err(e) = self
                    .client
                    .send_function_output(&call_id, &name, &reply, false)
                    .await
                {
                    warn!(
                        session_id = %self.session_id,
                        call_id = %call_id,
                        tool = %name,
                        error = %e,
                        "realtime send_function_output failed"
                    );
                    return Ok(());
                }
                // Without a kick, providers with server VAD may never
                // speak the tool result (they wait for user audio).
                self.request_kick(responses, kick_pending).await;
            }
            AgentEvent::CompleteTool {
                call_id,
                result,
                scheduling,
            } => {
                if scheduling == Scheduling::WhenIdle && responses.is_active() {
                    crate::agent::queue_pending(
                        pending_agent,
                        AgentEvent::CompleteTool {
                            call_id,
                            result,
                            scheduling,
                        },
                        false,
                        self.session_id,
                    );
                    return Ok(());
                }
                let text = format!(
                    "The background task for tool call {call_id} has completed. \
                     Result: {result}"
                );
                match scheduling {
                    Scheduling::Silent => {
                        let _ = self.client.send_context_message(&text, false).await;
                    }
                    Scheduling::Interrupt => {
                        self.interrupt_with_context(
                            responses,
                            &format!("{text} Tell the user about this now."),
                        )
                        .await;
                    }
                    Scheduling::WhenIdle => {
                        let _ = self
                            .client
                            .send_context_message(
                                &format!("{text} Tell the user about this now."),
                                true,
                            )
                            .await;
                        responses.on_created();
                    }
                }
            }
            AgentEvent::Generate {
                context,
                scheduling,
            } => {
                if scheduling == Scheduling::WhenIdle && responses.is_active() {
                    crate::agent::queue_pending(
                        pending_agent,
                        AgentEvent::Generate {
                            context,
                            scheduling,
                        },
                        false,
                        self.session_id,
                    );
                    return Ok(());
                }
                match scheduling {
                    Scheduling::Silent => {
                        let _ = self.client.send_context_message(&context, false).await;
                    }
                    Scheduling::Interrupt => {
                        self.interrupt_with_context(responses, &context).await;
                    }
                    Scheduling::WhenIdle => {
                        let _ = self.client.send_context_message(&context, true).await;
                        responses.on_created();
                    }
                }
            }
            AgentEvent::Say { text, scheduling } => {
                if scheduling == Scheduling::WhenIdle && responses.is_active() {
                    crate::agent::queue_pending(
                        pending_agent,
                        AgentEvent::Say { text, scheduling },
                        false,
                        self.session_id,
                    );
                    return Ok(());
                }
                let instruction =
                    format!("Immediately relay the following to the user, faithfully: {text}");
                match scheduling {
                    Scheduling::Silent => {
                        let _ = self.client.send_context_message(&text, false).await;
                    }
                    Scheduling::Interrupt => {
                        self.interrupt_with_context(responses, &instruction).await;
                    }
                    Scheduling::WhenIdle => {
                        let _ = self.client.send_context_message(&instruction, true).await;
                        responses.on_created();
                    }
                }
            }
            AgentEvent::Message {
                message,
                record_in_history,
            } => {
                // Out-of-turn client message: no turn, no state change,
                // no TTS (out-of-turn
                // client messages).
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
                    let text = format!("A non-spoken message was shown to the user: {repr}");
                    let _ = self.client.send_context_message(&text, false).await;
                }
            }
            AgentEvent::Cancel => {
                let _ = self.client.cancel().await;
            }
            AgentEvent::ClientToolCall { name, .. } => {
                // Cascade-only event: on S2S the provider model calls
                // client tools directly (FunctionCall branch above), so
                // nothing should ever send this here.
                warn!(
                    session_id = %self.session_id,
                    tool = %name,
                    "ClientToolCall on a realtime session -- dropped (client tools are \
                     dispatched on the provider FunctionCall path)"
                );
            }
        }
        Ok(())
    }

    /// Cancel whatever the provider is saying and immediately ask for a
    /// response to `context` instead. The cancelled response's terminal
    /// event is still on its way; the tracker is told so it does not
    /// deactivate the replacement when it arrives.
    async fn interrupt_with_context(&mut self, responses: &mut ResponseTracker, context: &str) {
        let _ = self.client.cancel().await;
        responses.note_superseded();
        let _ = self.client.send_context_message(context, true).await;
        responses.on_created();
    }

    /// Fire a previously-deferred `response.create` (the response that
    /// blocked it has completed). Called from the select loop's
    /// ResponseCompleted/Cancelled arms.
    pub async fn request_deferred_kick(&mut self, responses: &mut ResponseTracker) {
        if let Err(e) = self.client.create_response().await {
            warn!(
                session_id = %self.session_id,
                error = %e,
                "realtime deferred response kick failed"
            );
            return;
        }
        responses.on_created();
    }

    /// Kick a `response.create` now, or defer it until the active
    /// response completes (providers reject concurrent responses).
    async fn request_kick(&mut self, responses: &mut ResponseTracker, kick_pending: &mut bool) {
        if responses.is_active() {
            *kick_pending = true;
            return;
        }
        if let Err(e) = self.client.create_response().await {
            warn!(
                session_id = %self.session_id,
                error = %e,
                "realtime response kick failed"
            );
            return;
        }
        responses.on_created();
    }

    pub async fn handle_provider_error(
        &mut self,
        code: Option<String>,
        message: String,
        recoverable: bool,
    ) -> Result<bool, anyhow::Error> {
        let severity = if recoverable {
            crate::transport::ErrorSeverity::Warning
        } else {
            crate::transport::ErrorSeverity::Error
        };
        warn!(
            session_id = %self.session_id,
            provider = %self.provider_label,
            code = ?code,
            recoverable,
            error = %message,
            "realtime provider error"
        );
        let mapped_code = match code.as_deref() {
            Some("REALTIME_CONNECT_FAILED") => ErrorCode::RealtimeConnectFailed,
            Some("REALTIME_CONFIGURE_FAILED") => ErrorCode::RealtimeConfigureFailed,
            _ => ErrorCode::RealtimeProviderError,
        };
        let _ = self
            .out_tx
            .send(ServerMessage::Control(ServerControl::Error {
                code: mapped_code,
                message,
                severity,
                turn_id: None,
                component: Some(crate::transport::ErrorComponent::Transport),
            }))
            .await;
        Ok(recoverable)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the close's facts, the loop's close counters, and what a reconnect needs"
    )]
    pub async fn handle_provider_closed(
        &mut self,
        code: Option<u16>,
        reason: String,
        rapid_closes: u32,
        total_closes: u32,
        rt_session_cfg: &RealtimeSessionConfig,
        state: &TransportState,
        resumption: &mut Resumption,
        in_rx: &mut mpsc::Receiver<ClientMessage>,
    ) -> Result<bool, anyhow::Error> {
        use crate::realtime::protocol::CloseClass;
        let reading = resumption.on_close(self.client.classify_close(code, &reason));
        if reading.refused_resumption {
            info!(
                session_id = %self.session_id,
                provider = %self.provider_label,
                reason = %reason,
                "provider refused to resume the session; starting a fresh one"
            );
        }
        let class = reading.class;
        // Reconnecting reproduces a permanent condition (quota, auth,
        // entitlement) on every attempt — and each attempt may open a
        // fresh billed provider conversation. Give up instead of
        // storming; the rapid-close cap catches permanent conditions
        // the classifier's vocabulary doesn't know, and the lifetime cap
        // catches a provider that fails slowly but forever.
        let fatal_reason = if class == CloseClass::Fatal {
            Some(format!("provider closed the session permanently: {reason}"))
        } else if rapid_closes >= MAX_RAPID_PROVIDER_CLOSES {
            Some(format!(
                "provider closed {rapid_closes} times in quick succession \
                 (last: {reason}); giving up on reconnects"
            ))
        } else if total_closes > MAX_SESSION_RECONNECTS {
            Some(format!(
                "provider closed {total_closes} times during this session \
                 (last: {reason}); giving up on reconnects"
            ))
        } else {
            None
        };
        if let Some(message) = fatal_reason {
            warn!(
                session_id = %self.session_id,
                provider = %self.provider_label,
                close_code = ?code,
                reason = %reason,
                rapid_closes,
                "realtime provider close is not recoverable; ending session"
            );
            if let Some(f) = flush_turn_as_interrupted(
                self.current_turn,
                self.input_audio_ms_acc,
                self.provider_label,
                InterruptionTrigger::ServerVad,
                "provider_closed",
                self.out_tx,
                self.pe_tx,
                self.streaming_audit.as_ref(),
                self.annotators,
            )
            .await
            {
                self.suppress_audio.arm(f.snapshot.utterance_id);
                (self.persist_turn)(f.snapshot, f.user_transcript);
            }
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::Error {
                    code: ErrorCode::RealtimeConnectFailed,
                    message,
                    severity: crate::transport::ErrorSeverity::Error,
                    turn_id: None,
                    component: Some(crate::transport::ErrorComponent::Transport),
                }))
                .await;
            return Ok(false);
        }
        info!(
            session_id = %self.session_id,
            provider = %self.provider_label,
            reason = %reason,
            "realtime provider closed connection; attempting reconnect"
        );
        if let Some(f) = flush_turn_as_interrupted(
            self.current_turn,
            self.input_audio_ms_acc,
            self.provider_label,
            InterruptionTrigger::ServerVad,
            "provider_closed",
            self.out_tx,
            self.pe_tx,
            self.streaming_audit.as_ref(),
            self.annotators,
        )
        .await
        {
            self.suppress_audio.arm(f.snapshot.utterance_id);
            if let Some(audit) = self.streaming_audit {
                let samples = std::mem::take(self.pending_input_audio);
                audit.record(AuditEvent::MicRaw {
                    utterance_id: f.snapshot.utterance_id,
                    audio: crate::audio::AudioFrame {
                        samples,
                        sample_rate: *self.pending_input_rate,
                        channels: 1,
                    },
                });
            } else {
                self.pending_input_audio.clear();
            }
            (self.persist_turn)(f.snapshot, f.user_transcript);
        }
        *self.pending_interruption = None;
        *self.playback_tail = None;
        let resume_token = resumption.for_reconnect();
        let resumed = resume_token.is_some();
        // The back-off and the connect run beside the client, not instead
        // of it: together they can take half a minute.
        let session_cfg = rt_session_cfg.clone().with_resume_token(resume_token);
        let store_arc = state.param_registry.store("realtime");
        let store_ref = store_arc.as_ref().and_then(|s| {
            s.as_any()
                .downcast_ref::<crate::api::param_store::AtomicParamStore>()
        });
        let (session_id, provider_label) = (self.session_id, self.provider_label);
        // Boxed: opening a provider connection is a large future, and this
        // one is held across the whole wait.
        let attempt = Box::pin(async {
            tokio::time::sleep(reconnect_backoff(rapid_closes)).await;
            try_reconnect(
                &state.config,
                &session_cfg,
                session_id,
                provider_label,
                store_ref,
            )
            .await
        });
        let Some(attempt) =
            while_serving_client(in_rx, self.out_tx, &state.shutdown, attempt).await
        else {
            info!(
                session_id = %self.session_id,
                provider = %self.provider_label,
                "client left or worker stopping during the reconnect; ending the session"
            );
            return Ok(false);
        };
        match attempt.map(|client| *self.client = client) {
            Ok(()) => {
                self.suppress_audio.release();
                info!(
                    session_id = %self.session_id,
                    provider = %self.provider_label,
                    resumed,
                    "realtime reconnect succeeded"
                );
                // A request in flight when the provider closed was
                // dropped, and unless the session resumed, so is the
                // conversation's history. Tell the client so this
                // surfaces as a warning instead of an unexplained silence.
                let message = if resumed {
                    format!(
                        "realtime provider closed the connection ({reason}); reconnected and resumed the session — any pending request was dropped"
                    )
                } else {
                    format!(
                        "realtime provider closed the connection ({reason}); reconnected with a fresh session — conversation context was reset and any pending request was dropped"
                    )
                };
                let _ = self
                    .out_tx
                    .send(ServerMessage::Control(ServerControl::Error {
                        code: ErrorCode::RealtimeProviderError,
                        message,
                        severity: crate::transport::ErrorSeverity::Warning,
                        turn_id: None,
                        component: Some(crate::transport::ErrorComponent::Transport),
                    }))
                    .await;
                Ok(true)
            }
            Err(e) => {
                warn!(
                    session_id = %self.session_id,
                    provider = %self.provider_label,
                    error = %e,
                    "realtime reconnect failed; closing session"
                );
                let _ = self
                    .out_tx
                    .send(ServerMessage::Control(ServerControl::Error {
                        code: ErrorCode::RealtimeConnectFailed,
                        message: format!(
                            "realtime provider closed ({reason}) and reconnect failed: {e}"
                        ),
                        severity: crate::transport::ErrorSeverity::Error,
                        turn_id: None,
                        component: Some(crate::transport::ErrorComponent::Transport),
                    }))
                    .await;
                Err(e)
            }
        }
    }
}

impl RealtimeSessionContext<'_> {
    /// Open a new provider connection for this session, resuming the
    /// provider session `resume_token` came from when there is one, and
    /// put it in place of the current one.
    async fn reconnect(
        &mut self,
        rt_session_cfg: &RealtimeSessionConfig,
        state: &TransportState,
        resume_token: Option<Secret>,
    ) -> Result<(), anyhow::Error> {
        let store_arc = state.param_registry.store("realtime");
        let store_ref = store_arc.as_ref().and_then(|s| {
            s.as_any()
                .downcast_ref::<crate::api::param_store::AtomicParamStore>()
        });
        let session_cfg = rt_session_cfg.clone().with_resume_token(resume_token);
        *self.client = try_reconnect(
            &state.config,
            &session_cfg,
            self.session_id,
            self.provider_label,
            store_ref,
        )
        .await?;
        Ok(())
    }

    /// The provider is about to close the connection, and the session is
    /// quiet: move it to a new connection now, resumed, rather than lose
    /// the turn the close would cut into. Returns whether it moved; if not, the old connection carries
    /// on until the provider closes it, and the close is handled then.
    pub async fn move_to_new_connection(
        &mut self,
        rt_session_cfg: &RealtimeSessionConfig,
        state: &TransportState,
        resumption: &mut Resumption,
    ) -> bool {
        let resume_token = resumption.for_reconnect();
        match self.reconnect(rt_session_cfg, state, resume_token).await {
            Ok(()) => {
                info!(
                    session_id = %self.session_id,
                    provider = %self.provider_label,
                    "moved the session to a new provider connection before the old one closed"
                );
                true
            }
            Err(e) => {
                warn!(
                    session_id = %self.session_id,
                    provider = %self.provider_label,
                    error = %e,
                    "could not open a new provider connection; staying on the closing one"
                );
                resumption.confirmed();
                false
            }
        }
    }
}

fn build_realtime_audio_caption(
    turn: &mut super::super::turn::RealtimeTurn,
    words: Vec<crate::pipeline::WordTiming>,
    duration_ms: u64,
) -> crate::pipeline::CaptionMeta {
    let chunk_start_ms = turn.start_audio_ms + turn.audio_emitted_ms();
    for w in &words {
        turn.aligned_words.push(super::super::turn::AlignedWord {
            word: w.word.clone(),
            end_ms: chunk_start_ms + w.offset_ms as u64 + w.duration_ms as u64,
        });
    }
    crate::pipeline::CaptionMeta {
        text: words
            .iter()
            .map(|w| w.word.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        fragment_idx: turn.output_fragment_idx,
        is_final: false,
        total_fragment_duration_ms: Some(duration_ms.min(u32::MAX as u64) as u32),
        word_timings: Some(words),
        spoken_text: None,
        tts_directives: Vec::new(),
        stripped_directives: Vec::new(),
    }
}

#[cfg(test)]
#[path = "tests/provider_handlers.rs"]
mod tests;
