//! The `generateContent` REST call: request and response wire types, the
//! response decoding and error mapping, and the `TtsProvider` built on them.

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use voice_pipes_net::reqwest::Client;

use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::TraceEvent;
use voice_pipes_sdk::component::{Component, ComponentCore, CoreComponent};
use voice_pipes_sdk::{SynthesisRequest, TtsError, TtsProvider};

use crate::config::GeminiConfig;

// ── Constants ────────────────────────────────────────────────

pub(crate) const SAMPLE_RATE: u32 = 24_000;

/// Number of samples per chunk when streaming (~100 ms of audio at 24 kHz).
const STREAM_CHUNK_SAMPLES: usize = 2400;

/// Maximum text input size in bytes (Gemini API limit).
pub(crate) const MAX_TEXT_BYTES: usize = 4000;

/// Largest response read: the whole utterance arrives as one base64 JSON
/// body, about 64 KB per second of 24 kHz audio, so this is minutes of speech.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

// ── REST API request types ──────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerateContentRequest {
    contents: Vec<Content>,
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct Content {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct Part {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    response_modalities: Vec<String>,
    speech_config: SpeechConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeechConfig {
    voice_config: VoiceConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceConfig {
    prebuilt_voice_config: PrebuiltVoiceConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrebuiltVoiceConfig {
    voice_name: String,
}

// ── REST API response types ─────────────────────────────────

#[derive(Deserialize)]
struct GenerateContentResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<ApiErrorBody>,
}

#[derive(Deserialize)]
struct Candidate {
    content: CandidateContent,
}

#[derive(Deserialize)]
struct CandidateContent {
    parts: Vec<ResponsePart>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponsePart {
    inline_data: Option<InlineData>,
}

#[derive(Deserialize)]
struct InlineData {
    #[expect(dead_code, reason = "retained for future format validation")]
    mime_type: Option<String>,
    data: String,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    code: Option<u16>,
    message: Option<String>,
}

// ── Provider ────────────────────────────────────────────────

/// Gemini REST API TTS provider.
#[derive(Debug)]
pub(crate) struct GeminiTtsProvider {
    pub(crate) client: Client,
    pub(crate) config: GeminiConfig,
    pub(crate) audit_tx: Option<mpsc::Sender<TraceEvent>>,
    pub(crate) core: std::sync::Arc<ComponentCore>,
}

impl GeminiTtsProvider {
    /// Build the JSON request body for `generateContent`.
    pub(crate) fn build_request(&self, text: &str, voice: Option<&str>) -> GenerateContentRequest {
        // Prepend style prompt if configured.
        let full_text = if self.config.prompt.is_empty() {
            text.to_string()
        } else {
            format!("{}\n\n{}", self.config.prompt, text)
        };

        // Truncate to API limit, backing the cut off to a char boundary — a
        // fixed byte slice panics when byte MAX_TEXT_BYTES falls inside a
        // multi-byte character.
        let truncated = if full_text.len() > MAX_TEXT_BYTES {
            let mut end = MAX_TEXT_BYTES;
            while !full_text.is_char_boundary(end) {
                end -= 1;
            }
            tracing::warn!(
                len = full_text.len(),
                max = MAX_TEXT_BYTES,
                "Gemini TTS input truncated to API limit"
            );
            full_text[..end].to_string()
        } else {
            full_text
        };

        let voice_name =
            voice.map_or_else(|| (*self.config.voice.load_full()).clone(), str::to_string);

        GenerateContentRequest {
            contents: vec![Content {
                parts: vec![Part { text: truncated }],
            }],
            generation_config: GenerationConfig {
                response_modalities: vec!["AUDIO".to_string()],
                speech_config: SpeechConfig {
                    voice_config: VoiceConfig {
                        prebuilt_voice_config: PrebuiltVoiceConfig { voice_name },
                    },
                },
            },
        }
    }

    /// Full URL for the `generateContent` endpoint.
    pub(crate) fn url(&self) -> String {
        format!(
            "{}/models/{}:generateContent",
            self.config.endpoint.trim_end_matches('/'),
            self.config.model_name,
        )
    }

    /// Call the Gemini API and return decoded PCM i16 samples.
    async fn call_api(&self, text: &str, voice: Option<&str>) -> Result<Vec<i16>, TtsError> {
        let request_body = self.build_request(text, voice);
        let url = self.url();

        let key = voice_pipes_net::sensitive_header(self.config.api_key.expose()).map_err(
            |_invalid| TtsError::AuthError("the API key is not a valid header value".into()),
        )?;
        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| TtsError::NetworkError(format!("Gemini TTS request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = voice_pipes_net::error_body(response).await;
            if let Ok(err_resp) = serde_json::from_str::<GenerateContentResponse>(&body)
                && let Some(err) = err_resp.error
            {
                return Err(map_api_error(
                    err.code.unwrap_or(status.as_u16()),
                    &err.message.unwrap_or_default(),
                ));
            }
            return Err(map_api_error(status.as_u16(), &body));
        }

        let body = voice_pipes_net::limited_body(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| match e {
                voice_pipes_net::BodyError::Read(e) => {
                    TtsError::NetworkError(format!("Gemini TTS response: {e}"))
                }
                e => TtsError::InferenceFailed(format!("Gemini TTS response: {e}")),
            })?;
        decode_response(&body)
    }

    /// Emit a `tts_detail` audit event.
    fn emit_timing(&self, text: &str, sample_count: usize, elapsed: Duration, voice: &str) {
        if let Some(ref tx) = self.audit_tx {
            let _ = tx.try_send(voice_pipes_sdk::tts::detail_event(
                "tts:gemini",
                text.len(),
                sample_count,
                SAMPLE_RATE,
                elapsed,
                serde_json::json!({
                    "voice": voice,
                    "provider": "gemini",
                    "model": self.config.model_name,
                }),
            ));
        }
    }
}

#[async_trait]
impl TtsProvider for GeminiTtsProvider {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let start = Instant::now();
        let samples = match self.call_api(text, None).await {
            Ok(s) => s,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };
        let elapsed = start.elapsed();
        self.core.record_ok(elapsed);

        self.emit_timing(text, samples.len(), elapsed, &self.config.voice.load());

        Ok(AudioFrame {
            samples,
            sample_rate: SAMPLE_RATE,
            channels: 1,
        })
    }

    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let text = req.text;
        let hints = req.hints;
        if cancel.is_cancelled() {
            return Ok(());
        }
        let voice_override = hints.and_then(|h| h.voice.as_deref());

        let start = Instant::now();
        let samples = tokio::select! {
            () = cancel.cancelled() => {
                return Ok(());
            }
            res = self.call_api(text, voice_override) => match res {
                Ok(s) => s,
                Err(e) => {
                    self.core.record_error(&e);
                    return Err(e);
                }
            }
        };
        let elapsed = start.elapsed();
        self.core.record_ok(elapsed);

        let default_voice = self.config.voice.load_full();
        let voice = voice_override.unwrap_or(&default_voice);
        self.emit_timing(text, samples.len(), elapsed, voice);

        for chunk in samples.chunks(STREAM_CHUNK_SAMPLES) {
            if cancel.is_cancelled() {
                break;
            }
            let frame = AudioFrame {
                samples: chunk.to_vec(),
                sample_rate: SAMPLE_RATE,
                channels: 1,
            };
            if tx.send(frame).await.is_err() {
                break;
            }
        }

        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn name(&self) -> &'static str {
        "gemini"
    }

    fn as_component(&self) -> Option<std::sync::Arc<dyn Component>> {
        Some(std::sync::Arc::new(CoreComponent::new(
            std::sync::Arc::clone(&self.core),
        )))
    }
}

// ── Error mapping ───────────────────────────────────────────

/// The PCM samples in a successful `generateContent` response: its first
/// candidate's inline audio, base64 of 16-bit little-endian PCM. Anything
/// else (not JSON, no audio part, bad base64) is an error, never a panic; a
/// trailing odd byte is dropped.
pub(crate) fn decode_response(body: &[u8]) -> Result<Vec<i16>, TtsError> {
    let resp: GenerateContentResponse = serde_json::from_slice(body).map_err(|e| {
        TtsError::InferenceFailed(format!(
            "failed to parse response: {}",
            voice_pipes_sdk::secrets::json_error(&e)
        ))
    })?;
    let audio_b64 = resp
        .candidates
        .as_ref()
        .and_then(|c| c.first())
        .and_then(|c| c.content.parts.first())
        .and_then(|p| p.inline_data.as_ref())
        .map(|d| &d.data)
        .ok_or_else(|| {
            TtsError::InferenceFailed("Gemini TTS response contained no audio data".into())
        })?;
    let pcm_bytes = base64::engine::general_purpose::STANDARD
        .decode(audio_b64)
        .map_err(|e| TtsError::InferenceFailed(format!("base64 decode failed: {e}")))?;
    let samples: Vec<i16> = pcm_bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();
    if samples.is_empty() {
        // A success with no audio would otherwise pass as a spoken reply
        // the caller never hears.
        return Err(TtsError::InferenceFailed(
            "Gemini TTS returned no audio".into(),
        ));
    }
    Ok(samples)
}

/// Map an HTTP status code to the appropriate [`TtsError`] variant.
pub(crate) fn map_api_error(status: u16, message: &str) -> TtsError {
    TtsError::from_http_status("Gemini TTS", status, message)
}
