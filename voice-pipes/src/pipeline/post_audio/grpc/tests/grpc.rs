use super::*;
use proto::audio_post_processor_server::{
    AudioPostProcessor as AudioPostProcessorSvc, AudioPostProcessorServer,
};
use std::sync::atomic::AtomicU32;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

/// A fake service that echoes selected request fields back as JSON so a
/// test can assert the processor attaches the response verbatim and sends
/// the expected request. `mode` picks the behaviour.
#[derive(Clone)]
struct FakeSvc {
    mode: &'static str,
    calls: Arc<AtomicU32>,
}

#[tonic::async_trait]
impl AudioPostProcessorSvc for FakeSvc {
    async fn process(
        &self,
        request: Request<proto::ProcessRequest>,
    ) -> Result<Response<proto::ProcessResponse>, Status> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let r = request.into_inner();
        let extras_json = match self.mode {
            // Arbitrary nested JSON -- proves vp is domain-agnostic.
            "arbitrary" => {
                r#"{"blendshapes":{"fps":30,"frames":[{"weights":[0.1,0.2]}]},"anything":[1,2,3]}"#
                    .to_string()
            }
            // Echo the request contract fields back for assertions.
            "echo" => serde_json::json!({
                "seq": r.seq,
                "utterance_id": r.utterance_id,
                "eou": r.end_of_utterance,
                "sample_rate": r.sample_rate,
                "n_bytes": r.audio.len(),
                "opt": r.options.get("kind").cloned().unwrap_or_default(),
            })
            .to_string(),
            // Malformed JSON -- processor must treat as non-fatal error.
            "badjson" => "{not json".to_string(),
            // Empty -> attach nothing.
            _ => String::new(),
        };
        Ok(Response::new(proto::ProcessResponse { extras_json }))
    }

    async fn get_info(
        &self,
        _req: Request<proto::InfoRequest>,
    ) -> Result<Response<proto::InfoResponse>, Status> {
        Ok(Response::new(proto::InfoResponse {
            name: "fake".into(),
            healthy: true,
            services: vec!["Process".into()],
        }))
    }
}

/// Spin up an in-process server and return (endpoint, call-counter, handle).
async fn spawn(mode: &'static str) -> (String, Arc<AtomicU32>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicU32::new(0));
    let svc = FakeSvc {
        mode,
        calls: Arc::clone(&calls),
    };
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AudioPostProcessorServer::new(svc))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (format!("http://{addr}"), calls, handle)
}

fn cfg(endpoint: &str) -> AudioPostProcessorConfig {
    let mut options = HashMap::new();
    options.insert("endpoint".into(), serde_json::json!(endpoint));
    let mut opt = serde_json::Map::new();
    opt.insert("kind".into(), serde_json::json!("blendshapes"));
    options.insert("options".into(), serde_json::Value::Object(opt));
    AudioPostProcessorConfig {
        processor_type: "grpc".into(),
        options,
        ..AudioPostProcessorConfig::default()
    }
}

fn chunk(samples: Vec<i16>, uid: u32, eou: bool) -> AudioChunk {
    AudioChunk {
        samples,
        sample_rate: 24000,
        channels: 1,
        is_end_of_utterance: eou,
        utterance_id: uid,
        ..AudioChunk::default()
    }
}

#[tokio::test]
async fn attaches_arbitrary_json_verbatim() {
    let (endpoint, _calls, _h) = spawn("arbitrary").await;
    let proc = GrpcPostProcessorFactory.build(&cfg(&endpoint)).unwrap();
    let (tx, _rx) = mpsc::channel::<ServerMessage>(4);
    let mut c = chunk(vec![1, 2, 3, 4], 7, false);

    proc.process(&mut c, &tx).await.unwrap();

    let extras = c.extras.expect("extras attached");
    // vp attaches the service's JSON tree without interpreting it.
    assert_eq!(extras["blendshapes"]["fps"], 30);
    assert_eq!(extras["anything"], serde_json::json!([1, 2, 3]));
}

#[tokio::test]
async fn request_carries_contract_fields() {
    let (endpoint, _calls, _h) = spawn("echo").await;
    let proc = GrpcPostProcessorFactory.build(&cfg(&endpoint)).unwrap();
    let (tx, _rx) = mpsc::channel::<ServerMessage>(4);

    let mut audio = chunk(vec![0i16; 240], 3, false);
    proc.process(&mut audio, &tx).await.unwrap();
    let e = audio.extras.expect("extras");
    assert_eq!(e["utterance_id"], 3);
    assert_eq!(e["eou"], false);
    assert_eq!(e["sample_rate"], 24000);
    assert_eq!(e["n_bytes"], 480); // 240 i16 samples * 2 bytes
    assert_eq!(e["opt"], "blendshapes"); // options forwarded verbatim
    assert_eq!(e["seq"], 0);

    // EOU sentinel: no audio, but still calls so the service can reset.
    let mut eou = chunk(Vec::new(), 3, true);
    proc.process(&mut eou, &tx).await.unwrap();
    let e2 = eou.extras.expect("extras on eou");
    assert_eq!(e2["eou"], true);
    assert_eq!(e2["n_bytes"], 0);
    assert_eq!(e2["seq"], 1); // monotonic across calls
}

#[tokio::test]
async fn empty_non_eou_chunk_skips_rpc() {
    let (endpoint, calls, _h) = spawn("echo").await;
    let proc = GrpcPostProcessorFactory.build(&cfg(&endpoint)).unwrap();
    let (tx, _rx) = mpsc::channel::<ServerMessage>(4);

    let mut c = chunk(Vec::new(), 1, false);
    proc.process(&mut c, &tx).await.unwrap();
    assert!(c.extras.is_none());
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "no RPC for empty non-EOU chunk"
    );
}

#[tokio::test]
async fn invalid_json_is_non_fatal() {
    let (endpoint, _calls, _h) = spawn("badjson").await;
    let proc = GrpcPostProcessorFactory.build(&cfg(&endpoint)).unwrap();
    let (tx, _rx) = mpsc::channel::<ServerMessage>(4);
    let mut c = chunk(vec![1, 2], 1, false);

    // Errors are returned so `run_chain` logs-and-continues; the chunk
    // is left unmodified and audio is never dropped.
    assert!(proc.process(&mut c, &tx).await.is_err());
    assert!(c.extras.is_none());
}

#[test]
fn validate_requires_endpoint() {
    let missing = AudioPostProcessorConfig {
        processor_type: "grpc".into(),
        ..AudioPostProcessorConfig::default()
    };
    assert!(!GrpcPostProcessorFactory.validate(&missing).is_empty());
    assert!(
        GrpcPostProcessorFactory
            .validate(&cfg("http://127.0.0.1:50051"))
            .is_empty()
    );
}
