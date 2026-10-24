//! WebSocket client for the `AssemblyAI` V3 Streaming API.
//!
//! Manages the lifecycle of a single streaming session: connect,
//! send audio as binary frames, receive JSON messages, and handle
//! graceful shutdown and reconnection with exponential backoff.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument, warn};
use voice_pipes_net::tokio_tungstenite::tungstenite::Message;
use voice_pipes_net::tokio_tungstenite::tungstenite::client::IntoClientRequest;
use voice_pipes_net::tokio_tungstenite::tungstenite::http::HeaderValue;

use voice_pipes_sdk::audio::i16_to_le_bytes;
use voice_pipes_sdk::reconnect::{ReconnectBudget, SessionOutcomeInfo, backoff_delay, jittered};
use voice_pipes_sdk::{AudioChunk, SttError, TranscriptionResult, WordInfo};

use super::config::AssemblyAiConfig;
use super::messages::{FORCE_ENDPOINT, ServerMessage, TERMINATE, TurnMessage};

/// Target audio frame duration (in milliseconds) to send per WebSocket message.
/// Slightly above the 50 ms minimum accepted by `AssemblyAI`.
const TARGET_FRAME_DURATION_MS: u32 = 100;

/// Longest audio message `AssemblyAI` accepts: a longer one ends the
/// session with error 3007 ("Expected between 50 and 1000 ms").
const MAX_FRAME_DURATION_MS: u32 = 1000;

/// Largest message accepted from `AssemblyAI`. Its frames are small JSON
/// (a turn's transcript and words); anything near this is not a transcript.
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Timeout for the server to respond with `Termination` after we send `Terminate`.
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on writing the close frame once the termination deadline has
/// passed. A close is a courtesy at that point, not something to wait on.
const CLOSE_TIMEOUT: Duration = Duration::from_millis(250);

/// Timeout for the server to deliver `Begin` after the WebSocket connects.
/// A session that never begins must not be held open.
const BEGIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on the TCP + TLS + WebSocket handshake. A black-holed endpoint
/// must fail the attempt (and honour cancellation) instead of holding the
/// pipeline's STT worker for the OS connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// WebSocket client that bridges pipeline audio to `AssemblyAI`.
#[derive(Debug)]
pub struct WsClient {
    config: AssemblyAiConfig,
    /// Optional channel to emit trace events to the audit system.
    audit_tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
}

impl WsClient {
    /// Create a new client from the given provider configuration.
    pub fn new(config: AssemblyAiConfig) -> Self {
        Self {
            config,
            audit_tx: None,
        }
    }

    /// Attach an audit trace channel for emitting STT usage events.
    #[must_use]
    pub fn with_audit_tx(
        mut self,
        tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::TraceEvent>>,
    ) -> Self {
        self.audit_tx = tx;
        self
    }

    /// Build the full WebSocket URL with query parameters.
    pub fn build_url(&self) -> String {
        let mut params = vec![
            ("speech_model".to_string(), self.config.speech_model.clone()),
            (
                "sample_rate".to_string(),
                self.config.sample_rate.to_string(),
            ),
        ];

        if let Some(v) = self.config.min_turn_silence {
            params.push(("min_turn_silence".to_string(), v.to_string()));
        }
        if let Some(v) = self.config.max_turn_silence {
            params.push(("max_turn_silence".to_string(), v.to_string()));
        }
        if let Some(v) = self.config.end_of_turn_confidence_threshold {
            params.push((
                "end_of_turn_confidence_threshold".to_string(),
                v.to_string(),
            ));
        }
        if let Some(v) = self.config.format_turns {
            params.push(("format_turns".to_string(), v.to_string()));
        }
        if let Some(ref terms) = self.config.keyterms_prompt
            && let Ok(json) = serde_json::to_string(terms)
        {
            params.push(("keyterms_prompt".to_string(), json));
        }
        if let Some(ref v) = self.config.prompt {
            params.push(("prompt".to_string(), v.clone()));
        }
        if let Some(v) = self.config.language_detection {
            params.push(("language_detection".to_string(), v.to_string()));
        }

        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&params)
            .finish();

