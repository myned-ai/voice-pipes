//! Pure-Rust constant-time HMAC-SHA1 (for Twilio `X-Twilio-Signature`) and
//! HMAC-SHA256 (for single-use stream tokens) with zero extra crate dependencies.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

/// Constant-time byte slice equality (prevents timing side-channels).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Pure-Rust RFC 3174 SHA-1 (used exclusively for Twilio's legacy
/// `X-Twilio-Signature` HMAC-SHA1 webhook verification).
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut schedule = [0u32; 80];
        for (slot, word_bytes) in schedule.iter_mut().take(16).zip(chunk.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*word_bytes);
        }
        for idx in 16..80 {
            let val = schedule.get(idx - 3).copied().unwrap_or(0)
                ^ schedule.get(idx - 8).copied().unwrap_or(0)
                ^ schedule.get(idx - 14).copied().unwrap_or(0)
                ^ schedule.get(idx - 16).copied().unwrap_or(0);
            if let Some(slot) = schedule.get_mut(idx) {
                *slot = val.rotate_left(1);
            }
        }

        let (mut s0, mut s1, mut s2, mut s3, mut s4) = (h0, h1, h2, h3, h4);
        for (idx, &wi) in schedule.iter().enumerate() {
            let (round_f, round_k) = match idx {
                0..=19 => ((s1 & s2) | ((!s1) & s3), 0x5A82_7999u32),
                20..=39 => (s1 ^ s2 ^ s3, 0x6ED9_EBA1u32),
                40..=59 => ((s1 & s2) | (s1 & s3) | (s2 & s3), 0x8F1B_BCDC_u32),
                _ => (s1 ^ s2 ^ s3, 0xCA62_C1D6_u32),
            };
            let temp = s0
                .rotate_left(5)
                .wrapping_add(round_f)
                .wrapping_add(s4)
                .wrapping_add(round_k)
                .wrapping_add(wi);
            s4 = s3;
            s3 = s2;
            s2 = s1.rotate_left(30);
            s1 = s0;
            s0 = temp;
        }

        h0 = h0.wrapping_add(s0);
        h1 = h1.wrapping_add(s1);
        h2 = h2.wrapping_add(s2);
        h3 = h3.wrapping_add(s3);
        h4 = h4.wrapping_add(s4);
    }

    let mut out = [0u8; 20];
    for (dest, word) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip([h0, h1, h2, h3, h4])
    {
        *dest = word.to_be_bytes();
    }
    out
}

/// RFC 2104 HMAC-SHA1.
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    const BLOCK: usize = 64;
    let mut k_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = sha1(key);
        if let Some(dst) = k_block.get_mut(..20) {
            dst.copy_from_slice(&hashed);
        }
    } else if let Some(dst) = k_block.get_mut(..key.len()) {
        dst.copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5Cu8; BLOCK];
    for i in 0..BLOCK {
        if let (Some(k), Some(ip), Some(op)) = (k_block.get(i), ipad.get_mut(i), opad.get_mut(i)) {
            *ip ^= *k;
            *op ^= *k;
        }
    }

    let mut inner = Vec::with_capacity(BLOCK + data.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(data);
    let inner_hash = sha1(&inner);

    let mut outer = Vec::with_capacity(BLOCK + 20);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_hash);
    sha1(&outer)
}

/// RFC 2104 HMAC-SHA256 using `sha2::Sha256`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = Sha256::digest(key);
        if let Some(dst) = k_block.get_mut(..32) {
            dst.copy_from_slice(&hashed);
        }
    } else if let Some(dst) = k_block.get_mut(..key.len()) {
        dst.copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5Cu8; BLOCK];
    for i in 0..BLOCK {
        if let (Some(k), Some(ip), Some(op)) = (k_block.get(i), ipad.get_mut(i), opad.get_mut(i)) {
            *ip ^= *k;
            *op ^= *k;
        }
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let digest = outer.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Derive a domain-separated 32-byte stream-token signing key from a secret.
pub fn derive_stream_key(secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"voice-pipes-twilio-stream-token-v1:");
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Compute Twilio's `X-Twilio-Signature` (Base64-encoded HMAC-SHA1 over
/// `canonical_url` concatenated with lexicographically sorted `key + value` pairs).
pub fn compute_twilio_signature(
    auth_token: &str,
    canonical_url: &str,
    params: &std::collections::HashMap<String, String>,
) -> String {
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();

    let mut payload = String::from(canonical_url);
    for k in keys {
        payload.push_str(k);
        if let Some(v) = params.get(k) {
            payload.push_str(v);
        }
    }
    let mac = hmac_sha1(auth_token.as_bytes(), payload.as_bytes());
    STANDARD.encode(mac)
}

/// Verify Twilio's `X-Twilio-Signature` header in constant time.
pub fn verify_twilio_signature(
    auth_token: &str,
    canonical_url: &str,
    params: &std::collections::HashMap<String, String>,
    provided_signature: &str,
) -> bool {
    let expected = compute_twilio_signature(auth_token, canonical_url, params);
    ct_eq(expected.as_bytes(), provided_signature.trim().as_bytes())
}

/// Encode bytes as URL-safe base64 without padding.
pub fn b64url_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

/// Decode URL-safe base64 without padding.
pub fn b64url_decode(encoded: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(encoded.as_bytes()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn twilio_official_signature_vector() {
        // Official Twilio documentation validation example:
        // https://www.twilio.com/docs/usage/webhooks/webhooks-security
        let auth_token = "12345";
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        let mut params = HashMap::new();
        params.insert("CallSid".to_string(), "CA1234567890ABCDE".to_string());
        params.insert("Caller".to_string(), "+14158675310".to_string());
        params.insert("Digits".to_string(), "1234".to_string());
        params.insert("From".to_string(), "+14158675310".to_string());
        params.insert("To".to_string(), "+18005551212".to_string());

        let sig = compute_twilio_signature(auth_token, url, &params);
        assert_eq!(sig, "GvWf1cFY/Q7PnoempGyD5oXAezc=");
        assert!(verify_twilio_signature(auth_token, url, &params, &sig));
        assert!(!verify_twilio_signature(
            auth_token,
            "https://evil.com/myapp.php?foo=1&bar=2",
            &params,
            &sig
        ));
    }

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            let _ = write!(&mut out, "{b:02x}");
        }
        out
    }

    #[test]
    fn hmac_sha1_rfc2202_case1() {
        let key = [0x0bu8; 20];
        let mac = hmac_sha1(&key, b"Hi There");
        assert_eq!(to_hex(&mac), "b617318655057264e28bc0b6fb378c8ef146be00");
    }

    #[test]
    fn hmac_sha256_rfc4231_case1() {
        let key = [0x0bu8; 20];
        let data = b"Hi There";
        let mac = hmac_sha256(&key, data);
        assert_eq!(
            to_hex(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
}
