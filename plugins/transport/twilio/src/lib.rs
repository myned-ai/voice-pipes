//! Isolated Twilio Media Streams transport plugin for Voice Pipes.
//!
//! Depends **only** on `voice-pipes-sdk` (never on `voice-pipes` core).
//! Implements:
//! - Bidirectional Twilio Media Streams JSON-over-WebSocket framing
//!   (`connected`, `start`, `media`, `mark`, `dtmf`, `stop`, outbound
//!   `media`, `mark`, `clear`)
//! - Constant-time `X-Twilio-Signature` HMAC-SHA1 webhook validation over a
//!   server-configured canonical URL
//! - Static, XML-escaped `TwiML` `<Connect><Stream>` generator
//! - Single-use, short-lived HMAC-SHA256 stream tokens bound to `(CallSid,
//!   tenant_id, nonce, exp)` with a bounded replay cache

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

mod crypto;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use voice_pipes_sdk::media_stream::{
    DialectError, DialectInbound, DialectStart, MediaStreamDialect, MediaStreamDialectFactory,
    MediaStreamEncoding, MediaStreamFrame, WebhookError, WebhookRequest, WebhookResponse,
};

const DEFAULT_TOKEN_TTL_SECS: u64 = 30;
const MAX_REPLAY_CACHE_ENTRIES: usize = 16_384;

// ── Configuration ───────────────────────────────────────────

/// Configuration under `[transport.media_streams.twilio]`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TwilioConfig {
    /// Twilio Account Auth Token supplied by the host configuration.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Canonical public HTTPS URL configured in the Twilio Console for the
    /// `TwiML` webhook (never derived from untrusted `Host` headers).
    #[serde(default)]
    pub public_webhook_url: Option<String>,
    /// Static `wss://` WebSocket URL embedded in the `TwiML` `<Stream>` verb.
    #[serde(default)]
    pub stream_ws_url: Option<String>,
    /// Optional dedicated secret for signing short-lived stream tokens.
    /// Defaults to a domain-separated key derived from `auth_token`.
    #[serde(default)]
    pub stream_signing_secret: Option<String>,
    /// Default tenant identifier attached to calls handled by this webhook.
    #[serde(default)]
    pub default_tenant_id: Option<String>,
    /// Stream token TTL in seconds (default: 30 s).
    #[serde(default = "default_token_ttl_secs")]
    pub token_ttl_secs: u64,
    /// Fail-closed by default: require a valid single-use stream token in
    /// `start.customParameters.auth`.
    #[serde(default = "default_require_auth")]
    pub require_auth: bool,
}

fn default_token_ttl_secs() -> u64 {
    DEFAULT_TOKEN_TTL_SECS
}

fn default_require_auth() -> bool {
    true
}

impl Default for TwilioConfig {
    fn default() -> Self {
        Self {
            auth_token: None,
            public_webhook_url: None,
            stream_ws_url: None,
            stream_signing_secret: None,
            default_tenant_id: None,
            token_ttl_secs: DEFAULT_TOKEN_TTL_SECS,
            require_auth: true,
        }
    }
}

impl TwilioConfig {
    fn from_json(val: &serde_json::Value) -> Result<Self, DialectError> {
        if val.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(val.clone())
            .map_err(|e| DialectError::Protocol(format!("invalid twilio config: {e}")))
    }

    fn effective_auth_token(&self) -> Option<String> {
        self.auth_token.clone().filter(|s| !s.trim().is_empty())
    }

    fn signing_key(&self) -> Option<[u8; 32]> {
        let raw = self
            .stream_signing_secret
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.effective_auth_token())?;
        Some(crypto::derive_stream_key(&raw))
    }
}

// ── Single-Use Stream Token & Replay Cache ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StreamTokenClaims {
    call_sid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<String>,
    exp: u64,
    nonce: String,
}

/// Thread-safe bounded TTL replay cache for single-use stream token nonces.
#[derive(Debug, Default)]
struct ReplayCache {
    seen: Mutex<HashMap<String, Instant>>,
}

