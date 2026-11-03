//! The synthesis provider: HTTP request, PCM and NDJSON stream reading, and
//! the `TtsProvider` impl.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, instrument};
use voice_pipes_net::reqwest::Client;

use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::TraceEvent;
use voice_pipes_sdk::audio::PcmFramer;
use voice_pipes_sdk::component::{Component, ComponentCore, CoreComponent};
use voice_pipes_sdk::options::as_f32;
use voice_pipes_sdk::tts::WordGrouper;
use voice_pipes_sdk::{SynthesisHints, SynthesisRequest, TtsError, TtsProvider, WordBoundary};

use crate::config::ElevenLabsConfig;
use crate::wire::{ElevenLabsTtsRequest, TimestampChunk, VoiceSettings, map_http_error};

/// Longest NDJSON line accepted on the word-timing path: one line carries a
/// chunk of base64 audio and its alignment, a few hundred KB at most.
const MAX_JSON_LINE_BYTES: usize = 4 * 1024 * 1024;

// ── Provider ────────────────────────────────────────────────

/// `ElevenLabs` REST streaming TTS provider.
pub(crate) struct ElevenLabsTts {
    pub(crate) client: Client,
    pub(crate) config: ElevenLabsConfig,
    pub(crate) audit_tx: Option<mpsc::Sender<TraceEvent>>,
    pub(crate) core: std::sync::Arc<ComponentCore>,
}

impl std::fmt::Debug for ElevenLabsTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElevenLabsTts")
            .field("endpoint", &self.config.endpoint)
            .field("model", &self.config.model_id)
            .field("voice", &*self.config.current_voice())
            .finish_non_exhaustive()
    }
}

impl ElevenLabsTts {
    /// Samples per streamed frame (~100 ms at the configured rate).
    fn chunk_samples(&self) -> usize {
        (self.config.sample_rate / 10) as usize
    }

