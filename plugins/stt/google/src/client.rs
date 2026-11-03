//! gRPC stream lifecycle for Google Cloud Speech-to-Text v2.
//!
//! One [`GoogleClient::run`] call serves a whole pipeline session and owns
//! a *sequence* of `StreamingRecognize` RPCs underneath it:
//!
//! - **Per-utterance mode** (default): a stream is opened lazily when the
//!   first audio of an utterance arrives and half-closed on the pipeline's
//!   end-of-utterance (`is_last`). Google finalises on the half-close, the
//!   finals are drained, and the next utterance opens a fresh stream. Audio
//!   that arrives during the drain is carried over, so nothing is lost and
//!   the pipeline never has to respawn the worker.
//! - **Persistent mode**: one stream across utterances (only sensible with
//!   continuous audio — Google times out a stream that goes quiet).
//!
//! In both modes the stream is rotated proactively before Google's
//! 5-minute cap, preferring a moment when no speech is active.
//! Transient failures reconnect with the same capped backoff as the
//! `AssemblyAI` client; rotation is routine and never counts as a failure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, error, info, instrument, warn};

use voice_pipes_sdk::audio::i16_to_le_bytes;
use voice_pipes_sdk::reconnect::{ReconnectBudget, SessionOutcomeInfo, backoff_delay, jittered};
use voice_pipes_sdk::stt::{AudioChunk, SttError, TranscriptionResult, WordInfo};

use super::auth::AuthSource;
use super::config::{EndpointingSensitivity, GOOGLE_STREAM_CAP_MS, GoogleSttConfig};

pub mod proto {
    #![allow(
        dead_code,
        missing_debug_implementations,
        clippy::all,
        clippy::pedantic,
        clippy::restriction,
        reason = "tonic-generated code"
    )]
    tonic::include_proto!("google.cloud.speech.v2");
}

use proto::explicit_decoding_config::AudioEncoding;
use proto::recognition_config::DecodingConfig;
use proto::streaming_recognition_features::{
    EndpointingSensitivity as ProtoSensitivity, VoiceActivityTimeout,
};
use proto::streaming_recognize_request::StreamingRequest;
use proto::streaming_recognize_response::SpeechEventType;

/// Audio per `audio` message: 100 ms, well under Google's 25 KB cap.
const TARGET_FRAME_DURATION_MS: u32 = 100;
/// Google rejects `audio` messages above 25 KB. Larger buffers (carry-over
/// after a rotation, the pipeline's end-of-utterance flush) are split.
pub const MAX_AUDIO_MESSAGE_BYTES: usize = 25 * 1024;
/// Split threshold — leaves headroom under the cap.
const AUDIO_MESSAGE_BYTES: usize = 16 * 1024;
/// Bound on establishing the connection (TCP + TLS). Deliberately *not* a
/// bound on the call's response headers: Google withholds those until it
/// has a first message, which on a quiet stream can be a long time.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// After a half-close, how long to wait for the trailing finals.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound on fetching a bearer token or the project id before a stream.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
/// Rotate regardless of speech activity this long before Google's cap.
const HARD_ROTATE_MARGIN: Duration = Duration::from_secs(10);
/// While speech is active at the soft rotation mark, re-check this often.
const ROTATE_RECHECK: Duration = Duration::from_millis(500);
/// Outbound request queue depth (≈ 6 s of 100 ms frames of backpressure).
const REQUEST_QUEUE: usize = 64;
/// Audio sent before Google answered the call is kept (up to this much)
/// so a rejected open can replay it on the retry instead of losing the
/// utterance.
const RETAIN_UNACKED_SECS: u32 = 30;

/// Channel shared by every stream of a provider instance, created on
/// first use (tonic spawns its connection driver, so this needs a runtime).
pub type SharedChannel = Arc<tokio::sync::OnceCell<Channel>>;

/// A stream that has carried no audio for this long and is then closed by
/// Google was idle-timed-out, not broken: rotate without backoff.
const QUIET_STREAM_GRACE: Duration = Duration::from_secs(5);

/// gRPC client bridging pipeline audio to Google Speech v2.
#[derive(Debug)]
pub struct GoogleClient {
    config: GoogleSttConfig,
    channel: SharedChannel,
    auth: Arc<AuthSource>,
    audit_tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::audit::TraceEvent>>,
    /// Component health: one latency sample per final result, measured
    /// from the end of speech (not the session length — a session-long
    /// stream would read as a multi-minute "latency" and trip the
    /// auto-degrade threshold).
    core: Option<Arc<voice_pipes_sdk::component::ComponentCore>>,
}

/// How one upstream stream ended (no error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamOutcome {
    /// Session over: cancelled, downstream gone, or audio input closed.
    Complete,
    /// Utterance boundary (per-utterance mode) or timed rotation — open
    /// the next stream and keep going.
    Rotate,
}

/// Where the current stream is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Audio flows to Google.
    Streaming,
    /// Request side half-closed; waiting for the trailing finals. Audio
    /// arriving now is carried to the next stream.
    Draining(StreamOutcome),
}

