use super::*;
use crate::agent::Scheduling;
use crate::agent::link::{AgentLinkError, LinkToolSpec};
use tokio::sync::mpsc;

struct InMemoryLink {
    to_agent: mpsc::Sender<PipelineToAgent>,
    from_agent: tokio::sync::Mutex<mpsc::Receiver<AgentToPipeline>>,
    tools: Vec<LinkToolSpec>,
}

#[async_trait]
impl AgentLink for InMemoryLink {
    async fn send(&self, event: PipelineToAgent) -> Result<(), AgentLinkError> {
        self.to_agent
            .send(event)
            .await
            .map_err(|_closed| AgentLinkError::new("test link closed"))
    }
    async fn recv(&self) -> Option<AgentToPipeline> {
        self.from_agent.lock().await.recv().await
    }
    async fn list_tools(&self) -> Result<Vec<LinkToolSpec>, AgentLinkError> {
        Ok(self.tools.clone())
    }
}

fn test_backend() -> (
    AgentToolBackend,
    mpsc::Receiver<PipelineToAgent>,
    mpsc::Sender<AgentToPipeline>,
    mpsc::Receiver<AgentEvent>,
) {
    let (to_agent_tx, to_agent_rx) = mpsc::channel(32);
    let (from_agent_tx, from_agent_rx) = mpsc::channel(32);
    let link = Arc::new(InMemoryLink {
        to_agent: to_agent_tx,
        from_agent: tokio::sync::Mutex::new(from_agent_rx),
        tools: vec![LinkToolSpec {
            name: "check_order".into(),
            description: "look up an order".into(),
            parameters_json_schema: "{\"type\":\"object\"}".into(),
        }],
    });
    let (agent_tx, agent_rx) = mpsc::channel(16);
    let handle = AgentSessionHandle::new(agent_tx).with_session_id("s2s-1");
    let backend =
        AgentToolBackend::start(link, handle).with_ack_timeout(Duration::from_millis(200));
    (backend, to_agent_rx, from_agent_tx, agent_rx)
}

/// `ListTools` → registry; invoke → `ToolCall` on the wire → Done.
#[tokio::test]
async fn remote_tool_done_round_trip() {
    let (backend, mut to_agent, from_agent, _agent_rx) = test_backend();
    let registry = backend.registry().await.expect("registry");
    let tool = registry.lookup("check_order").expect("tool registered");

    let agent = tokio::spawn(async move {
        let _ = to_agent.recv().await; // SessionStart
        let call = to_agent.recv().await.expect("tool call");
        let PipelineToAgent::ToolCall {
            call_id,
            name,
            arguments_json,
        } = call
        else {
            panic!("expected ToolCall, got {call:?}");
        };
        assert_eq!(name, "check_order");
        assert_eq!(arguments_json, "{\"order\":7}");
        let _ = from_agent
            .send(AgentToPipeline::ToolResult {
                call_id,
                pending: false,
                output: "{\"status\":\"shipped\"}".into(),
            })
            .await;
    });

    match tool.invoke("{\"order\":7}").await.expect("invoke ok") {
        ToolOutcome::Done(out) => assert!(out.contains("shipped")),
        other @ ToolOutcome::Pending { .. } => panic!("expected Done, got {other:?}"),
    }
    agent.await.unwrap();
}

/// Pending ack now, `CompleteTool` later on the agent channel.
#[tokio::test]
async fn remote_tool_pending_then_complete() {
    let (backend, mut to_agent, from_agent, mut agent_rx) = test_backend();
    let registry = backend.registry().await.expect("registry");
    let tool = registry.lookup("check_order").expect("tool registered");

    let agent = tokio::spawn(async move {
        let _ = to_agent.recv().await; // SessionStart
        let call = to_agent.recv().await.expect("tool call");
        let PipelineToAgent::ToolCall { call_id, .. } = call else {
            panic!("expected ToolCall");
        };
        let _ = from_agent
            .send(AgentToPipeline::ToolResult {
                call_id: call_id.clone(),
                pending: true,
                output: "{\"status\":\"working\"}".into(),
            })
            .await;
        // ... later, the real result.
        let _ = from_agent
            .send(AgentToPipeline::CompleteTool {
                call_id,
                result: "order shipped yesterday".into(),
                scheduling: Scheduling::WhenIdle,
            })
            .await;
    });

    match tool.invoke("{}").await.expect("invoke ok") {
        ToolOutcome::Pending { ack } => assert!(ack.contains("working")),
        other @ ToolOutcome::Done(_) => panic!("expected Pending, got {other:?}"),
    }
    match agent_rx.recv().await.expect("complete tool event") {
        AgentEvent::CompleteTool {
            result, scheduling, ..
        } => {
            assert_eq!(result, "order shipped yesterday");
            assert_eq!(scheduling, Scheduling::WhenIdle);
        }
        other => panic!("expected CompleteTool, got {other:?}"),
    }
    agent.await.unwrap();
}

/// A silent agent fails the invocation at the ack timeout.
#[tokio::test]
async fn remote_tool_ack_timeout() {
    let (backend, _to_agent, _from_agent, _agent_rx) = test_backend();
    let registry = backend.registry().await.expect("registry");
    let tool = registry.lookup("check_order").expect("tool registered");
    let err = tool.invoke("{}").await.expect_err("must time out");
    assert!(err.message.contains("did not ack"), "got: {}", err.message);
}

