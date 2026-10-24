//! gRPC server wrapper for STT -- wraps a local [`SttProvider`] behind a
//! tonic [`SttService`] implementation.

use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::stt::{AudioChunk, RecognitionRequest, SttProvider, TranscriptionResult};

/// Auto-generated gRPC server stubs for the STT service.
mod proto {
    #![allow(
        dead_code,
        clippy::all,
        clippy::pedantic,
        clippy::restriction,
        reason = "tonic-generated code"
    )]
    tonic::include_proto!("voice_pipes.stt");
}

mod params {
    pub(crate) use crate::grpc::params::proto::*;
}

pub use proto::stt_service_server::SttServiceServer;

/// Wraps a local [`SttProvider`] as a gRPC service.
pub struct SttServiceImpl {
    provider: Arc<dyn SttProvider>,
    param_schema: Vec<crate::api::param_schema::ParamDescriptor>,
    current_values: serde_json::Map<String, serde_json::Value>,
}

impl std::fmt::Debug for SttServiceImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttServiceImpl")
            .field("provider", &self.provider.name())
            .finish_non_exhaustive()
    }
}

impl SttServiceImpl {
    pub fn new(
        provider: Arc<dyn SttProvider>,
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
impl proto::stt_service_server::SttService for SttServiceImpl {
    async fn transcribe(
        &self,
        request: Request<proto::TranscribeRequest>,
    ) -> Result<Response<proto::TranscriptionResult>, Status> {
        let req = request.into_inner();
        let audio = proto_to_audio_frame(&req).map_err(|rate| {
            Status::invalid_argument(format!(
                "sample_rate {rate} Hz is outside {}..={} Hz",
                crate::audio::resample::SUPPORTED_RATES.start(),
                crate::audio::resample::SUPPORTED_RATES.end()
            ))
        })?;

        let result = self
            .provider
            .transcribe(&audio)
            .await
            .map_err(|e| super::status::stt_error_to_status(&e))?;

        Ok(Response::new(transcription_to_proto(&result)))
    }

    type TranscribeStreamStream = GrpcStream<proto::TranscriptionResult>;

    async fn transcribe_stream(
        &self,
        request: Request<Streaming<proto::AudioChunk>>,
    ) -> Result<Response<Self::TranscribeStreamStream>, Status> {
        let mut inbound = request.into_inner();

        // Channel: forward proto chunks -> domain AudioChunks.
        let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(64);
        // Channel: domain TranscriptionResults -> proto results.
        let (result_tx, result_rx) = mpsc::channel::<TranscriptionResult>(64);

        // Spawn the provider transcription stream, keeping its result so
        // a failure is surfaced to the client instead of a clean EOF.
        // The cancel guard rides in the response stream: a client disconnect
        // drops the stream, which cancels the provider session instead of
        // leaving it open.
        let provider = Arc::clone(&self.provider);
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_guard = cancel.clone().drop_guard();
        let handle = tokio::spawn(async move {
            let mut rec_req = RecognitionRequest::default();
            // Read initial chunk to obtain session configuration (language, slot_wait)
            match inbound.message().await {
                Ok(Some(chunk)) => {
                    if let Some(ref l) = chunk.language {
                        rec_req.language = Some(l.clone());
                    }
                    if let Some(wait_ms) = chunk.slot_wait_ms {
                        rec_req.slot_wait = Some(std::time::Duration::from_millis(wait_ms));
                    }
                    let samples: Vec<i16> = chunk
                        .samples_i16
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| i16::from_le_bytes(*c))
                        .collect();
                    let ac = AudioChunk {
                        samples,
                        is_last: chunk.is_last,
                    };
                    if audio_tx.send(ac).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(None) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "STT inbound audio stream error on initial chunk");
                    return Err(crate::stt::SttError::new(
                        provider.name(),
                        format!("failed to receive initial audio chunk: {e}"),
                    ));
                }
            }

            // Spawn task to forward remaining inbound chunks
            tokio::spawn(async move {
                while let Ok(Some(chunk)) = inbound.message().await {
                    let samples: Vec<i16> = chunk
                        .samples_i16
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| i16::from_le_bytes(*c))
                        .collect();
                    let ac = AudioChunk {
                        samples,
                        is_last: chunk.is_last,
                    };
                    if audio_tx.send(ac).await.is_err() {
                        break;
                    }
                }
            });

            provider
                .transcribe_stream(rec_req, audio_rx, result_tx, cancel)
                .await
        });

