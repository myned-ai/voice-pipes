//! Generic WebSocket media-stream session driver, raw PCM dialect, and
//! correlation-header sanitization.
//!
//! Bridges any [`MediaStreamDialect`] (built-in [`RawPcmDialect`] or external
//! plugins such as `voice-pipes-transport-twilio`) onto Voice Pipes' canonical
//! [`TransportSession`] trait so `run_session`, `AudioPacer`, `AudioLedger`,
//! and all pipeline plugins work unchanged with zero vendor code in core.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use axum::http::HeaderMap;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex as SyncMutex;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};
use voice_pipes_sdk::audio::AudioFrame;
use voice_pipes_sdk::audio::resample::resample;
use voice_pipes_sdk::media_stream::{
    DialectInbound, DialectStart, MediaStreamDialect, MediaStreamEncoding, MediaStreamFrame,
};

use crate::audio::g711;
use crate::pipeline::AudioChunk;

/// Canonical internal pipeline sample rate (16 kHz mono PCM).
const TARGET_SAMPLE_RATE: u32 = 16_000;
use crate::transport::error::TransportError;
use crate::transport::health::ConnectionHealthMonitor;
use crate::transport::session::TransportSession;
use crate::transport::{
    AudioFormat, ClientControl, ClientMessage, ConnectionQuality, MessageRole, PipelineMessage,
    ServerControl, ServerMessage, SessionConfig, SessionMetadata, TimingEventKind, TransportType,
};

pub use voice_pipes_sdk::media_stream::register_media_stream_factory;

/// Maximum time a carrier WebSocket may wait before sending an authenticated
/// `Start` handshake frame.
pub const PRE_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of non-`Start` frames allowed before `Start` arrives.
pub const MAX_PRE_START_FRAMES: usize = 3;

/// Maximum permitted length for a sanitized correlation identifier.
const MAX_CORRELATION_ID_LEN: usize = 128;

// ── Correlation Header Sanitization ─────────────────────────

/// Validate and sanitize an external correlation identifier (e.g., from an
/// allowlisted HTTP upgrade header or W3C `traceparent`).
///
/// Permits only `1..=128` ASCII characters in `[A-Za-z0-9._-]` to prevent
/// log/span injection and high-cardinality abuse.
pub fn sanitize_correlation_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_CORRELATION_ID_LEN {
        return None;
    }
    trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        .then(|| trimmed.to_string())
}

/// Extract a sanitized external correlation ID from HTTP upgrade headers.
///
/// Checks each header name in `allowlist` (case-insensitive) in order, then
/// falls back to the standard W3C `traceparent` header.
pub fn extract_correlation_id(headers: &HeaderMap, allowlist: &[String]) -> Option<String> {
    for name in allowlist {
        let lower = name.trim().to_ascii_lowercase();
        if lower.is_empty() {
            continue;
        }
        if let Some(val) = headers.get(lower.as_str()).and_then(|v| v.to_str().ok())
            && let Some(clean) = sanitize_correlation_id(val)
        {
            return Some(clean);
        }
    }
    if let Some(tp) = headers.get("traceparent").and_then(|v| v.to_str().ok())
        && let Some(clean) = sanitize_correlation_id(tp)
    {
        return Some(clean);
    }
    None
}

// ── Built-in Generic Raw PCM / G.711 Dialect ────────────────

/// Handshake-free binary audio WebSocket dialect (`/ws/raw`).
///
/// Streams raw binary audio frames in the negotiated [`MediaStreamEncoding`]
/// (`pcm16_le`, `mulaw`, or `alaw`) with zero JSON envelope overhead. Usable
/// by evaluation harnesses, SIP/PBX bridges, and simple embedded or mobile
/// clients.
#[derive(Debug, Clone)]
pub struct RawPcmDialect {
    stream_id: String,
    sample_rate: u32,
    encoding: MediaStreamEncoding,
    external_id: Option<String>,
    tenant_id: Option<String>,
}