/// A `CompleteTool` is applied only for a call the agent acked as
/// Pending: an unknown id, a second completion, and a completion after
/// the ack timed out are all dropped.
#[tokio::test]
async fn complete_tool_needs_a_pending_ack_and_applies_once() {
    let (backend, mut to_agent, from_agent, mut agent_rx) = test_backend();
    let registry = backend.registry().await.expect("registry");
    let tool = registry.lookup("check_order").expect("tool registered");
    let _ = to_agent.recv().await; // SessionStart

    // Never issued.
    let _ = from_agent
        .send(AgentToPipeline::CompleteTool {
            call_id: "ghost".into(),
            result: "made up".into(),
            scheduling: Scheduling::WhenIdle,
        })
        .await;

    // Issued, but the agent stays silent past the ack timeout, then
    // completes late.
    let invoke = tool.invoke("{}");
    let agent = tokio::spawn(async move {
        let call = to_agent.recv().await.expect("tool call");
        let PipelineToAgent::ToolCall { call_id, .. } = call else {
            panic!("expected ToolCall");
        };
        tokio::time::sleep(Duration::from_millis(400)).await;
        let _ = from_agent
            .send(AgentToPipeline::CompleteTool {
                call_id: call_id.clone(),
                result: "too late".into(),
                scheduling: Scheduling::WhenIdle,
            })
            .await;
        (from_agent, to_agent)
    });
    invoke.await.expect_err("silent agent times out");
    let (from_agent, mut to_agent) = agent.await.unwrap();

    // A properly acked call completes exactly once.
    let invoke = tool.invoke("{}");
    let agent = tokio::spawn(async move {
        let PipelineToAgent::ToolCall { call_id, .. } = to_agent.recv().await.expect("tool call")
        else {
            panic!("expected ToolCall");
        };
        let _ = from_agent
            .send(AgentToPipeline::ToolResult {
                call_id: call_id.clone(),
                pending: true,
                output: "working".into(),
            })
            .await;
        for result in ["done", "done again"] {
            let _ = from_agent
                .send(AgentToPipeline::CompleteTool {
                    call_id: call_id.clone(),
                    result: result.into(),
                    scheduling: Scheduling::WhenIdle,
                })
                .await;
        }
        // A trailing Say proves the pump processed everything above.
        let _ = from_agent
            .send(AgentToPipeline::Say {
                text: "fence".into(),
                scheduling: Scheduling::WhenIdle,
            })
            .await;
    });
    invoke.await.expect("acked invoke");
    agent.await.unwrap();

    let mut seen = Vec::new();
    loop {
        match agent_rx.recv().await.expect("event") {
            AgentEvent::CompleteTool { result, .. } => seen.push(result),
            AgentEvent::Say { text, .. } if text == "fence" => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(
        seen,
        vec!["done".to_string()],
        "exactly one completion applied"
    );
}

/// Frames past the text bound and reserved message kinds never reach
/// the session; `Say` bursts are throttled.
#[tokio::test]
async fn oversized_reserved_and_bursty_frames_are_dropped() {
    let (_backend, mut to_agent, from_agent, mut agent_rx) = test_backend();
    let _ = to_agent.recv().await; // SessionStart

    let _ = from_agent
        .send(AgentToPipeline::Say {
            text: "x".repeat(crate::agent::link::MAX_AGENT_TEXT_BYTES + 1),
            scheduling: Scheduling::WhenIdle,
        })
        .await;
    let _ = from_agent
        .send(AgentToPipeline::Message {
            kind: "trigger_action".into(),
            text: None,
            payload_json: Some("{\"name\":\"issue_refund\"}".into()),
            record_in_history: false,
        })
        .await;
    for i in 0..(crate::agent::link::OUT_OF_TURN_BURST + 3) {
        let _ = from_agent
            .send(AgentToPipeline::Say {
                text: format!("say {i}"),
                scheduling: Scheduling::WhenIdle,
            })
            .await;
    }
    let _ = from_agent
        .send(AgentToPipeline::Message {
            kind: "card".into(),
            text: Some("fence".into()),
            payload_json: None,
            record_in_history: false,
        })
        .await;

    let mut says = 0;
    loop {
        match agent_rx.recv().await.expect("event") {
            AgentEvent::Say { text, .. } => {
                assert!(
                    text.starts_with("say "),
                    "oversized Say leaked: {}",
                    text.len()
                );
                says += 1;
            }
            AgentEvent::Message { message, .. } => {
                assert_eq!(message.kind, "card", "reserved kind leaked");
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(says, crate::agent::link::OUT_OF_TURN_BURST as usize);
}

/// Out-of-turn Say from the tool backend reaches the session.
#[tokio::test]
async fn out_of_turn_say_routes_to_session() {
    let (_backend, mut to_agent, from_agent, mut agent_rx) = test_backend();
    let _ = to_agent.recv().await; // SessionStart
    let _ = from_agent
        .send(AgentToPipeline::Say {
            text: "your order update is ready".into(),
            scheduling: Scheduling::WhenIdle,
        })
        .await;
    match agent_rx.recv().await.expect("say event") {
        AgentEvent::Say { text, .. } => assert!(text.contains("order update")),
        other => panic!("expected Say, got {other:?}"),
    }
}