    /// Full URL for one synthesis call. `with_timestamps` selects the
    /// NDJSON alignment endpoint; the voice id is path-encoded.
    pub(crate) fn url(&self, voice: &str, with_timestamps: bool) -> String {
        let ep = self.config.endpoint.trim_end_matches('/');
        let suffix = if with_timestamps {
            "/stream/with-timestamps"
        } else {
            "/stream"
        };
        let voice_enc: String = voice
            .bytes()
            .flat_map(|b| {
                if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                    vec![b as char]
                } else {
                    format!("%{b:02X}").chars().collect()
                }
            })
            .collect();
        format!(
            "{ep}/text-to-speech/{voice_enc}{suffix}?output_format=pcm_{}",
            self.config.sample_rate
        )
    }

    /// Merge config defaults with per-request hints into the request body's
    /// `voice_settings`. `None` when nothing is set, so account-level voice
    /// defaults stay in effect.
    pub(crate) fn voice_settings(&self, hints: Option<&SynthesisHints>) -> Option<VoiceSettings> {
        let hint_f32 = |key: &str| {
            hints
                .and_then(|h| h.options.get(key))
                .and_then(serde_json::Value::as_f64)
                .map(as_f32)
        };
        let settings = VoiceSettings {
            stability: hint_f32("stability").or(self.config.stability),
            similarity_boost: hint_f32("similarity_boost").or(self.config.similarity_boost),
            style: hint_f32("style").or(self.config.style),
            use_speaker_boost: self.config.use_speaker_boost,
            speed: hints.and_then(|h| h.speed).or(self.config.speed),
        };
        (settings != VoiceSettings::default()).then_some(settings)
    }

    /// Send the synthesis request and stream PCM `i16` samples back through
    /// `audio_tx` (plus word boundaries through `word_tx` when given, which
    /// also selects the `with-timestamps` endpoint).
    #[instrument(skip_all, fields(chars = text.len(), voice))]
    async fn stream_synthesis(
        &self,
        text: &str,
        voice: &str,
        hints: Option<&SynthesisHints>,
        audio_tx: mpsc::Sender<AudioFrame>,
        word_tx: Option<mpsc::Sender<WordBoundary>>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let started = Instant::now();
        let with_timestamps = word_tx.is_some();
        let url = self.url(voice, with_timestamps);

        let language_code = hints
            .and_then(|h| h.language.as_deref())
            .or(self.config.language_code.as_deref());

        let request_body = ElevenLabsTtsRequest {
            text,
            model_id: &self.config.model_id,
            language_code,
            voice_settings: self.voice_settings(hints),
        };

        let key = voice_pipes_net::sensitive_header(self.config.api_key.expose()).map_err(
            |_invalid| TtsError::AuthError("the API key is not a valid header value".into()),
        )?;
        let response = self
            .client
            .post(&url)
            .header("xi-api-key", key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| TtsError::NetworkError(format!("ElevenLabs TTS request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = voice_pipes_net::error_body(response).await;
            return Err(map_http_error(status.as_u16(), &body));
        }

        let mut byte_stream = response.bytes_stream();
        let mut pcm = PcmFramer::new(self.config.sample_rate, self.chunk_samples());
        // NDJSON reassembly state (with-timestamps path).
        let mut lines = voice_pipes_net::BoundedLines::new(MAX_JSON_LINE_BYTES);
        let mut words = WordGrouper::new();
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
                TtsError::NetworkError(format!("ElevenLabs TTS stream read failed: {e}"))
            })?;
            if bytes.is_empty() {
                continue;
            }

            if with_timestamps {
                let complete = lines.push(&bytes).map_err(|e| {
                    TtsError::InferenceFailed(format!("ElevenLabs TTS stream: {e}"))
                })?;
                for line in complete {
                    self.consume_json_line(&line, &mut pcm, &mut words, word_tx.as_ref())
                        .await?;
                }
            } else {
                pcm.push(&bytes);
            }

            if !first_chunk_logged && pcm.samples_in() > 0 {
                first_chunk_logged = true;
                debug!(
                    elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    with_timestamps, "ElevenLabs TTS first audio bytes"
                );
            }
            if !pcm.send_frames(&audio_tx).await {
                return Ok(());
            }
        }

        // A final JSON object without a trailing newline.
        if let Some(last) = lines.finish() {
            self.consume_json_line(&last, &mut pcm, &mut words, word_tx.as_ref())
                .await?;
        }

        // Flush the trailing partial frame and the trailing partial word.
        pcm.send_frames(&audio_tx).await;
        pcm.send_rest(&audio_tx).await;
        if let Some(wtx) = word_tx.as_ref() {
            let mut out = Vec::new();
            words.flush(&mut out);
            for w in out {
                let _ = wtx.send(w).await;
            }
        }

        let total_elapsed = started.elapsed();
        let delivery_wait = pcm.delivery_wait();
        let compute_elapsed = total_elapsed.saturating_sub(delivery_wait);
        let total_samples = pcm.samples_out();
        if total_samples == 0 {
            // A success status with no audio would otherwise pass as a
            // spoken reply the caller never hears.
            return Err(TtsError::InferenceFailed(
                "ElevenLabs TTS returned no audio".into(),
            ));
        }
        self.emit_timing(text, total_samples, compute_elapsed, voice, delivery_wait);
        Ok(())
    }

    /// Decode one NDJSON chunk of a `with-timestamps` response: base64
    /// audio into `accum`, alignment into word boundaries on `word_tx`.
    pub(crate) async fn consume_json_line(
        &self,
        line: &[u8],
        pcm: &mut PcmFramer,
        words: &mut WordGrouper,
        word_tx: Option<&mpsc::Sender<WordBoundary>>,
    ) -> Result<(), TtsError> {
        let trimmed = line
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map_or(&[][..], |s| line.split_at(s).1);
        if trimmed.is_empty() {
            return Ok(());
        }
        let chunk: TimestampChunk = serde_json::from_slice(trimmed).map_err(|e| {
            TtsError::InferenceFailed(format!(
                "ElevenLabs TTS: malformed stream chunk: {}",
                voice_pipes_sdk::secrets::json_error(&e)
            ))
        })?;

        if let Some(b64) = chunk.audio_base64.as_deref() {
            let pcm_bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    TtsError::InferenceFailed(format!("ElevenLabs TTS: base64 decode failed: {e}"))
                })?;
            pcm.push(&pcm_bytes);
        }

        if let Some(wtx) = word_tx
            && let Some(a) = chunk
                .alignment
                .as_ref()
                .or(chunk.normalized_alignment.as_ref())
        {
            let mut out = Vec::new();
            a.group_into(words, &mut out);
            for w in out {
                // Receiver dropped: caller no longer wants word data;
                // audio keeps streaming.
                let _ = wtx.send(w).await;
            }
        }
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
            let _ = tx.try_send(voice_pipes_sdk::tts::detail_event(
                "tts:elevenlabs",
                text.chars().count(),
                sample_count,
                self.config.sample_rate,
                elapsed,
                serde_json::json!({
                    "voice": voice,
                    "provider": "elevenlabs",
                    "model": self.config.model_id,
                    "delivery_wait_ms": u64::try_from(delivery_wait.as_millis()).unwrap_or(u64::MAX),
                }),
            ));
        }
    }
}

#[async_trait]
impl TtsProvider for ElevenLabsTts {
    /// Reachability probe: any HTTP response (the synthesis URL answers a
    /// bare `GET` with 405) proves the endpoint is up; only connect/timeout
    /// errors mean it's down. No synthesis, no quota spend.
    async fn warm_up(&self) -> Result<(), TtsError> {
        let url = self.url(&self.config.current_voice(), false);
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

    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let voice = (*self.config.current_voice()).clone();
        let stream = voice_pipes_sdk::tts::collect_stream(self.config.sample_rate, |tx| {
            self.stream_synthesis(
                text,
                &voice,
                None,
                tx,
                None,
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
            self.stream_synthesis(text, &voice, hints, tx, req.word_tx, cancel),
        )
        .await
    }

    fn supports_word_timestamps(&self) -> bool {
        self.config.word_timestamps
    }

    fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    fn as_component(&self) -> Option<std::sync::Arc<dyn Component>> {
        Some(std::sync::Arc::new(CoreComponent::new(
            std::sync::Arc::clone(&self.core),
        )))
    }
}