impl RawPcmDialect {
    /// Construct a new [`RawPcmDialect`] for an incoming `/ws/raw` connection.
    pub fn new(
        stream_id: impl Into<String>,
        sample_rate: u32,
        encoding: MediaStreamEncoding,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            sample_rate,
            encoding,
            external_id: None,
            tenant_id: None,
        }
    }

    /// Attach an optional sanitized external correlation ID.
    #[must_use]
    pub fn with_external_id(mut self, external_id: Option<String>) -> Self {
        self.external_id = external_id;
        self
    }

    /// Attach an optional tenant identifier.
    #[must_use]
    pub fn with_tenant_id(mut self, tenant_id: Option<String>) -> Self {
        self.tenant_id = tenant_id;
        self
    }
}

impl MediaStreamDialect for RawPcmDialect {
    fn name(&self) -> &'static str {
        "raw"
    }

    fn expects_start_frame(&self) -> bool {
        false
    }

    fn initial_start(&self) -> Option<DialectStart> {
        Some(
            DialectStart::new(self.stream_id.clone(), self.sample_rate, self.encoding)
                .with_external_id(self.external_id.clone())
                .with_tenant_id(self.tenant_id.clone()),
        )
    }

    fn decode_frame(
        &mut self,
        frame: MediaStreamFrame,
    ) -> Result<DialectInbound, voice_pipes_sdk::media_stream::DialectError> {
        match frame {
            MediaStreamFrame::Binary(bytes) if !bytes.is_empty() => {
                Ok(DialectInbound::Audio(bytes))
            }
            _ => Ok(DialectInbound::Ignore),
        }
    }

    fn encode_audio(
        &self,
        payload: &[u8],
    ) -> Result<Option<MediaStreamFrame>, voice_pipes_sdk::media_stream::DialectError> {
        if payload.is_empty() {
            return Ok(None);
        }
        Ok(Some(MediaStreamFrame::Binary(payload.to_vec())))
    }

    fn encode_clear(
        &self,
    ) -> Result<Option<MediaStreamFrame>, voice_pipes_sdk::media_stream::DialectError> {
        Ok(None)
    }

    fn encode_mark(
        &self,
        _name: &str,
    ) -> Result<Option<MediaStreamFrame>, voice_pipes_sdk::media_stream::DialectError> {
        Ok(None)
    }
}

// ── Outbound 20 ms Frame Chunker ────────────────────────────

/// Buffers encoded outbound audio bytes and yields exact fixed-size frames
/// (e.g., 160 bytes for 20 ms of 8 kHz G.711 µ-law).
#[derive(Debug)]
pub struct OutboundChunker {
    frame_bytes: usize,
    pending: Vec<u8>,
}

impl OutboundChunker {
    /// Create a chunker for the given wire sample rate, encoding, and frame duration.
    pub fn new(sample_rate: u32, encoding: MediaStreamEncoding, frame_duration_ms: u32) -> Self {
        let samples_per_frame =
            ((u64::from(sample_rate) * u64::from(frame_duration_ms)) / 1000).max(1) as usize;
        let frame_bytes = samples_per_frame.saturating_mul(encoding.bytes_per_sample());
        Self {
            frame_bytes: frame_bytes.max(1),
            pending: Vec::with_capacity(frame_bytes * 2),
        }
    }

    /// Push newly encoded bytes and drain all complete fixed-size frames.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while self.pending.len() >= self.frame_bytes {
            let frame: Vec<u8> = self.pending.drain(..self.frame_bytes).collect();
            frames.push(frame);
        }
        frames
    }

    /// Discard any buffered partial frame (called immediately on barge-in `ClearAudio`).
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    /// Configured frame size in bytes.
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }
}

// ── Playback Mark Tracker ───────────────────────────────────

#[derive(Debug, Clone)]
struct PendingMark {
    name: String,
    utterance_id: u32,
    offset_ms: u32,
}

