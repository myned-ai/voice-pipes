//! Off-path gRPC annotator (`mode = "annotate"`, feature `post-grpc`).
//!
//! [`GrpcAnnotator`] implements [`AudioAnnotator::run`] against the same
//! endpoint the inline processor uses. On every stream open it decides once
//! (cached) how to drive the service:
//!
//! * **Streaming** -- `AudioAnnotator.GetInfo` lists `"Annotate"`: one
//!   bidirectional `Annotate` stream per session. Commands from the hub are
//!   forwarded 1:1 as [`proto::AnnotateRequest`]s (static `options` on the
//!   first request only) and every [`proto::AnnotateResponse`] is mapped to an
//!   [`AudioAnnotation`] as it arrives.
//! * **Unary fallback** -- `GetInfo` is unimplemented (a service written
//!   against the inline-only contract) or does not list `"Annotate"`: each
//!   segment becomes one unary `Process` call and its response is wrapped as
//!   an annotation spanning the segment. This is what makes the migration a
//!   config change for existing services.
//!
//! Batching, timeouts, the circuit breaker and cancellation bookkeeping all
//! live in the hub; this type only forwards and maps. When the service ends
//! the stream or fails, `run` returns `Err` and the hub reopens it after the
//! breaker cooldown -- there is no reconnect loop here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tonic::Code;
use tracing::{debug, info, warn};

use crate::grpc::auth::AuthedChannel;
use crate::pipeline::annotate::{
    AnnotatorCommand, AnnotatorError, AudioAnnotation, AudioAnnotator, AudioSegment,
    WARMUP_UTTERANCE_ID,
};

use super::proto;
use super::proto::audio_annotator_client::AudioAnnotatorClient;
use super::proto::audio_post_processor_client::AudioPostProcessorClient;
use super::{parse_extras_json, samples_to_le_bytes};

/// `kind` used when neither the service (streaming `kind` field) nor the
/// config (`options.kind`) names one.
pub const DEFAULT_KIND: &str = "extras";

/// Silence sent with the warm-up request: one nominal batch, enough to push
/// a real inference through the service's model.
pub const WARMUP_SILENCE_MS: u64 = 160;

/// Deadline for the `GetInfo` probe, the `Annotate` stream open (response
/// headers) and the unary warm-up call.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Pause before the single retry of a stream open that failed with
/// `Unavailable` (lazy channel racing a service that is just coming up).
const RETRY_DELAY: Duration = Duration::from_millis(250);

/// Outbound request queue towards tonic (HTTP/2 flow control applies
/// beyond it).
const REQUEST_QUEUE: usize = 64;

/// How the remote service is driven, decided from `GetInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// Bidirectional `AudioAnnotator.Annotate`.
    Stream,
    /// Per-segment unary `AudioPostProcessor.Process`.
    Unary,
}

pub struct GrpcAnnotator {
    name: String,
    endpoint: String,
    channel: AuthedChannel,
    /// `[audio_post_processor.options]`, sent on the first request of every
    /// stream and on every unary `Process` call.
    options: HashMap<String, String>,
    /// `options["kind"]` or [`DEFAULT_KIND`]: the `kind` of unary-fallback
    /// annotations and of streaming responses that leave `kind` empty.
    kind: String,
    /// Per-call budget for the unary fallback (`timeout_ms`).
    unary_timeout: Duration,
    /// Cached probe result; `None` until `GetInfo` gave a definitive answer.
    transport: Mutex<Option<Transport>>,
    /// Monotonic `seq` for unary `Process` calls.
    seq: AtomicU64,
}

impl std::fmt::Debug for GrpcAnnotator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcAnnotator")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("kind", &self.kind)
            .field("transport", &*self.transport.lock())
            .finish_non_exhaustive()
    }
}

impl GrpcAnnotator {
    pub fn new(
        name: String,
        endpoint: String,
        channel: AuthedChannel,
        options: HashMap<String, String>,
        unary_timeout: Duration,
    ) -> Self {
        let kind = options
            .get("kind")
            .filter(|k| !k.is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_KIND.to_string());
        Self {
            name,
            endpoint,
            channel,
            options,
            kind,
            unary_timeout,
            transport: Mutex::new(None),
            seq: AtomicU64::new(0),
        }
    }

    fn err(&self, message: impl Into<String>) -> AnnotatorError {
        AnnotatorError::new(&self.name, message)
    }

    /// PCM s16le silence of [`WARMUP_SILENCE_MS`] at the session's format.
    fn warmup_audio(sample_rate: u32, channels: u16) -> Vec<u8> {
        let frames = u64::from(sample_rate) * WARMUP_SILENCE_MS / 1000;
        let bytes = frames * u64::from(channels.max(1)) * 2;
        vec![0u8; usize::try_from(bytes).unwrap_or(0)]
    }

