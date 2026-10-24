//! Generic [`RealtimeClient`] over a [`RealtimeProtocol`].
//!
//! Owns the WebSocket connection plus a writer task (drains an
//! outbound channel) and a reader task (parses incoming frames via
//! the protocol). Adapters above this layer (such as the
//! synthetic-caller backends) only need to translate the typed [`RealtimeEvent`] stream into their
//! own surface.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::codec::encode_pcm16_b64;
use super::event::RealtimeEvent;
use super::protocol::{RealtimeProtocol, SessionConfig};
use super::rate::{Downlink, StreamRate};

/// Internal outbound-message envelope for the writer task.
#[derive(Debug)]
enum OutMsg {
    Text(String),
    Close,
}

/// Writer-task queue depth. Bounded: a stalled provider socket
/// must not buffer an unbounded outbound backlog. 256 envelopes is tens of
/// seconds of 100 ms audio appends; a queue this deep means the socket is
/// effectively dead.
const OUT_QUEUE_CAPACITY: usize = 256;

/// Upper bound on the provider WebSocket handshake (TCP + TLS +
/// upgrade). Generous for any healthy provider; short enough that a
/// black-holed endpoint cannot park the session runner for the OS connect
/// timeout.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a *control* envelope (commit, cancel, truncate, session
/// update) may wait for outbound-queue space before the send is declared
/// failed. The queue is shared with high-rate audio appends, so
/// a transient writer stall fills it — an instant `try_send` failure here
/// would lose barge-in truncations and cancels that could have been
/// delivered once the stall cleared.
const CONTROL_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Reader-task queue depth. Bounded: when the consumer stalls the
/// reader task blocks on `send`, which stops reading the WS and lets TCP
/// backpressure throttle the provider instead of buffering audio deltas in
/// RAM.
const IN_QUEUE_CAPACITY: usize = 1024;

/// Timing and size limits of one connection. The defaults suit the
/// hosted providers; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct ClientLimits {
    /// Largest WebSocket message accepted from the provider. A frame is
    /// parsed whole, so this bounds one allocation; the largest legitimate
    /// frame is a few hundred KB of base64 audio.
    pub max_message_bytes: usize,
    /// Bound on one outbound `send`. A provider that stops reading leaves
    /// the socket's send window full; without this the writer task would
    /// sit in `send` past every cancellation and hold the socket open until
    /// the OS gives up.
    pub write_timeout: Duration,
    /// How often the writer sends a WebSocket ping.
    pub ping_interval: Duration,
    /// A connection over which nothing (no frame, no pong) arrived for this
    /// long is reported as closed so the runner can reconnect. Pings keep a
    /// healthy but quiet provider well inside this window.
    pub liveness_timeout: Duration,
}

impl Default for ClientLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 4 * 1024 * 1024,
            write_timeout: Duration::from_secs(10),
            ping_interval: Duration::from_secs(20),
            liveness_timeout: Duration::from_mins(1),
        }
    }
}

/// Milliseconds since the Unix epoch; the reader stamps it on every
/// inbound message and the writer's ping timer reads it.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Minimum uplink frame duration. Google's Live API best-practices doc
/// recommends 20-40 ms audio chunks; browser clients deliver 128-sample
/// `AudioWorklet` quanta (~2.7 ms at 48 kHz), which would otherwise become
/// ~375 tiny base64/JSON envelopes per second. Coalescing here covers
/// every client in one place; chunks already at or above the threshold
/// pass through on the same call.
const UPLINK_FRAME_MS: u64 = 20;

/// Accumulates uplink PCM until at least [`UPLINK_FRAME_MS`] of audio is
/// buffered, so tiny client capture quanta reach the provider as
/// recommended-size frames.
struct UplinkCoalescer {
    buf: Vec<i16>,
    rate: u32,
    frame_ms: u64,
}

impl UplinkCoalescer {
    fn new(frame_ms: u64) -> Self {
        Self {
            buf: Vec::new(),
            rate: 0,
            frame_ms,
        }
    }

