use super::*;

#[test]
fn binary_header_roundtrip() {
    let header = BinaryHeader::audio_pcm(42, true, false);
    let payload = b"hello";
    let encoded = header.encode_with_payload(payload);
    let (decoded, decoded_payload) = BinaryHeader::decode(&encoded).unwrap();
    assert_eq!(decoded, header);
    assert_eq!(decoded_payload, payload);
}

#[test]
fn binary_header_flags() {
    let h1 = BinaryHeader::audio_pcm(0, true, true);
    assert!(h1.is_end_of_utterance());
    assert!(h1.is_end_of_response());

    let h2 = BinaryHeader::audio_pcm(0, false, false);
    assert!(!h2.is_end_of_utterance());
    assert!(!h2.is_end_of_response());
}

#[test]
fn binary_header_too_short() {
    let result = BinaryHeader::decode(&[0x01, 0x00]);
    assert!(result.is_err());
}

#[test]
fn json_envelope_roundtrip() {
    let env = JsonEnvelope::new("text", serde_json::json!({"text": "hello"}));
    let encoded = env.encode().unwrap();
    let decoded = JsonEnvelope::decode(&encoded).unwrap();
    assert_eq!(decoded.msg_type, "text");
    assert_eq!(decoded.payload["text"], "hello");
}

#[test]
fn json_envelope_invalid() {
    let result = JsonEnvelope::decode("not json at all {{{");
    assert!(result.is_err());
}

#[test]
fn pcm_i16_roundtrip() {
    let samples: Vec<i16> = vec![-32768, 0, 32767, 1234, -5678];
    let encoded = encode_pcm_i16(&samples);
    let decoded = decode_pcm_i16(&encoded).unwrap();
    assert_eq!(decoded, samples);
}

#[test]
fn pcm_i16_empty() {
    let samples: Vec<i16> = vec![];
    let encoded = encode_pcm_i16(&samples);
    let decoded = decode_pcm_i16(&encoded).unwrap();
    assert_eq!(decoded, samples);
}

#[test]
fn pcm_i16_odd_bytes() {
    let result = decode_pcm_i16(&[0x01, 0x02, 0x03]);
    assert!(result.is_err());
}

#[test]
fn binary_header_seq_num_max() {
    let header = BinaryHeader::audio_pcm(u32::MAX, false, false);
    let encoded = header.encode_with_payload(&[]);
    let (decoded, _) = BinaryHeader::decode(&encoded).unwrap();
    assert_eq!(decoded.seq_num, u32::MAX);
}
