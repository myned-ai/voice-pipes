//! Generic gRPC audio post-processor (feature `post-grpc`).
//!
//! Two shapes over one endpoint, chosen by `mode` in the config:
//!
//! * **Inline** ([`GrpcPostProcessor`], `mode = "inline"`): streams each audio
//!   chunk to a remote `AudioPostProcessor` service and attaches the service's
//!   opaque JSON response verbatim to the chunk's
//!   [`extras`](crate::pipeline::AudioChunk::extras), which the WS transport
//!   forwards as `audio_meta.extras`.
//! * **Annotate** ([`annotate::GrpcAnnotator`], `mode = "annotate"`): drives
//!   the bidirectional `AudioAnnotator.Annotate` stream off the audio path
//!   (or, for a unary-only service, calls `Process` per batched segment) and
//!   emits `audio_annotation` frames.
//!
//! Domain-agnostic by construction: voice-pipes never parses the payload.
//! The blendshape / viseme / phoneme meaning lives entirely in the remote
//! service.
//!
//! # Statelessness
//!
//! A single processor instance and gRPC connection are shared across every
//! session and both engines (cascade + realtime), and the trait method is
//! `&self`. The service therefore MUST derive its output from the request
//! alone -- audio, sample rate, utterance id and the end-of-utterance flag
//! all ride [`ProcessRequest`](proto::ProcessRequest). Per-session buffering
//! and cross-chunk stateful context are intentionally out of scope for the
//! inline contract; the annotate stream is per session and may keep context
//! (see `proto/audio_post_processor.proto`), see the per-session note in
//! [`transport::serve::runner`](crate::transport).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tonic::transport::Channel;

use crate::config::{AudioPostProcessorConfig, ConfigError};
use crate::grpc::auth::{AuthedChannel, authed_channel};
use crate::transport::ServerMessage;

use super::{AudioChunk, AudioPostProcessor, AudioPostProcessorError, AudioPostProcessorFactory};

pub(crate) mod proto {
    #![allow(
        dead_code,
        clippy::all,
        clippy::pedantic,
        clippy::restriction,
        reason = "tonic-generated code"
    )]
    tonic::include_proto!("voice_pipes.post");
}
use proto::audio_post_processor_client::AudioPostProcessorClient;

pub(crate) mod annotate;

/// Default per-call deadline. The chain runs inline before the pacer, so a
/// slow service must not stall audio; on timeout the chunk continues with
/// no extras attached (non-fatal).
const DEFAULT_TIMEOUT_MS: u64 = 1000;

// ── Shared helpers ──────────────────────────────────────────

/// Pack `i16` PCM samples into little-endian bytes for the wire.
pub(crate) fn samples_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Parse the opaque `extras_json` a service returned. `Ok(None)` for an
/// empty string (nothing to attach), `Err` for malformed JSON.
pub(crate) fn parse_extras_json(
    json: &str,
) -> Result<Option<serde_json::Value>, serde_json::Error> {
    if json.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<serde_json::Value>(json).map(Some)
}

// ── Processor ───────────────────────────────────────────────

pub struct GrpcPostProcessor {
    name: String,
    endpoint: String,
    client: AudioPostProcessorClient<AuthedChannel>,
    timeout: Duration,
    options: HashMap<String, String>,
    seq: AtomicU64,
}

impl std::fmt::Debug for GrpcPostProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcPostProcessor")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AudioPostProcessor for GrpcPostProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    async fn process(
        &self,
        chunk: &mut AudioChunk,
        _event_tx: &mpsc::Sender<ServerMessage>,
    ) -> Result<(), AudioPostProcessorError> {
        // Nothing to send and no boundary to signal: skip the round-trip.
        // (Every EOU sentinel still calls through so the service can reset.)
        if chunk.samples.is_empty() && !chunk.is_end_of_utterance {
            return Ok(());
        }

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let request = proto::ProcessRequest {
            audio: samples_to_le_bytes(&chunk.samples),
            sample_rate: chunk.sample_rate,
            channels: u32::from(chunk.channels.max(1)),
            utterance_id: chunk.utterance_id,
            seq,
            end_of_utterance: chunk.is_end_of_utterance,
            options: self.options.clone(),
        };

        let mut client = self.client.clone();
        let call = client.process(tonic::Request::new(request));
        let response = match tokio::time::timeout(self.timeout, call).await {
            Ok(Ok(resp)) => resp.into_inner(),
            Ok(Err(status)) => {
                return Err(AudioPostProcessorError {
                    processor: self.name.clone(),
                    message: format!("gRPC Process failed: {status}"),
                    source: None,
                });
            }
            Err(_elapsed) => {
                return Err(AudioPostProcessorError {
                    processor: self.name.clone(),
                    message: format!(
                        "gRPC Process timed out after {} ms",
                        self.timeout.as_millis()
                    ),
                    source: None,
                });
            }
        };

        match parse_extras_json(&response.extras_json) {
            // Attach verbatim -- vp never inspects the shape.
            Ok(Some(value)) => {
                chunk.extras = Some(value);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => Err(AudioPostProcessorError {
                processor: self.name.clone(),
                message: format!("service returned invalid JSON: {e}"),
                source: Some(Box::new(e)),
            }),
        }
    }
}

