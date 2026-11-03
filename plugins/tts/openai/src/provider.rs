//! The `/audio/speech` request, its streamed reply, the `/models` identity
//! probe, and the `TtsProvider` built on them.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, instrument};
use voice_pipes_net::reqwest::Client;

use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::TraceEvent;
use voice_pipes_sdk::audio::{PcmFramer, WavStripper};
use voice_pipes_sdk::component::{Component, ComponentCore, CoreComponent};
use voice_pipes_sdk::{SynthesisHints, SynthesisRequest, TtsError, TtsProvider};

use crate::config::OpenAiConfig;

// ── Constants ────────────────────────────────────────────────

pub(crate) const SAMPLE_RATE: u32 = 24_000;

/// Largest `/v1/models` answer read: a model listing is a few hundred bytes,
/// even a server hosting many models stays far below this.
const MODELS_BODY_LIMIT: usize = 256 * 1024;

/// Number of samples per chunk when streaming (~100 ms of audio at 24 kHz).
const STREAM_CHUNK_SAMPLES: usize = 2400;

// ── REST API request types ──────────────────────────────────

#[derive(Serialize)]
struct OpenAiTtsRequest {
    model: String,
    input: String,
    voice: String,
    response_format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
}

// ── REST API response types ─────────────────────────────────

#[derive(Deserialize)]
struct OpenAiErrorResponse {
    error: OpenAiErrorBody,
}

#[derive(Deserialize)]
struct OpenAiErrorBody {
    message: String,
    #[expect(
        dead_code,
        reason = "deserialized to mirror the API error shape; only `message` is surfaced"
    )]
    #[serde(rename = "type")]
    error_type: Option<String>,
    #[expect(
        dead_code,
        reason = "deserialized to mirror the API error shape; only `message` is surfaced"
    )]
    code: Option<String>,
}

// ── /v1/models (cross-process model identity) ───────────────

/// Model identity resolved from the sidecar's `/v1/models` at warm-up, so the
/// session report shows the model actually running in the (separate) TTS
/// process — not just the locally-configured `model_name`.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedModel {
    id: String,
    version: Option<String>,
}

#[derive(Deserialize)]
struct ModelsListResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    version: Option<String>,
}

/// The model a `/models` listing says synthesises: the configured one when
/// it is listed, or the only one a single-model server lists. The real API
/// lists every model an account may use, so its first entry says nothing
/// about this one.
fn serving_model(listed: Vec<ModelEntry>, configured: &str) -> Option<ModelEntry> {
    if listed.len() == 1 {
        return listed.into_iter().next();
    }
    listed.into_iter().find(|m| m.id == configured)
}

// ── Provider ────────────────────────────────────────────────

/// `OpenAI` REST API TTS provider.
pub(crate) struct OpenAiTts {
    pub(crate) client: Client,
    pub(crate) config: OpenAiConfig,
    pub(crate) audit_tx: Option<mpsc::Sender<TraceEvent>>,
    pub(crate) core: std::sync::Arc<ComponentCore>,
    /// Model identity learned from the sidecar's `/v1/models` at warm-up.
    /// `None` until `warm_up` runs (or if the endpoint has no `/v1/models`);
    /// `emit_timing` then falls back to the configured `model_name`.
    pub(crate) resolved_model: std::sync::Arc<arc_swap::ArcSwapOption<ResolvedModel>>,
}

impl std::fmt::Debug for OpenAiTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiTts")
            .field("endpoint", &self.config.endpoint)
            .field("model", &self.config.model_name)
            .field("voice", &*self.config.current_voice())
            .finish_non_exhaustive()
    }
}

impl OpenAiTts {
    /// Full URL for the `OpenAI` TTS endpoint.
    pub(crate) fn url(&self) -> String {
        let ep = self.config.endpoint.trim_end_matches('/');
        if ep.ends_with("/audio/speech") {
            ep.to_string()
        } else {
            format!("{ep}/audio/speech")
        }
    }

    /// URL for the `/v1/models` endpoint (sibling of `/audio/speech`).
    pub(crate) fn models_url(&self) -> String {
        let ep = self.config.endpoint.trim_end_matches('/');
        let base = ep
            .strip_suffix("/audio/speech")
            .unwrap_or(ep)
            .trim_end_matches('/');
        format!("{base}/models")
    }

