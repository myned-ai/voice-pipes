//! WebSocket transport implementation.
//!
//! Uses axum's built-in WebSocket support (`axum::extract::ws`).
//! Frames audio as binary (wire header + PCM payload) and
//! control/text/data as JSON text frames.

#![expect(
    clippy::map_err_ignore,
    reason = "transport sink errors carry no signal beyond peer-gone; mapped to ConnectionClosed"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

use super::error::TransportError;
use super::health::ConnectionHealthMonitor;
use super::session::TransportSession;
use super::{ClientMessage, ConnectionQuality, ServerMessage, SessionMetadata, TransportType};
use crate::transport::wire::JsonEnvelope;

mod codec;
pub use codec::{MAX_CLIENT_MESSAGE_BYTES, decode_client_message, encode_server_message};

use super::rate::AudioRateLimiter;

// ── WsSession ───────────────────────────────────────────────

/// WebSocket-backed transport session.
///
/// Wraps an axum [`WebSocket`] with health monitoring and
/// binary/text frame routing.
#[derive(Debug)]
pub struct WsSession {
    /// Sender half -- used by `send()`.
    ws_tx: mpsc::Sender<Message>,
    /// Receiver half -- used by `recv()`.
    ws_rx: Arc<Mutex<mpsc::Receiver<DecodedClientMessage>>>,
    /// Connection health monitor.
    health: Arc<ConnectionHealthMonitor>,
    /// Session metadata.
    metadata: SessionMetadata,
    /// Whether the session has been closed.
    closed: std::sync::atomic::AtomicBool,
    /// Per-session monotonic counter for outbound binary frame sequencing.
    outbound_seq: AtomicU32,
    /// Last utterance ID for which we emitted an `audio_meta` event.
    last_utterance_sent: std::sync::atomic::AtomicU32,
    /// The session's negotiated input sample rate (0 until the handshake
    /// completes), shared with the read pump so the inbound audio rate
    /// limiter is sized for the client's actual stream — the
    /// codec stamps every chunk with `DEFAULT_SAMPLE_RATE`, which would
    /// give a 16 kHz session a 3× too-loose budget.
    negotiated_input_rate: Arc<AtomicU32>,
    /// The read pump, so `close()` / `Drop` can stop it instead of
    /// leaving it parked on `ws_stream.next()` (and the socket open) until
    /// the peer's TCP stack gives up.
    read_task: tokio::task::JoinHandle<()>,
}

impl Drop for WsSession {
    fn drop(&mut self) {
        self.read_task.abort();
    }
}

/// An already-decoded client message (decoded in the read pump task).
#[derive(Debug)]
enum DecodedClientMessage {
    Message(ClientMessage),
    Close,
}

// ── Inbound audio rate limiting ───────────

/// Decision for an inbound audio chunk after rate-limit evaluation.
#[derive(Debug, PartialEq)]
enum RateDecision {
    /// Within budget: forward as-is.
    Admit,
    /// Over budget with no commit signal: drop the whole chunk.
    Drop,
    /// Over budget but carrying `is_end_of_utterance`: forward with the
    /// samples stripped so the turn still commits — dropping the flag
    /// left the transcript truncated *and* the turn hanging with no
    /// client-visible signal.
    ForwardEouOnly,
}

