//! gRPC STT client provider -- connects to a remote STT service.
//!
//! Implements [`SttProvider`] by forwarding calls over gRPC, allowing the
//! STT stage to run in a separate process or container (split-mode).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::grpc::auth::{AuthedChannel, authed_channel};
use crate::grpc::client;

use crate::audio::AudioFrame;
use crate::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use crate::config::SttConfig;
use crate::stt::{
    AudioChunk, RecognitionRequest, SttCapability, SttError, SttFactory, SttProvider,
    TranscriptionResult, WordInfo,
};

pub(crate) mod params {
    #![allow(
        dead_code,
        clippy::all,
        clippy::pedantic,
        clippy::restriction,
        reason = "tonic-generated code"
    )]
    tonic::include_proto!("voice_pipes.params");
}
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

// ── Provider ────────────────────────────────────────────────

/// gRPC-backed STT provider that delegates to a remote [`SttService`].
#[derive(Debug)]
pub struct GrpcSttProvider {
    client: proto::stt_service_client::SttServiceClient<AuthedChannel>,
    #[expect(dead_code, reason = "retained for diagnostics and reconnection")]
    endpoint: String,
    sample_rate: u32,
    provider_name: String,
    core: Arc<ComponentCore>,
    capabilities: HashSet<SttCapability>,
    needs_flush_silence: bool,
}

impl GrpcSttProvider {
    /// Connect to the remote STT service and query its metadata.
    pub async fn connect(endpoint: &str) -> Result<Self, anyhow::Error> {
        let channel = client::connect(endpoint, client::CONNECT_TIMEOUT, "STT").await?;

        let mut client = proto::stt_service_client::SttServiceClient::new(authed_channel(channel));

        // Query provider metadata to learn expected sample rate and name.
        let info = client::within(
            client::CONNECT_TIMEOUT,
            "STT GetInfo",
            client.get_info(proto::Empty {}),
        )
        .await?
        .into_inner();

        tracing::info!(
            endpoint = endpoint,
            provider = %info.provider_name,
            sample_rate = info.expected_sample_rate,
            "connected to remote STT service"
        );

        let mut dynamic_schema = Vec::new();
        for p in info.param_schema {
            let value_type = match p.value_type.as_str() {
                "float" => crate::api::param_schema::ParamValueType::Float,
                "int" => crate::api::param_schema::ParamValueType::Int,
                "bool" => crate::api::param_schema::ParamValueType::Bool,
                "enum" => crate::api::param_schema::ParamValueType::Enum,
                "path" => crate::api::param_schema::ParamValueType::Path,
                // "str" and any unknown type both map to a plain string.
                _ => crate::api::param_schema::ParamValueType::Str,
            };

            let constraints = p.constraints.map(|c| {
                crate::api::param_schema::ParamConstraints::default()
                    .with_min(c.min)
                    .with_max(c.max)
                    .with_options(if c.options.is_empty() {
                        None
                    } else {
                        Some(c.options)
                    })
            });

            let only_when = p
                .only_when
                .map(|o| crate::api::param_schema::OnlyWhen::new(o.field, o.values));

            dynamic_schema.push(
                crate::api::param_schema::ParamDescriptor::new(
                    p.key,
                    p.label,
                    p.description,
                    value_type,
                )
                .with_only_when(only_when)
                .with_mutable(p.mutable)
                .with_constraints(constraints),
            );
        }

        let core = Arc::new(ComponentCore::new(
            format!("stt:grpc:{}", info.provider_name),
            ComponentKind::Stt,
        ));
        core.set_dynamic_schema(dynamic_schema);

        let capabilities: HashSet<SttCapability> = info
            .capabilities
            .into_iter()
            .filter_map(|s| match s.as_str() {
                "Streaming" => Some(SttCapability::Streaming),
                "InterimResults" => Some(SttCapability::InterimResults),
                "WordTimestamps" => Some(SttCapability::WordTimestamps),
                "BuiltinVad" => Some(SttCapability::BuiltinVad),
                "EndOfTurn" => Some(SttCapability::EndOfTurn),
                "Diarization" => Some(SttCapability::Diarization),
                "LanguageDetection" => Some(SttCapability::LanguageDetection),
                "CustomVocabulary" => Some(SttCapability::CustomVocabulary),
                "CumulativeFinals" => Some(SttCapability::CumulativeFinals),
                _ => None,
            })
            .collect();
        let capabilities = if capabilities.is_empty() {
            let mut caps = HashSet::new();
            caps.insert(SttCapability::Streaming);
            caps
        } else {
            capabilities
        };

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            sample_rate: info.expected_sample_rate,
            provider_name: info.provider_name.clone(),
            core,
            capabilities,
            needs_flush_silence: info.needs_flush_silence,
        })
    }
}