    /// Parse `extras_json`, logging and skipping anything unusable. The
    /// stream stays open either way -- one bad frame must not cost the
    /// session its annotator.
    fn extras(&self, extras_json: &str) -> Option<serde_json::Value> {
        match parse_extras_json(extras_json) {
            Ok(Some(v)) => Some(v),
            Ok(None) => {
                debug!(annotator = %self.name, "annotation without extras skipped");
                None
            }
            Err(e) => {
                warn!(annotator = %self.name, error = %e, "annotation with invalid extras_json skipped");
                None
            }
        }
    }

    // ── Probe ───────────────────────────────────────────────

    /// Decide (once) between the streaming and unary paths.
    ///
    /// Transport errors are not cached: the next `run` (after the hub's
    /// cooldown) probes again.
    async fn transport(&self) -> Result<Transport, AnnotatorError> {
        if let Some(t) = *self.transport.lock() {
            return Ok(t);
        }
        let mut client = AudioAnnotatorClient::new(self.channel.clone());
        let call = client.get_info(tonic::Request::new(proto::InfoRequest {}));
        let decided = match tokio::time::timeout(OPEN_TIMEOUT, call).await {
            Ok(Ok(resp)) => {
                let info = resp.into_inner();
                if info.services.iter().any(|s| s == "Annotate") {
                    Transport::Stream
                } else {
                    Transport::Unary
                }
            }
            // Inline-only service: it does not know the AudioAnnotator service at all.
            Ok(Err(status)) if status.code() == Code::Unimplemented => Transport::Unary,
            Ok(Err(status)) => {
                return Err(self.err(format!("gRPC GetInfo failed: {status}")));
            }
            Err(_elapsed) => {
                return Err(self.err(format!(
                    "gRPC GetInfo timed out after {} ms",
                    OPEN_TIMEOUT.as_millis()
                )));
            }
        };
        info!(
            annotator = %self.name,
            endpoint = %self.endpoint,
            transport = ?decided,
            "grpc audio annotator probed"
        );
        *self.transport.lock() = Some(decided);
        Ok(decided)
    }

    // ── Streaming path ──────────────────────────────────────

