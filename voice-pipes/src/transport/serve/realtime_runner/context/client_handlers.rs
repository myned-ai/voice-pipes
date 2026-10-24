//! Client-message handlers for [`RealtimeSessionContext`].

#[allow(
    clippy::wildcard_imports,
    reason = "handler submodules deliberately share the context module's import surface"
)]
use super::*;

/// Cap on the session-scoped mic-capture buffer, in seconds of audio.
/// The buffer flushes at turn boundaries, but a session whose
/// turns never finish (provider never emits a turn end) must not buffer
/// mic audio without bound (~5.5 MB/minute at 48 kHz). Past the cap the
/// oldest samples are dropped — the capture widget cares about recent
/// audio.
const MAX_PENDING_INPUT_SECONDS: usize = 120;

impl RealtimeSessionContext<'_> {
    pub async fn handle_client_audio(
        &mut self,
        mut chunk: AudioChunk,
    ) -> Result<(), anyhow::Error> {
        chunk.sample_rate = self.client_sample_rate;
        *self.audio_chunks_uplinked += 1;
        if let Some(recorder) = self.session_recorder {
            recorder.record(
                crate::audit::SessionAudioSide::User,
                &chunk.samples,
                chunk.sample_rate,
                chunk.channels,
            );
        }
        self.pending_input_audio.extend_from_slice(&chunk.samples);
        let rate = if chunk.sample_rate > 0 {
            chunk.sample_rate as usize
        } else {
            16_000
        };
        let cap = rate * MAX_PENDING_INPUT_SECONDS;
        if self.pending_input_audio.len() > cap {
            // Drain down to half the cap so the shift amortises instead of
            // moving the whole buffer on every subsequent chunk.
            let drop = self.pending_input_audio.len() - cap / 2;
            self.pending_input_audio.drain(..drop);
            tracing::warn!(
                dropped_samples = drop,
                cap,
                "input-audio capture buffer exceeded cap; dropped oldest samples"
            );
        }
        if chunk.sample_rate > 0 {
            *self.pending_input_rate = chunk.sample_rate;
        }
        if chunk.sample_rate > 0 {
            let frame_ms =
                (chunk.samples.len() as u64).saturating_mul(1000) / chunk.sample_rate as u64;
            *self.input_audio_ms_acc = self.input_audio_ms_acc.saturating_add(frame_ms);
        }
        // Passive end-of-speech observer: synchronous, O(samples), never touches the audio.
        if let Some(observer) = self.endpoint_observer.as_mut()
            && let Some(stamp) = observer.observe(&chunk.samples, chunk.sample_rate)
        {
            self.note_vp_speech_end(stamp);
        }
        self.client
            .send_audio(&chunk.samples, chunk.sample_rate)
            .await
    }

    /// Client-initiated interrupt. Local bookkeeping comes first — the turn
    /// is flushed, persisted and the client told to drop its audio — and the
    /// provider cancel goes out last, so a failing provider socket cannot
    /// leave the session half-interrupted. The audio gate is armed only when
    /// a `ClearAudio` actually went out; with no turn to flush there is
    /// nothing in flight to suppress and nothing the client would ever
    /// acknowledge.
    pub async fn handle_client_cancel(&mut self) -> Result<(), anyhow::Error> {
        debug!(session_id = %self.session_id, "client cancel forwarded to realtime provider");
        // Cancel during the post-completion playback tail: no active turn
        // to flush, but the client stops playback and sends a
        // `PlaybackReport` — hand it the tail so the report truncates the
        // transcript and re-persists the turn as interrupted.
        if self.current_turn.is_none()
            && let Some(tail) = self.playback_tail.take()
            && tail.is_live()
        {
            *self.pending_interruption = Some(tail.pending);
        }
        if let Some(f) = flush_turn_as_interrupted(
            self.current_turn,
            self.input_audio_ms_acc,
            self.provider_label,
            InterruptionTrigger::ClientCancel,
            "client_cancel",
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
            // The record carries the interrupted turn's own user words; the
            // session-level pending transcript is only a fallback for a turn
            // that never adopted one.
            let user_transcript = f
                .user_transcript
                .or_else(|| self.pending_user_transcript.take());
            (self.persist_turn)(f.snapshot, user_transcript);
            *self.pending_interruption = Some(f.pending);
            *self.ignoring_stray_deltas = true;
        }
        let _ = self
            .out_tx
            .send(ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Listening,
            }))
            .await;
        self.cancel_turn().await
    }

    pub async fn handle_client_ping(&mut self, timestamp_ms: u64) -> Result<(), anyhow::Error> {
        crate::transport::serve::gateway::shared_handlers::handle_ping(self.out_tx, timestamp_ms)
            .await;
        Ok(())
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "runner dispatch hands the event over by value; the enum is trivially small"
    )]
    pub fn handle_client_timing_report(
        &mut self,
        utterance_id: u32,
        event: crate::transport::TimingEventKind,
        client_timestamp_ms: u64,
    ) {
        // The client's own VAD end-of-speech is the last-resort TTFB anchor:
        // any provider anchor that arrives afterwards replaces it, and the
        // vp observer's stamp upgrades it at turn creation.
        //
        // A later client report replaces an earlier *client* one. A detector
        // cannot tell a pause from the end of a turn, so a speaker who
        // hesitates produces one report at the hesitation and another when
        // they stop; keeping the first measured the reply from the pause. It
        // never replaces an anchor from a better source, which is why this
        // checks the source rather than merely whether one is set.
        if matches!(event, crate::transport::TimingEventKind::SpeechEnd)
            && self.current_turn.is_none()
            && client_speech_end_may_anchor(self.request_anchor.as_ref().map(|a| a.source))
        {
            *self.request_anchor = Some(RequestAnchor::now(AnchorSource::ClientSpeechEnd));
        }
        let event_name = event.as_str().to_string();
        crate::transport::serve::gateway::shared_handlers::emit_client_timing(
            self.session_id,
            self.pe_tx,
            self.streaming_audit,
            utterance_id,
            &event_name,
            client_timestamp_ms,
        );
    }

    pub async fn handle_client_playback_report(
        &mut self,
        utterance_id: u32,
        last_played_offset_ms: u32,
        last_played_text: Option<String>,
    ) -> Result<(), anyhow::Error> {
        let take = self
            .pending_interruption
            .as_ref()
            .is_some_and(|p| p.utterance_id == utterance_id);
        tracing::info!(
            session_id = %self.session_id,
            utterance_id,
            last_played_offset_ms,
            has_text = last_played_text.is_some(),
            pending = self.pending_interruption.is_some(),
            pending_utt = self.pending_interruption.as_ref().map_or(0, |p| p.utterance_id),
            consumed = take,
            "realtime: PlaybackReport arrived"
        );
        if take {
            // Resolve the played text from the (still-present) pending
            // interruption via the engine seam *before* taking ownership of
            // it below. The client's own report wins when it is non-empty.
            let final_text = match last_played_text {
                Some(t) if !t.is_empty() => t,
                _ => self
                    .resolve_played_text(utterance_id, last_played_offset_ms)
                    .unwrap_or_default(),
            };

            let mut p = self.pending_interruption.take().unwrap();
            let turn_played_ms = (last_played_offset_ms as u64).min(p.audio_emitted_ms);

            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::TextTruncated {
                    utterance_id: p.utterance_id,
                    final_text: final_text.clone(),
                }))
                .await;

            p.snapshot.llm_response_text = if final_text.is_empty() {
                None
            } else {
                Some(final_text.clone())
            };
            p.snapshot.interrupted = true;
            (self.persist_turn)(p.snapshot.clone(), p.user_transcript.clone());

            // Emit HistoryModified and BargeInResolved events so the session details page can discover and render the interruption details.
            let modification = crate::pipeline::events::HistoryModification {
                utterance_id: p.utterance_id,
                original_text: p.full_text.clone(),
                retained_text: final_text.clone(),
                method: crate::pipeline::events::TruncationMethod::PlaybackReport,
                played_ms: Some(last_played_offset_ms),
            };
            let hm_event = PipelineEvent::HistoryModified { modification };
            // Both recorded once, by the session's event recorder
            // (`session_events`).
            let _ = self.pe_tx.send(hm_event);

            let br_event = PipelineEvent::BargeInResolved {
                utterance_id: p.utterance_id,
                trigger: InterruptionTrigger::ServerVad,
                played_ms: Some(last_played_offset_ms),
                retained_text: final_text.clone(),
                barge_in_rtt_ms: None,
            };
            let _ = self.pe_tx.send(br_event);

            // Provider-side history truncation goes LAST: it can
            // fail on a stalled provider socket, and an early `?` would
            // abort the handler after `pending_interruption` was already
            // consumed — the turn snapshot would never be persisted and the
            // events above never emitted. Local state is complete
            // before the provider call; a failure only means the provider's
            // conversation history keeps the unheard tail, which the error
            // (logged by the caller) makes visible.
            if let Some(item_id) = p.item_id {
                self.client
                    .truncate_item(&item_id, turn_played_ms as u32)
                    .await?;
            }
        }
        Ok(())
    }

    /// Store client-reported playback health (underruns / inserted silence)
    /// for the turn record and write the `NNN_playback.json` capture
    /// artifact.
    pub fn handle_client_playback_quality(
        &self,
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
        // S2 for the adaptive controller (same signal as the classic engine).
        self.observations
            .observe(crate::transport::serve::turn::playback_observation(&health));
        *self.pending_playback_health.lock() = Some(health);
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "fallible signature kept for uniformity with the sibling client handlers"
    )]
    pub fn handle_client_playback_complete(
        &mut self,
        utterance_id: u32,
    ) -> Result<(), anyhow::Error> {
        if self.suppress_audio.release_for(utterance_id) {
            debug!(
                session_id = %self.session_id,
                utterance_id,
                "realtime: PlaybackComplete -> audio gate released"
            );
        }
        // Natural end of playback: the post-completion barge-in window for
        // this utterance is over. Only a matching (or older) tail is
        // dropped — a stale PlaybackComplete must not disarm a newer turn.
        if self
            .playback_tail
            .as_ref()
            .is_some_and(|t| t.utterance_id() <= utterance_id)
        {
            *self.playback_tail = None;
        }
        Ok(())
    }

    pub async fn handle_client_message(
        &mut self,
        pm: PipelineMessage,
    ) -> Result<(), anyhow::Error> {
        if let Some(text) = pm.text.as_deref()
            && !text.trim().is_empty()
        {
            *self.pending_user_transcript = Some(text.to_string());
            // The text submission is this turn's "end of user input" — anchor
            // TTFB here so it measures submit -> first response byte.
            if self.current_turn.is_none() {
                *self.request_anchor = Some(RequestAnchor::now(AnchorSource::TextSubmit));
            }
            // Set flushed to true so we don't emit a redundant `stt-` pipeline message,
            // which causes the frontend to render a duplicate user bubble.
            *self.pending_user_transcript_flushed = true;
            let utterance_id = self
                .current_turn
                .as_ref()
                .map_or(*self.next_utterance_id, |t| t.utterance_id);
            let _ = self.pe_tx.send(PipelineEvent::UtteranceReceived {
                utterance_id,
                text: text.to_string(),
                source: "Text".to_string(),
            });
            self.client.send_text(text).await?;
            let _ = self
                .out_tx
                .send(ServerMessage::Control(ServerControl::StateChanged {
                    state: PipelineState::Processing,
                }))
                .await;
        }
        Ok(())
    }

    pub async fn flush_pending_user_transcript(&mut self) -> Result<(), anyhow::Error> {
        if *self.pending_user_transcript_flushed {
            return Ok(());
        }
        let text_opt = if let Some(turn) = &mut *self.current_turn {
            turn.user_transcript.take()
        } else {
            self.pending_user_transcript.take()
        };

        if let Some(text) = text_opt {
            if text.trim().is_empty() {
                if let Some(turn) = &mut *self.current_turn {
                    turn.user_transcript = Some(text);
                } else {
                    *self.pending_user_transcript = Some(text);
                }
            } else {
                let utterance_id = self
                    .current_turn
                    .as_ref()
                    .map_or(*self.next_utterance_id, |t| t.utterance_id);
                if let Some(audit) = self.streaming_audit {
                    audit.record(AuditEvent::SttResult {
                        utterance_id,
                        text: text.clone(),
                        latency: std::time::Duration::ZERO,
                    });
                }
                let _ = self.pe_tx.send(PipelineEvent::UtteranceReceived {
                    utterance_id,
                    text: text.clone(),
                    source: "realtime".to_string(),
                });
                let _ = self
                    .out_tx
                    .send(ServerMessage::Message(PipelineMessage {
                        kind: "text".into(),
                        text: Some(text.clone()),
                        payload: None,
                        binary: None,
                        role: MessageRole::User,
                        turn_id: Some(format!("rt-{utterance_id}")),
                        message_id: format!("stt-{utterance_id}"),
                    }))
                    .await;
                // Dedupe memory: a provider may re-deliver the final
                // transcription after the turn consumed it (OpenAI's
                // transcription.completed lags fast tool-call turns);
                // handle_provider_input_transcript drops an identical
                // final while no new speech has started.
                *self.last_flushed_user_transcript = Some(text.clone());
                // Put it back so persist_turn has it
                if let Some(turn) = &mut *self.current_turn {
                    turn.user_transcript = Some(text);
                } else {
                    *self.pending_user_transcript = Some(text);
                }
                *self.pending_user_transcript_flushed = true;
            }
        }
        Ok(())
    }
}

/// Whether a client `speech_end` may become the request anchor, given the
/// source of the one already held.
///
/// Yes when there is none, and yes when the one held is itself a client
/// report -- the later report is where the speaker actually stopped. No
/// otherwise: a provider or vp-observer anchor is better evidence than any
/// client report and must survive one.
fn client_speech_end_may_anchor(held: Option<AnchorSource>) -> bool {
    matches!(held, None | Some(AnchorSource::ClientSpeechEnd))
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

    #[test]
    fn a_later_client_report_replaces_an_earlier_one_but_nothing_better() {
        // First report of the turn.
        assert!(client_speech_end_may_anchor(None));
        // The speaker paused, was reported, and carried on: the second report
        // is the real end and must win.
        assert!(client_speech_end_may_anchor(Some(
            AnchorSource::ClientSpeechEnd
        )));
        // Anything from the provider or the vp observer outranks the client.
        for better in [
            AnchorSource::ProviderSpeechStopped,
            AnchorSource::VpVad,
            AnchorSource::InputTranscriptProxy,
        ] {
            assert!(
                !client_speech_end_may_anchor(Some(better)),
                "{better:?} must not be displaced by a client report"
            );
        }
    }
}
