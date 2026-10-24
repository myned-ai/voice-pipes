use super::*;
use crate::agent::Scheduling;
use crate::agent::link::AgentLinkError;

/// In-memory [`AgentLink`]: the test plays the agent on the other
/// end of a channel pair.
struct InMemoryLink {
    to_agent: mpsc::Sender<PipelineToAgent>,
    from_agent: tokio::sync::Mutex<mpsc::Receiver<AgentToPipeline>>,
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
}

/// (gateway, frames the pipeline sent, sender for agent frames)
fn test_link() -> (
    Arc<InMemoryLink>,
    mpsc::Receiver<PipelineToAgent>,
    mpsc::Sender<AgentToPipeline>,
) {
    let (to_agent_tx, to_agent_rx) = mpsc::channel(32);
    let (from_agent_tx, from_agent_rx) = mpsc::channel(32);
    (
        Arc::new(InMemoryLink {
            to_agent: to_agent_tx,
            from_agent: tokio::sync::Mutex::new(from_agent_rx),
        }),
        to_agent_rx,
        from_agent_tx,
    )
}

fn bound_gateway(link: Arc<InMemoryLink>) -> (AgentGateway, mpsc::Receiver<AgentEvent>) {
    let gateway = AgentGateway::new(link, "test-pipeline");
    let (agent_tx, agent_rx) = mpsc::channel(8);
    gateway.bind_session(
        AgentSessionHandle::new(agent_tx)
            .with_session_id("sess-1")
            .session_handle(),
    );
    (gateway, agent_rx)
}

/// Full turn round trip: `UserTurn` out, streamed fragments back
/// in, completion on end-of-response — with `SessionStart` first.
#[tokio::test]
async fn turn_round_trip_streams_fragments() {
    let (link, mut to_agent, from_agent) = test_link();
    let (gateway, _agent_rx) = bound_gateway(link);

    // Scripted agent.
    let agent = tokio::spawn(async move {
        let start = to_agent.recv().await.expect("session start");
        assert!(matches!(
            start,
            PipelineToAgent::SessionStart {
                protocol_version: AGENT_PROTOCOL_VERSION,
                ..
            }
        ));
        let _ = from_agent
            .send(AgentToPipeline::SessionAck {
                protocol_version: AGENT_PROTOCOL_VERSION,
                agent_name: "scripted".into(),
            })
            .await;
        let turn = to_agent.recv().await.expect("user turn");
        let PipelineToAgent::UserTurn {
            turn_id,
            text,
            is_context,
        } = turn
        else {
            panic!("expected UserTurn, got {turn:?}");
        };
        assert_eq!(text, "hello");
        assert!(!is_context);
        for (frag, end) in [("Hi ", false), ("there!", true)] {
            let _ = from_agent
                .send(AgentToPipeline::TurnFragment {
                    turn_id,
                    text: frag.into(),
                    is_end_of_response: end,
                    first_token_ms: None,
                    token_count: None,
                })
                .await;
        }
    });

    let (tx, mut rx) = mpsc::channel::<TextFragment>(8);
    gateway
        .process_stream("hello", tx, CancellationToken::new())
        .await
        .expect("turn should complete");
    let first = rx.recv().await.expect("first fragment");
    assert_eq!(first.text, "Hi ");
    let second = rx.recv().await.expect("second fragment");
    assert_eq!(second.text, "there!");
    assert!(second.is_end_of_response);
    agent.await.unwrap();
}