/// State that survives across the streams of one session.
#[derive(Debug, Default)]
struct SessionState {
    /// Monotonic segment index; a turn's partials and its final share it.
    segment_idx: u32,
    /// Audio received while a stream was draining, sent first on the next.
    carry: Vec<i16>,
    /// The pipeline closed the audio channel — no more streams after this.
    audio_closed: bool,
    /// The utterance whose audio is in `carry` already ended (`is_last`
    /// arrived while the previous stream was draining): the next stream
    /// must half-close right after replaying the carry instead of waiting
    /// for a boundary that will never come.
    carry_closes_utterance: bool,
    /// The "shedding queued audio" warning has been logged for this
    /// session; later sheds are counted, not repeated.
    backlog_warned: bool,
    /// What the current stream achieved: when Google answered the call and
    /// whether any transcript reached the pipeline. Decides whether a
    /// failure restores the reconnect budget.
    session: SessionOutcomeInfo,
    /// Samples sent on the current stream, for the usage audit event.
    stream_samples: usize,
    stream_opened_at: Option<Instant>,
    /// When audio last went out on the wire. A stream Google drops after
    /// this has been quiet for a while is a routine idle timeout, not a
    /// failure.
    last_audio_sent_at: Option<Instant>,
    /// When the current utterance's speech ended (Google's
    /// `SPEECH_ACTIVITY_END`, or the pipeline's `is_last`): the anchor the
    /// final-result latency is measured from.
    speech_end_at: Option<Instant>,
}

impl GoogleClient {
    pub fn new(config: GoogleSttConfig, channel: SharedChannel, auth: Arc<AuthSource>) -> Self {
        Self {
            config,
            channel,
            auth,
            audit_tx: None,
            core: None,
        }
    }

    /// Attach the provider's component core for per-final latency samples.
    #[must_use]
    pub fn with_core(mut self, core: Arc<voice_pipes_sdk::component::ComponentCore>) -> Self {
        self.core = Some(core);
        self
    }

    /// The shared channel, built on first use.
    async fn channel(&self) -> Result<Channel, SttError> {
        self.channel
            .get_or_try_init(|| async {
                Self::build_channel(&self.config).map_err(|e| SttError::ConfigError(e.to_string()))
            })
            .await
            .cloned()
    }

    /// Attach an audit trace channel for emitting STT usage events.
    #[must_use]
    pub fn with_audit_tx(
        mut self,
        tx: Option<tokio::sync::mpsc::Sender<voice_pipes_sdk::audit::TraceEvent>>,
    ) -> Self {
        self.audit_tx = tx;
        self
    }