        format!("{}?{}", self.config.endpoint, query)
    }

    /// Run the streaming session with reconnection support.
    ///
    /// Reads audio from `audio_rx`, forwards it to `AssemblyAI` over
    /// WebSocket, and converts incoming Turn messages into
    /// [`TranscriptionResult`]s sent via `result_tx`.
    #[instrument(name = "assemblyai.session", skip_all, fields(
        provider = "assemblyai",
        speech_model = %self.config.speech_model,
    ))]
    pub async fn run(
        &self,
        mut audio_rx: mpsc::Receiver<AudioChunk>,
        result_tx: mpsc::Sender<TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        let mut budget = ReconnectBudget::new(self.config.max_reconnect_attempts);
        let mut segment_idx: u32 = 0;
        let mut input = AudioInput {
            rx: &mut audio_rx,
            pending: None,
        };

        loop {
            // Downstream gone (pipeline torn down) counts the same as an
            // explicit cancel: opening another session would leak it.
            if cancel.is_cancelled() || result_tx.is_closed() {
                break Ok(());
            }
            let url = self.build_url();

            // Filled in by `connect_and_stream`: when `Begin` arrived and
            // whether any transcript reached the pipeline. Only a session
            // that lived a while or produced something restores the
            // reconnect budget; one that was accepted and then rejected on
            // the first frame does not, so a deterministic post-connect
            // rejection surfaces as an error instead of looping forever.
            let mut session = SessionOutcomeInfo::default();
            match self
                .connect_and_stream(
                    &url,
                    &mut input,
                    &result_tx,
                    &mut segment_idx,
                    &mut session,
                    budget.attempt(),
                    cancel.clone(),
                )
                .await
            {
                Ok(SessionOutcome::Complete) => {
                    info!("AssemblyAI session completed normally");
                    break Ok(());
                }
                Ok(SessionOutcome::UtteranceEnded) => {
                    // One vendor session per utterance, one stream for the
                    // pipeline's session: the next vendor session opens
                    // when the next utterance's audio arrives, not before,
                    // so none sits open between turns.
                    let next = tokio::select! {
                        chunk = input.rx.recv() => chunk,
                        () = cancel.cancelled() => break Ok(()),
                        () = result_tx.closed() => break Ok(()),
                    };
                    match next {
                        Some(chunk) => input.pending = Some(chunk),
                        None => break Ok(()),
                    }
                }
                Err(e) if !e.is_transient() => {
                    error!(error = %e, "AssemblyAI permanent error");
                    break Err(e);
                }
                Err(e) => {
                    let attempt = match budget.on_failure(&session, std::time::Instant::now(), &e) {
                        Ok(attempt) => attempt,
                        Err(exhausted) => {
                            error!(error = %exhausted, "AssemblyAI reconnect budget exhausted");
                            break Err(exhausted);
                        }
                    };

                    let delay = jittered(self.backoff_delay(attempt));
                    warn!(
                        attempt,
                        delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                        error = %e,
                        "AssemblyAI reconnecting"
                    );
                    // A worker whose pipeline died mid-backoff must not wake
                    // up and open a session nobody consumes.
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = cancel.cancelled() => break Ok(()),
                        () = result_tx.closed() => break Ok(()),
                    }
                }
            }
        }
    }

    /// Connect to the WebSocket and run the send/receive loop for one session.
    #[instrument(
        name = "assemblyai.connect",
        skip_all,
        fields(attempt, session_id = tracing::field::Empty)
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "one session's state, threaded through the reconnect loop"
    )]
    async fn connect_and_stream(
        &self,
        url: &str,
        audio_rx: &mut AudioInput<'_>,
        result_tx: &mpsc::Sender<TranscriptionResult>,
        segment_idx: &mut u32,
        session: &mut SessionOutcomeInfo,
        attempt: u32,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<SessionOutcome, SttError> {
        tracing::Span::current().record("attempt", attempt);
        let Some(ws_stream) = self.open(url, &cancel, result_tx).await? else {
            return Ok(SessionOutcome::Complete);
        };
        let (ws_tx, mut ws_rx) = ws_stream.split();
        // Audio is sent in frames of at least the duration AssemblyAI
        // requires (>= 50 ms).
        let target_samples = frame_samples(self.config.sample_rate, TARGET_FRAME_DURATION_MS);
        let mut stream = Streaming::new(
            ws_tx,
            target_samples,
            frame_samples(self.config.sample_rate, MAX_FRAME_DURATION_MS),
            self.config.persistent_session,
        );

        let Some(session_id) =
            await_begin(&mut stream.ws_tx, &mut ws_rx, &cancel, result_tx, session).await?
        else {
            return Ok(SessionOutcome::Complete);
        };
        tracing::Span::current().record("session_id", session_id.as_str());

        loop {
            tokio::select! {
                // Cancellation and downstream teardown both request a graceful
                // terminate; the armed deadline bounds the wait for the
                // server's `Termination` so no session is held open.
                () = cancel.cancelled(), if !stream.terminating => {
                    let _ = stream.terminate().await;
                }
                () = result_tx.closed(), if !stream.terminating => {
                    let _ = stream.terminate().await;
                }
                chunk_opt = audio_rx.recv(), if !stream.terminating => {
                    // A vendor that stops reading leaves a send waiting on
                    // a full socket; cancel and teardown are polled beside
                    // it so they still end the session.
                    let sent = tokio::select! {
                        sent = stream.forward_audio(chunk_opt) => Some(sent),
                        () = cancel.cancelled() => None,
                        () = result_tx.closed() => None,
                    };
                    match sent {
                        Some(sent) => sent?,
                        None => {
                            let _ = stream.terminate().await;
                        }
                    }
                }
                () = tokio::time::sleep_until(stream.termination_deadline), if stream.terminating => {
                    warn!("Termination response timed out, closing");
                    // A server that stopped reading would hold the close
                    // frame as it held everything else.
                    let _ = tokio::time::timeout(CLOSE_TIMEOUT, stream.ws_tx.close()).await;
                    return Ok(stream.ended());
                }
                msg_opt = ws_rx.next() => {
                    if let Some(outcome) = self
                        .on_message(msg_opt, &mut stream, session, segment_idx, result_tx, &cancel)
                        .await
                    {
                        return outcome;
                    }
                }
            }
        }
    }

    /// The WebSocket handshake, with the key in its `Authorization` header.
    /// Bounded and cancel-aware: a connect that hangs (a black-holed
    /// endpoint, a proxy that never answers) fails the attempt after
    /// `CONNECT_TIMEOUT`, and a pipeline that goes away meanwhile stops
    /// waiting at once (`None`).
    async fn open(
        &self,
        url: &str,
        cancel: &tokio_util::sync::CancellationToken,
        result_tx: &mpsc::Sender<TranscriptionResult>,
    ) -> Result<Option<voice_pipes_net::WebSocket>, SttError> {
        let mut request = url
            .into_client_request()
            .map_err(|e| SttError::NetworkError(format!("invalid URL: {e}")))?;
        let mut key = HeaderValue::from_str(self.config.api_key.expose())
            .map_err(|e| SttError::AuthError(format!("invalid API key header: {e}")))?;
        key.set_sensitive(true);
        request.headers_mut().insert("Authorization", key);

        tokio::select! {
            () = cancel.cancelled() => Ok(None),
            () = result_tx.closed() => Ok(None),
            connected = voice_pipes_net::connect_websocket(
                request,
                MAX_MESSAGE_BYTES,
                CONNECT_TIMEOUT,
            ) => match connected {
                Ok((ws, _response)) => Ok(Some(ws)),
                Err(voice_pipes_net::ConnectError::Handshake(e)) => Err(classify_ws_error(&e)),
                Err(_timed_out) => Err(SttError::Timeout(CONNECT_TIMEOUT)),
            },
        }
    }

    /// One message from `AssemblyAI` while streaming. `Some` ends the
    /// session with that outcome; `None` carries on.
    async fn on_message(
        &self,
        msg_opt: Option<Result<Message, WsError>>,
        stream: &mut Streaming,
        session: &mut SessionOutcomeInfo,
        segment_idx: &mut u32,
        result_tx: &mpsc::Sender<TranscriptionResult>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Option<Result<SessionOutcome, SttError>> {
        let text = match msg_opt {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(frame))) => {
                if stream.terminating {
                    return Some(Ok(stream.ended()));
                }
                let code = frame.as_ref().map(|f| f.code);
                let reason = frame
                    .as_ref()
                    .map(|f| f.reason.to_string())
                    .unwrap_or_default();
                return Some(Err(classify_close(code, &reason)));
            }
            Some(Err(e)) => {
                // The socket broke while we were waiting for the server's
                // `Termination`: the session is over either way, and a
                // reconnect would open a session nobody asked for.
                if stream.terminating {
                    debug!(error = %e, "socket error while terminating; treating as closed");
                    return Some(Ok(stream.ended()));
                }
                return Some(Err(classify_ws_error(&e)));
            }
            None => {
                if stream.terminating {
                    return Some(Ok(stream.ended()));
                }
                return Some(Err(SttError::NetworkError(
                    "WebSocket stream ended unexpectedly".into(),
                )));
            }
            Some(Ok(_)) => return None, // ping/pong/binary
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::Turn(turn)) => {
                if !turn.transcript.is_empty() {
                    session.delivered_transcript = true;
                }
                handle_turn(&turn, segment_idx, result_tx, cancel).await;
            }
            Ok(ServerMessage::SpeechStarted(s)) => {
                debug!(
                    timestamp_ms = s.timestamp,
                    confidence = s.confidence,
                    "speech detected"
                );
                forward(
                    result_tx,
                    TranscriptionResult::new(String::new(), false)
                        .with_confidence(Some(confidence_f32(s.confidence)))
                        .with_segment_index(*segment_idx)
                        .with_start_time(Some(Duration::from_millis(s.timestamp)))
                        .with_speech_started(true),
                    cancel,
                )
                .await;
            }
            Ok(ServerMessage::Termination(t)) => {
                info!(
                    audio_duration_s = t.audio_duration_seconds,
                    session_duration_s = t.session_duration_seconds,
                    "AssemblyAI session terminated"
                );
                if let Some(tx) = &self.audit_tx {
                    let _ = tx.try_send(voice_pipes_sdk::TraceEvent::new(
                        "stt:assemblyai",
                        "stt_usage",
                        serde_json::json!({
                            "audio_duration_seconds": t.audio_duration_seconds,
                            "session_duration_seconds": t.session_duration_seconds,
                            "speech_model": &self.config.speech_model,
                        }),
                    ));
                }
                return Some(Ok(stream.ended()));
            }
            Ok(ServerMessage::Begin(_)) => warn!("unexpected duplicate Begin message"),
            Ok(ServerMessage::Error(e)) => {
                let err = classify_server_error(e.error_code, &e.error);
                warn!(
                    code = ?e.error_code,
                    error_chars = e.error.chars().count(),
                    "AssemblyAI server error"
                );
                // Don't abandon a writable socket, nor wait on one the
                // server has stopped reading.
                let _ = tokio::time::timeout(CLOSE_TIMEOUT, stream.ws_tx.close()).await;
                if stream.terminating {
                    return Some(Ok(stream.ended()));
                }
                return Some(Err(err));
            }
            Err(e) => warn!(
                error = %voice_pipes_sdk::secrets::json_error(&e),
                len = text.len(),
                "skipping unparseable message"
            ),
        }
        None
    }

    /// Exponential backoff delay for a given attempt number.
    fn backoff_delay(&self, attempt: u32) -> Duration {
        backoff_delay(
            self.config.reconnect_delay,
            self.config.reconnect_delay_max,
            attempt,
        )
    }
}