#[async_trait]
impl SttProvider for GrpcSttProvider {
    async fn transcribe(&self, audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = std::time::Instant::now();
        let samples_bytes = audio
            .samples
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect::<Vec<u8>>();

        let request = proto::TranscribeRequest {
            samples_i16: samples_bytes,
            sample_rate: audio.sample_rate,
            channels: u32::from(audio.channels),
        };

        let mut client = self.client.clone();
        let response = match client.transcribe(request).await.map_err(grpc_to_stt_error) {
            Ok(r) => r,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };

        self.core.record_ok(start.elapsed());
        Ok(proto_to_transcription(response.into_inner()))
    }

    async fn transcribe_stream(
        &self,
        request: RecognitionRequest,
        mut audio_rx: mpsc::Receiver<AudioChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        let cancel_outbound = cancel.clone();
        let lang = request.language;
        let slot_wait_ms = request.slot_wait.map(|d| d.as_millis() as u64);
        let mut is_first = true;
        let outbound = async_stream::stream! {
            while !cancel_outbound.is_cancelled() {
                let chunk_opt = tokio::select! {
                    () = cancel_outbound.cancelled() => None,
                    ch = audio_rx.recv() => ch,
                };
                let Some(chunk) = chunk_opt else { break; };
                let samples_bytes = chunk
                    .samples
                    .iter()
                    .flat_map(|s| s.to_le_bytes())
                    .collect::<Vec<u8>>();
                let (chunk_lang, chunk_slot_wait) = if is_first {
                    is_first = false;
                    (lang.clone(), slot_wait_ms)
                } else {
                    (None, None)
                };
                yield proto::AudioChunk {
                    samples_i16: samples_bytes,
                    is_last: chunk.is_last,
                    sample_rate: 0, // server knows its own rate
                    language: chunk_lang,
                    slot_wait_ms: chunk_slot_wait,
                };
            }
        };

        let mut client = self.client.clone();
        let response = match client
            .transcribe_stream(outbound)
            .await
            .map_err(grpc_to_stt_error)
        {
            Ok(r) => r,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };

        let mut stream = response.into_inner();
        let stream_start = std::time::Instant::now();
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                msg = stream.message() => match msg.map_err(grpc_to_stt_error) {
                    Ok(Some(result)) => {
                        let tr = proto_to_transcription(result);
                        if tr.is_final {
                            self.core.record_ok(stream_start.elapsed());
                        }
                        if tx.send(tr).await.is_err() {
                            break; // receiver dropped
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        self.core.record_error(&e);
                        return Err(e);
                    }
                }
            }
        }

        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn name(&self) -> &str {
        &self.provider_name
    }

    fn needs_flush_silence(&self) -> bool {
        self.needs_flush_silence
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        self.capabilities.clone()
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

// ── Factory ─────────────────────────────────────────────────

/// Factory for the gRPC STT provider.
#[derive(Debug)]
pub struct GrpcSttFactory;

impl SttFactory for GrpcSttFactory {
    fn name(&self) -> &'static str {
        "grpc"
    }

    fn aliases(&self) -> &[&str] {
        &["stt-grpc"]
    }

    fn is_local(&self) -> bool {
        false
    }

    fn validate(&self, cfg: &SttConfig) -> Vec<crate::config::ConfigError> {
        let mut errors = vec![];
        if cfg.endpoint.is_none() {
            errors.push(crate::config::ConfigError::new(
                "endpoint",
                "gRPC STT requires an endpoint URL",
            ));
        }
        // The client has no settings of its own.
        errors.extend(cfg.options().into_errors());
        errors
    }

    fn build(
        &self,
        cfg: &SttConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let endpoint = cfg
            .endpoint
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("gRPC STT requires `endpoint` in config"))?;
        let rt = tokio::runtime::Handle::current();
        let provider =
            tokio::task::block_in_place(|| rt.block_on(GrpcSttProvider::connect(endpoint)))?;
        Ok(Box::new(provider))
    }