impl ReplayCache {
    fn consume(&self, nonce: &str, ttl: Duration) -> bool {
        let now = Instant::now();
        let mut guard = self.seen.lock();
        guard.retain(|_, expires_at| *expires_at > now);
        if guard.contains_key(nonce) {
            return false;
        }
        if guard.len() >= MAX_REPLAY_CACHE_ENTRIES {
            // Fail closed if cache is saturated under an attack.
            return false;
        }
        guard.insert(nonce.to_string(), now + ttl);
        true
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Mint a signed, single-use stream token bound to `call_sid` and optional `tenant_id`.
pub(crate) fn mint_stream_token(
    key: &[u8; 32],
    call_sid: &str,
    tenant_id: Option<String>,
    ttl_secs: u64,
) -> Result<String, WebhookError> {
    let claims = StreamTokenClaims {
        call_sid: call_sid.to_string(),
        tenant_id,
        exp: now_unix_secs().saturating_add(ttl_secs.max(5)),
        nonce: uuid::Uuid::new_v4().to_string(),
    };
    let payload = serde_json::to_vec(&claims)
        .map_err(|e| WebhookError::Misconfigured(format!("token serialize failed: {e}")))?;
    let mac = crypto::hmac_sha256(key, &payload);
    Ok(format!(
        "{}.{}",
        crypto::b64url_encode(&payload),
        crypto::b64url_encode(&mac)
    ))
}

fn verify_stream_token(
    key: &[u8; 32],
    token: &str,
    expected_call_sid: Option<&str>,
    replay_cache: &ReplayCache,
    ttl_secs: u64,
) -> Result<StreamTokenClaims, DialectError> {
    let (payload_b64, sig_b64) = token
        .split_once('.')
        .ok_or_else(|| DialectError::AuthFailed("malformed stream token".to_string()))?;

    let payload = crypto::b64url_decode(payload_b64)
        .ok_or_else(|| DialectError::AuthFailed("invalid stream token payload".to_string()))?;
    let provided_mac = crypto::b64url_decode(sig_b64)
        .ok_or_else(|| DialectError::AuthFailed("invalid stream token signature".to_string()))?;

    let expected_mac = crypto::hmac_sha256(key, &payload);
    if !crypto::ct_eq(&expected_mac, &provided_mac) {
        return Err(DialectError::AuthFailed(
            "stream token signature mismatch".to_string(),
        ));
    }

    let claims: StreamTokenClaims = serde_json::from_slice(&payload).map_err(|_parse_err| {
        DialectError::AuthFailed("invalid stream token claims".to_string())
    })?;

    if now_unix_secs() > claims.exp {
        return Err(DialectError::AuthFailed("stream token expired".to_string()));
    }

    if let Some(call_sid) = expected_call_sid
        && claims.call_sid != call_sid
    {
        return Err(DialectError::AuthFailed(
            "stream token call_sid mismatch".to_string(),
        ));
    }

    let cache_ttl = Duration::from_secs(ttl_secs.max(30) + 10);
    if !replay_cache.consume(&claims.nonce, cache_ttl) {
        return Err(DialectError::AuthFailed(
            "stream token already used (replay rejected)".to_string(),
        ));
    }

    Ok(claims)
}

// ── Static XML-Escaped TwiML Generator ──────────────────────

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Build a static, XML-escaped `TwiML` `<Connect><Stream>` response.
/// Never interpolates untrusted caller fields (`From`, `To`, query params).
pub(crate) fn build_twiml_connect_stream(
    stream_ws_url: &str,
    auth_token: Option<&str>,
    tenant_id: Option<&str>,
) -> String {
    let escaped_url = xml_escape(stream_ws_url);
    let mut params = String::new();
    if let Some(tok) = auth_token {
        let escaped_tok = xml_escape(tok);
        let _ = write!(
            params,
            "\n      <Parameter name=\"auth\" value=\"{escaped_tok}\" />"
        );
    }
    if let Some(t) = tenant_id.map(str::trim).filter(|t| !t.is_empty()) {
        let escaped_tenant = xml_escape(t);
        let _ = write!(
            params,
            "\n      <Parameter name=\"tenant_id\" value=\"{escaped_tenant}\" />"
        );
    }
    if params.is_empty() {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <Response>\n  \
               <Connect>\n    \
                 <Stream url=\"{escaped_url}\" />\n  \
               </Connect>\n\
             </Response>"
        )
    } else {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <Response>\n  \
               <Connect>\n    \
                 <Stream url=\"{escaped_url}\">{params}\n    \
                 </Stream>\n  \
               </Connect>\n\
             </Response>"
        )
    }
}

