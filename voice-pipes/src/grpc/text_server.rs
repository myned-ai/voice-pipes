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

/// Wraps a local [`TextProcessor`] as a gRPC service.
pub struct TextProcessorServiceImpl {
    processor: Arc<dyn TextProcessor>,
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
            param_schema,
            current_values,
        }
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
        let req = request.into_inner();
        let result = self
            .processor
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
        let req = request.into_inner();
        let (tx, rx) = mpsc::channel::<TextFragment>(64);
        let cancel = CancellationToken::new();
        // Cancel the processor when the client drops the stream
        // (guard moved into the trailer below, same pattern as STT/TTS).
        let cancel_guard = cancel.clone().drop_guard();

        let processor = Arc::clone(&self.processor);
        let text = req.text.clone();
        let handle = tokio::spawn(async move { processor.process_stream(&text, tx, cancel).await });

        let frags = ReceiverStream::new(rx)
            .map(|frag| proto::TextFragment {
                text: frag.text,
                is_end_of_response: frag.is_end_of_response,
                first_token_ms: frag.first_token_ms,
                token_count: frag.token_count,
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