type WsError = voice_pipes_net::tokio_tungstenite::tungstenite::Error;
type WsTx = futures_util::stream::SplitSink<voice_pipes_net::WebSocket, Message>;
type WsRx = futures_util::stream::SplitStream<voice_pipes_net::WebSocket>;

/// Wait for `AssemblyAI`'s `Begin` and return the session id. Bounded and
/// cancel-aware: a session that never begins, or whose pipeline died
/// mid-connect (`None`), is not held open.
async fn await_begin(
    ws_tx: &mut WsTx,
    ws_rx: &mut WsRx,
    cancel: &tokio_util::sync::CancellationToken,
    result_tx: &mpsc::Sender<TranscriptionResult>,
    session: &mut SessionOutcomeInfo,
) -> Result<Option<String>, SttError> {
    let deadline = tokio::time::Instant::now() + BEGIN_TIMEOUT;
    loop {
        let msg_opt = tokio::select! {
            () = cancel.cancelled() => {
                let _ = ws_tx.close().await;
                return Ok(None);
            }
            () = result_tx.closed() => {
                let _ = ws_tx.close().await;
                return Ok(None);
            }
            () = tokio::time::sleep_until(deadline) => {
                let _ = ws_tx.close().await;
                return Err(SttError::Timeout(BEGIN_TIMEOUT));
            }
            msg_opt = ws_rx.next() => msg_opt,
        };
        let text = match msg_opt {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(frame))) => {
                let code = frame.as_ref().map(|f| f.code);
                let reason = frame
                    .as_ref()
                    .map(|f| f.reason.to_string())
                    .unwrap_or_default();
                return Err(classify_close(code, &reason));
            }
            Some(Err(e)) => return Err(classify_ws_error(&e)),
            None => {
                return Err(SttError::NetworkError(
                    "connection closed before Begin".into(),
                ));
            }
            Some(Ok(_)) => continue, // ping/pong/binary
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::Begin(b)) => {
                info!(
                    session_id = %b.id,
                    expires_at = b.expires_at,
                    "AssemblyAI session started"
                );
                session.established_at = Some(std::time::Instant::now());
                return Ok(Some(b.id));
            }
            Ok(ServerMessage::Error(e)) => {
                let err = classify_server_error(e.error_code, &e.error);
                warn!(
                    code = ?e.error_code,
                    error_chars = e.error.chars().count(),
                    "AssemblyAI error before Begin"
                );
                let _ = ws_tx.close().await;
                return Err(err);
            }
            Ok(other) => warn!(kind = other.kind(), "unexpected message before Begin"),
            Err(e) => warn!(
                error = %voice_pipes_sdk::secrets::json_error(&e),
                len = text.len(),
                "unparseable message before Begin"
            ),
        }
    }
}