/// Tracks outbound `mark` events and resolves actual played audio offset
/// (`played_offset_ms`) when the carrier echoes them back.
#[derive(Debug, Default)]
pub struct MarkTracker {
    next_seq: u64,
    queued_audio_ms: u32,
    played_offset_ms: u32,
    current_utterance_id: u32,
    pending: VecDeque<PendingMark>,
}

impl MarkTracker {
    /// Record `duration_ms` of audio sent for `utterance_id` and return a new
    /// mark name if a mark should be emitted.
    pub fn on_audio_sent(&mut self, utterance_id: u32, duration_ms: u32) -> String {
        if utterance_id != self.current_utterance_id {
            self.current_utterance_id = utterance_id;
            self.queued_audio_ms = 0;
            self.played_offset_ms = 0;
        }
        self.queued_audio_ms = self.queued_audio_ms.saturating_add(duration_ms);
        self.next_seq = self.next_seq.saturating_add(1);
        let name = format!("vp-u{utterance_id}-m{}", self.next_seq);
        self.pending.push_back(PendingMark {
            name: name.clone(),
            utterance_id,
            offset_ms: self.queued_audio_ms,
        });
        name
    }

    /// Acknowledge an echoed `mark` from the carrier, advancing `played_offset_ms`.
    /// Returns `(utterance_id, offset_ms)` if the mark matched an in-flight entry.
    pub fn on_mark_ack(&mut self, name: &str) -> Option<(u32, u32)> {
        while let Some(front) = self.pending.pop_front() {
            if front.name == name {
                self.played_offset_ms = front.offset_ms;
                return Some((front.utterance_id, front.offset_ms));
            }
        }
        None
    }

    /// Clear all pending marks on barge-in (`ClearAudio`).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.queued_audio_ms = 0;
    }

    /// Last confirmed played offset in milliseconds for the current utterance.
    pub fn played_offset_ms(&self) -> u32 {
        self.played_offset_ms
    }
}

// ── Audio Transcoding Helpers ───────────────────────────────

fn decode_wire_audio(
    bytes: &[u8],
    encoding: MediaStreamEncoding,
    wire_rate: u32,
) -> Result<Vec<i16>, TransportError> {
    let pcm = match encoding {
        MediaStreamEncoding::Mulaw => g711::mulaw_to_pcm16(bytes),
        MediaStreamEncoding::Alaw => g711::alaw_to_pcm16(bytes),
        MediaStreamEncoding::Pcm16Le => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect(),
        _ => {
            return Err(TransportError::Codec(
                "unsupported inbound media stream encoding".to_string(),
            ));
        }
    };

    if wire_rate == TARGET_SAMPLE_RATE || pcm.is_empty() {
        return Ok(pcm);
    }

    let frame = AudioFrame {
        samples: pcm,
        sample_rate: wire_rate,
        channels: 1,
    };
    resample(frame, TARGET_SAMPLE_RATE)
        .map(|f| f.samples)
        .map_err(|e| TransportError::Codec(format!("inbound resample failed: {e}")))
}

fn encode_wire_audio(
    samples: &[i16],
    source_rate: u32,
    wire_rate: u32,
    encoding: MediaStreamEncoding,
) -> Result<Vec<u8>, TransportError> {
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let resampled = if source_rate == wire_rate {
        samples.to_vec()
    } else {
        let frame = AudioFrame {
            samples: samples.to_vec(),
            sample_rate: source_rate,
            channels: 1,
        };
        resample(frame, wire_rate)
            .map(|f| f.samples)
            .map_err(|e| TransportError::Codec(format!("outbound resample failed: {e}")))?
    };

    match encoding {
        MediaStreamEncoding::Mulaw => Ok(g711::pcm16_to_mulaw(&resampled)),
        MediaStreamEncoding::Alaw => Ok(g711::pcm16_to_alaw(&resampled)),
        MediaStreamEncoding::Pcm16Le => {
            let mut out = Vec::with_capacity(resampled.len() * 2);
            for s in resampled {
                out.extend_from_slice(&s.to_le_bytes());
            }
            Ok(out)
        }
        _ => Err(TransportError::Codec(
            "unsupported outbound media stream encoding".to_string(),
        )),
    }
}

