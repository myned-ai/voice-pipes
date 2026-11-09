use super::*;
use crate::agent::{AgentEvent, AgentSessionHandle};
use crate::text::TextFragment;
use proto::agent_service_server::{AgentService, AgentServiceServer};
use std::pin::Pin;
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;

/// Scripted tonic agent: acks the handshake, echoes turns
/// ("drop" closes the stream mid-turn to exercise link death),
/// and follows each normal turn with an out-of-turn Say.
struct ScriptedAgent;

type OutStream = Pin<Box<dyn Stream<Item = Result<proto::AgentToPipeline, tonic::Status>> + Send>>;

#[tonic::async_trait]
impl AgentService for ScriptedAgent {
    type AgentSessionStream = OutStream;

    async fn agent_session(
        &self,
        request: tonic::Request<tonic::Streaming<proto::PipelineToAgent>>,
    ) -> Result<tonic::Response<Self::AgentSessionStream>, tonic::Status> {
        let mut inbound = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<proto::AgentToPipeline, tonic::Status>>(16);
        tokio::spawn(async move {
            use proto::agent_to_pipeline::Event as Out;
            use proto::pipeline_to_agent::Event as In;
            while let Ok(Some(frame)) = inbound.message().await {
                match frame.event {
                    Some(In::SessionStart(s)) => {
                        let _ = tx
                            .send(Ok(proto::AgentToPipeline {
                                event: Some(Out::SessionAck(proto::SessionAck {
                                    protocol_version: s.protocol_version,
                                    agent_name: "scripted-tonic".into(),
                                })),
                            }))
                            .await;
                    }
                    Some(In::UserTurn(t)) => {
                        if t.text == "drop" {
                            // Close the outbound stream mid-turn.
                            return;
                        }
                        if t.text == "huge" {
                            // A frame past the engine's decode limit.
                            let _ = tx
                                .send(Ok(proto::AgentToPipeline {
                                    event: Some(Out::TurnFragment(proto::TurnFragment {
                                        turn_id: t.turn_id,
                                        text: "z".repeat(2 * 1024 * 1024),
                                        is_end_of_response: true,
                                        first_token_ms: None,
                                        token_count: None,
                                    })),
                                }))
                                .await;
                            continue;
                        }
                        let _ = tx
                            .send(Ok(proto::AgentToPipeline {
                                event: Some(Out::TurnFragment(proto::TurnFragment {
                                    turn_id: t.turn_id,
                                    text: format!("echo:{}", t.text),
                                    is_end_of_response: true,
                                    first_token_ms: None,
                                    token_count: None,
                                })),
                            }))
                            .await;
                        let _ = tx
                            .send(Ok(proto::AgentToPipeline {
                                event: Some(Out::Say(proto::Say {
                                    text: "proactive follow-up".into(),
                                    scheduling: proto::Scheduling::WhenIdle as i32,
                                })),
                            }))
                            .await;
                    }
                    Some(In::SessionEnd(_)) | None => return,
                    _ => {}
                }
            }
        });
        Ok(tonic::Response::new(
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)) as OutStream,
        ))
    }

    async fn list_tools(
        &self,
        _request: tonic::Request<proto::ListToolsRequest>,
    ) -> Result<tonic::Response<proto::ListToolsResponse>, tonic::Status> {
        Ok(tonic::Response::new(proto::ListToolsResponse {
            tools: vec![proto::ToolSpec {
                name: "check_order".into(),
                description: "look up an order".into(),
                parameters_json_schema: "{\"type\":\"object\"}".into(),
            }],
        }))
    }
}

async fn spawn_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(AgentServiceServer::new(ScriptedAgent))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    format!("http://{addr}")
}