/// The sending half of a session once it has begun: audio framed to the
/// size `AssemblyAI` requires, and whether the session is being ended.
struct Streaming {
    ws_tx: WsTx,
    send_buf: Vec<i16>,
    target_samples: usize,
    /// Longest frame `AssemblyAI` accepts, in samples.
    max_frame_samples: usize,
    /// Keep the session open across utterances (force the endpoint) rather
    /// than one session per utterance.
    persistent_session: bool,
    /// `Terminate` has been sent; the session ends on the server's
    /// `Termination`, a close, or the deadline.
    terminating: bool,
    /// Terminating because the utterance ended (default mode), not because
    /// the stream is closing: the stream goes on afterwards.
    utterance_ended: bool,
    /// An absolute deadline, set once when `terminating` flips. A relative
    /// sleep made inside `select!` restarted on every trickled message and
    /// could never fire.
    termination_deadline: tokio::time::Instant,
}

impl Streaming {
    fn new(
        ws_tx: WsTx,
        target_samples: usize,
        max_frame_samples: usize,
        persistent_session: bool,
    ) -> Self {
        Self {
            ws_tx,
            send_buf: Vec::with_capacity(target_samples),
            target_samples,
            max_frame_samples,
            persistent_session,
            terminating: false,
            utterance_ended: false,
            termination_deadline: tokio::time::Instant::now(),
        }
    }