/// Evaluate one inbound audio chunk against the session's token bucket.
///
/// The limiter is created lazily and re-created when the effective rate
/// changes: it is sized from the *negotiated* input rate once the
/// handshake has published it (`negotiated_rate > 0`), falling back to
/// the codec's per-chunk stamp before that.
fn rate_limit_audio(
    limiter: &mut Option<AudioRateLimiter>,
    negotiated_rate: u32,
    chunk: &crate::pipeline::AudioChunk,
) -> RateDecision {
    if chunk.samples.is_empty() || chunk.sample_rate == 0 {
        // Zero-cost control chunks (e.g. a bare end-of-utterance marker)
        // are never dropped.
        return RateDecision::Admit;
    }
    let rate = if negotiated_rate > 0 {
        negotiated_rate
    } else {
        chunk.sample_rate
    };
    let rebuild = limiter.as_ref().is_none_or(|l| l.sample_rate() != rate);
    if rebuild {
        *limiter = Some(AudioRateLimiter::new(rate));
    }
    let limiter = limiter.as_mut().expect("limiter just created");
    if limiter.admit(chunk.samples.len()) {
        RateDecision::Admit
    } else if chunk.is_end_of_utterance {
        RateDecision::ForwardEouOnly
    } else {
        RateDecision::Drop
    }
}

// ── Heartbeat ──────────────────────────────────────

/// RTT computed from a pong echoing our timestamped server ping.
///
/// The ping payload is the session-epoch-relative send time in
/// milliseconds (8 bytes LE); RFC 6455 requires the peer to echo it.
/// Returns `None` for unsolicited/foreign pongs whose payload isn't ours.
fn rtt_from_pong(now_epoch_ms: u64, payload: &[u8]) -> Option<Duration> {
    let bytes: [u8; 8] = payload.try_into().ok()?;
    let sent_ms = u64::from_le_bytes(bytes);
    if sent_ms > now_epoch_ms {
        return None; // not a timestamp we produced
    }
    Some(Duration::from_millis(now_epoch_ms - sent_ms))
}

/// What the ping pump should do on this heartbeat tick.
#[derive(Debug, PartialEq)]
enum HeartbeatAction {
    /// Send the next timestamped ping.
    SendPing,
    /// Too many unanswered pings: the connection is half-open; close it.
    CloseLost,
}

/// Per-tick heartbeat policy: an unanswered previous ping counts as a
/// missed pong, and once the monitor declares the connection Lost
/// (`max_missed_pongs`) the socket must be closed instead of pinged again.
fn heartbeat_tick(health: &ConnectionHealthMonitor, awaiting_pong: &AtomicBool) -> HeartbeatAction {
    if awaiting_pong.load(Ordering::Acquire) {
        health.record_missed_pong();
        if health.quality() == ConnectionQuality::Lost {
            return HeartbeatAction::CloseLost;
        }
    }
    HeartbeatAction::SendPing
}

