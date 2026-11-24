//! Protocol-agnostic WebSocket media-stream dialect trait and registry.
//!
//! Carrier-specific telephony protocols (Twilio Media Streams, Telnyx, Infobip,
//! Vonage, Plivo) and raw PCM evaluation harnesses share the same session
//! lifecycle: handshake (`Start`), bidirectional audio frames, barge-in flush
//! (`Clear`), playback acknowledgement (`Mark`), optional DTMF, and teardown
//! (`Stop`).
//!
//! Implement [`MediaStreamDialect`] and [`MediaStreamDialectFactory`] in an
//! isolated plugin crate that depends only on `voice-pipes-sdk`, and register
//! the factory at startup via [`register_media_stream_factory`]. The core
//! `voice-pipes` engine owns resampling, G.711 transcoding, 20 ms framing,
//! pacing, barge-in, and telemetry.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Audio wire encoding carried inside a media stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MediaStreamEncoding {
    /// Raw 16-bit signed little-endian linear PCM.
    #[default]
    Pcm16Le,
    /// ITU-T G.711 µ-law (8-bit per sample, standard North American / Twilio PSTN).
    Mulaw,
    /// ITU-T G.711 A-law (8-bit per sample, standard European / E1 PSTN).
    Alaw,
}

impl MediaStreamEncoding {
    /// Bytes per single mono audio sample in this encoding.
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Pcm16Le => 2,
            Self::Mulaw | Self::Alaw => 1,
        }
    }
}

/// A protocol-agnostic WebSocket frame passed to/from a [`MediaStreamDialect`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaStreamFrame {
    /// UTF-8 WebSocket text frame (typically JSON).
    Text(String),
    /// Raw WebSocket binary frame.
    Binary(Vec<u8>),
}

/// Session parameters negotiated by a dialect's initial handshake event.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DialectStart {
    /// Provider-assigned stream identifier (e.g., Twilio `streamSid`).
    pub stream_id: String,
    /// Optional provider-assigned call identifier (e.g., Twilio `callSid`).
    pub call_id: Option<String>,
    /// Wire sample rate in Hz (e.g., `8000` for PSTN G.711, `16000` for wideband PCM).
    pub sample_rate: u32,
    /// Wire audio encoding.
    pub encoding: MediaStreamEncoding,
    /// Optional authentication token carried inside the handshake (e.g., `<Parameter name="auth">`).
    pub auth_token: Option<String>,
    /// Optional external correlation identifier (e.g., simulation ID or call ID).
    pub external_id: Option<String>,
    /// Optional tenant identifier extracted from the handshake.
    pub tenant_id: Option<String>,
    /// Additional non-sensitive custom parameters supplied by the stream handshake.
    pub custom_params: HashMap<String, String>,
}

impl DialectStart {
    /// Construct a new [`DialectStart`] with required stream parameters.
    pub fn new(
        stream_id: impl Into<String>,
        sample_rate: u32,
        encoding: MediaStreamEncoding,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            call_id: None,
            sample_rate,
            encoding,
            auth_token: None,
            external_id: None,
            tenant_id: None,
            custom_params: HashMap::new(),
        }
    }

    with_setters!(
        with_call_id => call_id: Option<String>,
        with_auth_token => auth_token: Option<String>,
        with_external_id => external_id: Option<String>,
        with_tenant_id => tenant_id: Option<String>,
        with_custom_params => custom_params: HashMap<String, String>,
    );
}

/// Canonical inbound event decoded by a [`MediaStreamDialect`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DialectInbound {
    /// Stream handshake completed; audio may now flow.
    Start(DialectStart),
    /// Raw encoded audio payload in the dialect's negotiated [`MediaStreamEncoding`].
    Audio(Vec<u8>),
    /// Playback mark echoed back by the remote peer (indicates audio played up to this mark).
    MarkAck(String),
    /// DTMF keypad digit pressed by the caller (treat as sensitive data; never log at INFO/WARN).
    Dtmf(String),
    /// Clean stream termination requested by the remote peer.
    Stop,
    /// Valid protocol frame that requires no pipeline action (e.g., `connected` banner).
    Ignore,
}

/// Errors returned by a [`MediaStreamDialect`] or [`MediaStreamDialectFactory`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DialectError {
    /// Incoming frame was malformed or violated the dialect schema.
    InvalidFrame(String),
    /// Handshake authentication or replay check failed.
    AuthFailed(String),
    /// General protocol or configuration error.
    Protocol(String),
}

impl fmt::Display for DialectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFrame(msg) => write!(f, "invalid media stream frame: {msg}"),
            Self::AuthFailed(msg) => write!(f, "media stream authentication failed: {msg}"),
            Self::Protocol(msg) => write!(f, "media stream protocol error: {msg}"),
        }
    }
}

impl std::error::Error for DialectError {}

/// Inbound HTTP webhook request passed to a carrier dialect's webhook handler
/// (e.g., Twilio `TwiML` `POST` webhook).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WebhookRequest {
    /// Lowercase HTTP request header names mapped to their values.
    pub headers: HashMap<String, String>,
    /// Decoded `application/x-www-form-urlencoded` form parameters.
    pub form_params: HashMap<String, String>,
    /// Raw request body bytes.
    pub raw_body: Vec<u8>,
}

impl WebhookRequest {
    /// Construct an empty [`WebhookRequest`].
    pub fn new() -> Self {
        Self::default()
    }

    with_setters!(
        with_headers => headers: HashMap<String, String>,
        with_form_params => form_params: HashMap<String, String>,
        with_raw_body => raw_body: Vec<u8>,
    );
}

