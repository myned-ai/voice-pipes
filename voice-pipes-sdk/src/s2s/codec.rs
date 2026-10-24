//! Shared base64 / PCM16 helpers used by every
//! [`RealtimeProtocol`](super::protocol::RealtimeProtocol)
//! implementation, and [`count_u32`] for the counts their frames carry.
//!
//! They encode and decode only. Audio crosses the wire in pieces a few
//! milliseconds long, and resampling each piece on its own drifts and
//! restarts the filter at every edge, so a provider hands over audio at
//! the vendor's rate and the engine resamples the session's audio as one
//! stream in each direction.

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

/// Percent-encode an arbitrary string for use in a query parameter.
///
/// Kept dependency-free because realtime model IDs and API keys are
/// ASCII -- this avoids dragging in `percent-encoding` just to encode
/// a handful of bytes.
pub fn url_query_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// A count a vendor's frame carries (tokens used, a rate limit), as the
/// `u32` the SDK's events hold: 0 when the field is absent or not a
/// non-negative integer, and `u32::MAX` for a count past it, so a bad or
/// huge value is never wrapped.
///
/// ```
/// use voice_pipes_sdk::s2s::codec::count_u32;
///
/// let usage = serde_json::json!({ "input_tokens": 42, "output_tokens": -1 });
/// assert_eq!(count_u32(usage.get("input_tokens")), 42);
/// assert_eq!(count_u32(usage.get("output_tokens")), 0);
/// assert_eq!(count_u32(usage.get("total_tokens")), 0);
/// ```
pub fn count_u32(value: Option<&serde_json::Value>) -> u32 {
    value
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// Base64 of a mono PCM16 buffer's little-endian bytes, already at the
/// provider's rate.
///
/// Returns an empty string for empty input so callers can short-circuit
/// without a branch.
pub fn encode_pcm16_b64(samples: &[i16]) -> String {
    BASE64.encode(crate::audio::i16_to_le_bytes(samples))
}

/// Largest base64 audio field decoded from a provider frame. Providers
/// send audio in deltas of a few tens of KB; this is a bound on the
/// allocation one frame can force, not a working size.
pub const MAX_AUDIO_B64_BYTES: usize = 1024 * 1024;

/// Sample rates a provider may announce for its audio. Anything else is
/// refused before it can size a resampler buffer.
pub const SUPPORTED_SAMPLE_RATES: &[u32] = &[8_000, 16_000, 22_050, 24_000, 44_100, 48_000];

/// Decode a base64-encoded little-endian PCM16 buffer sent at
/// `src_rate`; the samples come back at that rate. Refuses a field above
/// [`MAX_AUDIO_B64_BYTES`] and a rate outside [`SUPPORTED_SAMPLE_RATES`]
/// (zero included), so the rate a provider puts on its audio events is
/// always one the engine can resample.
pub fn decode_pcm16_b64(b64: &str, src_rate: u32) -> Result<Vec<i16>> {
    if b64.len() > MAX_AUDIO_B64_BYTES {
        anyhow::bail!(
            "audio field of {} bytes exceeds the {} byte limit",
            b64.len(),
            MAX_AUDIO_B64_BYTES
        );
    }
    if !SUPPORTED_SAMPLE_RATES.contains(&src_rate) {
        anyhow::bail!("unsupported source sample rate {src_rate} Hz");
    }
    let bytes = BASE64.decode(b64)?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect())
}

#[cfg(test)]
#[path = "../tests/s2s_codec.rs"]
mod tests;