    /// Ask the server to end the session, and bound the wait for its
    /// `Termination`. Writing `Terminate` is bounded by the same deadline:
    /// a server that has stopped reading cannot hold the session open.
    /// Called again after an interrupted send, it only flushes what that
    /// send left queued, so `Terminate` goes out once.
    async fn terminate(&mut self) -> Result<(), WsError> {
        let written = if self.terminating {
            tokio::time::timeout_at(self.termination_deadline, self.ws_tx.flush()).await
        } else {
            self.terminating = true;
            self.termination_deadline = tokio::time::Instant::now() + TERMINATION_TIMEOUT;
            tokio::time::timeout_at(
                self.termination_deadline,
                self.ws_tx.send(Message::Text(TERMINATE.into())),
            )
            .await
        };
        // A write still waiting at the deadline is abandoned; the loop's
        // deadline arm, already due, ends the session.
        written.unwrap_or(Ok(()))
    }

    /// One read from the pipeline's audio: a chunk to send, or the end of
    /// the input.
    async fn forward_audio(&mut self, chunk: Option<AudioChunk>) -> Result<(), SttError> {
        if let Some(chunk) = chunk {
            return self.send_audio(chunk).await;
        }
        self.finish_input().await;
        Ok(())
    }

    /// Buffer `chunk`, sending a frame once it is long enough or the
    /// utterance ends; at its end, force the endpoint (persistent session)
    /// or terminate the session.
    async fn send_audio(&mut self, chunk: AudioChunk) -> Result<(), SttError> {
        self.send_buf.extend_from_slice(&chunk.samples);
        if self.send_buf.len() >= self.target_samples || chunk.is_last {
            self.send_buffered()
                .await
                .map_err(|e| classify_ws_error(&e))?;
        }
        if !chunk.is_last {
            return Ok(());
        }
        if self.persistent_session {
            // Avoids per-utterance session churn.
            self.ws_tx
                .send(Message::Text(FORCE_ENDPOINT.into()))
                .await
                .map_err(|e| classify_ws_error(&e))?;
        } else {
            self.terminate().await.map_err(|e| classify_ws_error(&e))?;
            self.utterance_ended = true;
        }
        Ok(())
    }

