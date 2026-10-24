//! WebSocket frame encoding and decoding.
//!
//! Maps between [`ServerMessage`] / [`ClientMessage`] and axum
//! WebSocket [`Message`] frames (text for JSON, binary for audio).

use axum::extract::ws::Message;

use crate::pipeline::AudioChunk;
use crate::transport::error::TransportError;
use crate::transport::wire::{self, BinaryHeader, JsonEnvelope};
use crate::transport::{ClientControl, ClientMessage, PipelineMessage, ServerMessage};

// ── Client -> Server (decoding) ──────────────────────────────

/// Maximum accepted inbound client frame, in bytes. Real client
/// audio chunks are ~10 KiB (100 ms of 48 kHz PCM); anything near this
/// limit is hostile or broken. Also applied at the axum WS layer
/// (`max_message_size` on the upgrade) so oversized messages are refused
/// before they are ever buffered.
pub const MAX_CLIENT_MESSAGE_BYTES: usize = 1024 * 1024;

/// Decode an axum WebSocket [`Message`] into a [`ClientMessage`].
///
/// - Binary frames are decoded as audio (wire header + PCM payload).
/// - Text frames are decoded as JSON envelopes (control/text/data).
/// - Ping/Pong/Close are handled at a higher level; this function
///   returns `None` for them.
/// - Frames over [`MAX_CLIENT_MESSAGE_BYTES`] are rejected with
///   [`TransportError::MessageTooLarge`].
pub fn decode_client_message(msg: Message) -> Result<Option<ClientMessage>, TransportError> {
    let size = match &msg {
        Message::Binary(bytes) => bytes.len(),
        Message::Text(text) => text.len(),
        _ => 0,
    };
    if size > MAX_CLIENT_MESSAGE_BYTES {
        return Err(TransportError::MessageTooLarge {
            size,
            max: MAX_CLIENT_MESSAGE_BYTES,
        });
    }
    match msg {
        Message::Binary(bytes) => decode_client_binary(&bytes).map(Some),
        Message::Text(text) => decode_client_text(&text).map(Some),
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) => Ok(None),
    }
}

/// Decode a binary WebSocket frame into a [`ClientMessage`].
fn decode_client_binary(bytes: &[u8]) -> Result<ClientMessage, TransportError> {
    let (header, payload) = BinaryHeader::decode(bytes)?;

    match header.msg_type {
        wire::MSG_TYPE_AUDIO_PCM_I16 => {
            let samples = wire::decode_pcm_i16(payload)?;
            Ok(ClientMessage::Audio(AudioChunk {
                samples,
                sample_rate: super::super::DEFAULT_SAMPLE_RATE,
                channels: 1,
                is_end_of_utterance: header.is_end_of_utterance(),
                caption: None,
                utterance_id: 0,
                full_text: None,
                extras: None,
                last_speech_at: None,
                synth_duration: None,
            }))
        }
        other => Err(TransportError::Codec(format!(
            "unknown binary message type: 0x{other:02x}"
        ))),
    }
}

/// Decode a JSON text frame into a [`ClientMessage`].
fn decode_client_text(text: &str) -> Result<ClientMessage, TransportError> {
    let envelope = JsonEnvelope::decode(text)?;

    match envelope.msg_type.as_str() {
        "message" => {
            let msg: PipelineMessage = serde_json::from_value(envelope.payload)
                .map_err(|e| TransportError::Codec(format!("message payload: {e}")))?;
            Ok(ClientMessage::Message(msg))
        }
        "control" => {
            let ctrl: ClientControl = serde_json::from_value(envelope.payload)
                .map_err(|e| TransportError::Codec(format!("control payload: {e}")))?;
            Ok(ClientMessage::Control(ctrl))
        }
        other => Err(TransportError::Codec(format!(
            "unknown message type: {other}"
        ))),
    }
}

// ── Server -> Client (encoding) ──────────────────────────────

/// Encode a [`ServerMessage`] into an axum WebSocket [`Message`].
///
/// - Audio -> binary frame (wire header + PCM).  `seq` is the per-session
///   monotonic counter and is only used for Audio frames; pass `0` for
///   non-audio messages or in tests.
/// - Everything else -> JSON text frame.
pub fn encode_server_message(msg: &ServerMessage, seq: u32) -> Result<Message, TransportError> {
    match msg {
        ServerMessage::Audio(chunk) => {
            let header = BinaryHeader::audio_pcm(
                seq,
                chunk.is_end_of_utterance,
                false, // is_end_of_response -- set when we know
            );
            let pcm = wire::encode_pcm_i16(&chunk.samples);
            let frame = header.encode_with_payload(&pcm);
            Ok(Message::Binary(frame.into()))
        }
        ServerMessage::Message(msg) => {
            let envelope = JsonEnvelope {
                msg_type: "message".into(),
                message_id: Some(msg.message_id.clone()),
                timestamp_ms: Some(now_epoch_ms()),
                payload: serde_json::to_value(msg)
                    .map_err(|e| TransportError::Codec(e.to_string()))?,
            };
            let json = envelope.encode()?;
            Ok(Message::Text(json.into()))
        }
        ServerMessage::Control(ctrl) => {
            // ServerControl uses `#[serde(tag = "action")]`, so serialising
            // the whole enum produces `{"action": "state_changed", ...}` etc.
            // Wrap under type "control" so the client gets a single envelope
            // format: `{"type": "control", "payload": {"action": "...", ...}}`.
            let payload =
                serde_json::to_value(ctrl).map_err(|e| TransportError::Codec(e.to_string()))?;
            let envelope = JsonEnvelope {
                msg_type: "control".into(),
                message_id: None,
                timestamp_ms: Some(now_epoch_ms()),
                payload,
            };
            let json = envelope.encode()?;
            Ok(Message::Text(json.into()))
        }
        ServerMessage::Annotation(a) => {
            // Off-path metadata: its own envelope
            // type so clients can route it without touching audio_meta.
            let payload = serde_json::json!({
                "utterance_id": a.utterance_id,
                "start_ms": a.start_ms,
                "duration_ms": a.duration_ms,
                "kind": a.kind,
                "extras": a.extras,
                "server_timestamp_ms": now_epoch_ms(),
            });
            let envelope = JsonEnvelope {
                msg_type: "audio_annotation".into(),
                message_id: None,
                timestamp_ms: Some(now_epoch_ms()),
                payload,
            };
            let json = envelope.encode()?;
            Ok(Message::Text(json.into()))
        }
    }
}

/// Current time as epoch milliseconds.
fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/codec.rs"]
mod tests;