        // Convert domain results -> proto stream, then append a trailing error
        // if the provider stream failed.
        let results = ReceiverStream::new(result_rx)
            .map(|r| transcription_to_proto(&r))
            .map(Ok);
        let trailer = futures_util::stream::once(async move {
            let _cancel_guard = cancel_guard;
            match handle.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(Err(super::status::stt_error_to_status(&e))),
                Err(_) => Some(Err(Status::internal(
                    "transcription task terminated unexpectedly",
                ))),
            }
        })
        .filter_map(|x| x);
        let output = results.chain(trailer);

        Ok(Response::new(Box::pin(output)))
    }

    async fn get_info(
        &self,
        _request: Request<proto::Empty>,
    ) -> Result<Response<proto::SttInfo>, Status> {
        let metadata = map_current_values_to_metadata(&self.current_values);
        let param_schema = self
            .param_schema
            .iter()
            .map(param_descriptor_to_proto)
            .collect();
        let capabilities = self
            .provider
            .capabilities()
            .iter()
            .map(|c| format!("{c:?}"))
            .collect();

        Ok(Response::new(proto::SttInfo {
            expected_sample_rate: self.provider.expected_sample_rate(),
            provider_name: self.provider.name().to_string(),
            metadata,
            param_schema,
            capabilities,
            needs_flush_silence: self.provider.needs_flush_silence(),
        }))
    }
}

// ── Conversions ─────────────────────────────────────────────

use crate::audio::AudioFrame;

/// The request's audio as a frame, or the rate refused: one outside what
/// the resamplers take is refused here, where it enters, since proto3 sends
/// 0 when a client leaves `sample_rate` out.
fn proto_to_audio_frame(req: &proto::TranscribeRequest) -> Result<AudioFrame, u32> {
    if !crate::audio::resample::SUPPORTED_RATES.contains(&req.sample_rate) {
        return Err(req.sample_rate);
    }
    let samples: Vec<i16> = req
        .samples_i16
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();

    Ok(AudioFrame {
        samples,
        sample_rate: req.sample_rate,
        // Proto3 defaults `channels` to 0 when a client omits it;
        // oversized values would wrap. Mono is the only sensible reading.
        channels: u16::try_from(req.channels).unwrap_or(1).max(1),
    })
}

fn transcription_to_proto(r: &TranscriptionResult) -> proto::TranscriptionResult {
    proto::TranscriptionResult {
        text: r.text.clone(),
        is_final: r.is_final,
        confidence: r.confidence,
        language: r.language.clone(),
        segment_index: r.segment_index,
        start_time_ms: r.start_time.map(|d| d.as_millis() as u64),
        end_time_ms: r.end_time.map(|d| d.as_millis() as u64),
        words: r
            .words
            .iter()
            .map(|w| proto::WordInfo {
                word: w.word.clone(),
                start_ms: w.start.as_millis() as u64,
                end_ms: w.end.as_millis() as u64,
                confidence: w.confidence,
            })
            .collect(),
        is_end_of_turn: r.is_end_of_turn,
        speaker_id: r.speaker_id,
        speech_started: r.speech_started,
        inference_metrics: r
            .inference_metrics
            .as_ref()
            .map(|m| proto::InferenceMetrics {
                audio_duration_ms: m.audio_duration_ms,
                inference_count: m.inference_count,
                rtfx: m.rtfx,
                algorithmic_delay_ms: m.algorithmic_delay_ms,
                effective_latency_ms: m.effective_latency_ms,
                streaming_window_ms: m.streaming_window_ms,
            }),
    }
}

#[cfg(test)]
#[path = "tests/stt_server.rs"]
mod tests;