/// HTTP response returned by a carrier dialect's webhook handler.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WebhookResponse {
    /// HTTP status code (e.g., `200`).
    pub status_code: u16,
    /// `Content-Type` header value (e.g., `"text/xml; charset=utf-8"`).
    pub content_type: String,
    /// Response body string (e.g., `TwiML` XML).
    pub body: String,
}

impl WebhookResponse {
    /// Construct a `200 OK` [`WebhookResponse`].
    pub fn ok(content_type: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            status_code: 200,
            content_type: content_type.into(),
            body: body.into(),
        }
    }
}

/// Errors returned by a carrier webhook handler.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WebhookError {
    /// Signature or credential validation failed (`403 Forbidden`).
    Unauthorized(String),
    /// Request body or required fields were invalid (`400 Bad Request`).
    BadRequest(String),
    /// Server-side webhook configuration is missing or invalid (`503 Service Unavailable`).
    Misconfigured(String),
}

impl fmt::Display for WebhookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized(msg) => write!(f, "webhook unauthorized: {msg}"),
            Self::BadRequest(msg) => write!(f, "webhook bad request: {msg}"),
            Self::Misconfigured(msg) => write!(f, "webhook misconfigured: {msg}"),
        }
    }
}

impl std::error::Error for WebhookError {}

/// Translates between a carrier-specific WebSocket framing protocol and
/// Voice Pipes' canonical media-stream events.
///
/// One instance is created per active WebSocket connection.
pub trait MediaStreamDialect: Send + Sync {
    /// Short dialect identifier (e.g., `"twilio"`, `"infobip"`, `"raw"`).
    fn name(&self) -> &str;

    /// Decode an inbound WebSocket frame into a [`DialectInbound`] event.
    fn decode_frame(&mut self, frame: MediaStreamFrame) -> Result<DialectInbound, DialectError>;

    /// Encode an outbound audio frame (already in the negotiated encoding and
    /// chunk size) into a WebSocket frame. Returning `Ok(None)` skips sending
    /// (e.g., before the handshake has completed).
    fn encode_audio(&self, payload: &[u8]) -> Result<Option<MediaStreamFrame>, DialectError>;

    /// Encode a barge-in playback buffer clear command, if supported.
    fn encode_clear(&self) -> Result<Option<MediaStreamFrame>, DialectError>;

    /// Encode a playback mark event, if supported.
    fn encode_mark(&self, name: &str) -> Result<Option<MediaStreamFrame>, DialectError>;

    /// Verify authentication/replay claims carried in [`DialectStart`] before
    /// pipeline resources are allocated.
    fn verify_start_auth(&self, _start: &DialectStart) -> Result<(), DialectError> {
        Ok(())
    }

    /// Whether the dialect expects to receive a `Start` handshake frame before
    /// audio flows. When `false`, the session starts immediately with
    /// [`initial_start`](Self::initial_start).
    fn expects_start_frame(&self) -> bool {
        true
    }

    /// Optional immediate [`DialectStart`] used by handshake-free dialects
    /// (e.g., raw PCM WebSocket).
    fn initial_start(&self) -> Option<DialectStart> {
        None
    }

    /// Target outbound frame duration in milliseconds (default: `20` ms).
    fn frame_duration_ms(&self) -> u32 {
        20
    }

    /// Maximum inbound WebSocket frame/message size in bytes (default: 16 KiB).
    fn max_frame_bytes(&self) -> usize {
        16 * 1024
    }
}

/// Factory that constructs per-connection [`MediaStreamDialect`] instances and
/// optionally handles carrier HTTP webhooks (e.g., `TwiML`).
pub trait MediaStreamDialectFactory: Send + Sync {
    /// Unique dialect name used in routes (`/telephony/{name}/ws`) and config.
    fn name(&self) -> &'static str;

    /// Construct a fresh [`MediaStreamDialect`] for a new WebSocket connection.
    fn create_dialect(
        &self,
        config: &serde_json::Value,
    ) -> Result<Box<dyn MediaStreamDialect>, DialectError>;

    /// Optional HTTP webhook handler for carrier call-setup requests
    /// (e.g., `POST /telephony/{name}/voice`). Returning `Ok(None)` indicates
    /// this dialect does not expose a webhook.
    fn handle_webhook(
        &self,
        _config: &serde_json::Value,
        _req: &WebhookRequest,
    ) -> Result<Option<WebhookResponse>, WebhookError> {
        Ok(None)
    }
}

// ── Global registry ─────────────────────────────────────────

static REGISTRY: OnceLock<RwLock<HashMap<String, Arc<dyn MediaStreamDialectFactory>>>> =
    OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, Arc<dyn MediaStreamDialectFactory>>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register a [`MediaStreamDialectFactory`] by its [`MediaStreamDialectFactory::name`].
///
/// Returns `Err` if another factory is already registered under the same name.
pub fn register_media_stream_factory(
    factory: Arc<dyn MediaStreamDialectFactory>,
) -> Result<(), String> {
    let key = factory.name().to_ascii_lowercase();
    let mut guard = registry().write();
    if guard.contains_key(&key) {
        return Err(format!(
            "media stream dialect \"{key}\" is already registered"
        ));
    }
    guard.insert(key, factory);
    Ok(())
}

/// Look up a registered [`MediaStreamDialectFactory`] by name (case-insensitive).
pub fn get_media_stream_factory(name: &str) -> Option<Arc<dyn MediaStreamDialectFactory>> {
    let key = name.to_ascii_lowercase();
    registry().read().get(&key).map(Arc::clone)
}

/// Return the sorted names of all registered media-stream dialects.
pub fn registered_media_stream_factories() -> Vec<String> {
    let mut names: Vec<String> = registry().read().keys().cloned().collect();
    names.sort();
    names
}