    /// Build the (lazily connecting) channel for a configuration. TLS with
    /// the platform root store for `https://`; plaintext for the `http://`
    /// override used by the in-process test server.
    pub fn build_channel(config: &GoogleSttConfig) -> Result<Channel, anyhow::Error> {
        let mut endpoint = Endpoint::from_shared(config.endpoint.clone())
            .map_err(|e| anyhow::anyhow!("invalid Google STT endpoint {}: {e}", config.endpoint))?
            .connect_timeout(OPEN_TIMEOUT)
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_while_idle(true);
        if !config.plaintext() {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_native_roots())
                .map_err(|e| anyhow::anyhow!("Google STT TLS setup failed: {e}"))?;
        }
        Ok(endpoint.connect_lazy())
    }

    /// The `StreamingRecognitionConfig` sent as the first message of every
    /// stream — the wire form of [`GoogleSttConfig`].
    pub fn streaming_config(&self) -> proto::StreamingRecognitionConfig {
        let c = &self.config;
        let voice_activity_timeout = (c.speech_start_timeout.is_some()
            || c.speech_end_timeout.is_some())
        .then(|| VoiceActivityTimeout {
            speech_start_timeout: c.speech_start_timeout.map(to_proto_duration),
            speech_end_timeout: c.speech_end_timeout.map(to_proto_duration),
        });
        proto::StreamingRecognitionConfig {
            config: Some(proto::RecognitionConfig {
                decoding_config: Some(DecodingConfig::ExplicitDecodingConfig(
                    proto::ExplicitDecodingConfig {
                        encoding: AudioEncoding::Linear16 as i32,
                        sample_rate_hertz: c.sample_rate as i32,
                        audio_channel_count: 1,
                    },
                )),
                model: c.model.clone(),
                language_codes: c.language_codes.clone(),
                features: Some(proto::RecognitionFeatures {
                    profanity_filter: c.profanity_filter,
                    enable_word_time_offsets: c.enable_word_time_offsets,
                    enable_word_confidence: c.enable_word_confidence,
                    enable_automatic_punctuation: c.enable_automatic_punctuation,
                    max_alternatives: 1,
                }),
            }),
            streaming_features: Some(proto::StreamingRecognitionFeatures {
                enable_voice_activity_events: c.enable_voice_activity_events,
                interim_results: c.interim_results,
                voice_activity_timeout,
                endpointing_sensitivity: c
                    .endpointing_sensitivity
                    .map_or(ProtoSensitivity::Unspecified, to_proto_sensitivity)
                    as i32,
            }),
        }
    }

    /// Run the streaming session: a sequence of upstream streams with
    /// rotation and reconnection, until cancelled or the audio input closes.
    #[instrument(name = "google_stt.session", skip_all, fields(
        provider = "google",
        model = %self.config.model,
        location = %self.config.location,
    ))]
    pub async fn run(
        &self,
        mut audio_rx: mpsc::Receiver<AudioChunk>,
        result_tx: mpsc::Sender<TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        let mut state = SessionState::default();
        let mut budget = ReconnectBudget::new(self.config.max_reconnect_attempts);

        loop {
            // Downstream gone (pipeline torn down) counts the same as an
            // explicit cancel: opening another stream would leak it.
            if cancel.is_cancelled() || result_tx.is_closed() {
                break Ok(());
            }
            state.session = SessionOutcomeInfo::default();
            state.stream_samples = 0;
            state.stream_opened_at = None;

            let outcome = self
                .run_stream(&mut state, &mut audio_rx, &result_tx, &cancel)
                .await;
            self.emit_usage(&state, &outcome);

            match outcome {
                Ok(StreamOutcome::Complete) => {
                    info!("Google STT session completed");
                    break Ok(());
                }
                Ok(StreamOutcome::Rotate) => {
                    budget.on_success();
                }
                Err(e) if !e.is_transient() => {
                    error!(error = %e, "Google STT permanent error");
                    break Err(e);
                }
                Err(e) => {
                    // Only a stream that lived a while or delivered a
                    // transcript restores the budget; a stream Google
                    // accepted and then failed at once does not, so a
                    // deterministic rejection ends the run with an error
                    // instead of looping.
                    let attempt = match budget.on_failure(&state.session, Instant::now(), &e) {
                        Ok(attempt) => attempt,
                        Err(exhausted) => {
                            error!(error = %exhausted, "Google STT reconnect budget exhausted");
                            break Err(exhausted);
                        }
                    };
                    let delay = jittered(self.backoff_delay(attempt));
                    warn!(
                        attempt,
                        delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                        error = %e,
                        "Google STT reconnecting"
                    );
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = cancel.cancelled() => break Ok(()),
                        () = result_tx.closed() => break Ok(()),
                    }
                }
            }
        }
    }

    /// One upstream stream, opened lazily on the first audio.
    #[instrument(name = "google_stt.stream", skip_all)]
    async fn run_stream(
        &self,
        state: &mut SessionState,
        audio_rx: &mut mpsc::Receiver<AudioChunk>,
        result_tx: &mpsc::Sender<TranscriptionResult>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<StreamOutcome, SttError> {
        let persistent = self.config.persistent_session;
        let Some(close_after_carry) =
            wait_for_audio(state, audio_rx, result_tx, cancel, persistent).await
        else {
            return Ok(StreamOutcome::Complete);
        };
        let Some((request, req_tx, recognizer)) = self.prepare_call(cancel, result_tx).await?
        else {
            return Ok(StreamOutcome::Complete);
        };

        let mut client = proto::speech_client::SpeechClient::new(self.channel().await?);
        let opened_at = Instant::now();
        state.stream_opened_at = Some(opened_at);
        // Google holds the response headers until it has a first message to
        // send, and a first message needs audio — so the call is awaited
        // *concurrently* with the audio pump below, never before it
        // (awaiting it first deadlocks until the connect timeout).
        let open = client.streaming_recognize(request);
        tokio::pin!(open);

        let mut req_tx = Some(req_tx);
        let mut up = Upstream::new(
            &self.config,
            self.streaming_config(),
            recognizer,
            opened_at,
            state,
        );
        if state.audio_closed {
            // Replaying carry-over after the input already ended (a retry
            // after a failed open): nothing more will come.
            up.begin_drain(StreamOutcome::Complete);
        } else if close_after_carry {
            up.begin_drain(StreamOutcome::Rotate);
        }

        loop {
            // Half-close once everything queued ahead of it has gone out.
            if up.half_close_pending && up.outbound.is_empty() {
                req_tx = None;
                up.half_close_pending = false;
            }
            tokio::select! {
                biased;

                // Dropping `req_tx` and `inbound` cancels the RPC; nothing
                // to hand-shake with Google.
                () = cancel.cancelled() => return Ok(StreamOutcome::Complete),
                () = result_tx.closed() => return Ok(StreamOutcome::Complete),

                // The call is polled before anything is queued behind it,
                // so the request body is attached and tonic starts pulling
                // from the channel as soon as there is capacity.
                r = &mut open, if up.inbound.is_none() => match r {
                    Ok(resp) => up.opened(state, resp.into_inner()),
                    Err(status) => {
                        // Nothing was acknowledged: replay everything on
                        // the retry (transient) — or report (permanent).
                        requeue(state, &mut up.unacked);
                        return Err(classify_status(&status));
                    }
                },

                // One queued request per channel permit. Waiting for a
                // permit here (instead of awaiting a send inside another
                // arm) is what keeps cancel and the deadlines live while
                // Google is slow to read.
                permit = reserve_slot(req_tx.as_ref()), if req_tx.is_some() && !up.outbound.is_empty() => {
                    let Some(permit) = permit else {
                        return Err(SttError::NetworkError("request stream closed".into()));
                    };
                    if let Some(req) = up.outbound.pop_front() {
                        permit.send(req);
                    }
                }

                chunk_opt = audio_rx.recv(), if !state.audio_closed => {
                    up.on_chunk(state, chunk_opt, persistent);
                }

                msg = next_message(&mut up.inbound), if up.inbound.is_some() => {
                    if let Some(outcome) = self.on_message(msg, &mut up, state, result_tx, cancel).await {
                        return outcome;
                    }
                }

                () = tokio::time::sleep_until(up.rotate_at), if matches!(up.phase, Phase::Streaming) => {
                    up.on_rotate_mark(state);
                }

                () = tokio::time::sleep_until(up.drain_deadline), if matches!(up.phase, Phase::Draining(_)) => {
                    return up.drain_timed_out(state);
                }
            }
        }
    }

    /// The authenticated request for a new stream, its outbound channel and
    /// the recognizer it names; `None` when cancelled meanwhile.
    /// Credential fetches go to the network (a metadata server, a token
    /// endpoint); they are bounded and abandoned on cancel like every other
    /// wait.
    async fn prepare_call(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
        result_tx: &mpsc::Sender<TranscriptionResult>,
    ) -> Result<Option<PreparedCall>, SttError> {
        let Some(token) = until_cancelled(cancel, result_tx, self.auth.bearer()).await? else {
            return Ok(None);
        };
        let Some(project) = until_cancelled(cancel, result_tx, self.auth.project_id()).await?
        else {
            return Ok(None);
        };
        let recognizer = self.config.recognizer_path(&project);
        let (req_tx, req_rx) =
            mpsc::channel::<proto::StreamingRecognizeRequest>(request_queue_len(&self.config));
        let mut request = tonic::Request::new(ReceiverStream::new(req_rx));
        // The token lives only as long as it takes to build the header,
        // which is marked sensitive.
        let mut bearer = MetadataValue::try_from(format!("Bearer {}", token.expose()))
            .map_err(|e| SttError::AuthError(format!("token is not a valid header value: {e}")))?;
        bearer.set_sensitive(true);
        drop(token);
        request.metadata_mut().insert("authorization", bearer);
        // Quota attribution for user credentials; harmless for service
        // accounts.
        match MetadataValue::try_from(project.as_str()) {
            Ok(v) => {
                request.metadata_mut().insert("x-goog-user-project", v);
            }
            Err(e) => warn!(project = %project, error = %e, "project id not sendable as header"),
        }
        Ok(Some((request, req_tx, recognizer)))
    }

    /// One response from Google, or the end of them. `Some` ends the stream
    /// with that outcome; `None` carries on.
    async fn on_message(
        &self,
        msg: Result<Option<proto::StreamingRecognizeResponse>, tonic::Status>,
        up: &mut Upstream,
        state: &mut SessionState,
        result_tx: &mpsc::Sender<TranscriptionResult>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Option<Result<StreamOutcome, SttError>> {
        let resp = match msg {
            Ok(Some(resp)) => resp,
            Ok(None) => return Some(up.closed_by_server(state)),
            Err(status) => return Some(up.failed(state, &status)),
        };
        if state.session.established_at.is_none() {
            state.session.established_at = Some(Instant::now());
        }
        for tr in response_to_results(&resp, &mut state.segment_idx, &mut up.speech_active) {
            if !tr.text.is_empty() {
                state.session.delivered_transcript = true;
            }
            if tr.speech_started {
                state.speech_end_at = None;
            } else if tr.is_final {
                // Latency = end of speech → final. Fall back to the last
                // audio sent when Google gave no activity event.
                let anchor = state.speech_end_at.take().or(state.last_audio_sent_at);
                if let (Some(core), Some(at)) = (&self.core, anchor) {
                    core.record_ok(at.elapsed());
                }
            } else if tr.is_end_of_turn == Some(true) && tr.text.is_empty() {
                state.speech_end_at = Some(Instant::now());
            }
            // A permit, not a bare send: a pipeline that holds the receiver
            // but has stopped reading cannot keep cancellation from being
            // seen.
            let permit = tokio::select! {
                permit = result_tx.reserve() => permit,
                () = cancel.cancelled() => return Some(Ok(StreamOutcome::Complete)),
            };
            let Ok(permit) = permit else {
                return Some(Ok(StreamOutcome::Complete));
            };
            permit.send(tr);
        }
        None
    }

    /// Exponential backoff for reconnect attempt `attempt` (1-based), capped.
    fn backoff_delay(&self, attempt: u32) -> Duration {
        backoff_delay(
            self.config.reconnect_delay,
            self.config.reconnect_delay_max,
            attempt,
        )
    }

    /// Per-stream usage record: Chirp bills per 15 s of audio, so the
    /// session diagnostic should be able to add it up.
    fn emit_usage(&self, state: &SessionState, outcome: &Result<StreamOutcome, SttError>) {
        let Some(opened_at) = state.stream_opened_at else {
            return;
        };
        let audio_s = state.stream_samples as f64 / f64::from(self.config.sample_rate);
        let reason = match outcome {
            Ok(StreamOutcome::Complete) => "complete".to_string(),
            Ok(StreamOutcome::Rotate) => "rotate".to_string(),
            Err(e) => format!("error: {e}"),
        };
        debug!(
            audio_s = format!("{audio_s:.2}"),
            stream_s = opened_at.elapsed().as_secs_f64(),
            reason = %reason,
            "Google STT stream closed"
        );
        if let Some(tx) = &self.audit_tx {
            let _ = tx.try_send(voice_pipes_sdk::audit::TraceEvent::new(
                "stt:google",
                "stt_usage",
                serde_json::json!({
                    "audio_duration_seconds": audio_s,
                    "session_duration_seconds": opened_at.elapsed().as_secs_f64(),
                    "model": &self.config.model,
                    "reason": reason,
                }),
            ));
        }
    }
}

/// A stream's authenticated request, the channel feeding it, and the
/// recognizer it names.
type PreparedCall = (
    tonic::Request<ReceiverStream<proto::StreamingRecognizeRequest>>,
    mpsc::Sender<proto::StreamingRecognizeRequest>,
    String,
);

/// Outbound requests are queued locally and handed to the RPC one permit
/// at a time from inside the select loop, so a request stream that tonic
/// is not yet pulling from (the call has not been polled, or Google is not
/// reading) never blocks the task: cancellation and the drain deadline
/// stay responsive however much audio is waiting. The channel is sized for
/// the audio a retry may replay plus what arrives during a drain.
fn request_queue_len(config: &GoogleSttConfig) -> usize {
    REQUEST_QUEUE.max(
        pieces_for_samples(samples_usize(
            u64::from(config.sample_rate) * u64::from(RETAIN_UNACKED_SECS),
        )) + pieces_for_samples(samples_usize(
            u64::from(config.sample_rate) * DRAIN_TIMEOUT.as_secs(),
        )) + 1,
    )
}

/// Wait for audio worth opening a stream for: an idle stream is not free
/// (Google times it out), so in per-utterance mode the stream opens when
/// the utterance starts. The carry may already hold a whole utterance whose
/// end arrived while the previous stream was draining. `Some(close)` says
/// whether the stream half-closes right after the carry; `None` ends the
/// session.
async fn wait_for_audio(
    state: &mut SessionState,
    audio_rx: &mut mpsc::Receiver<AudioChunk>,
    result_tx: &mpsc::Sender<TranscriptionResult>,
    cancel: &tokio_util::sync::CancellationToken,
    persistent: bool,
) -> Option<bool> {
    let mut close_after_carry = state.carry_closes_utterance && !persistent;
    while state.carry.is_empty() {
        if state.audio_closed {
            return None;
        }
        tokio::select! {
            () = cancel.cancelled() => return None,
            () = result_tx.closed() => return None,
            chunk = audio_rx.recv() => {
                let Some(chunk) = chunk else {
                    state.audio_closed = true;
                    return None;
                };
                state.carry.extend_from_slice(&chunk.samples);
                // A bare end-of-utterance sentinel with no audio (VAD
                // discard) is not worth a stream: the loop waits on.
                close_after_carry = chunk.is_last && !persistent;
            }
        }
    }
    state.carry_closes_utterance = false;
    Some(close_after_carry)
}

/// One open stream: what is queued for Google, what it has not yet
/// acknowledged, and where the stream is in its life.
struct Upstream {
    outbound: std::collections::VecDeque<proto::StreamingRecognizeRequest>,
    queue_len: usize,
    /// Set when the request side should half-close; the channel is dropped
    /// once everything queued before it has gone out.
    half_close_pending: bool,
    send_buf: Vec<i16>,
    /// Audio Google has not acknowledged (no response headers yet).
    unacked: Vec<i16>,
    retain_cap: usize,
    target_samples: usize,
    phase: Phase,
    speech_active: bool,
    inbound: Option<tonic::Streaming<proto::StreamingRecognizeResponse>>,
    recognizer: String,
    opened_at: Instant,
    soft_rotate_at: Instant,
    hard_rotate_at: Instant,
    rotate_at: tokio::time::Instant,
    drain_deadline: tokio::time::Instant,
}

impl Upstream {
    /// The stream's state with its config request queued, then the carry,
    /// which goes out first: it is already late.
    fn new(
        config: &GoogleSttConfig,
        streaming_config: proto::StreamingRecognitionConfig,
        recognizer: String,
        opened_at: Instant,
        state: &mut SessionState,
    ) -> Self {
        let soft_rotate_at = opened_at + config.stream_rotate;
        let mut up = Self {
            outbound: std::collections::VecDeque::from([proto::StreamingRecognizeRequest {
                recognizer: recognizer.clone(),
                streaming_request: Some(StreamingRequest::StreamingConfig(streaming_config)),
            }]),
            queue_len: request_queue_len(config),
            half_close_pending: false,
            send_buf: std::mem::take(&mut state.carry),
            unacked: Vec::new(),
            retain_cap: (config.sample_rate * RETAIN_UNACKED_SECS) as usize,
            target_samples: (config.sample_rate * TARGET_FRAME_DURATION_MS / 1000) as usize,
            phase: Phase::Streaming,
            speech_active: false,
            inbound: None,
            recognizer,
            opened_at,
            soft_rotate_at,
            hard_rotate_at: opened_at
                + Duration::from_millis(GOOGLE_STREAM_CAP_MS).saturating_sub(HARD_ROTATE_MARGIN),
            rotate_at: soft_rotate_at.into(),
            drain_deadline: tokio::time::Instant::now() + Duration::from_hours(24),
        };
        if !up.send_buf.is_empty() {
            up.flush(state);
        }
        up
    }

    /// Queue the buffered audio. Until Google answers the call, what is
    /// sent is also kept, to replay on a retry.
    fn flush(&mut self, state: &mut SessionState) {
        let retain = if self.inbound.is_none() {
            self.retain_cap
        } else {
            0
        };
        queue_audio(
            &mut self.outbound,
            self.queue_len,
            &mut self.send_buf,
            state,
            &mut self.unacked,
            retain,
        );
    }

    /// Half-close the request side and wait, bounded, for the trailing
    /// finals; the stream then ends with `outcome`.
    fn begin_drain(&mut self, outcome: StreamOutcome) {
        self.half_close_pending = true;
        self.phase = Phase::Draining(outcome);
        self.drain_deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
    }

    /// Google answered the call: it has the audio sent so far.
    fn opened(
        &mut self,
        state: &mut SessionState,
        inbound: tonic::Streaming<proto::StreamingRecognizeResponse>,
    ) {
        state.session.established_at = Some(Instant::now());
        self.inbound = Some(inbound);
        self.unacked = Vec::new();
        info!(
            recognizer = %self.recognizer,
            open_ms = u64::try_from(self.opened_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            "Google STT stream opened"
        );
    }

    /// Audio from the pipeline. While streaming it goes out in frames, and
    /// an utterance's end half-closes the stream (per-utterance mode); while
    /// draining it belongs to the next stream.
    fn on_chunk(
        &mut self,
        state: &mut SessionState,
        chunk_opt: Option<AudioChunk>,
        persistent: bool,
    ) {
        let Some(chunk) = chunk_opt else {
            state.audio_closed = true;
            if matches!(self.phase, Phase::Streaming) {
                self.flush(state);
                self.begin_drain(StreamOutcome::Complete);
            }
            return;
        };
        if let Phase::Draining(_) = self.phase {
            // An utterance that ends while this one drains must still get
            // its own half-close on the next stream.
            state.carry.extend_from_slice(&chunk.samples);
            if chunk.is_last && !persistent {
                state.carry_closes_utterance = true;
            }
            return;
        }
        self.send_buf.extend_from_slice(&chunk.samples);
        if self.send_buf.len() >= self.target_samples || chunk.is_last {
            self.flush(state);
        }
        if chunk.is_last {
            state.speech_end_at = Some(Instant::now());
            if !persistent {
                // Google finalises what it has; the next utterance gets a
                // new stream.
                debug!("end of utterance: half-closing stream");
                self.begin_drain(StreamOutcome::Rotate);
            }
        }
    }

    /// Google ended the response stream.
    fn closed_by_server(&self, state: &SessionState) -> Result<StreamOutcome, SttError> {
        match self.phase {
            Phase::Draining(outcome) => Ok(outcome),
            // Google closed on us mid-stream. Past the rotation mark that is
            // the 5-minute cap, and on a quiet stream it is the idle timeout;
            // both simply mean "rotate now".
            Phase::Streaming if Instant::now() >= self.soft_rotate_at => {
                warn!("stream closed by server at rotation mark; rotating");
                Ok(StreamOutcome::Rotate)
            }
            Phase::Streaming if quiet_for(state) >= QUIET_STREAM_GRACE => {
                info!("idle stream closed by server; rotating");
                Ok(StreamOutcome::Rotate)
            }
            Phase::Streaming => Err(SttError::NetworkError("stream ended unexpectedly".into())),
        }
    }

    /// The response stream failed.
    fn failed(
        &self,
        state: &SessionState,
        status: &tonic::Status,
    ) -> Result<StreamOutcome, SttError> {
        let err = classify_status(status);
        if let Phase::Draining(outcome) = self.phase {
            // The utterance's audio is already sent; the finals may be lost,
            // but the session goes on.
            warn!(error = %err, "stream error while draining");
            return Ok(outcome);
        }
        if err.is_transient() && quiet_for(state) >= QUIET_STREAM_GRACE {
            // "Stream timed out after receiving no more client requests":
            // Google's idle timeout on a stream nothing was being sent on.
            // Routine — the next audio opens a fresh stream.
            info!(
                code = ?status.code(),
                reason_chars = status.message().chars().count(),
                "idle stream dropped by server; rotating"
            );
            return Ok(StreamOutcome::Rotate);
        }
        Err(err)
    }

    /// The stream reached its rotation mark: rotate now unless speech is
    /// under way, and past the hard mark rotate regardless.
    fn on_rotate_mark(&mut self, state: &mut SessionState) {
        if !self.speech_active || Instant::now() >= self.hard_rotate_at {
            info!(
                age_s = self.opened_at.elapsed().as_secs(),
                speech_active = self.speech_active,
                "rotating Google STT stream before the 5-minute cap"
            );
            self.flush(state);
            self.begin_drain(StreamOutcome::Rotate);
        } else {
            // Mid-utterance: look again shortly — prefer a turn boundary,
            // the margin gives slack.
            self.rotate_at = tokio::time::Instant::now() + ROTATE_RECHECK;
        }
    }

    /// The drain went on too long.
    fn drain_timed_out(&mut self, state: &mut SessionState) -> Result<StreamOutcome, SttError> {
        if self.inbound.is_none() {
            // Half-closed and still no headers: Google never took the call.
            // Retry with the audio.
            warn!("no response to the stream at all; retrying");
            requeue(state, &mut self.unacked);
            return Err(SttError::Timeout(DRAIN_TIMEOUT));
        }
        warn!("timed out waiting for Google to finish the stream");
        match self.phase {
            Phase::Draining(outcome) => Ok(outcome),
            // Not reached: the deadline is only watched while draining.
            Phase::Streaming => Ok(StreamOutcome::Rotate),
        }
    }
}

/// `inbound.message()` once the stream is open, pending until then. The
/// `select!` arm is disabled by its precondition while `inbound` is `None`,
/// but the arm's expression is still evaluated, so it must not unwrap.
#[expect(
    clippy::result_large_err,
    reason = "passes tonic's own Streaming::message result through; boxing Status would add an allocation per message"
)]
async fn next_message(
    inbound: &mut Option<tonic::Streaming<proto::StreamingRecognizeResponse>>,
) -> Result<Option<proto::StreamingRecognizeResponse>, tonic::Status> {
    match inbound {
        Some(s) => s.message().await,
        None => std::future::pending().await,
    }
}