    /// The pipeline's audio has ended: send what is buffered and terminate.
    async fn finish_input(&mut self) {
        let _ = self.send_buffered().await;
        let _ = self.terminate().await;
    }

    /// Send what is buffered, as frames no longer than the vendor accepts.
    /// The pipeline may hand over a long chunk at once (an utterance's
    /// end flush, audio queued while a session opened); one frame of it
    /// would be refused. Nothing is sent for an empty buffer.
    async fn send_buffered(&mut self) -> Result<(), WsError> {
        let samples = std::mem::take(&mut self.send_buf);
        for frame in split_frames(&samples, self.max_frame_samples) {
            self.ws_tx
                .send(Message::Binary(i16_to_le_bytes(frame).into()))
                .await?;
        }
        self.send_buf = samples;
        self.send_buf.clear();
        Ok(())
    }

    fn ended(&self) -> SessionOutcome {
        if self.utterance_ended {
            SessionOutcome::UtteranceEnded
        } else {
            SessionOutcome::Complete
        }
    }
}

/// Outcome of a single WebSocket session (no error).
#[derive(Debug)]
enum SessionOutcome {
    /// Server sent `Termination` or we closed gracefully; the stream ends.
    Complete,
    /// The utterance ended and its vendor session with it (default mode);
    /// the stream goes on to the next utterance.
    UtteranceEnded,
}