    /// Append `samples` at `rate`; returns zero, one, or two
    /// ready-to-send frames (a rate change flushes the old-rate
    /// remainder as its own frame).
    fn push(&mut self, samples: &[i16], rate: u32) -> Vec<(Vec<i16>, u32)> {
        let mut out = Vec::new();
        if rate != self.rate && !self.buf.is_empty() {
            out.push((std::mem::take(&mut self.buf), self.rate));
        }
        self.rate = rate;
        self.buf.extend_from_slice(samples);
        let threshold = (rate as u64 * self.frame_ms / 1000) as usize;
        if threshold == 0 || self.buf.len() >= threshold {
            out.push((std::mem::take(&mut self.buf), rate));
        }
        out
    }
}

/// A connected, configured S2S Realtime session.
///
/// Generic over the wire protocol. All IO happens on the two
/// background tasks spawned at [`RealtimeClient::connect`]; the
/// public methods just push/pull through bounded mpsc channels.
pub struct RealtimeClient<P: RealtimeProtocol> {
    proto: Arc<P>,
    cfg: Option<SessionConfig>,
    tx: mpsc::Sender<OutMsg>,
    rx: mpsc::Receiver<RealtimeEvent>,
    cancel: CancellationToken,
    uplink: UplinkCoalescer,
    /// The uplink converted to the provider's input rate as one stream.
    uplink_rate: StreamRate,
    /// Audio chunks dropped because the writer was stalled.
    dropped_audio_chunks: u64,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "channels/cancel-token/coalescer carry no diagnostic value; provider identity and rates are the useful state"
)]
impl<P: RealtimeProtocol> std::fmt::Debug for RealtimeClient<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeClient")
            .field("provider", &self.proto.name())
            .field(
                "native_input_sample_rate",
                &self.proto.native_input_sample_rate(),
            )
            .field(
                "native_output_sample_rate",
                &self.proto.native_output_sample_rate(),
            )
            .finish()
    }
}

impl<P: RealtimeProtocol> RealtimeClient<P> {
    /// Open the WS and spawn read/write tasks. No session-update
    /// envelope is sent yet -- call
    /// [`apply_session_config`](Self::apply_session_config) when the
    /// caller is ready (the existing synthetic-caller flow defers
    /// the persona prompt until the runner's `configure(prompt)`
    /// pass).
    ///
    /// The model's audio reaches [`next_event`](Self::next_event) at
    /// `output_rate`, resampled from the provider's rate as one stream.
    pub async fn connect(proto: P, output_rate: u32) -> Result<Self> {
        Self::connect_with(proto, output_rate, ClientLimits::default()).await
    }