// ── Wire Protocol Schemas ───────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum TwilioInbound {
    Connected,
    Start { start: TwilioStartPayload },
    Media { media: TwilioMediaPayload },
    Mark { mark: TwilioMarkPayload },
    Dtmf { dtmf: TwilioDtmfPayload },
    Stop,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TwilioStartPayload {
    stream_sid: String,
    #[serde(default)]
    call_sid: Option<String>,
    #[serde(default)]
    media_format: Option<TwilioMediaFormat>,
    #[serde(default)]
    custom_parameters: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TwilioMediaFormat {
    #[serde(default)]
    encoding: Option<String>,
    #[serde(default)]
    sample_rate: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TwilioMediaPayload {
    #[serde(default)]
    track: Option<String>,
    payload: String,
}

#[derive(Debug, Deserialize)]
struct TwilioMarkPayload {
    name: String,
}

#[derive(Debug, Deserialize)]
struct TwilioDtmfPayload {
    digit: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboundMedia<'a> {
    event: &'static str,
    stream_sid: &'a str,
    media: OutboundMediaInner<'a>,
}

#[derive(Debug, Serialize)]
struct OutboundMediaInner<'a> {
    payload: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboundClear<'a> {
    event: &'static str,
    stream_sid: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboundMark<'a> {
    event: &'static str,
    stream_sid: &'a str,
    mark: OutboundMarkInner<'a>,
}

#[derive(Debug, Serialize)]
struct OutboundMarkInner<'a> {
    name: &'a str,
}

// ── TwilioDialect ───────────────────────────────────────────

/// Per-connection Twilio Media Streams dialect.
#[derive(Debug)]
pub(crate) struct TwilioDialect {
    config: TwilioConfig,
    replay_cache: Arc<ReplayCache>,
    stream_sid: Option<String>,
    stopped: bool,
}

impl TwilioDialect {
    fn new(config: TwilioConfig, replay_cache: Arc<ReplayCache>) -> Self {
        Self {
            config,
            replay_cache,
            stream_sid: None,
            stopped: false,
        }
    }
}