/// The pipeline's audio, with room to hand back one chunk: the first of
/// the next utterance, read while waiting to open its session.
struct AudioInput<'a> {
    rx: &'a mut mpsc::Receiver<AudioChunk>,
    pending: Option<AudioChunk>,
}

impl AudioInput<'_> {
    async fn recv(&mut self) -> Option<AudioChunk> {
        match self.pending.take() {
            Some(chunk) => Some(chunk),
            None => self.rx.recv().await,
        }
    }
}

/// Convert a `TurnMessage` into one or two `TranscriptionResult`s and
/// send them downstream.
///
/// When `utterance` is non-empty AND `end_of_turn` is false, an extra
/// "eager" result is emitted for early LLM inference.
async fn handle_turn(
    turn: &TurnMessage,
    segment_idx: &mut u32,
    result_tx: &mpsc::Sender<TranscriptionResult>,
    cancel: &tokio_util::sync::CancellationToken,
) {
    // Emit eager utterance result if boundary detected mid-turn.
    if !turn.utterance.is_empty() && !turn.end_of_turn {
        let eager = TranscriptionResult::new(turn.utterance.clone(), true)
            .with_confidence(avg_confidence(&turn.words))
            .with_language(turn.language_code.clone())
            .with_segment_index(*segment_idx)
            .with_start_time(turn.words.first().map(|w| Duration::from_millis(w.start)))
            .with_end_time(turn.words.last().map(|w| Duration::from_millis(w.end)))
            .with_words(final_words(&turn.words))
            .with_is_end_of_turn(Some(false));
        if !forward(result_tx, eager, cancel).await {
            return;
        }
    }

    let result = turn_to_result(turn, segment_idx);

    debug!(
        turn_order = turn.turn_order,
        end_of_turn = turn.end_of_turn,
        confidence = turn.end_of_turn_confidence,
        transcript_len = turn.transcript.len(),
        word_count = turn.words.len(),
        "AssemblyAI turn"
    );

    forward(result_tx, result, cancel).await;
}

/// Hand `result` to the pipeline once its channel has room, unless the
/// session is cancelled first: a pipeline that holds the receiver but has
/// stopped reading cannot keep cancellation from being seen. `false` when
/// the result was not delivered.
async fn forward(
    result_tx: &mpsc::Sender<TranscriptionResult>,
    result: TranscriptionResult,
    cancel: &tokio_util::sync::CancellationToken,
) -> bool {
    tokio::select! {
        permit = result_tx.reserve() => match permit {
            Ok(permit) => {
                permit.send(result);
                true
            }
            Err(_closed) => false,
        },
        () = cancel.cancelled() => false,
    }
}

/// Map a `TurnMessage` to the pipeline's `TranscriptionResult`.
fn turn_to_result(turn: &TurnMessage, segment_idx: &mut u32) -> TranscriptionResult {
    let is_final = turn.end_of_turn;
    // Assign the current segment index to this result, then post-increment on
    // end-of-turn so a turn's partials and its final share the same index
    // (a pre-increment here would give the final a different index from its
    // own partials).
    let segment_index = *segment_idx;
    if turn.end_of_turn {
        *segment_idx += 1;
    }

    TranscriptionResult::new(turn.transcript.clone(), is_final)
        .with_confidence(avg_confidence(&turn.words))
        .with_language(turn.language_code.clone())
        .with_segment_index(segment_index)
        .with_start_time(turn.words.first().map(|w| Duration::from_millis(w.start)))
        .with_end_time(turn.words.last().map(|w| Duration::from_millis(w.end)))
        .with_words(final_words(&turn.words))
        .with_is_end_of_turn(Some(turn.end_of_turn))
}

/// Extract [`WordInfo`] entries for all finalised words.
fn final_words(words: &[super::messages::TurnWord]) -> Vec<WordInfo> {
    words
        .iter()
        .filter(|w| w.word_is_final)
        .map(|w| {
            WordInfo::new(
                w.text.clone(),
                Duration::from_millis(w.start),
                Duration::from_millis(w.end),
            )
            .with_confidence(Some(w.confidence))
        })
        .collect()
}