    /// [`connect`](Self::connect) with explicit [`ClientLimits`].
    #[expect(
        clippy::too_many_lines,
        reason = "connect owns the writer/reader task bodies; hoisting them into free fns would double the parameter plumbing"
    )]
    pub async fn connect_with(proto: P, output_rate: u32, limits: ClientLimits) -> Result<Self> {
        voice_pipes_net::install_crypto_provider();

        let proto = Arc::new(proto);
        let info = proto
            .connect_info()
            .with_context(|| format!("{} connect_info", proto.name()))?;

        // Validate the URL early so a typo doesn't surface as a
        // confusing tungstenite error.
        let parsed: Url = info
            .url
            .parse()
            // Never echo the URL — Gemini carries the API key in
            // its query string, so an error here must not interpolate it.
            .with_context(|| format!("invalid {} ws url (redacted)", proto.name()))?;

        // Build the upgrade request so per-provider headers (e.g.
        // OpenAI's `Authorization: Bearer`) attach cleanly.
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("{} ws url missing host", proto.name()))?
            .to_string();
        let mut builder = Request::builder()
            .method("GET")
            .uri(&info.url)
            .header("Host", host)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_key());
        for (k, v) in &info.headers {
            builder = builder.header(k, v);
        }
        let req = builder
            .body(())
            .with_context(|| format!("build {} upgrade request", proto.name()))?;

        // Bound the TCP+TLS+upgrade handshake. A black-holed
        // provider endpoint otherwise holds the caller — the session
        // runner's own task on the reconnect path — for the OS connect
        // timeout, during which no client frame is processed.
        // The provider's frames are parsed whole, so the message cap is
        // the bound on one allocation.
        let (ws, _resp) =
            voice_pipes_net::connect_websocket(req, limits.max_message_bytes, CONNECT_TIMEOUT)
                .await
                .with_context(|| format!("connect to {} ws", proto.name()))?;
        let (mut sink, mut stream) = ws.split();

        let cancel = CancellationToken::new();
        let cancel_writer = cancel.clone();
        let cancel_reader = cancel.clone();

        let (out_tx, mut out_rx) = mpsc::channel::<OutMsg>(OUT_QUEUE_CAPACITY);
        let (in_tx, in_rx) = mpsc::channel::<RealtimeEvent>(IN_QUEUE_CAPACITY);

        // Stamped by the reader on every inbound message (data, ping,
        // pong, close); read by the writer's ping timer to detect a
        // connection that went quiet without closing.
        let last_rx = Arc::new(AtomicU64::new(now_ms()));
        let last_rx_writer = Arc::clone(&last_rx);
        let in_tx_writer = in_tx.clone();

        // Writer task -- drains the outbound channel into the WS, sends
        // keepalive pings, and reports a dead connection.
        // Every writer event carries the provider's name.
        let writer_target: &'static str = proto.name();
        let write_timeout = limits.write_timeout;
        let liveness_timeout = limits.liveness_timeout;
        let mut ping_timer = tokio::time::interval(limits.ping_interval);
        ping_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::spawn(async move {
            // Every send is bounded and raced against cancellation: a
            // peer that stopped reading must not keep this task, and the
            // socket, alive past the session.
            async fn bounded_send<S>(
                sink: &mut S,
                msg: Message,
                cancel: &CancellationToken,
                bound: Duration,
            ) -> std::result::Result<(), String>
            where
                S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
                    + Unpin,
            {
                tokio::select! {
                    () = cancel.cancelled() => Err("cancelled".to_string()),
                    r = tokio::time::timeout(bound, sink.send(msg)) => match r {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(_) => Err(format!("send did not complete within {bound:?}")),
                    },
                }
            }
            loop {
                tokio::select! {
                    () = cancel_writer.cancelled() => {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(500),
                            sink.send(Message::Close(None)),
                        ).await;
                        break;
                    }
                    _ = ping_timer.tick() => {
                        let quiet = Duration::from_millis(
                            now_ms().saturating_sub(last_rx_writer.load(Ordering::Relaxed)),
                        );
                        if quiet >= liveness_timeout {
                            tracing::warn!(
                                provider = writer_target,
                                quiet_ms = quiet.as_millis() as u64,
                                "no frame from provider; treating the connection as closed"
                            );
                            let _ = in_tx_writer.send(RealtimeEvent::Closed {
                                code: None,
                                reason: format!(
                                    "no frame from provider for {} s",
                                    quiet.as_secs()
                                ),
                            }).await;
                            break;
                        }
                        if let Err(e) = bounded_send(
                            &mut sink,
                            Message::Ping(tokio_tungstenite::tungstenite::Bytes::new()),
                            &cancel_writer,
                            write_timeout,
                        ).await {
                            tracing::warn!(provider = writer_target, error = %e, "ws ping failed");
                            break;
                        }
                    }
                    msg = out_rx.recv() => {
                        let Some(msg) = msg else { break; };
                        let r = match msg {
                            OutMsg::Text(s) => {
                                bounded_send(&mut sink, Message::Text(s.into()), &cancel_writer, write_timeout).await
                            }
                            OutMsg::Close => {
                                let _ = bounded_send(
                                    &mut sink,
                                    Message::Close(None),
                                    &cancel_writer,
                                    Duration::from_millis(500),
                                ).await;
                                break;
                            }
                        };
                        if let Err(e) = r {
                            tracing::warn!(provider = writer_target, error = %e, "ws send failed");
                            break;
                        }
                    }
                }
            }
        });

        // Reader task -- parses inbound frames via the protocol and
        // forwards typed events.
        let reader_proto = Arc::clone(&proto);
        let in_tx_reader = in_tx.clone();
        // Protocol-level auto-replies (application keepalives such as
        // ElevenLabs' JSON ping→pong) are written straight back through
        // the writer queue from the reader task. Loss-tolerant
        // (`try_send`): a queue full of audio appends means the socket
        // is stalled anyway, and the server will re-ping.
        let out_tx_reader = out_tx.clone();
        // Every reader event carries the provider's name.
        let reader_target: &'static str = proto.name();
        let mut downlink = Downlink::new(output_rate);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel_reader.cancelled() => {
                        break;
                    }
                    msg = stream.next() => {
                        let Some(msg) = msg else {
                            let _ = in_tx_reader.send(RealtimeEvent::Closed {
                                code: None,
                                reason: "stream ended (None)".to_string(),
                            }).await;
                            break;
                        };
                        last_rx.store(now_ms(), Ordering::Relaxed);
                        match msg {
                            Ok(Message::Text(text)) => {
                                match serde_json::from_str::<serde_json::Value>(&text) {
                                    Ok(v) => {
                                        // Full frames carry conversation content and
                                        // occasionally provider echoes of credentials —
                                        // trace-only and, with
                                        // credential-shaped keys redacted even then.
                                        if tracing::enabled!(tracing::Level::TRACE) {
                                            tracing::trace!(
                                                provider = reader_target,
                                                frame = %redact_secrets(&v),
                                                "ws recv text"
                                            );
                                        }
                                        if let Some(reply) = reader_proto.build_auto_reply(&v)
                                            && out_tx_reader.try_send(OutMsg::Text(reply.to_string())).is_err()
                                        {
                                            tracing::warn!(
                                                provider = reader_target,
                                                "auto-reply dropped (outbound queue full or writer gone)"
                                            );
                                        }
                                        for ev in reader_proto.parse_event(&v) {
                                            for ev in downlink.on_event(bounded(ev, reader_target)) {
                                                let _ = in_tx_reader.send(ev).await;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        // One unparsable frame is not a dead
                                        // connection: report it and keep
                                        // reading, as for a non-JSON binary
                                        // frame.
                                        tracing::warn!(
                                            provider = reader_target,
                                            error = %e,
                                            bytes = text.len(),
                                            "ws text frame is not JSON; skipped"
                                        );
                                        let _ = in_tx_reader.send(RealtimeEvent::Error {
                                            code: Some("bad_frame".to_string()),
                                            message: format!("provider sent a non-JSON text frame: {e}"),
                                            recoverable: true,
                                        }).await;
                                    }
                                }
                            }
                            Ok(Message::Binary(bytes)) => {
                                // Gemini occasionally delivers JSON over Binary
                                // frames; OpenAI does not. Try-parse and fall
                                // back to ignoring opaque payloads.
                                match serde_json::from_slice::<serde_json::Value>(&bytes) {
                                    Ok(v) => {
                                        // Same trace as text frames: Gemini Live
                                        // delivers *all* its JSON this way, so
                                        // provider ordering (inputTranscription /
                                        // outputTranscription / inlineData /
                                        // turnComplete) is only observable here.
                                        if tracing::enabled!(tracing::Level::TRACE) {
                                            tracing::trace!(
                                                provider = reader_target,
                                                frame = %redact_secrets(&v),
                                                "ws recv binary-json"
                                            );
                                        }
                                        if let Some(reply) = reader_proto.build_auto_reply(&v)
                                            && out_tx_reader.try_send(OutMsg::Text(reply.to_string())).is_err()
                                        {
                                            tracing::warn!(
                                                provider = reader_target,
                                                "auto-reply dropped (outbound queue full or writer gone)"
                                            );
                                        }
                                        for ev in reader_proto.parse_event(&v) {
                                            for ev in downlink.on_event(bounded(ev, reader_target)) {
                                                let _ = in_tx_reader.send(ev).await;
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        tracing::trace!(
                                            provider = reader_target,
                                            bytes = bytes.len(),
                                            "ws recv non-json binary frame ignored"
                                        );
                                    }
                                }
                            }
                            Ok(Message::Ping(_) | Message::Pong(_)) => {}
                            Ok(Message::Close(frame)) => {
                                let (code, reason) = match frame {
                                    Some(f) => (u16::from(f.code), f.reason.to_string()),
                                    None => (0, String::from("(no close frame)")),
                                };
                                tracing::warn!(
                                    provider = reader_target,
                                    close_code = code,
                                    close_reason = %reason,
                                    "ws closed by server"
                                );
                                let _ = in_tx_reader.send(RealtimeEvent::Closed {
                                    code: (code != 0).then_some(code),
                                    reason: format!("server close (code={code}, reason={reason})"),
                                }).await;
                                break;
                            }
                            Ok(Message::Frame(_)) => break,
                            Err(e) => {
                                // A transport error (reset, TLS failure, an
                                // oversized frame) ends this connection the
                                // same way a close frame does: the runner
                                // classifies it and may reconnect.
                                tracing::warn!(provider = reader_target, error = %e, "ws read error");
                                let _ = in_tx_reader.send(RealtimeEvent::Closed {
                                    code: None,
                                    reason: format!("ws read error: {e}"),
                                }).await;
                                break;
                            }
                        }
                    }
                }
            }
        });

        // Send session configuration. Providers may emit multiple
        // envelopes (e.g. setup + opener kick) -- send each in order.
        // (Deferred until apply_session_config.)

        let uplink_rate = StreamRate::new(proto.native_input_sample_rate());
        Ok(Self {
            proto,
            cfg: None,
            tx: out_tx,
            rx: in_rx,
            cancel,
            uplink: UplinkCoalescer::new(UPLINK_FRAME_MS),
            uplink_rate,
            dropped_audio_chunks: 0,
        })
    }

    /// Send the provider-specific `session.update` / `setup`
    /// envelope(s) for `cfg`. May be called more than once to
    /// reconfigure mid-session (e.g. swap voice or instructions).
    pub async fn apply_session_config(&mut self, cfg: SessionConfig) -> Result<()> {
        for env in self.proto.build_session_update(&cfg) {
            self.send_envelope(env).await?;
        }
        self.cfg = Some(cfg);
        Ok(())
    }

    /// Provider name (useful for telemetry).
    pub fn provider(&self) -> &'static str {
        self.proto.name()
    }

    /// Classify a server close (see [`RealtimeProtocol::classify_close`]).
    pub fn classify_close(
        &self,
        code: Option<u16>,
        reason: &str,
    ) -> crate::realtime::protocol::CloseClass {
        self.proto.classify_close(code, reason)
    }

    /// See [`RealtimeProtocol::inferred_completion_idle_ms`]: `Some(ms)`
    /// when the provider has no end-of-response wire event and the
    /// consumer should treat a response as completed after this much
    /// audio/text silence.
    pub fn inferred_completion_idle_ms(&self) -> Option<u64> {
        self.proto.inferred_completion_idle_ms()
    }

    /// Session config that was last applied via
    /// [`apply_session_config`](Self::apply_session_config), if any.
    pub fn session_config(&self) -> Option<&SessionConfig> {
        self.cfg.as_ref()
    }

    /// Push a PCM16 audio chunk at `src_rate`. Coalesced into frames
    /// of at least [`UPLINK_FRAME_MS`], then resampled and
    /// base64-encoded into the provider's native input rate before
    /// being sent. No-op on empty input; sub-threshold input is
    /// buffered until enough audio accumulates (the next chunks of a
    /// continuous capture stream flush it within a frame's time).
    ///
    /// Completes immediately: audio appends are lossy `try_send`s by
    /// design, and the future keeps the signature of the other `send_*`
    /// methods.
    pub fn send_audio(
        &mut self,
        samples: &[i16],
        src_rate: u32,
    ) -> impl Future<Output = Result<()>> + use<P> {
        std::future::ready(self.push_audio(samples, src_rate))
    }

    fn push_audio(&mut self, samples: &[i16], src_rate: u32) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        for (frame, rate) in self.uplink.push(samples, src_rate) {
            self.send_audio_frame(&frame, rate)?;
        }
        Ok(())
    }

    fn send_audio_frame(&mut self, samples: &[i16], src_rate: u32) -> Result<()> {
        // Read per frame: a protocol can learn its input rate from the
        // vendor after the connection opens.
        self.uplink_rate
            .retarget(self.proto.native_input_sample_rate());
        let samples = self
            .uplink_rate
            .convert(samples, src_rate)
            .map_err(|rate| anyhow!("microphone audio at an unsupported rate ({rate} Hz)"))?;
        if samples.is_empty() {
            return Ok(());
        }
        let b64 = encode_pcm16_b64(&samples);
        let env = self.proto.build_audio_append(&b64);
        // Audio appends are high-rate and loss-tolerant: when the writer is
        // stalled (queue full) drop the chunk rather than erroring the
        // session or buffering without bound. The first drop is logged and
        // then exponentially rarer ones, so a long stall cannot flood the log.
        match self.tx.try_send(OutMsg::Text(env.to_string())) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_audio_chunks += 1;
                if self.dropped_audio_chunks.is_power_of_two() {
                    tracing::warn!(
                        provider = self.proto.name(),
                        dropped_chunks = self.dropped_audio_chunks,
                        "realtime writer stalled (outbound queue full); dropping audio"
                    );
                }
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(anyhow!("{} writer dropped", self.proto.name()))
            }
        }
    }

    /// Send a user text message and (if the provider needs it)
    /// follow up with a `response.create` kick. Returns an error if
    /// the provider does not support text input.
    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let env = self.proto.build_text_message(text).ok_or_else(|| {
            anyhow!(
                "{} provider does not support text-input messages",
                self.proto.name()
            )
        })?;
        self.send_envelope(env).await?;
        let mods = self
            .cfg
            .as_ref()
            .map_or(crate::realtime::protocol::Modalities::Audio, |c| {
                c.modalities
            });
        if let Some(kick) = self.proto.build_response_create(mods) {
            self.send_envelope(kick).await?;
        }
        Ok(())
    }

    /// Explicitly ask the model to produce a response (used when
    /// `TurnDetection::None`). No-op if the provider auto-responds.
    pub async fn create_response(&mut self) -> Result<()> {
        let mods = self
            .cfg
            .as_ref()
            .map_or(crate::realtime::protocol::Modalities::Audio, |c| {
                c.modalities
            });
        if let Some(env) = self.proto.build_response_create(mods) {
            self.send_envelope(env).await?;
        }
        Ok(())
    }

    /// Inject `text` into the conversation as background context
    /// (system/context role). When `respond` is true the provider's
    /// composite also triggers a model response. No-op on providers
    /// without context injection. Used for late `Pending` tool results
    /// and agent-initiated out-of-turn speech.
    pub async fn send_context_message(&mut self, text: &str, respond: bool) -> Result<()> {
        let mods = self
            .cfg
            .as_ref()
            .map_or(crate::realtime::protocol::Modalities::Audio, |c| {
                c.modalities
            });
        for env in self.proto.build_context_message(text, respond, mods) {
            self.send_envelope(env).await?;
        }
        Ok(())
    }

    /// Return a tool-call result to the model. Returns an error if
    /// the provider has not wired tools yet. `name` is the called
    /// function's name (required by Gemini's `FunctionResponse`);
    /// `silent` asks the provider not to voice a reaction to this
    /// output (client-side tool acks) — see
    /// [`RealtimeProtocol::build_function_call_output`].
    pub async fn send_function_output(
        &mut self,
        call_id: &str,
        name: &str,
        result: &str,
        silent: bool,
    ) -> Result<()> {
        let env = self
            .proto
            .build_function_call_output(call_id, name, result, silent)
            .ok_or_else(|| {
                anyhow!(
                    "{} provider does not support function-call outputs",
                    self.proto.name()
                )
            })?;
        self.send_envelope(env).await
    }

    /// Cancel the in-flight response. Safe to call when no response
    /// is in flight (providers either no-op or echo
    /// `response.cancelled`).
    pub async fn cancel(&mut self) -> Result<()> {
        if let Some(env) = self.proto.build_cancel() {
            self.send_envelope(env).await?;
        }
        Ok(())
    }

    /// Truncate a stored assistant item to `audio_end_ms` of audio so
    /// the provider's conversation history reflects only what the
    /// client actually played before a barge-in. Silently succeeds
    /// (no envelope sent) on providers that don't support per-item
    /// truncation.
    pub async fn truncate_item(&mut self, item_id: &str, audio_end_ms: u32) -> Result<()> {
        if let Some(env) = self.proto.build_item_truncate(item_id, audio_end_ms) {
            self.send_envelope(env).await?;
        }
        Ok(())
    }

    /// Await the next typed event from the server.
    pub async fn next_event(&mut self) -> Option<RealtimeEvent> {
        self.rx.recv().await
    }

    /// Close the WS gracefully. Consumes `self`.
    ///
    /// Completes immediately: the close is fire-and-forget, and the future
    /// lets callers treat teardown uniformly.
    pub fn close(self) -> impl Future<Output = Result<()>> + use<P> {
        // Best-effort: if the queue is full the cancel token below closes
        // the socket anyway.
        let _ = self.tx.try_send(OutMsg::Close);
        self.cancel.cancel();
        std::future::ready(Ok(()))
    }

    async fn send_envelope(&self, env: serde_json::Value) -> Result<()> {
        // Control envelopes are low-rate and must not be lost to a
        // *transient* writer stall: the queue is shared with high-rate
        // audio appends, so wait (bounded) for space instead of failing
        // instantly, which would lose barge-in truncations/cancels that
        // could be delivered once the stall clears. A queue still full
        // after the timeout means the writer is truly wedged; surface that
        // as an error.
        let msg = OutMsg::Text(env.to_string());
        match tokio::time::timeout(CONTROL_SEND_TIMEOUT, self.tx.send(msg)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(anyhow!("{} writer dropped", self.proto.name())),
            Err(_) => Err(anyhow!(
                "{} outbound queue full for {}s (writer stalled)",
                self.proto.name(),
                CONTROL_SEND_TIMEOUT.as_secs()
            )),
        }
    }
}