    /// Open the `Annotate` stream; one retry on `Unavailable`.
    async fn open_stream(
        &self,
    ) -> Result<
        (
            mpsc::Sender<proto::AnnotateRequest>,
            tonic::Streaming<proto::AnnotateResponse>,
        ),
        AnnotatorError,
    > {
        let mut retried = false;
        loop {
            let (req_tx, mut req_rx) = mpsc::channel::<proto::AnnotateRequest>(REQUEST_QUEUE);
            let requests = futures_util::stream::poll_fn(move |cx| req_rx.poll_recv(cx));
            let mut client = AudioAnnotatorClient::new(self.channel.clone());
            let call = client.annotate(tonic::Request::new(requests));
            match tokio::time::timeout(OPEN_TIMEOUT, call).await {
                Ok(Ok(resp)) => return Ok((req_tx, resp.into_inner())),
                Ok(Err(status)) if status.code() == Code::Unavailable && !retried => {
                    debug!(annotator = %self.name, %status, "Annotate open unavailable; retrying once");
                    retried = true;
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                Ok(Err(status)) => {
                    return Err(self.err(format!("gRPC Annotate open failed: {status}")));
                }
                Err(_elapsed) => {
                    return Err(self.err(format!(
                        "gRPC Annotate open timed out after {} ms (no response headers)",
                        OPEN_TIMEOUT.as_millis()
                    )));
                }
            }
        }
    }

    fn stream_request(session_id: &str, cmd: &AnnotatorCommand) -> proto::AnnotateRequest {
        let base = proto::AnnotateRequest {
            session_id: session_id.to_string(),
            ..proto::AnnotateRequest::default()
        };
        match *cmd {
            AnnotatorCommand::Segment(ref seg) => proto::AnnotateRequest {
                utterance_id: seg.utterance_id,
                start_ms: seg.start_ms,
                audio: samples_to_le_bytes(&seg.samples),
                sample_rate: seg.sample_rate,
                channels: u32::from(seg.channels.max(1)),
                end_of_utterance: seg.end_of_utterance,
                ..base
            },
            AnnotatorCommand::Cancel { utterance_id } => proto::AnnotateRequest {
                utterance_id,
                cancel: true,
                ..base
            },
            AnnotatorCommand::Warmup {
                sample_rate,
                channels,
            } => proto::AnnotateRequest {
                utterance_id: WARMUP_UTTERANCE_ID,
                audio: Self::warmup_audio(sample_rate, channels),
                sample_rate,
                channels: u32::from(channels.max(1)),
                warmup: true,
                ..base
            },
        }
    }

    fn map_response(&self, resp: proto::AnnotateResponse) -> Option<AudioAnnotation> {
        let extras = self.extras(&resp.extras_json)?;
        let kind = if resp.kind.is_empty() {
            self.kind.clone()
        } else {
            resp.kind
        };
        Some(AudioAnnotation {
            utterance_id: resp.utterance_id,
            start_ms: resp.start_ms,
            duration_ms: resp.duration_ms,
            kind,
            extras,
        })
    }

    async fn run_stream(
        &self,
        session_id: &str,
        mut commands: mpsc::Receiver<AnnotatorCommand>,
        out: mpsc::Sender<AudioAnnotation>,
    ) -> Result<(), AnnotatorError> {
        let (req_tx, mut inbound) = self.open_stream().await?;
        let mut first = true;
        loop {
            tokio::select! {
                cmd = commands.recv() => {
                    // Hub closed the command channel: session end. Dropping
                    // `req_tx` ends the request stream on the wire.
                    let Some(cmd) = cmd else { return Ok(()) };
                    let mut req = Self::stream_request(session_id, &cmd);
                    if first {
                        req.options.clone_from(&self.options);
                        first = false;
                    }
                    if req_tx.send(req).await.is_err() {
                        return Err(self.err("Annotate request stream closed"));
                    }
                }
                msg = inbound.message() => match msg {
                    Ok(Some(resp)) => {
                        if let Some(ann) = self.map_response(resp)
                            && out.send(ann).await.is_err()
                        {
                            // Hub gone: nothing left to deliver to.
                            return Ok(());
                        }
                    }
                    Ok(None) => return Err(self.err("service closed the Annotate stream")),
                    Err(status) => {
                        return Err(self.err(format!("gRPC Annotate stream failed: {status}")));
                    }
                },
            }
        }
    }

    // ── Unary fallback ──────────────────────────────────────

    /// One `Process` call. `Ok(None)` on timeout (the batch is dropped, the
    /// hub counts it) or empty/invalid extras; `Err` on a gRPC status so the
    /// hub's breaker takes over.
    async fn process(
        &self,
        client: &mut AudioPostProcessorClient<AuthedChannel>,
        request: proto::ProcessRequest,
        timeout: Duration,
    ) -> Result<Option<serde_json::Value>, AnnotatorError> {
        let call = client.process(tonic::Request::new(request));
        match tokio::time::timeout(timeout, call).await {
            Ok(Ok(resp)) => Ok(self.extras(&resp.into_inner().extras_json)),
            Ok(Err(status)) => Err(self.err(format!("gRPC Process failed: {status}"))),
            Err(_elapsed) => {
                warn!(
                    annotator = %self.name,
                    timeout_ms = timeout.as_millis(),
                    "gRPC Process timed out; segment dropped"
                );
                Ok(None)
            }
        }
    }

    fn unary_request(&self, seg: &AudioSegment) -> proto::ProcessRequest {
        proto::ProcessRequest {
            audio: samples_to_le_bytes(&seg.samples),
            sample_rate: seg.sample_rate,
            channels: u32::from(seg.channels.max(1)),
            utterance_id: seg.utterance_id,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            end_of_utterance: seg.end_of_utterance,
            options: self.options.clone(),
        }
    }

    async fn run_unary(
        &self,
        mut commands: mpsc::Receiver<AnnotatorCommand>,
        out: mpsc::Sender<AudioAnnotation>,
    ) -> Result<(), AnnotatorError> {
        let mut client = AudioPostProcessorClient::new(self.channel.clone());
        while let Some(cmd) = commands.recv().await {
            match cmd {
                AnnotatorCommand::Segment(seg) => {
                    // Same rule as the inline processor: an empty non-EOU
                    // segment carries nothing to process; EOU always calls
                    // through so the service can reset per-utterance state.
                    if seg.samples.is_empty() && !seg.end_of_utterance {
                        continue;
                    }
                    let request = self.unary_request(&seg);
                    let Some(extras) = self
                        .process(&mut client, request, self.unary_timeout)
                        .await?
                    else {
                        continue;
                    };
                    let annotation = AudioAnnotation {
                        utterance_id: seg.utterance_id,
                        start_ms: seg.start_ms,
                        duration_ms: seg.duration_ms(),
                        kind: self.kind.clone(),
                        extras,
                    };
                    if out.send(annotation).await.is_err() {
                        return Ok(());
                    }
                }
                // A unary service holds no queue to drain; the hub drops
                // late annotations for cancelled utterances itself.
                AnnotatorCommand::Cancel { .. } => {}
                AnnotatorCommand::Warmup {
                    sample_rate,
                    channels,
                } => {
                    let request = proto::ProcessRequest {
                        audio: Self::warmup_audio(sample_rate, channels),
                        sample_rate,
                        channels: u32::from(channels.max(1)),
                        utterance_id: WARMUP_UTTERANCE_ID,
                        seq: self.seq.fetch_add(1, Ordering::Relaxed),
                        end_of_utterance: false,
                        options: self.options.clone(),
                    };
                    // A cold model may legitimately exceed the per-segment
                    // budget here -- that is the point of warming it.
                    let _discarded = self.process(&mut client, request, OPEN_TIMEOUT).await?;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl AudioAnnotator for GrpcAnnotator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(
        &self,
        session_id: String,
        commands: mpsc::Receiver<AnnotatorCommand>,
        out: mpsc::Sender<AudioAnnotation>,
    ) -> Result<(), AnnotatorError> {
        match self.transport().await? {
            Transport::Stream => self.run_stream(&session_id, commands, out).await,
            Transport::Unary => self.run_unary(commands, out).await,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/annotate.rs"]
mod tests;
