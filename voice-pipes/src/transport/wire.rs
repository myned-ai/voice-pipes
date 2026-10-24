//! Wire protocol -- binary frame headers and JSON message envelopes.
//!
//! Defines the on-the-wire encoding shared by all frame-based
//! transports (WebSocket binary frames, WebTransport datagrams).
//!
//! # Binary frame layout
//!
//! ```text
//! ┌──────────┬──────────┬──────────┬──────────────────────┐
//! │ msg_type │ flags    │ seq_num  │ payload              │
//! │ 1 byte   │ 1 byte   │ 4 bytes  │ variable             │
//! │          │          │ (LE u32) │                      │
//! └──────────┴──────────┴──────────┴──────────────────────┘
//! ```

use serde::{Deserialize, Serialize};

use super::error::TransportError;

/// Minimum size of a binary frame header in bytes.
const HEADER_SIZE: usize = 6;

// ── Binary frame types (msg_type byte) ──────────────────────

/// Audio encoded as PCM signed 16-bit little-endian.
pub const MSG_TYPE_AUDIO_PCM_I16: u8 = 0x01;
/// Audio encoded as Opus packets.
///
/// Reserved for transports/sessions that negotiate `AudioFormat::Opus`.
/// WebSocket currently only decodes PCM in the main session path.
pub const MSG_TYPE_AUDIO_OPUS: u8 = 0x02;

// Flag bits within the `flags` byte.

/// Bit 0: this chunk ends the current utterance.
pub const FLAG_END_OF_UTTERANCE: u8 = 0x01;
/// Bit 1: this chunk ends the full server response. Reserved: the
/// serve codec currently always sends this bit cleared (clients infer
/// end-of-response from control messages); only the bench synthetic
/// callers read it on injected audio.
pub const FLAG_END_OF_RESPONSE: u8 = 0x02;

/// Decoded binary frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryHeader {
    /// Message type discriminator.
    pub msg_type: u8,
    /// Bit-flags (end-of-utterance, end-of-response).
    pub flags: u8,
    /// Monotonically increasing sequence number per direction.
    pub seq_num: u32,
}

impl BinaryHeader {
    /// Create a header for a PCM i16 audio frame.
    pub fn audio_pcm(seq_num: u32, is_end_of_utterance: bool, is_end_of_response: bool) -> Self {
        let mut flags = 0u8;
        if is_end_of_utterance {
            flags |= FLAG_END_OF_UTTERANCE;
        }
        if is_end_of_response {
            flags |= FLAG_END_OF_RESPONSE;
        }
        Self {
            msg_type: MSG_TYPE_AUDIO_PCM_I16,
            flags,
            seq_num,
        }
    }

    /// Whether the end-of-utterance flag is set.
    pub fn is_end_of_utterance(self) -> bool {
        self.flags & FLAG_END_OF_UTTERANCE != 0
    }

    /// Whether the end-of-response flag is set.
    pub fn is_end_of_response(self) -> bool {
        self.flags & FLAG_END_OF_RESPONSE != 0
    }

    /// Encode this header into a byte buffer, followed by `payload`.
    pub fn encode_with_payload(self, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE + payload.len());
        buf.push(self.msg_type);
        buf.push(self.flags);
        buf.extend_from_slice(&self.seq_num.to_le_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    /// Decode a header from a byte slice, returning the header and payload.
    pub fn decode(data: &[u8]) -> Result<(Self, &[u8]), TransportError> {
        if data.len() < HEADER_SIZE {
            return Err(TransportError::Codec(format!(
                "binary frame too short: {} bytes, need at least {HEADER_SIZE}",
                data.len()
            )));
        }
        let msg_type = data[0];
        let flags = data[1];
        let seq_num = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
        let payload = &data[HEADER_SIZE..];
        Ok((
            Self {
                msg_type,
                flags,
                seq_num,
            },
            payload,
        ))
    }
}

// ── JSON message envelope ───────────────────────────────────

/// JSON envelope for control, text, and data messages on the wire.
///
/// Sent as WebSocket text frames or WebTransport reliable-stream messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonEnvelope {
    /// Message type discriminator.
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Optional unique message identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Epoch-millisecond timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
    /// Type-specific payload.
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl JsonEnvelope {
    /// Create an envelope with the given type and payload.
    pub fn new(msg_type: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            msg_type: msg_type.into(),
            message_id: None,
            timestamp_ms: None,
            payload,
        }
    }

    /// Encode this envelope to a JSON string.
    pub fn encode(&self) -> Result<String, TransportError> {
        serde_json::to_string(self)
            .map_err(|e| TransportError::Codec(format!("JSON encode failed: {e}")))
    }

    /// Decode an envelope from a JSON string.
    pub fn decode(data: &str) -> Result<Self, TransportError> {
        // The serde message quotes the offending value, which can be what
        // the caller typed; keep only where the parse failed.
        serde_json::from_str(data).map_err(|e| {
            TransportError::Codec(format!(
                "JSON decode failed: {}",
                voice_pipes_sdk::secrets::json_error(&e)
            ))
        })
    }
}

// ── Encode/decode helpers for PCM i16 audio ─────────────────

/// Encode a slice of `i16` samples as raw little-endian bytes.
pub fn encode_pcm_i16(samples: &[i16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    buf
}

/// Decode raw little-endian bytes into `i16` samples.
pub fn decode_pcm_i16(data: &[u8]) -> Result<Vec<i16>, TransportError> {
    if !data.len().is_multiple_of(2) {
        return Err(TransportError::Codec(format!(
            "PCM i16 data has odd byte count: {}",
            data.len()
        )));
    }
    let samples = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();
    Ok(samples)
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/wire.rs"]
mod tests;
