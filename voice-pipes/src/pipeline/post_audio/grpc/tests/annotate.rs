use super::*;
use crate::config::AudioPostProcessorConfig;
use crate::pipeline::post_audio::AudioPostProcessorFactory;
use crate::pipeline::post_audio::grpc::GrpcPostProcessorFactory;
use proto::audio_annotator_server::{AudioAnnotator as AudioAnnotatorSvc, AudioAnnotatorServer};
use proto::audio_post_processor_server::{
    AudioPostProcessor as AudioPostProcessorSvc, AudioPostProcessorServer,
};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use tokio::net::TcpListener;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{Request, Response, Status};

type ResponseStream =
    Pin<Box<dyn futures_util::Stream<Item = Result<proto::AnnotateResponse, Status>> + Send>>;

/// Streaming fake: one response per request echoing the request's
/// identity fields, plus a record of every request for assertions.
/// `mode`: "echo" | "badjson" (first response malformed) | "close"
/// (ends the response stream after the first request) | "process-only"
/// (`GetInfo` lists only "Process").
#[derive(Clone)]
struct StreamFake {
    mode: &'static str,
    requests: Arc<parking_lot::Mutex<Vec<proto::AnnotateRequest>>>,
    streams: Arc<AtomicU32>,
}

#[tonic::async_trait]
impl AudioAnnotatorSvc for StreamFake {
    type AnnotateStream = ResponseStream;