impl MediaStreamDialect for TwilioDialect {
    fn name(&self) -> &'static str {
        "twilio"
    }

    fn decode_frame(&mut self, frame: MediaStreamFrame) -> Result<DialectInbound, DialectError> {
        let MediaStreamFrame::Text(text) = frame else {
            return Ok(DialectInbound::Ignore);
        };

        let parsed: TwilioInbound = serde_json::from_str(&text)
            .map_err(|e| DialectError::InvalidFrame(format!("invalid twilio json: {e}")))?;

        match parsed {
            TwilioInbound::Connected => Ok(DialectInbound::Ignore),
            TwilioInbound::Start { start } => {
                if start.stream_sid.trim().is_empty() {
                    return Err(DialectError::InvalidFrame("empty streamSid".to_string()));
                }
                let sample_rate = start
                    .media_format
                    .as_ref()
                    .and_then(|m| m.sample_rate)
                    .unwrap_or(8_000);
                let encoding = match start
                    .media_format
                    .as_ref()
                    .and_then(|m| m.encoding.as_deref())
                {
                    Some("audio/x-alaw") => MediaStreamEncoding::Alaw,
                    Some("audio/l16" | "audio/pcm") => MediaStreamEncoding::Pcm16Le,
                    _ => MediaStreamEncoding::Mulaw,
                };

                let auth_token = start.custom_parameters.get("auth").cloned();
                let tenant_id = start.custom_parameters.get("tenant_id").cloned();
                let external_id = start
                    .custom_parameters
                    .get("external_id")
                    .cloned()
                    .or_else(|| start.call_sid.clone());

                let mut sanitized_params = start.custom_parameters;
                // Never propagate raw auth tokens in custom_params metadata.
                sanitized_params.remove("auth");

                self.stream_sid = Some(start.stream_sid.clone());

                Ok(DialectInbound::Start(
                    DialectStart::new(start.stream_sid, sample_rate, encoding)
                        .with_call_id(start.call_sid)
                        .with_auth_token(auth_token)
                        .with_tenant_id(tenant_id)
                        .with_external_id(external_id)
                        .with_custom_params(sanitized_params),
                ))
            }
            TwilioInbound::Media { media } => {
                // Only process caller (inbound) track audio; ignore outbound echoes.
                if let Some(track) = media.track.as_deref()
                    && track != "inbound"
                    && track != "inbound_track"
                {
                    return Ok(DialectInbound::Ignore);
                }
                let bytes = STANDARD.decode(media.payload.as_bytes()).map_err(|e| {
                    DialectError::InvalidFrame(format!("invalid base64 media payload: {e}"))
                })?;
                Ok(DialectInbound::Audio(bytes))
            }
            TwilioInbound::Mark { mark } => Ok(DialectInbound::MarkAck(mark.name)),
            TwilioInbound::Dtmf { dtmf } => {
                // Never log DTMF digits at INFO/WARN.
                debug!("twilio dtmf frame decoded");
                Ok(DialectInbound::Dtmf(dtmf.digit))
            }
            TwilioInbound::Stop => {
                self.stopped = true;
                Ok(DialectInbound::Stop)
            }
        }
    }

    fn verify_start_auth(&self, start: &DialectStart) -> Result<(), DialectError> {
        if !self.config.require_auth {
            return Ok(());
        }
        let key = self.config.signing_key().ok_or_else(|| {
            DialectError::AuthFailed(
                "twilio stream auth is required but no signing secret or TWILIO_AUTH_TOKEN is configured"
                    .to_string(),
            )
        })?;
        let token = start.auth_token.as_deref().ok_or_else(|| {
            DialectError::AuthFailed("missing auth parameter in Twilio start event".to_string())
        })?;
        let _claims = verify_stream_token(
            &key,
            token,
            start.call_id.as_deref(),
            &self.replay_cache,
            self.config.token_ttl_secs,
        )?;
        Ok(())
    }

    fn encode_audio(&self, payload: &[u8]) -> Result<Option<MediaStreamFrame>, DialectError> {
        if self.stopped || payload.is_empty() {
            return Ok(None);
        }
        let Some(ref stream_sid) = self.stream_sid else {
            return Ok(None);
        };
        let b64 = STANDARD.encode(payload);
        let msg = OutboundMedia {
            event: "media",
            stream_sid,
            media: OutboundMediaInner { payload: &b64 },
        };
        let json = serde_json::to_string(&msg)
            .map_err(|e| DialectError::Protocol(format!("serialize media failed: {e}")))?;
        Ok(Some(MediaStreamFrame::Text(json)))
    }

    fn encode_clear(&self) -> Result<Option<MediaStreamFrame>, DialectError> {
        if self.stopped {
            return Ok(None);
        }
        let Some(ref stream_sid) = self.stream_sid else {
            return Ok(None);
        };
        let msg = OutboundClear {
            event: "clear",
            stream_sid,
        };
        let json = serde_json::to_string(&msg)
            .map_err(|e| DialectError::Protocol(format!("serialize clear failed: {e}")))?;
        Ok(Some(MediaStreamFrame::Text(json)))
    }

    fn encode_mark(&self, name: &str) -> Result<Option<MediaStreamFrame>, DialectError> {
        if self.stopped {
            return Ok(None);
        }
        let Some(ref stream_sid) = self.stream_sid else {
            return Ok(None);
        };
        let msg = OutboundMark {
            event: "mark",
            stream_sid,
            mark: OutboundMarkInner { name },
        };
        let json = serde_json::to_string(&msg)
            .map_err(|e| DialectError::Protocol(format!("serialize mark failed: {e}")))?;
        Ok(Some(MediaStreamFrame::Text(json)))
    }

    fn frame_duration_ms(&self) -> u32 {
        20
    }

    fn max_frame_bytes(&self) -> usize {
        16 * 1024
    }
}

// ── Factory & Registration ──────────────────────────────────

/// Factory for the Twilio Media Streams dialect and optional `TwiML` webhook.
#[derive(Debug, Default)]
pub(crate) struct TwilioDialectFactory {
    replay_cache: Arc<ReplayCache>,
}

