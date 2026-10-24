use std::pin::Pin;
use std::time::{Duration, Instant};

use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

use super::*;
use proto::text_processor_service_server::{TextProcessorService, TextProcessorServiceServer};

/// A service that says who it is and then streams nothing.
struct SilentService;

type Fragments =
    Pin<Box<dyn futures_util::Stream<Item = Result<proto::TextFragment, Status>> + Send>>;

#[tonic::async_trait]
impl TextProcessorService for SilentService {
    async fn process(
        &self,
        _request: Request<proto::ProcessRequest>,
    ) -> Result<Response<proto::ProcessResponse>, Status> {
        Err(Status::unimplemented("streams only"))
    }

    type ProcessStreamStream = Fragments;

    async fn process_stream(
        &self,
        _request: Request<proto::ProcessRequest>,
    ) -> Result<Response<Self::ProcessStreamStream>, Status> {
        Ok(Response::new(Box::pin(futures_util::stream::pending())))
    }

    async fn get_info(
        &self,
        _request: Request<proto::Empty>,
    ) -> Result<Response<proto::TextProcessorInfo>, Status> {
        Ok(Response::new(proto::TextProcessorInfo {
            name: "silent".into(),
            ..Default::default()
        }))
    }
}

async fn serve_silent() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(TextProcessorServiceServer::new(SilentService))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    format!("http://{addr}")
}

/// A service that answers and then goes quiet fails the turn after the idle
/// bound, as a transient error, instead of holding it open.
#[tokio::test]
async fn a_stream_that_goes_quiet_fails_after_the_idle_bound() {
    let endpoint = serve_silent().await;
    let mut processor = GrpcTextProcessor::connect(&endpoint)
        .await
        .expect("connect");
    processor.stream_idle = Duration::from_millis(300);
    let (tx, _rx) = mpsc::channel(4);
    let started = Instant::now();
    let err = processor
        .process_stream("hello", tx, CancellationToken::new())
        .await
        .expect_err("nothing came");
    assert!(err.is_transient(), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// Cancel ends the wait at once, whatever the idle bound.
#[tokio::test]
async fn cancel_ends_a_quiet_stream() {
    let endpoint = serve_silent().await;
    let processor = GrpcTextProcessor::connect(&endpoint)
        .await
        .expect("connect");
    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    processor
        .process_stream("hello", tx, cancel)
        .await
        .expect("cancelled is not an error");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// An endpoint that takes the connection and never speaks gRPC fails the
/// connect at its bound.
#[tokio::test]
async fn a_mute_endpoint_fails_the_connect_at_its_bound() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let started = Instant::now();
    let result = crate::grpc::client::within(Duration::from_millis(500), "GetInfo", async {
        let channel = crate::grpc::client::connect(
            &format!("http://{addr}"),
            Duration::from_millis(500),
            "test",
        )
        .await
        .map_err(|e| Status::unavailable(e.to_string()))?;
        let mut client = proto::text_processor_service_client::TextProcessorServiceClient::new(
            authed_channel(channel),
        );
        client.get_info(proto::Empty {}).await
    })
    .await;
    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    held.abort();
}