impl WsSession {
    /// Spawn a new WebSocket session from an axum `WebSocket`.
    ///
    /// This splits the socket into read/write halves, spawns a read
    /// pump task that decodes frames, and returns the session handle.
    pub fn new(
        ws: WebSocket,
        session_id: String,
        remote_addr: Option<SocketAddr>,
        health: ConnectionHealthMonitor,
        write_timeout: Duration,
    ) -> Self {
        use futures_util::StreamExt;

        let (ws_sink, mut ws_stream) = ws.split();

        let health = Arc::new(health);
        // Session-local epoch for heartbeat timestamps: ping
        // payloads carry "ms since epoch"; the echoed pong yields real RTT.
        let epoch = Instant::now();
        // True while a server ping is outstanding; cleared by the read pump
        // when the matching pong arrives.
        let awaiting_pong = Arc::new(AtomicBool::new(false));
        // Published by the runner after the handshake (see
        // `TransportSession::negotiated_input_rate`); 0 = not yet negotiated.
        let negotiated_input_rate = Arc::new(AtomicU32::new(0));

        // ── Read pump channel (created first: the write pump reports a
        // stalled peer through it) ─────────────────────
        let (decoded_tx, decoded_rx) = mpsc::channel::<DecodedClientMessage>(128);

        // ── Write pump: mpsc -> WebSocket ────────────────────
        let (ws_tx, outgoing_rx) = mpsc::channel::<Message>(128);
        tokio::spawn(write_pump(
            ws_sink,
            outgoing_rx,
            write_timeout,
            decoded_tx.clone(),
        ));

        // ── Read pump: WebSocket -> mpsc ─────────────────────
        let health_read = Arc::clone(&health);
        let awaiting_pong_read = Arc::clone(&awaiting_pong);
        let negotiated_rate_read = Arc::clone(&negotiated_input_rate);
        let decoded_tx_ping = decoded_tx.clone();
        let read_task = tokio::spawn(async move {
            // Token-bucket limiter on inbound audio. A client
            // streaming audio much faster than realtime (hostile or broken)
            // gets its excess chunks dropped here instead of driving STT
            // buffering without bound. Sized from the negotiated input rate
            // once the handshake publishes it.
            let mut audio_limiter: Option<AudioRateLimiter> = None;
            let mut dropped_audio_chunks: u64 = 0;
            // Undecodable frames cost a full parse each and never
            // enter the bounded channel, so a peer can spin this task at
            // socket speed. Log with power-of-two backoff and close after
            // a run of consecutive failures; any good frame resets the run.
            let mut decode_failures: u32 = 0;
            while let Some(result) = ws_stream.next().await {
                match result {
                    Ok(msg) => {
                        let is_close = matches!(msg, Message::Close(_));

                        if let Message::Pong(payload) = &msg {
                            // Any pong is evidence of liveness: clear
                            // the outstanding-ping flag and reset the missed
                            // count so a peer/middlebox that strips or rewrites
                            // the 8-byte payload isn't killed mid-stream. Only
                            // record a real RTT sample when the payload echoes
                            // one of our timestamps.
                            awaiting_pong_read.store(false, Ordering::Release);
                            let now_ms = epoch.elapsed().as_millis() as u64;
                            match rtt_from_pong(now_ms, payload) {
                                Some(rtt) => health_read.record_pong(rtt),
                                None => health_read.record_liveness(),
                            }
                            continue;
                        }
                        if matches!(msg, Message::Ping(_)) {
                            // Client-initiated keepalive; axum answers it
                            // automatically. Not evidence of *our* ping
                            // round-trip, so no RTT sample.
                            continue;
                        }

                        if is_close {
                            let _ = decoded_tx.send(DecodedClientMessage::Close).await;
                            break;
                        }

                        match decode_client_message(msg) {
                            Ok(Some(mut client_msg)) => {
                                decode_failures = 0;
                                if let ClientMessage::Audio(chunk) = &mut client_msg {
                                    let negotiated = negotiated_rate_read.load(Ordering::Relaxed);
                                    match rate_limit_audio(&mut audio_limiter, negotiated, chunk) {
                                        RateDecision::Admit => {}
                                        RateDecision::Drop | RateDecision::ForwardEouOnly => {
                                            dropped_audio_chunks += 1;
                                            // Log the first drop and then
                                            // exponentially rarer, so a
                                            // sustained firehose can't flood
                                            // the logs either.
                                            if dropped_audio_chunks.is_power_of_two() {
                                                warn!(
                                                    dropped_chunks = dropped_audio_chunks,
                                                    "inbound audio exceeds rate limit; dropping chunk(s)"
                                                );
                                            }
                                            if chunk.is_end_of_utterance {
                                                // The commit signal must
                                                // survive the drop — forward the
                                                // chunk with its samples stripped
                                                // (costs no tokens) so the turn
                                                // still commits.
                                                chunk.samples = Vec::new();
                                            } else {
                                                continue;
                                            }
                                        }
                                    }
                                }
                                if decoded_tx
                                    .send(DecodedClientMessage::Message(client_msg))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(None) => decode_failures = 0, // ping/pong/close already handled
                            Err(e) => {
                                decode_failures += 1;
                                let action = decode_failure_action(decode_failures);
                                if action.log {
                                    warn!(
                                        error = %e,
                                        consecutive = decode_failures,
                                        "failed to decode ws frame"
                                    );
                                }
                                if action.close {
                                    warn!(
                                        consecutive = decode_failures,
                                        "too many undecodable frames in a row; closing session"
                                    );
                                    let _ = decoded_tx.send(DecodedClientMessage::Close).await;
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "ws read error");
                        health_read.record_missed_pong();
                        // Tear the session down now. The ping pump still
                        // holds a `decoded_tx` clone, so simply breaking would
                        // leave `recv()` blocked for up to ~2 ping intervals
                        // until the ping pump notices the dead writer — long
                        // enough for connect/RST churn to exhaust `max_sessions`.
                        // Send an explicit Close so the runner tears down at once.
                        let _ = decoded_tx.send(DecodedClientMessage::Close).await;
                        break;
                    }
                }
            }
        });

        // ── Ping pump: server-initiated heartbeat ───
        // Sends a timestamped Ping every `ping_interval`; the read pump
        // computes real RTT from the echoed payload. An unanswered ping
        // counts as a missed pong, and at `max_missed_pongs` the socket is
        // closed and `recv()` unblocked so the runner tears the session
        // down (whose teardown calls `session_store.disconnect`). Without
        // this, a silently half-open TCP connection holds its session slot
        // until the OS timeout.
        let health_ping = Arc::clone(&health);
        let ws_tx_ping = ws_tx.clone();
        let awaiting_pong_ping = Arc::clone(&awaiting_pong);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(health_ping.ping_interval());
            // After an executor stall or process suspend, the default
            // `Burst` behaviour fires all missed ticks back-to-back, recording
            // consecutive "missed pongs" microseconds apart and killing a
            // healthy connection. `Delay` spaces recovery ticks by the interval.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                match heartbeat_tick(&health_ping, &awaiting_pong_ping) {
                    HeartbeatAction::CloseLost => {
                        warn!(
                            missed_pongs = health_ping.missed_pong_count(),
                            "connection unresponsive; closing half-open session"
                        );
                        let close = Message::Close(Some(axum::extract::ws::CloseFrame {
                            code: 1001,
                            reason: "heartbeat timeout".into(),
                        }));
                        // Never block on the outbound channel —
                        // if it is full the write pump is stalled and its
                        // own timeout will finish the job.
                        let _ = ws_tx_ping.try_send(close);
                        // The peer will never reply, so unblock `recv()`
                        // directly to trigger the runner's teardown.
                        let _ = decoded_tx_ping.send(DecodedClientMessage::Close).await;
                        break;
                    }
                    HeartbeatAction::SendPing => {
                        let payload = (epoch.elapsed().as_millis() as u64).to_le_bytes();
                        match ws_tx_ping.try_send(Message::Ping(payload.to_vec().into())) {
                            Ok(()) => awaiting_pong_ping.store(true, Ordering::Release),
                            // A full outbound channel means the
                            // peer is not draining what we send. Count it
                            // as a missed pong so a persistently stalled
                            // peer reaches `CloseLost` on the next ticks
                            // instead of parking this task forever.
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                warn!(
                                    "outbound channel full at heartbeat; counting as missed pong"
                                );
                                health_ping.record_missed_pong();
                                if health_ping.quality() == ConnectionQuality::Lost {
                                    let _ = decoded_tx_ping.send(DecodedClientMessage::Close).await;
                                    break;
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => break, // write pump gone
                        }
                    }
                }
            }
        });

        Self {
            ws_tx,
            ws_rx: Arc::new(Mutex::new(decoded_rx)),
            health,
            metadata: SessionMetadata {
                session_id,
                transport_type: TransportType::WebSocket,
                remote_addr,
                connected_at: Instant::now(),
                quality: ConnectionQuality::Excellent,
                // No fake 0 ms placeholder: the live value comes from
                // `ConnectionHealthMonitor::rtt_ms()` once pongs arrive.
                rtt_ms: None,
                auth_identity: None,
            },
            closed: std::sync::atomic::AtomicBool::new(false),
            outbound_seq: AtomicU32::new(0),
            last_utterance_sent: std::sync::atomic::AtomicU32::new(0),
            negotiated_input_rate,
            read_task,
        }
    }
}