fn to_axum_message(frame: MediaStreamFrame) -> Message {
    match frame {
        MediaStreamFrame::Text(t) => Message::Text(t.into()),
        MediaStreamFrame::Binary(b) => Message::Binary(b.into()),
        _ => Message::Binary(Vec::new().into()),
    }
}

// ── MediaStreamSession (TransportSession) ───────────────────

struct SharedStreamState {
    dialect: Box<dyn MediaStreamDialect>,
    wire_rate: u32,
    encoding: MediaStreamEncoding,
    chunker: OutboundChunker,
    marks: MarkTracker,
    started: bool,
    last_utterance_id: u32,
}

/// Generic [`TransportSession`] that adapts any [`MediaStreamDialect`] onto
/// Voice Pipes' canonical pipeline orchestration.
pub struct MediaStreamSession {
    ws_tx: Arc<Mutex<SplitSink<WebSocket, Message>>>,
    decoded_rx: Arc<Mutex<mpsc::Receiver<Result<ClientMessage, TransportError>>>>,
    shared: Arc<SyncMutex<SharedStreamState>>,
    health: Arc<ConnectionHealthMonitor>,
    metadata: SessionMetadata,
    closed: AtomicBool,
    negotiated_input_rate: Arc<AtomicU32>,
    write_timeout: Duration,
    read_task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for MediaStreamSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaStreamSession")
            .field("session_id", &self.metadata.session_id)
            .field("external_id", &self.metadata.external_id)
            .finish_non_exhaustive()
    }
}

impl MediaStreamSession {
    /// Wrap an upgraded Axum WebSocket in a [`MediaStreamSession`] driven by `dialect`.
    pub fn new(
        socket: WebSocket,
        dialect: Box<dyn MediaStreamDialect>,
        session_id: String,
        remote_addr: Option<SocketAddr>,
        health: Arc<ConnectionHealthMonitor>,
        write_timeout: Duration,
    ) -> Self {
        let (ws_sink, ws_stream) = socket.split();
        let ws_tx = Arc::new(Mutex::new(ws_sink));
        let (decoded_tx, decoded_rx) = mpsc::channel(128);
        let negotiated_input_rate = Arc::new(AtomicU32::new(TARGET_SAMPLE_RATE));

        let initial_start = dialect.initial_start();
        let expects_start = dialect.expects_start_frame();
        let frame_duration_ms = dialect.frame_duration_ms();
        let dialect_name = dialect.name().to_string();

        let (init_rate, init_enc, started) = match &initial_start {
            Some(s) if !expects_start => (s.sample_rate, s.encoding, true),
            _ => (8_000, MediaStreamEncoding::Mulaw, false),
        };

        let shared = Arc::new(SyncMutex::new(SharedStreamState {
            dialect,
            wire_rate: init_rate,
            encoding: init_enc,
            chunker: OutboundChunker::new(init_rate, init_enc, frame_duration_ms),
            marks: MarkTracker::default(),
            started,
            last_utterance_id: 0,
        }));

        let read_task = tokio::spawn(run_media_stream_read_loop(
            ws_stream,
            decoded_tx,
            Arc::clone(&shared),
            Arc::clone(&negotiated_input_rate),
            session_id.clone(),
            dialect_name,
            initial_start,
            expects_start,
        ));

        Self {
            ws_tx,
            decoded_rx: Arc::new(Mutex::new(decoded_rx)),
            shared,
            health,
            metadata: SessionMetadata {
                session_id,
                tenant_id: None,
                transport_type: TransportType::MediaStream,
                remote_addr,
                connected_at: Instant::now(),
                quality: ConnectionQuality::Excellent,
                rtt_ms: None,
                auth_identity: None,
                external_id: None,
            },
            closed: AtomicBool::new(false),
            negotiated_input_rate,
            write_timeout,
            read_task,
        }
    }