// ── Factory ─────────────────────────────────────────────────

#[derive(Debug)]
pub struct GrpcPostProcessorFactory;

impl GrpcPostProcessorFactory {
    fn endpoint(cfg: &AudioPostProcessorConfig) -> Option<String> {
        cfg.options
            .get("endpoint")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    /// `name` from the config, defaulting to the type name.
    fn processor_name(cfg: &AudioPostProcessorConfig) -> String {
        cfg.options
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("grpc")
            .to_string()
    }

    /// Static string→string knobs (`[audio_post_processor.options]`)
    /// forwarded verbatim to the service.
    fn static_options(cfg: &AudioPostProcessorConfig) -> HashMap<String, String> {
        cfg.options
            .get("options")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Build the authed channel without connecting.
    ///
    /// `connect_lazy` establishes (and re-establishes) the HTTP/2 connection
    /// on first use, so a transiently unavailable service never fails worker
    /// startup and errors stay non-fatal inside the chain / hub.
    fn lazy_channel(endpoint: &str) -> Result<AuthedChannel, anyhow::Error> {
        let channel = Channel::from_shared(endpoint.to_string())
            .map_err(|e| anyhow::anyhow!("invalid gRPC endpoint \"{endpoint}\": {e}"))?
            .connect_lazy();
        Ok(authed_channel(channel))
    }
}

impl AudioPostProcessorFactory for GrpcPostProcessorFactory {
    fn name(&self) -> &'static str {
        "grpc"
    }

    fn validate(&self, cfg: &AudioPostProcessorConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        match Self::endpoint(cfg) {
            None => errors.push(ConfigError::new(
                "audio_post_processor.endpoint",
                "grpc post-processor requires a string `endpoint` \
                          (e.g. \"http://127.0.0.1:50051\")",
            )),
            Some(ep) => {
                if Channel::from_shared(ep.clone()).is_err() {
                    errors.push(ConfigError::new(
                        "audio_post_processor.endpoint",
                        format!("invalid gRPC endpoint URI: \"{ep}\""),
                    ));
                }
            }
        }
        errors
    }

    fn build(
        &self,
        cfg: &AudioPostProcessorConfig,
    ) -> Result<Box<dyn AudioPostProcessor>, anyhow::Error> {
        let endpoint = Self::endpoint(cfg)
            .ok_or_else(|| anyhow::anyhow!("grpc post-processor requires `endpoint`"))?;
        let client = AudioPostProcessorClient::new(Self::lazy_channel(&endpoint)?);

        // `timeout_ms` is a typed field on the config (shared with the
        // annotate mode), not an entry in the flattened `options` map.
        let timeout_ms = cfg.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let name = Self::processor_name(cfg);
        let options = Self::static_options(cfg);

        tracing::info!(
            endpoint = %endpoint,
            timeout_ms,
            "configured grpc audio post-processor",
        );

        Ok(Box::new(GrpcPostProcessor {
            name,
            endpoint,
            client,
            timeout: Duration::from_millis(timeout_ms),
            options,
            seq: AtomicU64::new(0),
        }))
    }

    fn build_annotator(
        &self,
        cfg: &AudioPostProcessorConfig,
    ) -> Result<Arc<dyn crate::pipeline::annotate::AudioAnnotator>, anyhow::Error> {
        let endpoint = Self::endpoint(cfg)
            .ok_or_else(|| anyhow::anyhow!("grpc audio annotator requires `endpoint`"))?;
        let channel = Self::lazy_channel(&endpoint)?;
        let name = Self::processor_name(cfg);
        let options = Self::static_options(cfg);
        // Per-segment budget for the unary fallback (annotate default 250 ms).
        let timeout_ms = cfg.annotator_config().timeout_ms;

        tracing::info!(
            endpoint = %endpoint,
            timeout_ms,
            "configured grpc audio annotator",
        );

        Ok(Arc::new(annotate::GrpcAnnotator::new(
            name,
            endpoint,
            channel,
            options,
            Duration::from_millis(timeout_ms),
        )))
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/grpc.rs"]
mod tests;