/// Compute the average confidence of all finalised words, or `None`
/// if there are no finalised words.
fn avg_confidence(words: &[super::messages::TurnWord]) -> Option<f32> {
    let final_words: Vec<f32> = words
        .iter()
        .filter(|w| w.word_is_final)
        .map(|w| w.confidence)
        .collect();

    if final_words.is_empty() {
        return None;
    }
    Some(final_words.iter().sum::<f32>() / final_words.len() as f32)
}

/// Samples in `ms` of mono audio at `sample_rate`.
fn frame_samples(sample_rate: u32, ms: u32) -> usize {
    usize::try_from(u64::from(sample_rate) * u64::from(ms) / 1000).unwrap_or(usize::MAX)
}

/// `samples` as the fewest frames of at most `max` samples, of near-equal
/// length: an even split leaves no short tail, so every frame stays above
/// the vendor's minimum whenever the whole does.
fn split_frames(samples: &[i16], max: usize) -> std::slice::Chunks<'_, i16> {
    let frames = samples.len().div_ceil(max.max(1)).max(1);
    samples.chunks(samples.len().div_ceil(frames).max(1))
}

/// Classify a `tungstenite::Error` into an `SttError`.
fn classify_ws_error(err: &voice_pipes_net::tokio_tungstenite::tungstenite::Error) -> SttError {
    use voice_pipes_net::tokio_tungstenite::tungstenite::Error;
    match err {
        Error::Http(resp) => SttError::from_http_status("AssemblyAI", resp.status().as_u16(), ""),
        Error::ConnectionClosed | Error::AlreadyClosed => {
            SttError::NetworkError("connection closed".into())
        }
        other => SttError::NetworkError(other.to_string()),
    }
}

/// Classify a server-reported error (an `Error` frame or a close reason)
/// into an `SttError` by its semantics.
///
/// The concurrency check runs FIRST: `AssemblyAI` phrases its capacity error
/// as "Unauthorized Connection: Too many concurrent sessions", which must
/// not be mistaken for a (permanent) auth failure — it is a transient
/// capacity condition that heals when a slot frees up.
fn classify_server_error(code: Option<u32>, msg: &str) -> SttError {
    let lower = msg.to_lowercase();
    if lower.contains("concurrent session") || lower.contains("too many") {
        return SttError::RateLimited { retry_after: None };
    }
    if lower.contains("api key")
        || lower.contains("unauthorized")
        || lower.contains("not authorized")
        || lower.contains("authentication")
    {
        return SttError::AuthError(msg.to_string());
    }
    SttError::ApiError {
        status: code.and_then(|c| u16::try_from(c).ok()).unwrap_or(1011),
        message: msg.to_string(),
    }
}

/// Classify a WebSocket close frame into an `SttError`.
fn classify_close(
    code: Option<
        voice_pipes_net::tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode,
    >,
    reason: &str,
) -> SttError {
    use voice_pipes_net::tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    match code {
        Some(CloseCode::Normal) => SttError::NetworkError("server closed normally".into()),
        // Policy (1008) covers both real auth failures and the concurrency
        // limit; route through the semantic classifier on the reason text.
        Some(CloseCode::Policy) => classify_server_error(Some(1008), reason),
        Some(CloseCode::Error) => SttError::ApiError {
            status: 1011,
            message: format!("server error: {reason}"),
        },
        _ => SttError::NetworkError(format!("WebSocket closed: code={code:?} reason={reason}")),
    }
}

// ── Tests ────────────────────────────────────────────────────

/// A vendor's confidence as the `f32` the SDK carries.
#[expect(
    clippy::cast_possible_truncation,
    reason = "an f64 to f32 cast rounds and saturates to infinity; it cannot panic"
)]
fn confidence_f32(confidence: f64) -> f32 {
    confidence as f32
}

#[cfg(test)]
#[path = "tests/ws_client.rs"]
mod tests;