    /// Attach an optional tenant identifier to this session.
    #[must_use]
    pub fn with_tenant(mut self, tenant_id: Option<voice_pipes_sdk::TenantId>) -> Self {
        self.metadata.tenant_id = tenant_id;
        self
    }

    /// Attach an optional sanitized external correlation ID to this session.
    #[must_use]
    pub fn with_external_id(mut self, external_id: Option<String>) -> Self {
        self.metadata.external_id = external_id;
        self
    }

    /// Attach an authenticated identity to this session.
    #[must_use]
    pub fn with_auth_identity(mut self, identity: Option<String>) -> Self {
        self.metadata.auth_identity = identity;
        self
    }

    async fn send_ws_frame(&self, frame: MediaStreamFrame) -> Result<(), TransportError> {
        let mut sink = self.ws_tx.lock().await;
        match tokio::time::timeout(self.write_timeout, sink.send(to_axum_message(frame))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(TransportError::ConnectionLost(format!(
                "media stream write failed: {e}"
            ))),
            Err(_) => Err(TransportError::ConnectionLost(format!(
                "media stream write timed out after {} ms",
                self.write_timeout.as_millis()
            ))),
        }
    }
}

fn build_synthetic_configure(start: &DialectStart) -> ClientMessage {
    let cfg = SessionConfig {
        audio_format: AudioFormat::PcmI16Le,
        sample_rate: TARGET_SAMPLE_RATE,
        output_sample_rate: Some(start.sample_rate),
        channels: 1,
        ..SessionConfig::default()
    };
    ClientMessage::Control(ClientControl::Configure(cfg))
}

async fn run_media_stream_read_loop(
    mut ws_stream: futures_util::stream::SplitStream<WebSocket>,
    decoded_tx: mpsc::Sender<Result<ClientMessage, TransportError>>,
    shared: Arc<SyncMutex<SharedStreamState>>,
    negotiated_rate: Arc<AtomicU32>,
    session_id: String,
    dialect_name: String,
    initial_start: Option<DialectStart>,
    expects_start: bool,
) {
    if let Some(start) = initial_start
        && !expects_start
    {
        let auth_res = shared.lock().dialect.verify_start_auth(&start);
        if let Err(err) = auth_res {
            let _ = decoded_tx
                .send(Err(TransportError::AuthFailed(err.to_string())))
                .await;
            return;
        }
        negotiated_rate.store(TARGET_SAMPLE_RATE, Ordering::Relaxed);
        if decoded_tx
            .send(Ok(build_synthetic_configure(&start)))
            .await
            .is_err()
        {
            return;
        }
    }

    let mut pre_start_frames: usize = 0;
    let pre_start_deadline = tokio::time::Instant::now() + PRE_START_TIMEOUT;

    loop {
        let is_started = shared.lock().started;
        let next_msg = if is_started {
            ws_stream.next().await
        } else if let Ok(item) = tokio::time::timeout_at(pre_start_deadline, ws_stream.next()).await
        {
            item
        } else {
            warn!(session_id = %session_id, dialect = %dialect_name, "pre-start handshake timed out");
            let _ = decoded_tx
                .send(Err(TransportError::AuthFailed(
                    "media stream start handshake timed out".to_string(),
                )))
                .await;
            return;
        };

        let Some(frame_res) = next_msg else { break };
        let msg = match frame_res {
            Ok(m) => m,
            Err(e) => {
                let _ = decoded_tx
                    .send(Err(TransportError::ConnectionLost(format!(
                        "media stream read error: {e}"
                    ))))
                    .await;
                return;
            }
        };

        let wire_frame = match msg {
            Message::Text(t) => MediaStreamFrame::Text(t.to_string()),
            Message::Binary(b) => MediaStreamFrame::Binary(b.to_vec()),
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
        };

        let decoded = shared.lock().dialect.decode_frame(wire_frame);
        let event = match decoded {
            Ok(ev) => ev,
            Err(e) => {
                let _ = decoded_tx
                    .send(Err(TransportError::Codec(e.to_string())))
                    .await;
                return;
            }
        };

        if !handle_inbound_event(
            event,
            is_started,
            &mut pre_start_frames,
            &shared,
            &negotiated_rate,
            &decoded_tx,
            &session_id,
            &dialect_name,
        )
        .await
        {
            break;
        }
    }
}

