//! gRPC TTS client provider -- connects to a remote TTS service.
//!
//! Implements [`TtsProvider`] by forwarding calls over gRPC, allowing the
//! TTS stage to run in a separate process or container (split-mode).

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::grpc::auth::{AuthedChannel, authed_channel};
use crate::grpc::client;

use crate::audio::AudioFrame;
use crate::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use crate::config::TtsConfig;
use crate::tts::{SynthesisRequest, TtsError, TtsFactory, TtsProvider};

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
    tonic::include_proto!("voice_pipes.tts");
}

// ── Provider ────────────────────────────────────────────────

/// gRPC-backed TTS provider that delegates to a remote [`TtsService`].
#[derive(Debug)]
pub struct GrpcTtsProvider {
    client: proto::tts_service_client::TtsServiceClient<AuthedChannel>,
    #[expect(dead_code, reason = "retained for diagnostics and reconnection")]
    endpoint: String,
    remote_sample_rate: u32,
    provider_name: String,
    core: Arc<ComponentCore>,
}

impl GrpcTtsProvider {
    /// Connect to the remote TTS service and query its metadata.
    pub async fn connect(endpoint: &str) -> Result<Self, anyhow::Error> {
        let channel = client::connect(endpoint, client::CONNECT_TIMEOUT, "TTS").await?;

        let mut client = proto::tts_service_client::TtsServiceClient::new(authed_channel(channel));

        let info = client::within(
            client::CONNECT_TIMEOUT,
            "TTS GetInfo",
            client.get_info(proto::Empty {}),
        )
        .await?
        .into_inner();

        tracing::info!(
            endpoint = endpoint,
            provider = %info.provider_name,
            sample_rate = info.sample_rate,
            "connected to remote TTS service"
        );

        let mut dynamic_schema = Vec::new();
        for p in info.param_schema {
            let value_type = match p.value_type.as_str() {
                "float" => crate::api::param_schema::ParamValueType::Float,
                "int" => crate::api::param_schema::ParamValueType::Int,
                "bool" => crate::api::param_schema::ParamValueType::Bool,
                "enum" => crate::api::param_schema::ParamValueType::Enum,
                "path" => crate::api::param_schema::ParamValueType::Path,
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
            format!("tts:grpc:{}", info.provider_name),
            ComponentKind::Tts,
        ));
        core.set_dynamic_schema(dynamic_schema);

        Ok(Self {
            client,
            endpoint: endpoint.to_string(),
            remote_sample_rate: info.sample_rate,
            provider_name: info.provider_name.clone(),
            core,
        })
    }
}

#[async_trait]
impl TtsProvider for GrpcTtsProvider {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let start = std::time::Instant::now();
        let request = proto::SynthesizeRequest {
            text: text.to_string(),
            hints: None,
        };

        let mut client = self.client.clone();
        let response = match client.synthesize(request).await.map_err(grpc_to_tts_error) {
            Ok(r) => r,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };
        self.core.record_ok(start.elapsed());
        Ok(proto_to_audio_frame(response.into_inner()))
    }

    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let text = req.text;
        let hints = req.hints;
        let start = std::time::Instant::now();
        let proto_hints = hints.map(|h| proto::SynthesisHints {
            voice: h.voice.clone(),
            language: h.language.clone(),
            speed: h.speed,
            options: h
                .options
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string()))
                .collect(),
        });

        let request = proto::SynthesizeRequest {
            text: text.to_string(),
            hints: proto_hints,
        };

        let mut client = self.client.clone();
        let response = match client
            .synthesize_stream(request)
            .await
            .map_err(grpc_to_tts_error)
        {
            Ok(r) => r,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };

        let mut stream = response.into_inner();
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    break;
                }
                msg_res = stream.message() => {
                    match msg_res.map_err(grpc_to_tts_error) {
                        Ok(Some(frame)) => {
                            if tx.send(proto_to_audio_frame(frame)).await.is_err() {
                                break;
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
        }
        self.core.record_ok(start.elapsed());
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.remote_sample_rate
    }

    fn name(&self) -> &str {
        &self.provider_name
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

// ── Factory ─────────────────────────────────────────────────

/// Factory for the gRPC TTS provider.
#[derive(Debug)]
pub struct GrpcTtsFactory;

impl TtsFactory for GrpcTtsFactory {
    fn name(&self) -> &'static str {
        "grpc"
    }

    fn is_local(&self) -> bool {
        false
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<crate::config::ConfigError> {
        let mut errors = vec![];
        if cfg.endpoint.is_none() {
            errors.push(crate::config::ConfigError::new(
                "endpoint",
                "gRPC TTS requires an endpoint URL",
            ));
        }
        // The client has no settings of its own.
        errors.extend(cfg.options().into_errors());
        errors
    }

    fn build(
        &self,
        cfg: &TtsConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let endpoint = cfg
            .endpoint
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("gRPC TTS requires `endpoint` in config"))?;
        let rt = tokio::runtime::Handle::current();
        let provider =
            tokio::task::block_in_place(|| rt.block_on(GrpcTtsProvider::connect(endpoint)))?;
        Ok(Box::new(provider))
    }

    fn provider_schema(&self, cfg: &TtsConfig) -> crate::api::param_schema::ProviderSchema {
        use crate::api::param_schema::{ParamDescriptor, ProviderSchema};
        let mut s = ProviderSchema::default();
        s.push(
            ParamDescriptor::static_str(
                "provider",
                "Provider",
                "The TTS framework or service used for speech synthesis.",
            ),
            serde_json::json!(cfg.provider),
        );
        if let Some(e) = &cfg.endpoint {
            s.push(
                ParamDescriptor::static_str(
                    "endpoint",
                    "gRPC endpoint",
                    "URL of the remote TTS component server (e.g. `http://localhost:50052`).",
                ),
                serde_json::json!(e),
            );
        }
        if let Some(v) = &cfg.voice {
            s.push(
                ParamDescriptor::static_str(
                    "voice",
                    "Voice",
                    "The default voice used if no hints are provided.",
                ),
                serde_json::json!(v),
            );
        }
        if let Some(m) = &cfg.model {
            s.push(
                ParamDescriptor::static_str("model", "Model", "Path or name of the model to use."),
                serde_json::json!(m),
            );
        }
        s.passthrough_options(
            &cfg.options,
            &["remote_provider", "remote_sample_rate"],
            "gRPC TTS option.",
        );
        s
    }
}

// ── Conversions ─────────────────────────────────────────────

/// Convert a gRPC status to a [`TtsError`].
fn grpc_to_tts_error(status: tonic::Status) -> TtsError {
    match status.code() {
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
            TtsError::NetworkError(status.message().to_string())
        }
        tonic::Code::Unauthenticated => TtsError::AuthError(status.message().to_string()),
        tonic::Code::ResourceExhausted => TtsError::RateLimited { retry_after: None },
        tonic::Code::InvalidArgument => TtsError::ConfigError(status.message().to_string()),
        _ => TtsError::InferenceFailed(format!(
            "gRPC error ({}): {}",
            status.code(),
            status.message()
        )),
    }
}

/// Convert a proto [`AudioFrame`](proto::AudioFrame) to the domain type.
fn proto_to_audio_frame(frame: proto::AudioFrame) -> AudioFrame {
    let samples: Vec<i16> = frame
        .samples_i16
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();

    AudioFrame {
        samples,
        sample_rate: frame.sample_rate,
        channels: 1,
    }
}