/// How long the current stream has carried no audio.
fn quiet_for(state: &SessionState) -> Duration {
    state
        .last_audio_sent_at
        .map_or(Duration::ZERO, |t| t.elapsed())
}

/// Put unacknowledged audio back in front of whatever arrived since, so
/// the next stream replays it in order.
fn requeue(state: &mut SessionState, unacked: &mut Vec<i16>) {
    if unacked.is_empty() {
        return;
    }
    let later = std::mem::take(&mut state.carry);
    state.carry = std::mem::take(unacked);
    state.carry.extend_from_slice(&later);
}

/// Await `fut`, giving up when the session is cancelled or the pipeline is
/// gone. `Ok(None)` means "stop, nothing wrong"; the credential fetch is
/// also bounded so a stalled token endpoint cannot hold the worker.
async fn until_cancelled<T>(
    cancel: &tokio_util::sync::CancellationToken,
    result_tx: &mpsc::Sender<TranscriptionResult>,
    fut: impl std::future::Future<Output = Result<T, SttError>>,
) -> Result<Option<T>, SttError> {
    tokio::select! {
        () = cancel.cancelled() => Ok(None),
        () = result_tx.closed() => Ok(None),
        r = tokio::time::timeout(AUTH_TIMEOUT, fut) => match r {
            Ok(inner) => inner.map(Some),
            Err(_elapsed) => Err(SttError::Timeout(AUTH_TIMEOUT)),
        },
    }
}