async fn handle_inbound_event(
    event: DialectInbound,
    is_started: bool,
    pre_start_frames: &mut usize,
    shared: &Arc<SyncMutex<SharedStreamState>>,
    negotiated_rate: &Arc<AtomicU32>,
    decoded_tx: &mpsc::Sender<Result<ClientMessage, TransportError>>,
    session_id: &str,
    dialect_name: &str,
) -> bool {
    match event {
        DialectInbound::Start(start) => {
            let auth_res = {
                let mut st = shared.lock();
                let auth = st.dialect.verify_start_auth(&start);
                if auth.is_ok() {
                    st.wire_rate = start.sample_rate;
                    st.encoding = start.encoding;
                    st.chunker = OutboundChunker::new(
                        start.sample_rate,
                        start.encoding,
                        st.dialect.frame_duration_ms(),
                    );
                    st.started = true;
                }
                auth
            };
            if let Err(err) = auth_res {
                warn!(session_id = %session_id, dialect = %dialect_name, "media stream start auth rejected");
                let _ = decoded_tx
                    .send(Err(TransportError::AuthFailed(err.to_string())))
                    .await;
                return false;
            }
            negotiated_rate.store(TARGET_SAMPLE_RATE, Ordering::Relaxed);
            info!(session_id = %session_id, dialect = %dialect_name, wire_rate = start.sample_rate, "media stream started");
            decoded_tx
                .send(Ok(build_synthetic_configure(&start)))
                .await
                .is_ok()
        }
        DialectInbound::Audio(payload) => {
            handle_inbound_audio(payload, pre_start_frames, shared, decoded_tx).await
        }
        DialectInbound::MarkAck(name) => {
            let ack = shared.lock().marks.on_mark_ack(&name);
            if let Some((utterance_id, offset_ms)) = ack {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                let report = ClientMessage::Control(ClientControl::TimingReport {
                    utterance_id,
                    event: TimingEventKind::Other("playback_mark".to_string()),
                    client_timestamp_ms: now_ms,
                    detail: Some(
                        serde_json::json!({ "played_offset_ms": offset_ms, "mark": name }),
                    ),
                });
                let _ = decoded_tx.send(Ok(report)).await;
            }
            true
        }
        DialectInbound::Dtmf(digit) => {
            debug!(session_id = %session_id, "dtmf event received");
            let msg = ClientMessage::Message(PipelineMessage {
                kind: "dtmf".to_string(),
                text: Some(digit),
                payload: None,
                binary: None,
                role: MessageRole::User,
                turn_id: None,
                message_id: uuid::Uuid::new_v4().to_string(),
            });
            decoded_tx.send(Ok(msg)).await.is_ok()
        }
        DialectInbound::Stop => {
            debug!(session_id = %session_id, "media stream stop received");
            false
        }
        DialectInbound::Ignore => {
            if !is_started {
                *pre_start_frames += 1;
                if *pre_start_frames > MAX_PRE_START_FRAMES {
                    let _ = decoded_tx
                        .send(Err(TransportError::AuthFailed(
                            "too many frames before start handshake".to_string(),
                        )))
                        .await;
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

async fn handle_inbound_audio(
    payload: Vec<u8>,
    pre_start_frames: &mut usize,
    shared: &Arc<SyncMutex<SharedStreamState>>,
    decoded_tx: &mpsc::Sender<Result<ClientMessage, TransportError>>,
) -> bool {
    let (started, wire_rate, encoding) = {
        let st = shared.lock();
        (st.started, st.wire_rate, st.encoding)
    };
    if !started {
        *pre_start_frames += 1;
        if *pre_start_frames > MAX_PRE_START_FRAMES {
            let _ = decoded_tx
                .send(Err(TransportError::AuthFailed(
                    "too many frames before start handshake".to_string(),
                )))
                .await;
            return false;
        }
        return true;
    }
    match decode_wire_audio(&payload, encoding, wire_rate) {
        Ok(samples) if !samples.is_empty() => {
            let chunk = AudioChunk::from_frame(
                AudioFrame {
                    samples,
                    sample_rate: TARGET_SAMPLE_RATE,
                    channels: 1,
                },
                false,
            );
            decoded_tx
                .send(Ok(ClientMessage::Audio(chunk)))
                .await
                .is_ok()
        }
        Ok(_) => true,
        Err(e) => {
            let _ = decoded_tx.send(Err(e)).await;
            false
        }
    }
}

#[async_trait]
impl TransportSession for MediaStreamSession {
    async fn recv(&mut self) -> Result<Option<ClientMessage>, TransportError> {
        let mut rx = self.decoded_rx.lock().await;
        match rx.recv().await {
            Some(Ok(msg)) => Ok(Some(msg)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    async fn send(&self, msg: ServerMessage) -> Result<(), TransportError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(TransportError::ConnectionClosed);
        }

        let outbound_frames: Vec<MediaStreamFrame> = match msg {
            ServerMessage::Audio(chunk) => {
                let (wire_rate, encoding, frame_ms) = {
                    let st = self.shared.lock();
                    if !st.started {
                        return Ok(());
                    }
                    (st.wire_rate, st.encoding, st.dialect.frame_duration_ms())
                };

                let encoded =
                    encode_wire_audio(&chunk.samples, chunk.sample_rate, wire_rate, encoding)?;

                let mut st = self.shared.lock();
                st.last_utterance_id = chunk.utterance_id;
                let fixed_chunks = st.chunker.push(&encoded);
                let mut frames = Vec::with_capacity(fixed_chunks.len() + 1);
                let mut sent_ms: u32 = 0;

                for slice in fixed_chunks {
                    if let Some(ws_frame) = st
                        .dialect
                        .encode_audio(&slice)
                        .map_err(|e| TransportError::Codec(e.to_string()))?
                    {
                        frames.push(ws_frame);
                        sent_ms = sent_ms.saturating_add(frame_ms);
                    }
                }

                if (chunk.is_end_of_utterance || sent_ms >= 200) && sent_ms > 0 {
                    let mark_name = st.marks.on_audio_sent(chunk.utterance_id, sent_ms);
                    if let Some(mark_frame) = st
                        .dialect
                        .encode_mark(&mark_name)
                        .map_err(|e| TransportError::Codec(e.to_string()))?
                    {
                        frames.push(mark_frame);
                    }
                }
                frames
            }
            ServerMessage::Control(ServerControl::ClearAudio { .. }) => {
                let mut st = self.shared.lock();
                st.chunker.clear();
                st.marks.clear();
                match st
                    .dialect
                    .encode_clear()
                    .map_err(|e| TransportError::Codec(e.to_string()))?
                {
                    Some(frame) => vec![frame],
                    None => Vec::new(),
                }
            }
            ServerMessage::Control(_)
            | ServerMessage::Message(_)
            | ServerMessage::Annotation(_) => Vec::new(),
        };

        for frame in outbound_frames {
            self.send_ws_frame(frame).await?;
        }
        Ok(())
    }

    async fn close(&self, _reason: &str) -> Result<(), TransportError> {
        self.closed.store(true, Ordering::Relaxed);
        let mut sink = self.ws_tx.lock().await;
        let _ = tokio::time::timeout(self.write_timeout, sink.send(Message::Close(None))).await;
        Ok(())
    }

    fn quality(&self) -> ConnectionQuality {
        self.health.quality()
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn negotiated_input_rate(&self) -> Option<Arc<AtomicU32>> {
        Some(Arc::clone(&self.negotiated_input_rate))
    }

    fn health_monitor(&self) -> Option<Arc<ConnectionHealthMonitor>> {
        Some(Arc::clone(&self.health))
    }
}

impl Drop for MediaStreamSession {
    fn drop(&mut self) {
        self.read_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    #[test]
    fn sanitize_correlation_id_accepts_valid_and_rejects_injection() {
        assert_eq!(
            sanitize_correlation_id("sim_2026-11-24.abc123"),
            Some("sim_2026-11-24.abc123".to_string())
        );
        // Reject newlines, spaces, quotes, control chars, empty, or >128 chars.
        assert_eq!(sanitize_correlation_id(""), None);
        assert_eq!(sanitize_correlation_id("   "), None);
        assert_eq!(sanitize_correlation_id("sim\nid"), None);
        assert_eq!(sanitize_correlation_id("sim id"), None);
        assert_eq!(sanitize_correlation_id("sim\"id"), None);
        assert_eq!(sanitize_correlation_id(&"a".repeat(129)), None);
        assert_eq!(
            sanitize_correlation_id(&"a".repeat(128)),
            Some("a".repeat(128))
        );
    }

    #[test]
    fn extract_correlation_id_uses_allowlist_and_traceparent_fallback() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-correlation-id"),
            HeaderValue::from_static("sim-42"),
        );
        headers.insert(
            HeaderName::from_static("traceparent"),
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );

        let allowlist = vec!["x-correlation-id".to_string()];
        assert_eq!(
            extract_correlation_id(&headers, &allowlist),
            Some("sim-42".to_string())
        );

        // Without allowlist, falls back to standard W3C traceparent.
        assert_eq!(
            extract_correlation_id(&headers, &[]),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string())
        );
    }

    #[test]
    fn outbound_chunker_emits_exact_20ms_mulaw_frames_and_clears_on_barge_in() {
        let mut chunker = OutboundChunker::new(8_000, MediaStreamEncoding::Mulaw, 20);
        assert_eq!(chunker.frame_bytes(), 160);

        let frames = chunker.push(&[0xFF; 350]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), 160);
        assert_eq!(frames[1].len(), 160);

        // 30 bytes remain; barge-in clear must drop them.
        chunker.clear();
        let next = chunker.push(&[0x7F; 160]);
        assert_eq!(next.len(), 1);
        assert!(next[0].iter().all(|&b| b == 0x7F));
    }

    #[test]
    fn mark_tracker_advances_played_offset_and_resets_on_clear() {
        let mut tracker = MarkTracker::default();
        let m1 = tracker.on_audio_sent(1, 200);
        let m2 = tracker.on_audio_sent(1, 200);
        assert_eq!(tracker.played_offset_ms(), 0);

        assert_eq!(tracker.on_mark_ack(&m1), Some((1, 200)));
        assert_eq!(tracker.played_offset_ms(), 200);

        // Barge-in clears pending mark m2; late echo of m2 is ignored.
        tracker.clear();
        assert_eq!(tracker.on_mark_ack(&m2), None);
        assert_eq!(tracker.played_offset_ms(), 200);
    }

    #[test]
    fn raw_pcm_dialect_is_handshake_free_and_roundtrips_binary() {
        let mut dialect = RawPcmDialect::new("stream-1", 16_000, MediaStreamEncoding::Pcm16Le)
            .with_external_id(Some("sim-1".to_string()));
        assert!(!dialect.expects_start_frame());
        let start = dialect.initial_start().expect("initial start");
        assert_eq!(start.sample_rate, 16_000);
        assert_eq!(start.external_id.as_deref(), Some("sim-1"));

        let inbound = dialect
            .decode_frame(MediaStreamFrame::Binary(vec![1, 2, 3, 4]))
            .expect("decode");
        assert_eq!(inbound, DialectInbound::Audio(vec![1, 2, 3, 4]));

        let out = dialect
            .encode_audio(&[5, 6, 7, 8])
            .expect("encode")
            .expect("frame");
        assert_eq!(out, MediaStreamFrame::Binary(vec![5, 6, 7, 8]));
    }
}
