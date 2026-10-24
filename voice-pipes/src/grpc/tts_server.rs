//! gRPC server wrapper for TTS -- wraps a local [`TtsProvider`] behind a
//! tonic [`TtsService`] implementation.

use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::audio::AudioFrame;
use crate::tts::{SynthesisHints, TtsProvider};

/// Auto-generated gRPC server stubs for the TTS service.
mod proto {
    #![allow(
        dead_code,
        clippy::all,
        clippy::pedantic,
        clippy::restriction,
        reason = "tonic-generated code"
    )]
    tonic::include_proto!("voice_pipes.tts");
}

mod params {
    pub(crate) use crate::grpc::params::proto::*;
}

pub use proto::tts_service_server::TtsServiceServer;

/// Wraps a local [`TtsProvider`] as a gRPC service.
pub struct TtsServiceImpl {
    provider: Arc<dyn TtsProvider>,
    param_schema: Vec<crate::api::param_schema::ParamDescriptor>,
    current_values: serde_json::Map<String, serde_json::Value>,
}

impl std::fmt::Debug for TtsServiceImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtsServiceImpl")
            .field("provider", &self.provider.name())
            .finish_non_exhaustive()
    }
}

impl TtsServiceImpl {
    pub fn new(
        provider: Arc<dyn TtsProvider>,
        param_schema: Vec<crate::api::param_schema::ParamDescriptor>,
        current_values: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            provider,
            param_schema,
            current_values,
        }
    }
}

use crate::grpc::params::{map_current_values_to_metadata, param_descriptor_to_proto};

type GrpcStream<T> = Pin<Box<dyn futures_util::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl proto::tts_service_server::TtsService for TtsServiceImpl {
    async fn synthesize(
        &self,
        request: Request<proto::SynthesizeRequest>,
    ) -> Result<Response<proto::AudioFrame>, Status> {
        let req = request.into_inner();

        let frame = if let Some(hints) = req.hints.as_ref() {
            let domain_hints = proto_to_hints(hints);
            // Drive the streaming path (which falls back to batch internally for
            // providers without a native stream) and coalesce into one frame.
            //
            // The provider is driven on its own task while this one
            // drains the channel. Awaiting the provider to completion first
            // (with a 1-slot channel) worked only for the default one-frame
            // implementation; any natively streaming provider blocked on its
            // second `send` forever and the RPC never returned.
            let (tx, mut rx) = mpsc::channel::<AudioFrame>(16);
            let provider = Arc::clone(&self.provider);
            let text = req.text.clone();
            let producer = tokio::spawn(async move {
                provider
                    .synthesize_stream(
                        crate::tts::SynthesisRequest::new(&text).with_hints(Some(&domain_hints)),
                        tx,
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
            });
            // Collect all frames into one.
            let mut all_samples = Vec::new();
            let mut sr = 0u32;
            while let Some(f) = rx.recv().await {
                sr = f.sample_rate;
                all_samples.extend(f.samples);
            }
            match producer.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(super::status::tts_error_to_status(&e)),
                Err(_) => return Err(Status::internal("synthesis task terminated unexpectedly")),
            }
            // Reject an empty synthesis instead of returning a frame with
            // sample_rate = 0 (downstream divide-by-zero).
            if all_samples.is_empty() || sr == 0 {
                return Err(Status::internal("synthesis produced no audio"));
            }
            AudioFrame {
                samples: all_samples,
                sample_rate: sr,
                channels: 1,
            }
        } else {
            self.provider
                .synthesize(&req.text)
                .await
                .map_err(|e| super::status::tts_error_to_status(&e))?
        };

        Ok(Response::new(audio_frame_to_proto(&frame)))
    }

    type SynthesizeStreamStream = GrpcStream<proto::AudioFrame>;

    async fn synthesize_stream(
        &self,
        request: Request<proto::SynthesizeRequest>,
    ) -> Result<Response<Self::SynthesizeStreamStream>, Status> {
        let req = request.into_inner();
        let (tx, rx) = mpsc::channel::<AudioFrame>(64);

        let provider = Arc::clone(&self.provider);
        let text = req.text.clone();
        let hints = req.hints.as_ref().map(proto_to_hints);
        // The cancel guard rides in the response stream (as the
        // STT server already does): a client that disconnects drops the
        // stream, which cancels the provider instead of letting a billable
        // synthesis run to completion for nobody.
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_guard = cancel.clone().drop_guard();

        let handle = tokio::spawn(async move {
            provider
                .synthesize_stream(
                    crate::tts::SynthesisRequest::new(&text).with_hints(hints.as_ref()),
                    tx,
                    cancel,
                )
                .await
        });

        // Stream frames, then a trailing error if synthesis failed, so the
        // client can tell a crash from a clean end.
        let frames = ReceiverStream::new(rx)
            .map(|frame| audio_frame_to_proto(&frame))
            .map(Ok);
        let trailer = futures_util::stream::once(async move {
            let _cancel_guard = cancel_guard;
            match handle.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(Err(super::status::tts_error_to_status(&e))),
                Err(_) => Some(Err(Status::internal(
                    "synthesis task terminated unexpectedly",
                ))),
            }
        })
        .filter_map(|x| x);
        let output = frames.chain(trailer);

        Ok(Response::new(Box::pin(output)))
    }

    async fn get_info(
        &self,
        _request: Request<proto::Empty>,
    ) -> Result<Response<proto::TtsInfo>, Status> {
        let metadata = map_current_values_to_metadata(&self.current_values);
        let param_schema = self
            .param_schema
            .iter()
            .map(param_descriptor_to_proto)
            .collect();

        Ok(Response::new(proto::TtsInfo {
            sample_rate: self.provider.sample_rate(),
            provider_name: self.provider.name().to_string(),
            metadata,
            param_schema,
        }))
    }
}

// ── Conversions ─────────────────────────────────────────────

fn audio_frame_to_proto(frame: &AudioFrame) -> proto::AudioFrame {
    let samples_bytes: Vec<u8> = frame.samples.iter().flat_map(|s| s.to_le_bytes()).collect();

    proto::AudioFrame {
        samples_i16: samples_bytes,
        sample_rate: frame.sample_rate,
        channels: u32::from(frame.channels),
    }
}

fn proto_to_hints(h: &proto::SynthesisHints) -> SynthesisHints {
    SynthesisHints::default()
        .with_voice(h.voice.clone())
        .with_language(h.language.clone())
        .with_speed(h.speed)
        .with_options(
            h.options
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        )
}

use tokio_stream::StreamExt;

#[cfg(test)]
#[path = "tests/tts_server.rs"]
mod tests;