/// Write pump: forwards messages from an mpsc channel to the WebSocket sink.
///
/// Each `send` is bounded by `write_timeout`. A peer that stops
/// reading while keeping TCP alive fills the kernel buffer, after which
/// `sink.send` never completes; without the bound the 128-slot channel
/// fills next, `WsSession::send` blocks the session pump, the ping pump
/// cannot record a missed pong, and the slot stays allocated until the
/// OS TCP timeout. On expiry the pump drops the sink (closing the socket),
/// pushes `Close` so the runner tears down now, and returns — every later
/// `ws_tx.send` fails with `ConnectionClosed`.
async fn write_pump<S>(
    mut sink: S,
    mut rx: mpsc::Receiver<Message>,
    write_timeout: Duration,
    decoded_tx: mpsc::Sender<DecodedClientMessage>,
) where
    S: futures_util::Sink<Message> + Unpin,
{
    use futures_util::SinkExt;
    while let Some(msg) = rx.recv().await {
        match tokio::time::timeout(write_timeout, sink.send(msg)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => break,
            Err(_) => {
                warn!(
                    timeout_ms = write_timeout.as_millis() as u64,
                    "peer not reading: ws write timed out; closing session"
                );
                // Dropping the sink closes our half of the socket. The
                // runner is told directly because the peer will never
                // send anything we could react to.
                let _ = decoded_tx.try_send(DecodedClientMessage::Close);
                return;
            }
        }
    }
    let _ = tokio::time::timeout(write_timeout, sink.close()).await;
}