/// A send permit on the request channel, or `None` once the RPC dropped
/// its end. Pending forever when there is no channel, which the `select!`
/// precondition rules out.
async fn reserve_slot(
    tx: Option<&mpsc::Sender<proto::StreamingRecognizeRequest>>,
) -> Option<mpsc::Permit<'_, proto::StreamingRecognizeRequest>> {
    match tx {
        Some(tx) => tx.reserve().await.ok(),
        None => std::future::pending().await,
    }
}

/// How many `audio` messages `samples` split into.
fn pieces_for_samples(samples: usize) -> usize {
    (samples * 2).div_ceil(AUDIO_MESSAGE_BYTES)
}

/// Queue `buf` for the request stream as one or more `audio` messages,
/// each under Google's 25 KB cap. While `retain_cap > 0` (no response
/// headers yet) a copy is kept in `unacked`, bounded to the most recent
/// `retain_cap` samples. Never blocks: the select loop hands the queued
/// messages to the RPC as it accepts them, and a backlog beyond
/// `max_queued` (a peer that stopped reading) sheds its oldest audio.
fn queue_audio(
    outbound: &mut std::collections::VecDeque<proto::StreamingRecognizeRequest>,
    max_queued: usize,
    buf: &mut Vec<i16>,
    state: &mut SessionState,
    unacked: &mut Vec<i16>,
    retain_cap: usize,
) {
    if buf.is_empty() {
        return;
    }
    if retain_cap > 0 {
        unacked.extend_from_slice(buf);
        if unacked.len() > retain_cap {
            let excess = unacked.len() - retain_cap;
            unacked.drain(..excess);
        }
    }
    state.last_audio_sent_at = Some(Instant::now());
    for piece in split_audio(buf) {
        debug_assert!(piece.len() <= MAX_AUDIO_MESSAGE_BYTES);
        outbound.push_back(proto::StreamingRecognizeRequest {
            recognizer: String::new(),
            streaming_request: Some(StreamingRequest::Audio(piece)),
        });
    }
    state.stream_samples += buf.len();
    buf.clear();
    let dropped = shed_backlog(outbound, max_queued);
    if dropped > 0 && !state.backlog_warned {
        state.backlog_warned = true;
        warn!(
            dropped_messages = dropped,
            max_queued, "Google STT is not accepting audio; shedding the oldest queued audio"
        );
    }
}