    fn provider_schema(&self, cfg: &SttConfig) -> crate::api::param_schema::ProviderSchema {
        use crate::api::param_schema::{ParamDescriptor, ProviderSchema};
        let mut s = ProviderSchema::default();
        s.push(
            ParamDescriptor::static_str(
                "provider",
                "Provider",
                "The STT framework or service used for transcription.",
            ),
            cfg.provider.clone(),
        );
        if let Some(e) = &cfg.endpoint {
            s.push(
                ParamDescriptor::static_str(
                    "endpoint",
                    "gRPC endpoint",
                    "URL of the remote STT component server (e.g. http://localhost:50051).",
                ),
                e.clone(),
            );
        }
        if let Some(m) = &cfg.model {
            s.push(
                ParamDescriptor::static_str("model", "Model", "Path or name of the model to use."),
                serde_json::json!(m),
            );
        }
        if let Some(mn) = &cfg.model_name {
            s.push(
                ParamDescriptor::static_str(
                    "model_name",
                    "Model Name",
                    "Name of the API model to use.",
                ),
                serde_json::json!(mn),
            );
        }
        s.passthrough_options(
            &cfg.options,
            &["remote_provider", "remote_sample_rate"],
            "gRPC STT pass-through option.",
        );
        s
    }
}

// ── Conversions ─────────────────────────────────────────────

/// Convert a gRPC status to an [`SttError`].
fn grpc_to_stt_error(status: tonic::Status) -> SttError {
    match status.code() {
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
            SttError::NetworkError(status.message().to_string())
        }
        tonic::Code::Unauthenticated => SttError::AuthError(status.message().to_string()),
        tonic::Code::ResourceExhausted => SttError::RateLimited { retry_after: None },
        tonic::Code::InvalidArgument => SttError::InvalidAudio(status.message().to_string()),
        _ => SttError::InferenceFailed(format!(
            "gRPC error ({}): {}",
            status.code(),
            status.message()
        )),
    }
}

/// Convert a proto [`TranscriptionResult`](proto::TranscriptionResult) to the domain type.
fn proto_to_transcription(r: proto::TranscriptionResult) -> TranscriptionResult {
    use std::time::Duration;

    TranscriptionResult::new(r.text, r.is_final)
        .with_confidence(r.confidence)
        .with_language(r.language)
        .with_segment_index(r.segment_index)
        .with_speech_started(r.speech_started)
        .with_start_time(r.start_time_ms.map(Duration::from_millis))
        .with_end_time(r.end_time_ms.map(Duration::from_millis))
        .with_words(
            r.words
                .into_iter()
                .map(|w| {
                    WordInfo::new(
                        w.word,
                        Duration::from_millis(w.start_ms),
                        Duration::from_millis(w.end_ms),
                    )
                    .with_confidence(w.confidence)
                })
                .collect(),
        )
        .with_is_end_of_turn(r.is_end_of_turn)
        .with_speaker_id(r.speaker_id)
        .with_inference_metrics(r.inference_metrics.map(|m| {
            crate::stt::InferenceMetrics::new()
                .with_audio_duration_ms(m.audio_duration_ms)
                .with_inference_count(m.inference_count)
                .with_rtfx(m.rtfx)
                .with_algorithmic_delay_ms(m.algorithmic_delay_ms)
                .with_effective_latency_ms(m.effective_latency_ms)
                .with_streaming_window_ms(m.streaming_window_ms)
        }))
}

#[cfg(test)]
#[path = "tests/grpc.rs"]
mod tests;
