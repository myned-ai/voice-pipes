//! gRPC text processor client -- connects to a remote text processing service.
//!
//! Implements [`TextProcessor`] by forwarding calls over gRPC, allowing the
//! text/LLM stage to run in a separate process or container (split-mode).

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::grpc::auth::{AuthedChannel, authed_channel};
use crate::grpc::client;

use crate::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use crate::text::{TextFragment, TextProcessor, TextProcessorError};

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
    tonic::include_proto!("voice_pipes.text");
}

// ── Provider ────────────────────────────────────────────────

static NEXT_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_session_id() -> String {
    let n = NEXT_SESSION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("vp-grpc-sess-{}-{n}", std::process::id())
}

#[derive(Clone)]
struct SharedGrpcTextEndpoint {
    client: proto::text_processor_service_client::TextProcessorServiceClient<AuthedChannel>,
    processor_name: String,
    core: Arc<ComponentCore>,
}

fn endpoint_cache()
-> &'static parking_lot::Mutex<std::collections::HashMap<String, SharedGrpcTextEndpoint>> {
    static CACHE: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<String, SharedGrpcTextEndpoint>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// gRPC-backed text processor that delegates to a remote [`TextProcessorService`].
#[derive(Debug)]
pub struct GrpcTextProcessor {
    client: proto::text_processor_service_client::TextProcessorServiceClient<AuthedChannel>,
    #[expect(dead_code, reason = "retained for diagnostics and reconnection")]
    endpoint: String,
    processor_name: String,
    session_id: String,
    core: Arc<ComponentCore>,
    /// Longest wait for the next streamed fragment.
    stream_idle: std::time::Duration,
}

impl GrpcTextProcessor {
    /// Connect to the remote text processor service and query its name.
    pub async fn connect(endpoint: &str) -> Result<Self, anyhow::Error> {
        let shared = Self::open_endpoint(endpoint).await?;
        Ok(Self::from_shared(endpoint, shared))
    }

    /// Connect or reuse a cached HTTP/2 channel for `endpoint`, assigning a
    /// fresh per-session ID so the remote component server isolates history.
    pub fn connect_cached_blocking(endpoint: &str) -> Result<Self, anyhow::Error> {
        if let Some(shared) = endpoint_cache().lock().get(endpoint).cloned() {
            return Ok(Self::from_shared(endpoint, shared));
        }
        let rt = tokio::runtime::Handle::current();
        let shared = tokio::task::block_in_place(|| rt.block_on(Self::open_endpoint(endpoint)))?;
        endpoint_cache()
            .lock()
            .insert(endpoint.to_string(), shared.clone());
        Ok(Self::from_shared(endpoint, shared))
    }

    fn from_shared(endpoint: &str, shared: SharedGrpcTextEndpoint) -> Self {
        Self {
            client: shared.client,
            endpoint: endpoint.to_string(),
            processor_name: shared.processor_name,
            session_id: next_session_id(),
            core: shared.core,
            stream_idle: client::STREAM_IDLE_TIMEOUT,
        }
    }

    async fn open_endpoint(endpoint: &str) -> Result<SharedGrpcTextEndpoint, anyhow::Error> {
        let channel = client::connect(endpoint, client::CONNECT_TIMEOUT, "TextProcessor").await?;

        let mut client = proto::text_processor_service_client::TextProcessorServiceClient::new(
            authed_channel(channel),
        );

        let info = client::within(
            client::CONNECT_TIMEOUT,
            "TextProcessor GetInfo",
            client.get_info(tonic::Request::new(proto::Empty {})),
        )
        .await?
        .into_inner();

        tracing::info!(
            endpoint = endpoint,
            processor = %info.name,
            "connected to remote text processor service"
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
            format!("text:grpc:{}", info.name),
            ComponentKind::Text,
        ));
        core.set_dynamic_schema(dynamic_schema);

        Ok(SharedGrpcTextEndpoint {
            client,
            processor_name: info.name,
            core,
        })
    }

    fn attach_session<T>(&self, payload: T) -> tonic::Request<T> {
        let mut req = tonic::Request::new(payload);
        if let Ok(val) = tonic::metadata::MetadataValue::try_from(self.session_id.as_str()) {
            req.metadata_mut().insert(client::SESSION_ID_HEADER, val);
        }
        req
    }
}

#[async_trait]
impl TextProcessor for GrpcTextProcessor {
    fn name(&self) -> &str {
        &self.processor_name
    }

    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        let start = std::time::Instant::now();
        let request = self.attach_session(proto::ProcessRequest {
            text: input.to_string(),
        });