impl MediaStreamDialectFactory for TwilioDialectFactory {
    fn name(&self) -> &'static str {
        "twilio"
    }

    fn default_auth_token_env(&self) -> Option<&'static str> {
        Some("TWILIO_AUTH_TOKEN")
    }

    fn create_dialect(
        &self,
        config: &serde_json::Value,
    ) -> Result<Box<dyn MediaStreamDialect>, DialectError> {
        let cfg = TwilioConfig::from_json(config)?;
        Ok(Box::new(TwilioDialect::new(
            cfg,
            Arc::clone(&self.replay_cache),
        )))
    }

    fn handle_webhook(
        &self,
        config: &serde_json::Value,
        req: &WebhookRequest,
    ) -> Result<Option<WebhookResponse>, WebhookError> {
        let cfg = TwilioConfig::from_json(config)
            .map_err(|e| WebhookError::Misconfigured(e.to_string()))?;

        let stream_ws_url = cfg
            .stream_ws_url
            .as_deref()
            .filter(|s| s.starts_with("wss://"))
            .ok_or_else(|| {
                WebhookError::Misconfigured(
                    "stream_ws_url must be configured as a wss:// URL".to_string(),
                )
            })?;

        if !cfg.require_auth {
            warn!(
                "twilio webhook require_auth=false; skipping X-Twilio-Signature validation (simulated/dev mode only)"
            );
            let twiml =
                build_twiml_connect_stream(stream_ws_url, None, cfg.default_tenant_id.as_deref());
            return Ok(Some(WebhookResponse::ok("text/xml; charset=utf-8", twiml)));
        }

        let auth_token = cfg.effective_auth_token().ok_or_else(|| {
            WebhookError::Misconfigured(
                "TWILIO_AUTH_TOKEN (or auth_token) is required for webhook validation".to_string(),
            )
        })?;
        let canonical_url = cfg
            .public_webhook_url
            .as_deref()
            .filter(|s| s.starts_with("https://"))
            .ok_or_else(|| {
                WebhookError::Misconfigured(
                    "public_webhook_url must be configured as an https:// URL".to_string(),
                )
            })?;

        let provided_sig = req
            .headers
            .get("x-twilio-signature")
            .ok_or_else(|| WebhookError::Unauthorized("missing X-Twilio-Signature".to_string()))?;

        if !crypto::verify_twilio_signature(
            &auth_token,
            canonical_url,
            &req.form_params,
            provided_sig,
        ) {
            return Err(WebhookError::Unauthorized(
                "invalid X-Twilio-Signature".to_string(),
            ));
        }

        let call_sid = req
            .form_params
            .get("CallSid")
            .map(String::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| WebhookError::BadRequest("missing CallSid".to_string()))?;

        let key = cfg
            .signing_key()
            .ok_or_else(|| WebhookError::Misconfigured("missing stream signing key".to_string()))?;
        let token = mint_stream_token(
            &key,
            call_sid,
            cfg.default_tenant_id.clone(),
            cfg.token_ttl_secs,
        )?;
        let twiml = build_twiml_connect_stream(
            stream_ws_url,
            Some(&token),
            cfg.default_tenant_id.as_deref(),
        );
        Ok(Some(WebhookResponse::ok("text/xml; charset=utf-8", twiml)))
    }
}