impl<P: RealtimeProtocol> Drop for RealtimeClient<P> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Copy of `v` with credential-shaped values replaced by `"***"`, for
/// the trace-level frame log. Every provider protocol we speak keeps its
/// credentials under keys the shared test recognises: `client_secret`
/// (`OpenAI`), `xi-api-key` and `conversation_token` (`ElevenLabs`),
/// `key` (Gemini), and the generic `token` / `authorization`. Structural
/// (walks objects and arrays), so nested echoes such as
/// `session.client_secret.value` are covered too.
pub(crate) fn redact_secrets(v: &serde_json::Value) -> serde_json::Value {
    voice_pipes_sdk::secrets::redacted(v, "***")
}

/// Hold a provider's event to the engine's bounds before anything else
/// sees it. Providers are separate crates, so a bound a provider could
/// skip (by building the variant directly instead of through
/// [`RealtimeEvent::function_call`]) is enforced here, where every parsed
/// event enters the engine: a tool call whose arguments exceed
/// [`MAX_TOOL_ARGUMENT_BYTES`](crate::realtime::event::MAX_TOOL_ARGUMENT_BYTES)
/// becomes a recoverable error and never reaches the agent layer, the
/// logs or the client.
fn bounded(ev: RealtimeEvent, provider: &str) -> RealtimeEvent {
    match ev {
        RealtimeEvent::FunctionCall {
            call_id,
            name,
            arguments,
        } => {
            let bytes = arguments.len();
            let ev = RealtimeEvent::function_call(call_id, name, arguments);
            if let RealtimeEvent::Error { code, .. } = &ev {
                tracing::warn!(provider, bytes, code = ?code, "tool call refused");
            }
            ev
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "tests/client.rs"]
mod tests;