        let mut client = self.client.clone();
        let response = match client
            .process(request)
            .await
            .map_err(|e| grpc_to_text_error(&self.processor_name, e))
        {
            Ok(r) => r,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };
        self.core.record_ok(start.elapsed());
        Ok(response.into_inner().text)
    }

    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let start = std::time::Instant::now();
        let request = self.attach_session(proto::ProcessRequest {
            text: input.to_string(),
        });

        let mut client = self.client.clone();
        let response = match client
            .process_stream(request)
            .await
            .map_err(|e| grpc_to_text_error(&self.processor_name, e))
        {
            Ok(r) => r,
            Err(e) => {
                self.core.record_error(&e);
                return Err(e);
            }
        };

        let mut stream = response.into_inner();
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    self.core.record_ok(start.elapsed());
                    return Ok(());
                }
                () = tokio::time::sleep(self.stream_idle) => {
                    let err = TextProcessorError::new(
                        &self.processor_name,
                        format!("the service sent nothing for {:?}", self.stream_idle),
                    )
                    .with_kind(crate::text::TextErrorKind::Transient);
                    self.core.record_error(&err);
                    return Err(err);
                }
                next = stream.message() => next,
            };
            match next {
                Ok(Some(fragment)) => {
                    let mut tf = TextFragment::new(fragment.text)
                        .with_is_end_of_response(fragment.is_end_of_response);
                    tf.first_token_ms = fragment.first_token_ms;
                    tf.token_count = fragment.token_count;
                    tf.spoken_text = fragment.spoken_text;
                    if tx.send(tf).await.is_err() {
                        break; // receiver dropped
                    }
                }
                Ok(None) => break, // stream ended
                Err(e) => {
                    let err = grpc_to_text_error(&self.processor_name, e);
                    self.core.record_error(&err);
                    return Err(err);
                }
            }
        }

        self.core.record_ok(start.elapsed());
        Ok(())
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(std::sync::Arc::clone(
            &self.core,
        ))))
    }
}

// ── Factory ─────────────────────────────────────────────────

/// Factory for the gRPC text processor.
#[derive(Debug)]
pub struct GrpcTextProcessorFactory;

impl crate::text::TextProcessorFactory for GrpcTextProcessorFactory {
    fn name(&self) -> &'static str {
        "grpc"
    }

    fn is_local(&self) -> bool {
        false
    }

    fn validate(
        &self,
        cfg: &crate::config::TextProcessorConfig,
    ) -> Vec<crate::config::ConfigError> {
        let mut o = cfg.options();
        if o.string("endpoint").is_none() {
            o.error("endpoint", "gRPC text processor requires `endpoint`");
        }
        o.into_errors()
    }

    fn build(
        &self,
        cfg: &crate::config::TextProcessorConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TextProcessor>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let endpoint = cfg
            .options
            .get("endpoint")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("gRPC text processor requires `endpoint` in options"))?;

        let processor = GrpcTextProcessor::connect_cached_blocking(endpoint)?;
        Ok(Box::new(processor))
    }

    fn provider_schema(
        &self,
        cfg: &crate::config::TextProcessorConfig,
    ) -> crate::api::param_schema::ProviderSchema {
        use crate::api::param_schema::{ParamDescriptor, ProviderSchema};
        let mut s = ProviderSchema::default();
        if let Some(v) = cfg.options.get("endpoint") {
            s.push(
                ParamDescriptor::static_str(
                    "endpoint",
                    "Endpoint",
                    "URL of the remote text processor component server.",
                ),
                v.clone(),
            );
        }
        s.passthrough_options(&cfg.options, &["endpoint"], "gRPC text processor option.");
        s
    }
}

// ── Conversions ─────────────────────────────────────────────

/// Convert a gRPC status to a [`TextProcessorError`]: a server that is
/// down, overloaded or slow may answer another attempt; anything else is
/// the request, and fails again.
fn grpc_to_text_error(processor_name: &str, status: tonic::Status) -> TextProcessorError {
    let kind = match status.code() {
        tonic::Code::Unavailable
        | tonic::Code::DeadlineExceeded
        | tonic::Code::Aborted
        | tonic::Code::Internal => crate::text::TextErrorKind::Transient,
        tonic::Code::ResourceExhausted => {
            crate::text::TextErrorKind::RateLimited { retry_after: None }
        }
        _ => crate::text::TextErrorKind::Permanent,
    };
    TextProcessorError::new(
        processor_name.to_string(),
        format!("gRPC error ({}): {}", status.code(), status.message()),
    )
    .caused_by(status)
    .with_kind(kind)
}

#[cfg(test)]
#[path = "tests/grpc.rs"]
mod tests;