/// Drop the oldest queued `audio` messages until at most `max_queued`
/// requests remain. The stream configuration message is never dropped.
/// Returns how many were removed.
fn shed_backlog(
    outbound: &mut std::collections::VecDeque<proto::StreamingRecognizeRequest>,
    max_queued: usize,
) -> usize {
    let mut dropped = 0;
    while outbound.len() > max_queued {
        let Some(idx) = outbound
            .iter()
            .position(|r| matches!(r.streaming_request, Some(StreamingRequest::Audio(_))))
        else {
            break;
        };
        outbound.remove(idx);
        dropped += 1;
    }
    dropped
}

/// Little-endian PCM bytes, split into pieces of at most
/// [`AUDIO_MESSAGE_BYTES`].
fn split_audio(samples: &[i16]) -> Vec<Vec<u8>> {
    let per_piece = AUDIO_MESSAGE_BYTES / 2;
    samples.chunks(per_piece).map(i16_to_le_bytes).collect()
}

/// Map one `StreamingRecognizeResponse` to pipeline results.
///
/// Results first, then the activity event, so a message that carried both
/// a final and `SPEECH_ACTIVITY_END` commits the text before the marker.
/// `END_OF_SINGLE_UTTERANCE` (v1 semantics) is ignored.
fn response_to_results(
    resp: &proto::StreamingRecognizeResponse,
    segment_idx: &mut u32,
    speech_active: &mut bool,
) -> Vec<TranscriptionResult> {
    let mut out = Vec::new();
    for r in &resp.results {
        let Some(alt) = r.alternatives.first() else {
            continue;
        };
        let tr = result_to_transcription(r, alt, segment_idx);
        if tr.text.is_empty() && !tr.is_final {
            continue;
        }
        if tr.is_final {
            *speech_active = false;
        }
        out.push(tr);
    }
    let offset = resp
        .speech_event_offset
        .as_ref()
        .and_then(from_proto_duration);
    match resp.speech_event_type() {
        SpeechEventType::SpeechActivityBegin => {
            *speech_active = true;
            out.push(
                TranscriptionResult::new(String::new(), false)
                    .with_segment_index(*segment_idx)
                    .with_speech_started(true)
                    .with_start_time(offset),
            );
        }
        SpeechEventType::SpeechActivityEnd => {
            *speech_active = false;
            out.push(
                TranscriptionResult::new(String::new(), false)
                    .with_segment_index(*segment_idx)
                    .with_end_time(offset)
                    .with_is_end_of_turn(Some(true)),
            );
        }
        SpeechEventType::Unspecified | SpeechEventType::EndOfSingleUtterance => {}
    }
    out
}