/// Construct the Twilio Media Streams dialect factory for host registration.
pub fn factory() -> Arc<dyn MediaStreamDialectFactory> {
    Arc::new(TwilioDialectFactory::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config_json() -> serde_json::Value {
        serde_json::json!({
            "auth_token": "test-twilio-auth-token",
            "public_webhook_url": "https://voice.example.com/telephony/twilio/voice",
            "stream_ws_url": "wss://voice.example.com/telephony/twilio/ws",
            "default_tenant_id": "acme",
            "token_ttl_secs": 30,
            "require_auth": true
        })
    }

    #[test]
    fn webhook_validates_signature_and_mints_single_use_token_accepted_once() {
        let factory = TwilioDialectFactory::default();
        let cfg = test_config_json();

        let mut form = HashMap::new();
        form.insert("CallSid".to_string(), "CA999".to_string());
        form.insert("From".to_string(), "+15551234567".to_string());

        let sig = crypto::compute_twilio_signature(
            "test-twilio-auth-token",
            "https://voice.example.com/telephony/twilio/voice",
            &form,
        );
        let mut headers = HashMap::new();
        headers.insert("x-twilio-signature".to_string(), sig);

        let req = WebhookRequest::new()
            .with_headers(headers)
            .with_form_params(form);

        let resp = factory
            .handle_webhook(&cfg, &req)
            .expect("valid webhook")
            .expect("twiml response");
        assert_eq!(resp.status_code, 200);
        assert!(
            resp.body
                .contains("wss://voice.example.com/telephony/twilio/ws")
        );
        assert!(!resp.body.contains("+15551234567"));

        // Extract minted auth token from TwiML.
        let marker = "<Parameter name=\"auth\" value=\"";
        let start_idx = resp.body.find(marker).expect("auth param") + marker.len();
        let rest = &resp.body[start_idx..];
        let end_idx = rest.find('"').expect("closing quote");
        let token = &rest[..end_idx];

        // First WebSocket start with matching CallSid succeeds.
        let mut dialect1 = factory.create_dialect(&cfg).expect("dialect1");
        let start_json = serde_json::json!({
            "event": "start",
            "start": {
                "streamSid": "MZ123",
                "callSid": "CA999",
                "mediaFormat": { "encoding": "audio/x-mulaw", "sampleRate": 8000 },
                "customParameters": { "auth": token, "tenant_id": "acme" }
            }
        })
        .to_string();
        let ev = dialect1
            .decode_frame(MediaStreamFrame::Text(start_json.clone()))
            .expect("decode start");
        let DialectInbound::Start(start_info) = ev else {
            panic!("expected Start");
        };
        assert!(!start_info.custom_params.contains_key("auth"));
        dialect1
            .verify_start_auth(&start_info)
            .expect("first use must succeed");

        // Replay of the exact same token MUST be rejected!
        let mut dialect2 = factory.create_dialect(&cfg).expect("dialect2");
        let DialectInbound::Start(start_replay) = dialect2
            .decode_frame(MediaStreamFrame::Text(start_json))
            .expect("decode replay")
        else {
            panic!("expected Start");
        };
        let err = dialect2
            .verify_start_auth(&start_replay)
            .expect_err("replay must be rejected");
        assert!(matches!(err, DialectError::AuthFailed(_)));
    }

    #[test]
    fn webhook_rejects_invalid_signature_and_spoofed_url() {
        let factory = TwilioDialectFactory::default();
        let cfg = test_config_json();
        let mut form = HashMap::new();
        form.insert("CallSid".to_string(), "CA999".to_string());
        let mut headers = HashMap::new();
        headers.insert(
            "x-twilio-signature".to_string(),
            "bad-signature==".to_string(),
        );
        let req = WebhookRequest::new()
            .with_headers(headers)
            .with_form_params(form);
        let err = factory
            .handle_webhook(&cfg, &req)
            .expect_err("must reject invalid sig");
        assert!(matches!(err, WebhookError::Unauthorized(_)));
    }

    #[test]
    fn token_rejects_call_sid_mismatch() {
        let factory = TwilioDialectFactory::default();
        let cfg = test_config_json();
        let key = crypto::derive_stream_key("test-twilio-auth-token");
        let token = mint_stream_token(&key, "CA_ORIGINAL", None, 30).expect("mint");

        let mut dialect = factory.create_dialect(&cfg).expect("dialect");
        let start_json = serde_json::json!({
            "event": "start",
            "start": {
                "streamSid": "MZ1",
                "callSid": "CA_HIJACKED",
                "customParameters": { "auth": token }
            }
        })
        .to_string();
        let DialectInbound::Start(s) = dialect
            .decode_frame(MediaStreamFrame::Text(start_json))
            .expect("decode")
        else {
            panic!("expected Start");
        };
        assert!(matches!(
            dialect.verify_start_auth(&s),
            Err(DialectError::AuthFailed(_))
        ));
    }

    #[test]
    fn outbound_media_clear_mark_and_stop_lifecycle() {
        let factory = TwilioDialectFactory::default();
        let cfg = serde_json::json!({ "require_auth": false });
        let mut dialect = factory.create_dialect(&cfg).expect("dialect");

        // Before `start`, outbound frames are suppressed (prevents Error 31951).
        assert_eq!(dialect.encode_audio(&[0xFF; 160]).expect("ok"), None);
        assert_eq!(dialect.encode_clear().expect("ok"), None);

        // Start stream.
        let start = r#"{"event":"start","start":{"streamSid":"MZ42","callSid":"CA42"}}"#;
        let _ = dialect
            .decode_frame(MediaStreamFrame::Text(start.to_string()))
            .expect("start");

        // Media frame carries streamSid and base64 payload.
        let MediaStreamFrame::Text(media_json) = dialect
            .encode_audio(&[0xFF; 4])
            .expect("encode media")
            .expect("frame")
        else {
            panic!("expected text frame");
        };
        let v: serde_json::Value = serde_json::from_str(&media_json).expect("json");
        assert_eq!(v["event"], "media");
        assert_eq!(v["streamSid"], "MZ42");
        assert_eq!(v["media"]["payload"], STANDARD.encode([0xFFu8; 4]));

        // Clear frame for barge-in.
        let MediaStreamFrame::Text(clear_json) = dialect
            .encode_clear()
            .expect("encode clear")
            .expect("frame")
        else {
            panic!("expected text frame");
        };
        let vc: serde_json::Value = serde_json::from_str(&clear_json).expect("json");
        assert_eq!(vc["event"], "clear");
        assert_eq!(vc["streamSid"], "MZ42");

        // Mark frame.
        let MediaStreamFrame::Text(mark_json) = dialect
            .encode_mark("vp-u1-m1")
            .expect("encode mark")
            .expect("frame")
        else {
            panic!("expected text frame");
        };
        let vm: serde_json::Value = serde_json::from_str(&mark_json).expect("json");
        assert_eq!(vm["event"], "mark");
        assert_eq!(vm["streamSid"], "MZ42");
        assert_eq!(vm["mark"]["name"], "vp-u1-m1");

        // Stop suppresses subsequent outbound frames.
        assert_eq!(
            dialect
                .decode_frame(MediaStreamFrame::Text(r#"{"event":"stop"}"#.to_string()))
                .expect("stop"),
            DialectInbound::Stop
        );
        assert_eq!(dialect.encode_audio(&[0xFF; 4]).expect("ok"), None);
    }

    #[test]
    fn twiml_escapes_xml_special_characters() {
        let xml = build_twiml_connect_stream(
            "wss://example.com/ws?a=1&b=2",
            Some("tok<>&\"'"),
            Some("tenant<1>"),
        );
        assert!(xml.contains("wss://example.com/ws?a=1&amp;b=2"));
        assert!(xml.contains("tok&lt;&gt;&amp;&quot;&apos;"));
        assert!(xml.contains("tenant&lt;1&gt;"));
    }

    #[test]
    fn webhook_allows_unsigned_simulated_caller_when_require_auth_false() {
        let factory = TwilioDialectFactory::default();
        let cfg = serde_json::json!({
            "stream_ws_url": "wss://tunnel.example.com/telephony/twilio/ws",
            "require_auth": false
        });
        let req = WebhookRequest::new();
        let resp = factory
            .handle_webhook(&cfg, &req)
            .expect("webhook ok")
            .expect("some response");
        assert_eq!(resp.status_code, 200);
        assert!(resp.body.contains("<Connect>"));
        assert!(
            resp.body
                .contains("wss://tunnel.example.com/telephony/twilio/ws")
        );
        assert!(!resp.body.contains("name=\"auth\""));
    }

    fn assert_transport_metrics_conformance(
        mut dialect: Box<dyn MediaStreamDialect>,
        injected_delay: Duration,
    ) {
        let start = r#"{"event":"start","start":{"streamSid":"MZ-CONF","callSid":"CA-CONF"}}"#;
        let _ = dialect
            .decode_frame(MediaStreamFrame::Text(start.to_string()))
            .expect("start");

        let t0 = Instant::now();
        let mark_frame = dialect
            .encode_mark("vp-u1-m1")
            .expect("encode mark")
            .expect("mark frame");
        assert!(matches!(mark_frame, MediaStreamFrame::Text(_)));

        // Deliberately slow carrier playback echo must show up in measured latency.
        std::thread::sleep(injected_delay);
        let ack = dialect
            .decode_frame(MediaStreamFrame::Text(
                r#"{"event":"mark","mark":{"name":"vp-u1-m1"}}"#.to_string(),
            ))
            .expect("decode mark ack");
        let elapsed = t0.elapsed();

        assert_eq!(ack, DialectInbound::MarkAck("vp-u1-m1".to_string()));
        assert!(
            elapsed >= injected_delay,
            "injected mark delay {injected_delay:?} must be reflected in elapsed {elapsed:?}"
        );
    }

    #[test]
    fn twilio_metrics_conformance_reflects_mark_echo_delay() {
        let f = factory();
        let dialect = f
            .create_dialect(&serde_json::json!({ "require_auth": false }))
            .expect("dialect");
        assert_transport_metrics_conformance(dialect, Duration::from_millis(25));
    }
}