    /// Send the synthesis request and stream PCM `i16` samples back through
    /// `audio_tx` as `AudioFrame`s.
    #[instrument(skip_all, fields(chars = text.len(), voice))]
    async fn stream_synthesis(
        &self,
        text: &str,
        voice: &str,
        hints: Option<&SynthesisHints>,
        audio_tx: mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let started = Instant::now();
        let url = self.url();

        let speed = hints.and_then(|h| h.speed).or(self.config.speed);

        let instructions = hints
            .and_then(|h| h.options.get("instructions").and_then(|v| v.as_str()))
            .map(str::to_string)
            .or_else(|| self.config.instructions.clone());

        let request_body = OpenAiTtsRequest {
            model: self.config.model_name.clone(),
            input: text.to_string(),
            voice: voice.to_string(),
            response_format: "pcm".to_string(),
            speed,
            instructions,
        };

        let mut request = self.client.post(&url).json(&request_body);
        if !self.config.api_key.is_empty() {
            request = request.bearer_auth(self.config.api_key.expose());
        }

        let response = request
            .send()
            .await
            .map_err(|e| TtsError::NetworkError(format!("OpenAI TTS request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = voice_pipes_net::error_body(response).await;
            return Err(map_http_error(status.as_u16(), &body));
        }

        let mut byte_stream = response.bytes_stream();
        let mut pcm = PcmFramer::new(SAMPLE_RATE, STREAM_CHUNK_SAMPLES);
        // Any OpenAI-compatible server may send WAV instead of raw PCM
        // (VibeVoice, Chatterbox); the header is stripped when present.
        let mut wav = WavStripper::new();
        let mut first_chunk_logged = false;

        loop {
            let item = tokio::select! {
                () = cancel.cancelled() => {
                    return Ok(());
                }
                res = byte_stream.next() => {
                    match res {
                        Some(val) => val,
                        None => break,
                    }
                }
            };

            let bytes = item.map_err(|e| {
                TtsError::NetworkError(format!("OpenAI TTS stream read failed: {e}"))
            })?;
            let pcm_bytes = wav
                .feed(&bytes)
                .map_err(|e| TtsError::InferenceFailed(format!("OpenAI TTS audio stream: {e}")))?;
            if pcm_bytes.is_empty() {
                continue;
            }
            if !pcm.push(&pcm_bytes).is_empty() && !first_chunk_logged {
                first_chunk_logged = true;
                debug!(
                    elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    wav_detected = wav.stripped_header(),
                    "OpenAI TTS first audio bytes"
                );
            }
            if !pcm.send_frames(&audio_tx).await {
                return Ok(());
            }
        }

        let rest = wav
            .finish()
            .map_err(|e| TtsError::InferenceFailed(format!("OpenAI TTS audio stream: {e}")))?;
        pcm.push(&rest);
        pcm.send_rest(&audio_tx).await;

        let total_elapsed = started.elapsed();
        let delivery_wait = pcm.delivery_wait();
        let compute_elapsed = total_elapsed.saturating_sub(delivery_wait);
        let total_samples = pcm.samples_out();
        if total_samples == 0 {
            // A success status with no audio would otherwise pass as a
            // spoken reply the caller never hears.
            return Err(TtsError::InferenceFailed(
                "OpenAI TTS returned no audio".into(),
            ));
        }
        self.emit_timing(text, total_samples, compute_elapsed, voice, delivery_wait);
        Ok(())
    }

    /// Emit a `tts_detail` audit event.
    fn emit_timing(
        &self,
        text: &str,
        sample_count: usize,
        elapsed: Duration,
        voice: &str,
        delivery_wait: Duration,
    ) {
        if let Some(ref tx) = self.audit_tx {
            // Prefer the model the sidecar reported (resolved at setup via
            // model_identity) over the locally-configured name. The version
            // lives in metadata.json (the report's primary source); the
            // per-turn event carries the id for fallback / live display.
            let model = self
                .resolved_model
                .load()
                .as_ref()
                .map_or_else(|| self.config.model_name.clone(), |r| r.id.clone());
            let _ = tx.try_send(voice_pipes_sdk::tts::detail_event(
                "tts:openai",
                text.chars().count(),
                sample_count,
                SAMPLE_RATE,
                elapsed,
                serde_json::json!({
                    "voice": voice,
                    "provider": "openai",
                    "model": model,
                    "delivery_wait_ms": u64::try_from(delivery_wait.as_millis()).unwrap_or(u64::MAX),
                }),
            ));
        }
    }
}

#[async_trait]
impl TtsProvider for OpenAiTts {
    /// Cheap reachability probe so the session runner can surface an
    /// unreachable TTS backend (e.g. a local sidecar that isn't running) as a
    /// `warning` at connect time, instead of only on the first synthesis.
    ///
    /// Any HTTP response — even `4xx`/`405` for a bare `GET` on the speech
    /// endpoint — proves the server is up; only a connect/timeout error means
    /// it's down. We deliberately do not synthesize or require auth here: we
    /// only care whether the TCP endpoint answers.
    async fn warm_up(&self) -> Result<(), TtsError> {
        // Reachability probe only: any HTTP response (even 4xx/405 for a bare
        // GET on the speech endpoint) proves the server is up. Identity is
        // resolved separately via `model_identity` at session setup.
        let url = self.url();
        match self
            .client
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Err(e) if e.is_connect() || e.is_timeout() => Err(TtsError::NetworkError(format!(
                "TTS endpoint unreachable at {url}: {e}"
            ))),
            Ok(_) | Err(_) => Ok(()),
        }
    }

    async fn model_identity(&self) -> Option<voice_pipes_sdk::ModelIdentity> {
        // Probe `/v1/models` once (memoized) so the report shows the model the
        // process actually loaded — e.g. a local VibeVoice server behind this
        // OpenAI-compatible endpoint — instead of the locally-configured name.
        if self.resolved_model.load().is_none() {
            let url = self.models_url();
            let mut req = self.client.get(&url).timeout(Duration::from_secs(3));
            if !self.config.api_key.is_empty() {
                req = req.bearer_auth(self.config.api_key.expose());
            }
            if let Ok(resp) = req.send().await
                && resp.status().is_success()
                && let Ok(body) = voice_pipes_net::limited_body(resp, MODELS_BODY_LIMIT).await
                && let Ok(list) = serde_json::from_slice::<ModelsListResponse>(&body)
                && let Some(entry) = serving_model(list.data, &self.config.model_name)
            {
                self.resolved_model
                    .store(Some(std::sync::Arc::new(ResolvedModel {
                        id: entry.id,
                        version: entry.version,
                    })));
            }
        }
        self.resolved_model.load().as_ref().map(|r| {
            voice_pipes_sdk::ModelIdentity::from_config(r.id.clone())
                .with_version(r.version.clone())
                .with_source(Some("endpoint".into()))
        })
    }

    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let voice = (*self.config.current_voice()).clone();
        let stream = voice_pipes_sdk::tts::collect_stream(SAMPLE_RATE, |tx| {
            self.stream_synthesis(
                text,
                &voice,
                None,
                tx,
                tokio_util::sync::CancellationToken::new(),
            )
        });
        voice_pipes_sdk::tts::recorded(&self.core, stream).await
    }

    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let text = req.text;
        let hints = req.hints;
        let voice = voice_pipes_sdk::tts::requested_voice(hints, &self.config.voice);
        let token = cancel.clone();
        voice_pipes_sdk::tts::recorded_unless_cancelled(
            &self.core,
            &token,
            self.stream_synthesis(text, &voice, hints, tx, cancel),
        )
        .await
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn name(&self) -> &'static str {
        "openai"
    }

    fn as_component(&self) -> Option<std::sync::Arc<dyn Component>> {
        Some(std::sync::Arc::new(CoreComponent::new(
            std::sync::Arc::clone(&self.core),
        )))
    }
}

// ── Helpers ─────────────────────────────────────────────────

pub(crate) fn map_http_error(status: u16, body: &str) -> TtsError {
    let message = serde_json::from_str::<OpenAiErrorResponse>(body)
        .map_or_else(|_| body.to_string(), |err| err.error.message);
    TtsError::from_http_status("OpenAI TTS", status, &message)
}
