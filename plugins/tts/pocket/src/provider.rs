//! The `TtsProvider`: the `POST /tts` request, the streamed WAV reply turned
//! into audio frames, and the silent-start probe that re-requests a fragment.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, instrument, warn};
use voice_pipes_net::reqwest::Client;

use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::TraceEvent;
use voice_pipes_sdk::audio::{PcmFramer, WavStripper};
use voice_pipes_sdk::component::{Component, ComponentCore, CoreComponent};
use voice_pipes_sdk::{SynthesisRequest, TtsError, TtsProvider};

use crate::config::{PocketHttpConfig, SAMPLE_RATE, SilentStartGuard, is_remote_voice};

/// Frame size emitted to the audio channel (~100 ms @ 24 kHz).
const STREAM_CHUNK_SAMPLES: usize = 2400;

// ── Provider ────────────────────────────────────────────────

/// Pocket-TTS HTTP client provider.
pub(crate) struct PocketHttpTts {
    pub(crate) client: Client,
    pub(crate) config: PocketHttpConfig,
    pub(crate) audit_tx: Option<mpsc::Sender<TraceEvent>>,
    pub(crate) core: std::sync::Arc<ComponentCore>,
}

impl std::fmt::Debug for PocketHttpTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PocketHttpTts")
            .field("endpoint", &self.config.endpoint)
            .field("voice", &*self.config.current_voice())
            .finish_non_exhaustive()
    }
}

impl PocketHttpTts {
    fn url(&self) -> String {
        format!("{}/tts", self.config.endpoint.trim_end_matches('/'))
    }