    async fn annotate(
        &self,
        request: Request<tonic::Streaming<proto::AnnotateRequest>>,
    ) -> Result<Response<Self::AnnotateStream>, Status> {
        self.streams.fetch_add(1, Ordering::Relaxed);
        let mut inbound = request.into_inner();
        let (tx, rx) = mpsc::channel(16);
        let requests = Arc::clone(&self.requests);
        let mode = self.mode;
        tokio::spawn(async move {
            let mut n = 0u32;
            while let Ok(Some(req)) = inbound.message().await {
                let frames = req.audio.len() as u64 / 2 / u64::from(req.channels.max(1));
                let duration_ms = if req.sample_rate == 0 {
                    0
                } else {
                    frames * 1000 / u64::from(req.sample_rate)
                };
                let extras_json = if mode == "badjson" && n == 0 {
                    "{not json".to_string()
                } else {
                    serde_json::json!({
                        "n": n,
                        "cancel": req.cancel,
                        "warmup": req.warmup,
                        "eou": req.end_of_utterance,
                        "n_options": req.options.len(),
                    })
                    .to_string()
                };
                let resp = proto::AnnotateResponse {
                    utterance_id: req.utterance_id,
                    start_ms: req.start_ms,
                    duration_ms,
                    kind: "fake".into(),
                    extras_json,
                };
                requests.lock().push(req);
                n += 1;
                if tx.send(Ok(resp)).await.is_err() {
                    break;
                }
                if mode == "close" {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn get_info(
        &self,
        _req: Request<proto::InfoRequest>,
    ) -> Result<Response<proto::InfoResponse>, Status> {
        let services = if self.mode == "process-only" {
            vec!["Process".to_string()]
        } else {
            vec!["Process".to_string(), "Annotate".to_string()]
        };
        Ok(Response::new(proto::InfoResponse {
            name: "stream-fake".into(),
            healthy: true,
            services,
        }))
    }
}

/// Unary fake (the inline-only contract): echoes contract fields.
#[derive(Clone)]
struct UnaryFake {
    calls: Arc<AtomicU32>,
}

#[tonic::async_trait]
impl AudioPostProcessorSvc for UnaryFake {
    async fn process(
        &self,
        request: Request<proto::ProcessRequest>,
    ) -> Result<Response<proto::ProcessResponse>, Status> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let r = request.into_inner();
        let extras_json = serde_json::json!({
            "seq": r.seq,
            "utterance_id": r.utterance_id,
            "eou": r.end_of_utterance,
            "n_bytes": r.audio.len(),
            "opt": r.options.get("kind").cloned().unwrap_or_default(),
        })
        .to_string();
        Ok(Response::new(proto::ProcessResponse { extras_json }))
    }

    async fn get_info(
        &self,
        _req: Request<proto::InfoRequest>,
    ) -> Result<Response<proto::InfoResponse>, Status> {
        Ok(Response::new(proto::InfoResponse {
            name: "unary-fake".into(),
            healthy: true,
            services: vec![],
        }))
    }
}

struct Server {
    endpoint: String,
    stream: Option<StreamFake>,
    unary: Option<UnaryFake>,
    _handle: tokio::task::JoinHandle<()>,
}

/// Spin up an in-process server with the selected services registered.
async fn spawn(stream_mode: Option<&'static str>, unary: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stream = stream_mode.map(|mode| StreamFake {
        mode,
        requests: Arc::new(parking_lot::Mutex::new(Vec::new())),
        streams: Arc::new(AtomicU32::new(0)),
    });
    let unary = unary.then(|| UnaryFake {
        calls: Arc::new(AtomicU32::new(0)),
    });
    let stream_svc = stream.clone().map(AudioAnnotatorServer::new);
    let unary_svc = unary.clone().map(AudioPostProcessorServer::new);
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_optional_service(stream_svc)
            .add_optional_service(unary_svc)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    Server {
        endpoint: format!("http://{addr}"),
        stream,
        unary,
        _handle: handle,
    }
}

fn cfg(endpoint: &str) -> AudioPostProcessorConfig {
    let mut options = HashMap::new();
    options.insert("endpoint".into(), serde_json::json!(endpoint));
    options.insert("name".into(), serde_json::json!("sync"));
    let mut opt = serde_json::Map::new();
    opt.insert("kind".into(), serde_json::json!("blendshapes"));
    options.insert("options".into(), serde_json::Value::Object(opt));
    AudioPostProcessorConfig {
        processor_type: "grpc".into(),
        mode: crate::config::PostProcessorMode::Annotate,
        options,
        ..AudioPostProcessorConfig::default()
    }
}

fn annotator(endpoint: &str) -> Arc<dyn AudioAnnotator> {
    GrpcPostProcessorFactory
        .build_annotator(&cfg(endpoint))
        .expect("build annotator")
}

fn seg(uid: u32, start_ms: u64, samples: usize, eou: bool) -> AnnotatorCommand {
    AnnotatorCommand::Segment(AudioSegment {
        utterance_id: uid,
        start_ms,
        samples: vec![100; samples],
        sample_rate: 24_000,
        channels: 1,
        end_of_utterance: eou,
    })
}

/// Run the annotator over `commands`, close the command channel and
/// return (annotations, run outcome).
async fn drive(
    annotator: Arc<dyn AudioAnnotator>,
    commands: Vec<AnnotatorCommand>,
) -> (Vec<AudioAnnotation>, Result<(), AnnotatorError>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (out_tx, mut out_rx) = mpsc::channel(64);
    let task = tokio::spawn(async move { annotator.run("sess-1".into(), cmd_rx, out_tx).await });
    for cmd in commands {
        cmd_tx.send(cmd).await.unwrap();
    }
    // Give the service time to answer everything before closing the
    // stream (closing ends `run` in streaming mode).
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(cmd_tx);
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("run finished")
        .expect("run did not panic");
    let mut annotations = Vec::new();
    while let Ok(a) = out_rx.try_recv() {
        annotations.push(a);
    }
    (annotations, outcome)
}

#[tokio::test]
async fn streaming_forwards_commands_and_maps_responses() {
    let server = spawn(Some("echo"), true).await;
    let (anns, outcome) = drive(
        annotator(&server.endpoint),
        vec![
            AnnotatorCommand::Warmup {
                sample_rate: 24_000,
                channels: 1,
            },
            seg(3, 0, 2400, false),
            seg(3, 100, 4800, true),
            AnnotatorCommand::Cancel { utterance_id: 3 },
        ],
    )
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");

    // One annotation per request, in order, mapped 1:1.
    assert_eq!(anns.len(), 4, "{anns:?}");
    assert_eq!(anns[0].utterance_id, WARMUP_UTTERANCE_ID);
    assert_eq!(anns[0].duration_ms, WARMUP_SILENCE_MS);
    assert_eq!(anns[0].extras["warmup"], true);
    assert_eq!(
        anns[0].extras["n_options"], 1,
        "options ride the first request"
    );
    assert_eq!(anns[1].utterance_id, 3);
    assert_eq!(anns[1].start_ms, 0);
    assert_eq!(anns[1].duration_ms, 100);
    assert_eq!(anns[1].kind, "fake", "service-chosen kind wins");
    assert_eq!(
        anns[1].extras["n_options"], 0,
        "options only on the first request"
    );
    assert_eq!(anns[2].start_ms, 100);
    assert_eq!(anns[2].duration_ms, 200);
    assert_eq!(anns[2].extras["eou"], true);
    assert_eq!(anns[3].extras["cancel"], true);
    assert_eq!(anns[3].utterance_id, 3);

    let fake = server.stream.as_ref().unwrap();
    let reqs = fake.requests.lock();
    assert_eq!(reqs.len(), 4);
    assert!(reqs.iter().all(|r| r.session_id == "sess-1"));
    assert!(reqs[0].warmup && !reqs[0].cancel);
    assert_eq!(
        reqs[0].options.get("kind").map(String::as_str),
        Some("blendshapes")
    );
    assert_eq!(reqs[0].audio.len(), 24 * 160 * 2, "160 ms of s16le silence");
    assert_eq!(reqs[1].audio.len(), 4800);
    assert_eq!(reqs[1].sample_rate, 24_000);
    assert_eq!(reqs[1].channels, 1);
    assert!(reqs[2].end_of_utterance);
    assert!(reqs[3].cancel && reqs[3].audio.is_empty());
    assert_eq!(fake.streams.load(Ordering::Relaxed), 1);
    // The unary service was never touched.
    assert_eq!(
        server.unary.as_ref().unwrap().calls.load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn unary_only_service_falls_back_to_process() {
    let server = spawn(None, true).await;
    let (anns, outcome) = drive(
        annotator(&server.endpoint),
        vec![
            AnnotatorCommand::Warmup {
                sample_rate: 24_000,
                channels: 1,
            },
            seg(5, 160, 3840, false),
            AnnotatorCommand::Cancel { utterance_id: 5 },
            seg(5, 320, 0, true),
        ],
    )
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
    let calls = server.unary.as_ref().unwrap().calls.load(Ordering::Relaxed);
    assert_eq!(
        calls, 3,
        "warm-up + segment + EOU sentinel; cancel is a no-op"
    );

    // Warm-up output is discarded; the segment and the EOU sentinel are
    // wrapped with the segment's own span and the configured kind.
    assert_eq!(anns.len(), 2, "{anns:?}");
    assert_eq!(anns[0].utterance_id, 5);
    assert_eq!(anns[0].start_ms, 160);
    assert_eq!(anns[0].duration_ms, 160);
    assert_eq!(anns[0].kind, "blendshapes");
    assert_eq!(
        anns[0].extras["seq"], 1,
        "seq is monotonic (warm-up took 0)"
    );
    assert_eq!(anns[0].extras["n_bytes"], 7680);
    assert_eq!(anns[0].extras["opt"], "blendshapes");
    assert_eq!(anns[1].start_ms, 320);
    assert_eq!(anns[1].duration_ms, 0);
    assert_eq!(anns[1].extras["eou"], true);
}

#[tokio::test]
async fn services_without_annotate_fall_back_to_process() {
    // Both services registered, but GetInfo only advertises "Process".
    let server = spawn(Some("process-only"), true).await;
    let (anns, outcome) = drive(annotator(&server.endpoint), vec![seg(1, 0, 2400, false)]).await;
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0].kind, "blendshapes");
    assert_eq!(
        server.unary.as_ref().unwrap().calls.load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        server
            .stream
            .as_ref()
            .unwrap()
            .streams
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn invalid_extras_json_is_skipped_and_stream_continues() {
    let server = spawn(Some("badjson"), false).await;
    let (anns, outcome) = drive(
        annotator(&server.endpoint),
        vec![seg(2, 0, 2400, false), seg(2, 100, 2400, false)],
    )
    .await;
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(anns.len(), 1, "first response dropped, second flows");
    assert_eq!(anns[0].start_ms, 100);
    assert_eq!(anns[0].extras["n"], 1);
}

#[tokio::test]
async fn server_closing_stream_is_an_error_for_the_hub() {
    let server = spawn(Some("close"), false).await;
    let (anns, outcome) = drive(
        annotator(&server.endpoint),
        vec![seg(2, 0, 2400, false), seg(2, 100, 2400, false)],
    )
    .await;
    assert_eq!(anns.len(), 1);
    let err = outcome.expect_err("server-side close surfaces as Err");
    assert!(err.to_string().contains("closed"), "{err}");
    assert_eq!(err.annotator, "sync");
}

#[tokio::test]
async fn unreachable_service_fails_without_caching() {
    // Nothing listens here; the probe must fail (not fall back) so the
    // hub retries after its cooldown.
    let a = annotator("http://127.0.0.1:1");
    let (anns, outcome) = drive(a, vec![seg(1, 0, 2400, false)]).await;
    assert!(anns.is_empty());
    assert!(outcome.is_err());
}

// `connect_lazy` needs a Tokio reactor, so the builder tests run on one.
#[tokio::test]
async fn build_annotator_requires_endpoint() {
    let missing = AudioPostProcessorConfig {
        processor_type: "grpc".into(),
        mode: crate::config::PostProcessorMode::Annotate,
        ..AudioPostProcessorConfig::default()
    };
    let err = GrpcPostProcessorFactory
        .build_annotator(&missing)
        .err()
        .expect("missing endpoint must fail");
    assert!(err.to_string().contains("endpoint"), "{err}");

    let built = annotator("http://127.0.0.1:50051");
    assert_eq!(built.name(), "sync");
}

#[tokio::test]
async fn kind_defaults_to_extras_without_option() {
    let channel = tonic::transport::Channel::from_shared("http://127.0.0.1:50051")
        .unwrap()
        .connect_lazy();
    let a = GrpcAnnotator::new(
        "x".into(),
        "http://127.0.0.1:50051".into(),
        crate::grpc::auth::authed_channel(channel),
        HashMap::new(),
        Duration::from_millis(250),
    );
    assert_eq!(a.kind, DEFAULT_KIND);
    assert!(a.transport.lock().is_none(), "probe happens on first run");
}

#[tokio::test]
async fn annotate_mode_entry_builds_grpc_annotator_not_inline_stage() {
    let cfg: crate::config::PipelineConfig = toml::from_str(
        r#"
[pipeline]
name = "t"

[[audio_post_processor]]
type = "grpc"
endpoint = "http://127.0.0.1:50051"
mode = "annotate"
batch_ms = 120

[audio_post_processor.options]
kind = "visemes"
"#,
    )
    .expect("parse config");
    let inline = crate::pipeline::post_audio::build_audio_post_processors(&cfg).unwrap();
    assert!(inline.is_empty(), "annotate entries are not inline stages");
    let specs = crate::pipeline::annotate::build_audio_annotators(&cfg).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].annotator.name(), "grpc");
    assert_eq!(specs[0].config.batch_ms, 120);
}