#[async_trait]
impl TransportSession for WsSession {
    async fn recv(&mut self) -> Result<Option<ClientMessage>, TransportError> {
        let mut rx = self.ws_rx.lock().await;
        match rx.recv().await {
            Some(DecodedClientMessage::Message(msg)) => Ok(Some(msg)),
            Some(DecodedClientMessage::Close) | None => {
                self.closed
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(None)
            }
        }
    }

    async fn send(&self, msg: ServerMessage) -> Result<(), TransportError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(TransportError::ConnectionClosed);
        }
        // ── Diagnostic tracing (RUST_LOG=voice_pipes::transport::ws=debug) ──
        match &msg {
            ServerMessage::Audio(chunk) => {
                if chunk.is_end_of_utterance {
                    tracing::debug!(
                        utterance_id = chunk.utterance_id,
                        samples = chunk.samples.len(),
                        has_full_text = chunk.full_text.is_some(),
                        "-> audio EOU sentinel"
                    );
                } else if chunk.caption.is_some() {
                    let cap = chunk.caption.as_ref().unwrap();
                    tracing::debug!(
                        utterance_id = chunk.utterance_id,
                        fragment_idx = cap.fragment_idx,
                        caption_chars = cap.text.chars().count(),
                        samples = chunk.samples.len(),
                        "-> audio + caption"
                    );
                } else {
                    tracing::trace!(
                        utterance_id = chunk.utterance_id,
                        samples = chunk.samples.len(),
                        "-> audio chunk"
                    );
                }
            }
            ServerMessage::Control(ctrl) => {
                tracing::debug!(?ctrl, "-> control");
            }
            ServerMessage::Annotation(a) => {
                tracing::trace!(
                    utterance_id = a.utterance_id,
                    start_ms = a.start_ms,
                    kind = %a.kind,
                    "-> audio_annotation"
                );
            }
            ServerMessage::Message(_) => {
                tracing::trace!("-> other server message");
            }
        }
        // Send an audio_meta JSON frame before any audio chunk that carries
        // caption or full-text metadata.  The caption frame is sent once per
        // TTS fragment (first chunk of each fragment).  On the EOU sentinel,
        // full_text is additionally included so the client can treat it as the
        // authoritative transcript confirmation travelling in the same ordered
        // stream as the audio -- no separate async channel, no race.
        // Even if TTS yields no captions, we MUST send at least one audio_meta
        // per utterance so the client can extract the server's utterance_id.
        if let ServerMessage::Audio(ref chunk) = msg {
            let is_first = self
                .last_utterance_sent
                .swap(chunk.utterance_id, Ordering::Relaxed)
                != chunk.utterance_id;

            let has_caption = chunk.caption.is_some();
            let has_full_text = chunk.full_text.is_some();
            let has_extras = chunk.extras.is_some();
            if is_first || has_caption || has_full_text || has_extras {
                let word_timings = chunk
                    .caption
                    .as_ref()
                    .and_then(|c| c.word_timings.as_deref())
                    .map(|wts| {
                        wts.iter()
                            .map(|wt| {
                                serde_json::json!({
                                    "word": wt.word,
                                    "start_ms": wt.offset_ms,
                                    "end_ms": wt.offset_ms + wt.duration_ms,
                                })
                            })
                            .collect::<Vec<_>>()
                    });
                let mut payload = serde_json::json!({
                    "utterance_id": chunk.utterance_id,
                });
                if let Some(ref caption) = chunk.caption {
                    payload["text"] = serde_json::Value::String(caption.text.clone());
                    payload["fragment_idx"] = serde_json::json!(caption.fragment_idx);
                    payload["is_final"] = serde_json::json!(caption.is_final);
                    if let Some(t_ms) = caption.total_fragment_duration_ms {
                        payload["total_fragment_duration_ms"] = serde_json::json!(t_ms);
                    }
                    payload["word_timings"] = serde_json::json!(word_timings);
                }
                if let Some(ref full_text) = chunk.full_text {
                    payload["full_text"] = serde_json::Value::String(full_text.clone());
                }
                if let Some(ref extras) = chunk.extras {
                    payload["extras"] = extras.clone();
                }
                payload["server_timestamp_ms"] = serde_json::json!(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64
                );
                let envelope = JsonEnvelope::new("audio_meta", payload);
                let json = envelope.encode()?;
                self.ws_tx
                    .send(Message::Text(json.into()))
                    .await
                    .map_err(|_| TransportError::ConnectionClosed)?;
            }
        }
        let seq = self.outbound_seq.fetch_add(1, Ordering::Relaxed);
        let frame = encode_server_message(&msg, seq)?;
        self.ws_tx
            .send(frame)
            .await
            .map_err(|_| TransportError::ConnectionClosed)
    }

    async fn close(&self, reason: &str) -> Result<(), TransportError> {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        let close = Message::Close(Some(axum::extract::ws::CloseFrame {
            code: 1000,
            reason: reason.to_owned().into(),
        }));
        // Bounded — a stalled write pump must not make teardown
        // hang; its own timeout closes the socket shortly anyway.
        let _ = tokio::time::timeout(Duration::from_millis(500), self.ws_tx.send(close)).await;
        self.read_task.abort();
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

// ── Tests ───────────────────────────────────────────────────

/// Consecutive undecodable frames tolerated before the read pump
/// closes the session. Generous enough for a buggy client to log a few
/// mistakes, small enough that a garbage firehose is cut off within a
/// few milliseconds.
pub(crate) const MAX_CONSECUTIVE_DECODE_FAILURES: u32 = 32;

/// What the read pump does after the `count`-th consecutive decode failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodeFailureAction {
    /// Emit the warn line (first failure, then powers of two).
    pub log: bool,
    /// Push `Close` and stop reading.
    pub close: bool,
}

pub(crate) fn decode_failure_action(count: u32) -> DecodeFailureAction {
    DecodeFailureAction {
        log: count.is_power_of_two(),
        close: count >= MAX_CONSECUTIVE_DECODE_FAILURES,
    }
}

#[cfg(test)]
#[path = "tests/ws.rs"]
mod tests;
