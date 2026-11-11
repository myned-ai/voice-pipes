//! gRPC server wrapper for text processing -- wraps a local [`TextProcessor`]
//! behind a tonic [`TextProcessorService`] implementation.

use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};

use crate::text::{TextFragment, TextProcessor};

/// Auto-generated gRPC server stubs for the text processor service.
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

mod params {
    pub(crate) use crate::grpc::params::proto::*;
}

pub use proto::text_processor_service_server::TextProcessorServiceServer;

/// Factory callback that builds a fresh per-session [`TextProcessor`] (with
/// isolated conversation history, sharing loaded model weights).
pub type SessionProcessorBuilder =
    Arc<dyn Fn() -> Result<Arc<dyn TextProcessor>, String> + Send + Sync>;

const MAX_SESSIONS: usize = 1024;

#[derive(Default)]
struct SessionTable {
    order: std::collections::VecDeque<String>,
    entries: std::collections::HashMap<String, Arc<dyn TextProcessor>>,
}

impl SessionTable {
    fn get(&self, id: &str) -> Option<Arc<dyn TextProcessor>> {
        self.entries.get(id).cloned()
    }

    fn insert(&mut self, id: String, proc: Arc<dyn TextProcessor>) {
        if !self.entries.contains_key(&id) {
            while self.entries.len() >= MAX_SESSIONS {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                } else {
                    break;
                }
            }
            self.order.push_back(id.clone());
        }
        self.entries.insert(id, proc);
    }
}

/// Wraps a local [`TextProcessor`] as a gRPC service.
pub struct TextProcessorServiceImpl {
    processor: Arc<dyn TextProcessor>,
    session_builder: Option<SessionProcessorBuilder>,
    sessions: parking_lot::Mutex<SessionTable>,
    param_schema: Vec<crate::api::param_schema::ParamDescriptor>,
    current_values: serde_json::Map<String, serde_json::Value>,
}

impl std::fmt::Debug for TextProcessorServiceImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextProcessorServiceImpl")
            .field("processor", &self.processor.name())
            .finish_non_exhaustive()
    }
}

impl TextProcessorServiceImpl {
    pub fn new(
        processor: Arc<dyn TextProcessor>,
        param_schema: Vec<crate::api::param_schema::ParamDescriptor>,
        current_values: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            processor,
            session_builder: None,
            sessions: parking_lot::Mutex::new(SessionTable::default()),
            param_schema,
            current_values,
        }
    }

    /// Attach a per-session builder so callers supplying the `x-vp-session-id`
    /// metadata header get an isolated [`TextProcessor`] conversation history.
    #[must_use]
    pub fn with_session_builder(mut self, builder: SessionProcessorBuilder) -> Self {
        self.session_builder = Some(builder);
        self
    }

    #[expect(
        clippy::result_large_err,
        reason = "tonic::Status is the framework error type on this boundary"
    )]
    fn resolve_processor(
        &self,
        metadata: &tonic::metadata::MetadataMap,
    ) -> Result<Arc<dyn TextProcessor>, Status> {
        let Some(ref builder) = self.session_builder else {
            return Ok(Arc::clone(&self.processor));
        };
        let Some(raw_id) = metadata
            .get(crate::grpc::client::SESSION_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(Arc::clone(&self.processor));
        };

        if let Some(existing) = self.sessions.lock().get(raw_id) {
            return Ok(existing);
        }

        let built = builder().map_err(Status::internal)?;
        let mut guard = self.sessions.lock();
        if let Some(existing) = guard.get(raw_id) {
            return Ok(existing);
        }
        guard.insert(raw_id.to_string(), Arc::clone(&built));
        Ok(built)
    }
}

use crate::grpc::params::{map_current_values_to_metadata, param_descriptor_to_proto};

type GrpcStream<T> = Pin<Box<dyn futures_util::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl proto::text_processor_service_server::TextProcessorService for TextProcessorServiceImpl {
    async fn process(
        &self,
        request: Request<proto::ProcessRequest>,
    ) -> Result<Response<proto::ProcessResponse>, Status> {
        let processor = self.resolve_processor(request.metadata())?;
        let req = request.into_inner();
        let result = processor
            .process(&req.text)
            .await
            .map_err(|e| super::status::text_error_to_status(&e))?;

        Ok(Response::new(proto::ProcessResponse { text: result }))
    }

    type ProcessStreamStream = GrpcStream<proto::TextFragment>;

    async fn process_stream(
        &self,
        request: Request<proto::ProcessRequest>,
    ) -> Result<Response<Self::ProcessStreamStream>, Status> {
        let processor = self.resolve_processor(request.metadata())?;
        let req = request.into_inner();
        let (tx, rx) = mpsc::channel::<TextFragment>(64);
        let cancel = CancellationToken::new();
        // Cancel the processor when the client drops the stream
        // (guard moved into the trailer below, same pattern as STT/TTS).
        let cancel_guard = cancel.clone().drop_guard();

        let text = req.text.clone();
        let handle = tokio::spawn(async move { processor.process_stream(&text, tx, cancel).await });

        let frags = ReceiverStream::new(rx)
            .map(|frag| proto::TextFragment {
                text: frag.text,
                is_end_of_response: frag.is_end_of_response,
                first_token_ms: frag.first_token_ms,
                token_count: frag.token_count,
                spoken_text: frag.spoken_text,
            })
            .map(Ok);
        // Surface a streaming failure to the client instead of a clean EOF.
        let trailer = futures_util::stream::once(async move {
            let _cancel_guard = cancel_guard;
            match handle.await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(Err(super::status::text_error_to_status(&e))),
                Err(_) => Some(Err(Status::internal(
                    "text-processing task terminated unexpectedly",
                ))),
            }
        })
        .filter_map(|x| x);
        let output = frags.chain(trailer);

        Ok(Response::new(Box::pin(output)))
    }

    async fn get_info(
        &self,
        _request: Request<proto::Empty>,
    ) -> Result<Response<proto::TextProcessorInfo>, Status> {
        let metadata = map_current_values_to_metadata(&self.current_values);
        let param_schema = self
            .param_schema
            .iter()
            .map(param_descriptor_to_proto)
            .collect();

        Ok(Response::new(proto::TextProcessorInfo {
            name: self.processor.name().to_string(),
            metadata,
            param_schema,
        }))
    }
}

use tokio_stream::StreamExt;