    /// Send the synthesis request and stream PCM `i16` samples back through
    /// `audio_tx` as `AudioFrame`s, re-requesting the fragment when the
    /// silent-start guard trips (crate docs).
    #[instrument(skip_all, fields(chars = text.len(), voice))]
    async fn stream_synthesis(
        &self,
        text: &str,
        voice: &str,
        audio_tx: mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let attempts = 1 + self.config.silent_start.map_or(0, |g| g.retries);
        for attempt in 1..=attempts {
            // The last allowed attempt streams to completion whatever it
            // sounds like: silence beats no fragment at all.
            let guard = if attempt < attempts {
                self.config.silent_start
            } else {
                None
            };
            match self
                .stream_synthesis_once(
                    text,
                    voice,
                    &audio_tx,
                    cancel.clone(),
                    guard,
                    attempt.saturating_sub(1),
                )
                .await?
            {
                StreamEnd::Done => return Ok(()),
                StreamEnd::SilentStart { window_ms, peak } => {
                    warn!(
                        attempt,
                        attempts,
                        window_ms,
                        peak,
                        chars = text.chars().count(),
                        "pocket-tts produced no sound at the start of the fragment; re-synthesizing"
                    );
                    if let Some(ref tx) = self.audit_tx {
                        let _ = tx.try_send(TraceEvent::new(
                            "tts:pocket-http",
                            "tts_silent_retry",
                            serde_json::json!({
                                "attempt": attempt,
                                "window_ms": window_ms,
                                "peak": peak,
                                "chars": text.chars().count(),
                            }),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// One HTTP synthesis request. Returns [`StreamEnd::SilentStart`] as soon
    /// as the guard window has passed without sound (the response is dropped,
    /// which aborts the request); [`StreamEnd::Done`] after the full stream.
    async fn stream_synthesis_once(
        &self,
        text: &str,
        voice: &str,
        audio_tx: &mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
        guard: Option<SilentStartGuard>,
        retries: u32,
    ) -> Result<StreamEnd, TtsError> {
        let started = Instant::now();
        let url = self.url();
        let mut probe = guard.map(SilentStartProbe::new);

        let form = [("text", text), ("voice_url", voice)];
        let response = self
            .client
            .post(&url)
            .form(&form)
            .send()
            .await
            .map_err(|e| TtsError::NetworkError(format!("pocket-tts request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = voice_pipes_net::error_body(response).await;
            return Err(map_http_error(status.as_u16(), &body));
        }

        let mut byte_stream = response.bytes_stream();
        // The sidecar always answers with WAV; a stream without a header is
        // refused rather than played as noise.
        let mut wav = WavStripper::requiring_header();
        let mut pcm = PcmFramer::new(SAMPLE_RATE, STREAM_CHUNK_SAMPLES);
        let mut first_chunk_logged = false;

        loop {
            let item = tokio::select! {
                () = cancel.cancelled() => {
                    return Ok(StreamEnd::Done);
                }
                res = byte_stream.next() => {
                    match res {
                        Some(val) => val,
                        None => break,
                    }
                }
            };

            let bytes = item.map_err(|e| {
                TtsError::NetworkError(format!("pocket-tts stream read failed: {e}"))
            })?;
            let pcm_bytes = wav
                .feed(&bytes)
                .map_err(|e| TtsError::InferenceFailed(format!("pocket-tts: {e}")))?;
            let decoded = pcm.push(&pcm_bytes);
            if let Some(p) = probe.as_mut() {
                p.observe(decoded);
            }
            if !first_chunk_logged && pcm.samples_in() > 0 {
                first_chunk_logged = true;
                debug!(
                    elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "pocket-tts first audio bytes"
                );
            }
            if !pcm.send_frames(audio_tx).await {
                return Ok(StreamEnd::Done);
            }

            if let Some(p) = probe.as_ref()
                && let Some(peak) = p.silent()
            {
                // Dropping `byte_stream` (the response) aborts the request.
                return Ok(StreamEnd::SilentStart {
                    window_ms: (p.guard.window_samples * 1000 / SAMPLE_RATE as usize) as u64,
                    peak,
                });
            }
        }

        pcm.send_rest(audio_tx).await;

        if !wav.stripped_header() {
            return Err(TtsError::InferenceFailed(
                "pocket-tts: server returned an empty/invalid WAV stream".into(),
            ));
        }

        let total_elapsed = started.elapsed();
        let delivery_wait = pcm.delivery_wait();
        let compute_elapsed = total_elapsed.saturating_sub(delivery_wait);
        let total_samples = pcm.samples_out();
        if total_samples == 0 {
            // A whole header and no samples would otherwise pass as a
            // spoken reply the caller never hears.
            return Err(TtsError::InferenceFailed(
                "pocket-tts: server returned no audio".into(),
            ));
        }
        self.emit_timing(
            text,
            total_samples,
            compute_elapsed,
            voice,
            delivery_wait,
            retries,
        );
        Ok(StreamEnd::Done)
    }

    fn emit_timing(
        &self,
        text: &str,
        sample_count: usize,
        elapsed: Duration,
        voice: &str,
        delivery_wait: Duration,
        retries: u32,
    ) {
        if let Some(ref tx) = self.audit_tx {
            let _ = tx.try_send(voice_pipes_sdk::tts::detail_event(
                "tts:pocket-http",
                text.chars().count(),
                sample_count,
                SAMPLE_RATE,
                elapsed,
                serde_json::json!({
                    "voice": voice,
                    "provider": "pocket-http",
                    "endpoint": self.config.endpoint,
                    "delivery_wait_ms": u64::try_from(delivery_wait.as_millis()).unwrap_or(u64::MAX),
                    "retries": retries,
                }),
            ));
        }
    }
}

#[async_trait]
impl TtsProvider for PocketHttpTts {
    async fn device_info(&self) -> Option<voice_pipes_sdk::DeviceInfo> {
        // Kyutai Pocket-TTS is a CPU-only sidecar by construction; it exposes
        // no device endpoint to probe.
        Some(voice_pipes_sdk::DeviceInfo::static_(
            "cpu",
            "pocket-tts is CPU-only",
        ))
    }

    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let voice = (*self.config.current_voice()).clone();
        let stream = voice_pipes_sdk::tts::collect_stream(SAMPLE_RATE, |tx| {
            self.stream_synthesis(text, &voice, tx, tokio_util::sync::CancellationToken::new())
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
        // Only the operator's configured voice may be a URL (the
        // sidecar downloads it). A per-turn hint — which can originate from
        // an LLM tool result or a client-side tool — falls back to the
        // configured voice unless `allow_voice_urls = true`.
        let voice = match hints.and_then(|h| h.voice.clone()) {
            Some(v) if is_remote_voice(&v) && !self.config.allow_voice_urls => {
                warn!(
                    voice = %v,
                    "pocket-tts: URL voice hint refused (allow_voice_urls = false); using configured voice"
                );
                (*self.config.current_voice()).clone()
            }
            Some(v) => v,
            None => (*self.config.current_voice()).clone(),
        };
        let token = cancel.clone();
        voice_pipes_sdk::tts::recorded_unless_cancelled(
            &self.core,
            &token,
            self.stream_synthesis(text, &voice, tx, cancel),
        )
        .await
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn name(&self) -> &'static str {
        "pocket-http"
    }

    fn as_component(&self) -> Option<std::sync::Arc<dyn Component>> {
        Some(std::sync::Arc::new(CoreComponent::new(
            std::sync::Arc::clone(&self.core),
        )))
    }
}

// ── Helpers ─────────────────────────────────────────────────

/// How one synthesis request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEnd {
    /// The server finished the stream (or the caller cancelled).
    Done,
    /// The guard window passed with no sound; the request was abandoned.
    SilentStart { window_ms: u64, peak: i16 },
}

/// Peak tracker over the first `window_samples` of a fragment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SilentStartProbe {
    guard: SilentStartGuard,
    seen: usize,
    peak: i16,
    decided: bool,
}

impl SilentStartProbe {
    pub(crate) fn new(guard: SilentStartGuard) -> Self {
        Self {
            guard,
            seen: 0,
            peak: 0,
            decided: false,
        }
    }

    /// Fold decoded samples in; only the first `window_samples` count.
    pub(crate) fn observe(&mut self, samples: &[i16]) {
        if self.decided || self.seen >= self.guard.window_samples {
            return;
        }
        let take = samples.len().min(self.guard.window_samples - self.seen);
        for &s in samples.iter().take(take) {
            self.peak = self.peak.max(s.saturating_abs());
        }
        self.seen += take;
        if self.peak >= self.guard.peak_threshold {
            // Sound found — no need to keep looking.
            self.decided = true;
        }
    }

    /// `Some(peak)` once the whole window has been seen without sound.
    pub(crate) fn silent(&self) -> Option<i16> {
        (!self.decided
            && self.seen >= self.guard.window_samples
            && self.peak < self.guard.peak_threshold)
            .then_some(self.peak)
    }
}

fn map_http_error(status: u16, body: &str) -> TtsError {
    TtsError::from_http_status("pocket-tts", status, body)
}
