//! Test fixture for the realtime stack: a scripted [`RealtimeProtocol`]
//! plus a local WebSocket server that records every outbound envelope
//! and pushes scripted inbound JSON frames to the client.
//!
//! Exercises the `RealtimeClient` + `RealtimeProtocol`
//! contract end-to-end without dialing `OpenAI` / Gemini.
//!
//! ## Wire format (neutral)
//!
//! The mock uses a flat `{ "type": "<event>", ... }` envelope on both
//! directions so the parser is trivial and tests can assert on the
//! recorded outbound payloads as plain JSON.
//!
//! - `audio_append`  -- client → server (b64 PCM16)
//! - `text_message`  -- client → server
//! - `cancel`        -- client → server
//! - `function_call_output` -- client → server
//! - `session_update`  -- client → server
//! - `response_create` -- client → server
//! - Inbound types match the [`RealtimeEvent`] variants 1:1.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::realtime::event::RealtimeEvent;
use crate::realtime::protocol::{
    ConnectInfo, Modalities, RealtimeProtocol, SessionConfig, TurnDetection,
};

/// Neutral [`RealtimeProtocol`] implementation pointed at a local
/// [`MockServer`].
#[derive(Debug)]
pub struct MockRealtimeProtocol {
    url: String,
    input_rate: Arc<AtomicU32>,
}

impl MockRealtimeProtocol {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            input_rate: Arc::new(AtomicU32::new(16_000)),
        }
    }

    /// The input rate the protocol reports, which a test may change after
    /// connecting, as a vendor's handshake can.
    pub fn input_rate(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.input_rate)
    }
}

impl RealtimeProtocol for MockRealtimeProtocol {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn native_input_sample_rate(&self) -> u32 {
        self.input_rate.load(Ordering::Relaxed)
    }
    fn native_output_sample_rate(&self) -> u32 {
        16_000
    }
    fn connect_info(&self) -> Result<ConnectInfo> {
        Ok(ConnectInfo::new(self.url.clone()))
    }
    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value> {
        vec![serde_json::json!({
            "type": "session_update",
            "instructions": cfg.instructions,
            "voice": cfg.voice,
            "modalities": match cfg.modalities {
                Modalities::Audio => "audio",
                Modalities::Text => "text",
                Modalities::AudioAndText => "audio_and_text",
                _ => "unknown",
            },
            "turn_detection": match cfg.turn_detection {
                TurnDetection::None => "none",
                TurnDetection::ServerVad { .. } => "server_vad",
                TurnDetection::SemanticVad { .. } => "semantic_vad",
                _ => "unknown",
            },
            "tool_names": cfg.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        })]
    }
    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value {
        serde_json::json!({ "type": "audio_append", "audio": b64_pcm })
    }
    fn build_response_create(&self, _modalities: Modalities) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "response_create" }))
    }
    fn build_text_message(&self, text: &str) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "text_message", "text": text }))
    }
    fn build_cancel(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "cancel" }))
    }
    fn build_function_call_output(
        &self,
        call_id: &str,
        name: &str,
        result: &str,
        silent: bool,
    ) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "function_call_output",
            "call_id": call_id,
            "name": name,
            "result": result,
            "silent": silent,
        }))
    }
    fn build_context_message(
        &self,
        text: &str,
        respond: bool,
        _modalities: Modalities,
    ) -> Vec<serde_json::Value> {
        let mut out = vec![serde_json::json!({
            "type": "context_message",
            "text": text,
        })];
        if respond {
            out.push(serde_json::json!({ "type": "response_create" }));
        }
        out
    }
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent> {
        let Some(kind) = v.get("type").and_then(|x| x.as_str()) else {
            return Vec::new();
        };
        match kind {
            "response_started" => vec![RealtimeEvent::ResponseStarted {
                response_id: v
                    .get("response_id")
                    .and_then(|x| x.as_str())
                    .map(String::from),
            }],
            "response_audio_delta" => {
                let samples = v
                    .get("samples")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|n| n.as_i64().map(|v| v as i16))
                            .collect()
                    })
                    .unwrap_or_default();
                let sample_rate = v
                    .get("sample_rate")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(16_000) as u32;
                vec![RealtimeEvent::ResponseAudioDelta {
                    samples,
                    sample_rate,
                    alignment: None,
                }]
            }
            "response_text_delta" => vec![RealtimeEvent::ResponseTextDelta {
                text: v
                    .get("text")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            }],
            "response_completed" => vec![RealtimeEvent::ResponseCompleted {
                response_id: v
                    .get("response_id")
                    .and_then(|x| x.as_str())
                    .map(String::from),
            }],
            "response_cancelled" => vec![RealtimeEvent::ResponseCancelled {
                response_id: v
                    .get("response_id")
                    .and_then(|x| x.as_str())
                    .map(String::from),
            }],
            "input_transcript" => vec![RealtimeEvent::InputTranscript {
                text: v
                    .get("text")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                is_final: v
                    .get("is_final")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
            }],
            "input_speech_started" => vec![RealtimeEvent::InputSpeechStarted],
            "input_speech_stopped" => vec![RealtimeEvent::InputSpeechStopped],
            "input_audio_committed" => vec![RealtimeEvent::InputAudioCommitted],
            "session_updated" => vec![RealtimeEvent::SessionUpdated],
            "function_call" => vec![RealtimeEvent::FunctionCall {
                call_id: v
                    .get("call_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                name: v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                arguments: v
                    .get("arguments")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            }],
            "error" => vec![RealtimeEvent::Error {
                code: v.get("code").and_then(|x| x.as_str()).map(String::from),
                message: v
                    .get("message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                recoverable: v
                    .get("recoverable")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            }],
            "closed" => vec![RealtimeEvent::Closed {
                code: v
                    .get("code")
                    .and_then(serde_json::Value::as_u64)
                    .map(|c| c as u16),
                reason: v
                    .get("reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("test")
                    .to_string(),
            }],
            _ => Vec::new(),
        }
    }
}