/// Map one `StreamingRecognitionResult`, by its top alternative `alt`, to
/// the pipeline's [`TranscriptionResult`]. A turn's partials and its final
/// share a segment index; the index advances after the final.
fn result_to_transcription(
    r: &proto::StreamingRecognitionResult,
    alt: &proto::SpeechRecognitionAlternative,
    segment_idx: &mut u32,
) -> TranscriptionResult {
    let segment_index = *segment_idx;
    if r.is_final {
        *segment_idx += 1;
    }
    let words: Vec<WordInfo> = alt
        .words
        .iter()
        .map(|w| {
            let offset = |d: Option<&prost_types::Duration>| {
                d.and_then(from_proto_duration).unwrap_or_default()
            };
            WordInfo::new(
                w.word.clone(),
                offset(w.start_offset.as_ref()),
                offset(w.end_offset.as_ref()),
            )
            .with_confidence((w.confidence > 0.0).then_some(w.confidence))
        })
        .collect();
    TranscriptionResult::new(alt.transcript.trim().to_string(), r.is_final)
        // Google only scores finals; interims report 0.
        .with_confidence((r.is_final && alt.confidence > 0.0).then_some(alt.confidence))
        .with_language((!r.language_code.is_empty()).then(|| r.language_code.clone()))
        .with_segment_index(segment_index)
        .with_start_time(words.first().map(|w| w.start))
        .with_end_time(
            r.result_end_offset
                .as_ref()
                .and_then(from_proto_duration)
                .or_else(|| words.last().map(|w| w.end)),
        )
        .with_words(words)
        .with_is_end_of_turn(Some(r.is_final))
}

