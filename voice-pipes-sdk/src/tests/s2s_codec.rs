//! Unit tests for [`super::encode_pcm16_b64`] / [`super::decode_pcm16_b64`].
//!
//! If these break, the Realtime VAD will hear silence or garbage.

use super::*;

/// The decoder refuses a field above the cap before touching base64, so
/// one frame cannot force a multi-megabyte decode.
#[test]
fn decode_refuses_oversized_field() {
    let huge = "A".repeat(MAX_AUDIO_B64_BYTES + 1);
    let err = decode_pcm16_b64(&huge, 24_000).unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");
    // Exactly at the cap is fine (valid base64 of zeros).
    let at_cap = "A".repeat(MAX_AUDIO_B64_BYTES);
    assert!(decode_pcm16_b64(&at_cap, 24_000).is_ok());
}

/// A source rate outside the known set is refused: the engine resamples
/// the audio from the rate the provider names, so `0` would divide by zero
/// and `1` would size gigabytes.
#[test]
fn decode_refuses_unknown_source_rate() {
    let b64 = BASE64.encode([0u8; 64]);
    for rate in [0u32, 1, 12_345, 4_000_000_000] {
        let err = decode_pcm16_b64(&b64, rate).unwrap_err();
        assert!(err.to_string().contains("unsupported"), "{rate}: {err}");
    }
    for rate in SUPPORTED_SAMPLE_RATES {
        assert!(decode_pcm16_b64(&b64, *rate).is_ok(), "{rate}");
    }
}

#[test]
fn encode_empty_input_yields_empty_string() {
    assert_eq!(encode_pcm16_b64(&[]), "");
}

/// Encoding and decoding change nothing but the representation: the
/// samples and their count come back as they went in.
#[test]
fn samples_round_trip_unchanged() {
    let samples: Vec<i16> = (0..480).map(|i| (i * 37 % 2000 - 1000) as i16).collect();
    let b64 = encode_pcm16_b64(&samples);
    assert_eq!(decode_pcm16_b64(&b64, 24_000).unwrap(), samples);
    assert_eq!(BASE64.decode(&b64).unwrap().len(), samples.len() * 2);
}

#[test]
fn url_query_encode_passes_ascii_identifiers() {
    assert_eq!(
        url_query_encode("gpt-4o-realtime-preview-2024-10-01"),
        "gpt-4o-realtime-preview-2024-10-01"
    );
    assert_eq!(url_query_encode("a_b.c~d"), "a_b.c~d");
}

/// A vendor's count is read as it is, saturates past `u32`, and is 0 when
/// absent, negative, fractional or not a number.
#[test]
fn count_u32_saturates_and_defaults_to_zero() {
    let v = serde_json::json!({
        "n": 7,
        "huge": u64::MAX,
        "neg": -3,
        "frac": 1.5,
        "text": "12",
    });
    assert_eq!(count_u32(v.get("n")), 7);
    assert_eq!(count_u32(v.get("huge")), u32::MAX);
    assert_eq!(count_u32(v.get("neg")), 0);
    assert_eq!(count_u32(v.get("frac")), 0);
    assert_eq!(count_u32(v.get("text")), 0);
    assert_eq!(count_u32(v.get("missing")), 0);
}

#[test]
fn url_query_encode_escapes_reserved_characters() {
    assert_eq!(url_query_encode("a/b"), "a%2Fb");
    assert_eq!(url_query_encode("x y"), "x%20y");
    assert_eq!(url_query_encode("?&="), "%3F%26%3D");
}