/// Scripted server-side WebSocket fixture.
///
/// Each accepted connection plays back the scripted JSON frames in
/// order (after a small per-frame yield so the client has time to
/// process them) and records every inbound envelope into a shared
/// `Vec` exposed via [`MockServer::recorded`].
pub struct MockServer {
    pub addr: SocketAddr,
    pub recorded: Arc<Mutex<Vec<serde_json::Value>>>,
    _shutdown: mpsc::Sender<()>,
}

impl MockServer {
    /// Start a server that, on the next accepted connection, sends
    /// every frame in `script` to the client. The connection stays
    /// open until either the client closes it or
    /// [`MockServer::drop`] runs.
    pub async fn start(script: Vec<serde_json::Value>) -> Self {
        Self::start_with(script, HashMap::new()).await
    }

    /// Variant that lets a test inject extra envelopes keyed off of
    /// the *first* envelope of a given `type` the client sends. Useful
    /// to script a "wait for `cancel` then push `response_cancelled`"
    /// flow.
    pub async fn start_with(
        script: Vec<serde_json::Value>,
        reactions: HashMap<String, serde_json::Value>,
    ) -> Self {
        let raw: Vec<String> = script.iter().map(ToString::to_string).collect();
        Self::start_raw(raw, reactions).await
    }

    /// Variant that sends the scripted frames as raw text, so a test
    /// can push something that is not JSON.
    pub async fn start_raw(
        script: Vec<String>,
        reactions: HashMap<String, serde_json::Value>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let recorded: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = Arc::clone(&recorded);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_rx.recv() => {}
                accept = listener.accept() => {
                    let Ok((stream, _)) = accept else { return };
                    let Ok(ws) = accept_async(stream).await else { return };
                    let (mut sink, mut stream) = ws.split();

                    // Push all unconditional scripted frames first.
                    for frame in &script {
                        let _ = sink.send(Message::Text(frame.clone().into())).await;
                        tokio::task::yield_now().await;
                    }

                    // Then react to whatever the client sends.
                    while let Some(Ok(msg)) = stream.next().await {
                        if let Message::Text(text) = msg
                            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
                        {
                            let kind = v
                                .get("type")
                                .and_then(|x| x.as_str())
                                .map(String::from);
                            recorded_clone.lock().unwrap().push(v);
                            if let Some(k) = kind
                                && let Some(reply) = reactions.get(&k)
                            {
                                let _ = sink
                                    .send(Message::Text(reply.to_string().into()))
                                    .await;
                            }
                        }
                    }
                }
            }
        });

        MockServer {
            addr,
            recorded,
            _shutdown: shutdown_tx,
        }
    }

    pub fn url(&self) -> String {
        format!("ws://{}/mock", self.addr)
    }

    /// A server that completes the WebSocket upgrade and then neither
    /// reads nor writes: pings go unanswered, so the connection looks
    /// alive at the TCP level and dead at the protocol level.
    pub async fn start_silent() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            // Hold the socket open without servicing it.
            let _ws = ws;
            let _ = shutdown_rx.recv().await;
        });
        MockServer {
            addr,
            recorded: Arc::new(Mutex::new(Vec::new())),
            _shutdown: shutdown_tx,
        }
    }

    /// A server that completes the upgrade and then resets the TCP
    /// connection (no close frame), the shape of a mid-call network
    /// failure.
    pub async fn start_resetting() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<()>(1);
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            // A zero linger is exactly what makes the drop below an RST;
            // the blocking-on-drop the deprecation warns about is a
            // no-op with nothing left to flush.
            #[expect(deprecated, reason = "zero linger is the point: drop sends RST")]
            let _ = stream.set_linger(Some(std::time::Duration::ZERO));
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            // Dropping a zero-linger socket sends RST instead of FIN.
            drop(ws);
        });
        MockServer {
            addr,
            recorded: Arc::new(Mutex::new(Vec::new())),
            _shutdown: shutdown_tx,
        }
    }

    /// Snapshot of all envelopes received from the client so far.
    pub fn recorded(&self) -> Vec<serde_json::Value> {
        self.recorded.lock().unwrap().clone()
    }
}

/// Encode a slice of PCM16 samples as the base64 string the mock
/// expects on `audio_append.audio`. Useful for tests that assert on
/// recorded uplinked audio.
pub fn encode_pcm16(samples: &[i16]) -> String {
    // Equal rates: no resampling, only the little-endian base64 encoding.
    voice_pipes_sdk::s2s::codec::encode_pcm16_b64(samples)
}