fn to_proto_sensitivity(s: EndpointingSensitivity) -> ProtoSensitivity {
    match s {
        EndpointingSensitivity::Standard => ProtoSensitivity::Standard,
        EndpointingSensitivity::Short => ProtoSensitivity::Short,
        EndpointingSensitivity::SuperShort => ProtoSensitivity::Supershort,
    }
}

fn to_proto_duration(d: Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: d.as_secs() as i64,
        nanos: d.subsec_nanos() as i32,
    }
}

fn from_proto_duration(d: &prost_types::Duration) -> Option<Duration> {
    // An offset into the stream is never negative, and prost's conversion
    // negates a negative one, which overflows (a panic) for `i64::MIN`.
    if d.seconds < 0 || d.nanos < 0 {
        return None;
    }
    Duration::try_from(*d).ok()
}

/// `tonic::Status` → [`SttError`].
///
/// Deviates from the generic gRPC provider in one place: `InvalidArgument`
/// is a *config* error here (Google names the offending field — an
/// unsupported model/language pair, a bad recognizer path), not bad audio.
/// `OutOfRange` is how Google reports both the 5-minute cap and an audio
/// timeout, so it reconnects.
fn classify_status(status: &tonic::Status) -> SttError {
    use tonic::Code;
    let msg = status.message().to_string();
    match status.code() {
        Code::Unavailable
        | Code::DeadlineExceeded
        | Code::Aborted
        | Code::Internal
        | Code::Unknown
        | Code::Cancelled
        | Code::OutOfRange => SttError::NetworkError(format!("{}: {msg}", status.code())),
        Code::Unauthenticated => SttError::AuthError(msg),
        Code::PermissionDenied => SttError::AuthError(format!("permission denied: {msg}")),
        Code::ResourceExhausted => SttError::RateLimited { retry_after: None },
        Code::InvalidArgument | Code::NotFound | Code::FailedPrecondition => {
            SttError::ConfigError(format!("{}: {msg}", status.code()))
        }
        other => SttError::ApiError {
            status: other as u16,
            message: msg,
        },
    }
}

// ── Tests ────────────────────────────────────────────────────

/// A sample count as a buffer length, saturating on a narrower `usize`.
fn samples_usize(samples: u64) -> usize {
    usize::try_from(samples).unwrap_or(usize::MAX)
}

#[cfg(test)]
#[path = "tests/client.rs"]
mod tests;