/// Full tonic round trip: handshake, turn echo, out-of-turn Say,
/// and the unary `ListTools`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tonic_round_trip_via_gateway() {
    let endpoint = spawn_server().await;
    let link = GrpcAgentLink::connect(&endpoint).await.expect("connect");

    let tools = link.list_tools().await.expect("list tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "check_order");

    let gateway = AgentGateway::new(Arc::new(link), "tonic-test");
    let (agent_tx, mut agent_rx) = mpsc::channel(8);
    gateway.bind_session(
        AgentSessionHandle::new(agent_tx)
            .with_session_id("tonic-1")
            .session_handle(),
    );

    let (tx, mut rx) = mpsc::channel::<TextFragment>(8);
    gateway
        .process_stream("hello", tx, CancellationToken::new())
        .await
        .expect("turn should complete");
    let frag = rx.recv().await.expect("fragment");
    assert_eq!(frag.text, "echo:hello");
    assert!(frag.is_end_of_response);

    match tokio::time::timeout(std::time::Duration::from_secs(5), agent_rx.recv())
        .await
        .expect("say timed out")
        .expect("agent channel closed")
    {
        AgentEvent::Say { text, .. } => assert_eq!(text, "proactive follow-up"),
        other => panic!("expected Say, got {other:?}"),
    }
}

/// The agent closing its stream mid-turn must fail the turn loudly
/// (not hang until the watchdog).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tonic_stream_drop_fails_turn_loudly() {
    let endpoint = spawn_server().await;
    let link = GrpcAgentLink::connect(&endpoint).await.expect("connect");
    let gateway = AgentGateway::new(Arc::new(link), "tonic-test")
        .with_turn_timeout(std::time::Duration::from_secs(20));
    let (agent_tx, _agent_rx) = mpsc::channel(8);
    gateway.bind_session(
        AgentSessionHandle::new(agent_tx)
            .with_session_id("tonic-2")
            .session_handle(),
    );

    let (tx, _rx) = mpsc::channel::<TextFragment>(8);
    let started = std::time::Instant::now();
    let err = gateway
        .process_stream("drop", tx, CancellationToken::new())
        .await
        .expect_err("stream drop must fail the turn");
    assert!(
        err.message.contains("closed mid-turn"),
        "got: {}",
        err.message
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "turn failure must come from link death, not the watchdog"
    );
}

/// A frame larger than the decode limit is refused by the transport:
/// the link reports an error and closes instead of buffering it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_frame_closes_the_link_instead_of_being_decoded() {
    let endpoint = spawn_server().await;
    let link = GrpcAgentLink::connect(&endpoint).await.expect("connect");
    let gateway = AgentGateway::new(Arc::new(link), "tonic-test")
        .with_turn_timeout(std::time::Duration::from_secs(20));
    let (agent_tx, _agent_rx) = mpsc::channel(8);
    gateway.bind_session(
        AgentSessionHandle::new(agent_tx)
            .with_session_id("tonic-3")
            .session_handle(),
    );

    let (tx, mut rx) = mpsc::channel::<TextFragment>(8);
    let started = std::time::Instant::now();
    let err = gateway
        .process_stream("huge", tx, CancellationToken::new())
        .await
        .expect_err("an oversized frame must not complete the turn");
    assert!(
        err.message.contains("closed mid-turn"),
        "got: {}",
        err.message
    );
    assert!(rx.try_recv().is_err(), "no fragment may be relayed");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "failure must come from the decode limit, not the watchdog"
    );
}

/// An agent that accepts the TCP connection but never answers the
/// HTTP/2 handshake (e.g. a Python grpc server that defers initial
/// metadata until its first yield) must fail connect with a timeout
/// error, not hang session setup forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_times_out_against_silent_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Accept connections and hold them open without ever responding.
    let held = Arc::new(Mutex::new(Vec::new()));
    let held_task = Arc::clone(&held);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            held_task.lock().await.push(sock);
        }
    });

    let started = std::time::Instant::now();
    let err = GrpcAgentLink::connect_with_timeout(
        &format!("http://{addr}"),
        std::time::Duration::from_millis(300),
    )
    .await
    .expect_err("silent server must not connect");
    assert!(
        err.to_string().contains("timed out"),
        "expected a timeout error, got: {err}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "connect must fail fast, took {:?}",
        started.elapsed()
    );
}