/// Out-of-turn frames route onto the `AgentSessionHandle`.
#[tokio::test]
async fn out_of_turn_frames_route_to_agent_channel() {
    let (link, mut to_agent, from_agent) = test_link();
    let (_gateway, mut agent_rx) = bound_gateway(link);
    let _ = to_agent.recv().await; // SessionStart

    let _ = from_agent
        .send(AgentToPipeline::Say {
            text: "proactive!".into(),
            scheduling: Scheduling::WhenIdle,
        })
        .await;
    let _ = from_agent
        .send(AgentToPipeline::Message {
            kind: "tool_call".into(),
            text: None,
            payload_json: Some("{\"name\":\"lookup\"}".into()),
            record_in_history: false,
        })
        .await;

    match agent_rx.recv().await.expect("say event") {
        AgentEvent::Say { text, scheduling } => {
            assert_eq!(text, "proactive!");
            assert_eq!(scheduling, Scheduling::WhenIdle);
        }
        other => panic!("expected Say, got {other:?}"),
    }
    match agent_rx.recv().await.expect("message event") {
        AgentEvent::Message { message, .. } => {
            assert_eq!(message.kind, "tool_call");
            assert_eq!(
                message.payload.and_then(|p| p.get("name").cloned()),
                Some(serde_json::Value::String("lookup".into()))
            );
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

/// An oversized turn fragment is dropped rather than spoken, and a
/// `Message` with an engine-reserved kind never reaches the client.
#[tokio::test]
async fn oversized_fragment_and_reserved_message_are_refused() {
    let (link, mut to_agent, from_agent) = test_link();
    let (gateway, mut agent_rx) = bound_gateway(link);
    let _ = to_agent.recv().await; // SessionStart

    let agent = tokio::spawn(async move {
        let PipelineToAgent::UserTurn { turn_id, .. } = to_agent.recv().await.expect("turn") else {
            panic!("expected UserTurn");
        };
        let _ = from_agent
            .send(AgentToPipeline::TurnFragment {
                turn_id,
                text: "y".repeat(crate::agent::link::MAX_AGENT_TEXT_BYTES + 1),
                is_end_of_response: false,
                first_token_ms: None,
                token_count: None,
            })
            .await;
        let _ = from_agent
            .send(AgentToPipeline::Message {
                kind: "client_tool_result".into(),
                text: None,
                payload_json: Some("{\"call_id\":\"c1\",\"ok\":true}".into()),
                record_in_history: false,
            })
            .await;
        let _ = from_agent
            .send(AgentToPipeline::TurnFragment {
                turn_id,
                text: "short".into(),
                is_end_of_response: true,
                first_token_ms: None,
                token_count: None,
            })
            .await;
        let _ = from_agent
            .send(AgentToPipeline::Message {
                kind: "card".into(),
                text: Some("fence".into()),
                payload_json: None,
                record_in_history: false,
            })
            .await;
    });

    let (tx, mut rx) = mpsc::channel::<TextFragment>(8);
    gateway
        .process_stream("hello", tx, CancellationToken::new())
        .await
        .expect("turn completes");
    let frag = rx.recv().await.expect("fragment");
    assert_eq!(frag.text, "short", "the oversized fragment was dropped");
    assert!(frag.is_end_of_response);
    agent.await.unwrap();

    match agent_rx.recv().await.expect("message") {
        AgentEvent::Message { message, .. } => assert_eq!(message.kind, "card"),
        other => panic!("reserved kind leaked or wrong event: {other:?}"),
    }
}

/// `Generate` turns go out with `is_context` = true.
#[tokio::test]
async fn context_stream_marks_is_context() {
    let (link, mut to_agent, from_agent) = test_link();
    let (gateway, _agent_rx) = bound_gateway(link);
    let agent = tokio::spawn(async move {
        let _ = to_agent.recv().await; // SessionStart
        let turn = to_agent.recv().await.expect("context turn");
        let PipelineToAgent::UserTurn {
            turn_id,
            is_context,
            ..
        } = turn
        else {
            panic!("expected UserTurn, got {turn:?}");
        };
        assert!(is_context, "Generate input must be marked as context");
        let _ = from_agent
            .send(AgentToPipeline::TurnFragment {
                turn_id,
                text: "done".into(),
                is_end_of_response: true,
                first_token_ms: None,
                token_count: None,
            })
            .await;
    });
    let (tx, _rx) = mpsc::channel::<TextFragment>(8);
    gateway
        .process_context_stream("task finished", tx, CancellationToken::new())
        .await
        .expect("context turn should complete");
    agent.await.unwrap();
}

/// A silent agent must not hang the turn: the watchdog fails it.
#[tokio::test]
async fn watchdog_fails_turn_when_agent_hangs() {
    let (link, mut _to_agent, _from_agent) = test_link();
    let gateway =
        AgentGateway::new(link, "test-pipeline").with_turn_timeout(Duration::from_millis(80));
    let (tx, _rx) = mpsc::channel::<TextFragment>(8);
    let err = gateway
        .process_stream("hello?", tx, CancellationToken::new())
        .await
        .expect_err("watchdog must fire");
    assert!(err.message.contains("watchdog"), "got: {}", err.message);
}

/// Barge-in ground truth reaches the agent as Interrupted.
#[tokio::test]
async fn truncate_history_sends_interrupted() {
    let (link, mut to_agent, from_agent) = test_link();
    let (gateway, _agent_rx) = bound_gateway(link);
    let agent = tokio::spawn(async move {
        let _ = to_agent.recv().await; // SessionStart
        let turn = to_agent.recv().await.expect("user turn");
        let PipelineToAgent::UserTurn { turn_id, .. } = turn else {
            panic!("expected UserTurn");
        };
        let _ = from_agent
            .send(AgentToPipeline::TurnFragment {
                turn_id,
                text: "a long reply".into(),
                is_end_of_response: true,
                first_token_ms: None,
                token_count: None,
            })
            .await;
        // After the turn, the pipeline reports what was heard.
        let next = to_agent.recv().await.expect("interrupted");
        let PipelineToAgent::Interrupted {
            turn_id: tid,
            played_text,
        } = next
        else {
            panic!("expected Interrupted, got {next:?}");
        };
        assert_eq!(tid, turn_id);
        assert_eq!(played_text, "a long");
    });
    let (tx, _rx) = mpsc::channel::<TextFragment>(8);
    gateway
        .process_stream("hi", tx, CancellationToken::new())
        .await
        .unwrap();
    assert!(gateway.truncate_history("a long").is_none());
    agent.await.unwrap();
}

/// Idle notifications go to the agent, which owns the wording: binding the
/// gateway registers it with the session, which then offers it the prompt.
#[tokio::test]
async fn on_user_idle_forwards_and_claims_the_prompt() {
    let (link, mut to_agent, _from_agent) = test_link();
    let gateway = AgentGateway::new(link, "test-pipeline");
    let (agent_tx, _agent_rx) = mpsc::channel(8);
    let agent = AgentSessionHandle::new(agent_tx).with_session_id("sess-1");
    assert!(!agent.offer_user_idle(9_000, 1).await, "nothing bound yet");
    gateway.bind_session(agent.session_handle());
    let _ = to_agent.recv().await; // SessionStart

    assert!(agent.offer_user_idle(9_000, 1).await);
    let frame = to_agent.recv().await.expect("idle frame");
    assert_eq!(
        frame,
        PipelineToAgent::UserIdle {
            idle_ms: 9_000,
            prompt_count: 1
        }
    );
}

/// Silent context notes reach the agent with their kind.
#[tokio::test]
async fn context_notes_carry_kind() {
    let (link, mut to_agent, _from_agent) = test_link();
    let (gateway, _agent_rx) = bound_gateway(link);
    let _ = to_agent.recv().await; // SessionStart

    gateway.inject_context("user saw a card").await;
    gateway
        .append_assistant_message("Are you still there?")
        .await;
    assert_eq!(
        to_agent.recv().await.unwrap(),
        PipelineToAgent::ContextNote {
            text: "user saw a card".into(),
            kind: "context".into()
        }
    );
    assert_eq!(
        to_agent.recv().await.unwrap(),
        PipelineToAgent::ContextNote {
            text: "Are you still there?".into(),
            kind: "assistant_message".into()
        }
    );
}
