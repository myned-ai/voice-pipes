use super::super::turn::{ResponseTracker, SuppressGate};
use super::common::*;
use crate::agent::{AgentEvent, Scheduling};
use crate::realtime::tests::mock::{MockRealtimeProtocol, MockServer};
use crate::realtime::{RealtimeClient, RealtimeProtocol};
use crate::realtime::{ToolOutcome, ToolRegistry};
use crate::transport::{MessageRole, PipelineMessage, ServerMessage};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc};

/// Regression test for the session freeze: dispatching a slow tool
/// must return to the select loop immediately; the outcome arrives
/// later on the agent channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_tool_dispatch_does_not_block_select_loop() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(SleepTool));
    let (agent_tx, mut agent_rx) = mpsc::channel(8);

    let start = Instant::now();
    ctx.handle_provider_function_call("c1".into(), "sleep".into(), "{}".into(), &reg, &agent_tx)
        .await;
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "dispatch blocked the loop for {:?}",
        start.elapsed()
    );

    let ev = tokio::time::timeout(Duration::from_secs(2), agent_rx.recv())
        .await
        .expect("tool completion timed out")
        .expect("agent channel closed");
    match ev {
        AgentEvent::ToolDispatchDone {
            call_id, outcome, ..
        } => {
            assert_eq!(call_id, "c1");
            assert!(matches!(outcome, Ok(ToolOutcome::Done(ref r)) if r.contains("42")));
        }
        other => panic!("expected ToolDispatchDone, got {other:?}"),
    }
}

/// Done outcome -> function output forwarded + response.create kick
/// (without the kick, server-VAD providers never speak the
/// result).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_done_sends_output_then_kicks_response() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    let (mut responses, mut kick_pending, mut queue) =
        (ResponseTracker::default(), false, VecDeque::new());
    ctx.handle_agent_event(
        AgentEvent::ToolDispatchDone {
            call_id: "c7".into(),
            name: "echo".into(),
            outcome: Ok(ToolOutcome::Done("{\"ok\":true}".into())),
        },
        &mut responses,
        &mut kick_pending,
        &mut queue,
    )
    .await
    .expect("agent event");

    let out = wait_for_env(&server, "function_call_output").await;
    assert_eq!(out.get("call_id").and_then(|x| x.as_str()), Some("c7"));
    wait_for_env(&server, "response_create").await;
    assert!(responses.is_active(), "kick must mark a response in flight");
    assert!(!kick_pending);
}

/// Pending outcome -> the ack (not the result) goes back as the one
/// function output, then a kick so the model can speak the filler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_pending_ack_goes_out_with_kick() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    let (mut responses, mut kick_pending, mut queue) =
        (ResponseTracker::default(), false, VecDeque::new());
    ctx.handle_agent_event(
        AgentEvent::ToolDispatchDone {
            call_id: "c8".into(),
            name: "slow".into(),
            outcome: Ok(ToolOutcome::Pending {
                ack: "{\"status\":\"working\"}".into(),
            }),
        },
        &mut responses,
        &mut kick_pending,
        &mut queue,
    )
    .await
    .expect("agent event");

    let out = wait_for_env(&server, "function_call_output").await;
    assert_eq!(
        out.get("result").and_then(|x| x.as_str()),
        Some("{\"status\":\"working\"}")
    );
    wait_for_env(&server, "response_create").await;
}

/// Late result of a `Pending` tool: injected as a context message (one
/// output per `call_id`!) and kicked. While a response is active,
/// `WhenIdle` queues instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_tool_when_idle_injects_context_or_queues() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    // 1) Response active -> queued, nothing sent.
    let (mut responses, mut kick_pending, mut queue) = (active_responses(), false, VecDeque::new());
    ctx.handle_agent_event(
        AgentEvent::CompleteTool {
            call_id: "c9".into(),
            result: "flight booked".into(),
            scheduling: Scheduling::WhenIdle,
        },
        &mut responses,
        &mut kick_pending,
        &mut queue,
    )
    .await
    .expect("agent event");
    assert_eq!(queue.len(), 1, "WhenIdle during a response must queue");
    assert_never_recorded(&server, "context_message").await;

    // 2) Idle -> context + kick.
    responses = ResponseTracker::default();
    let ev = queue.pop_front().unwrap();
    ctx.handle_agent_event(ev, &mut responses, &mut kick_pending, &mut queue)
        .await
        .expect("agent event");
    let env = wait_for_env(&server, "context_message").await;
    let text = env.get("text").and_then(|x| x.as_str()).unwrap_or_default();
    assert!(
        text.contains("flight booked"),
        "context carries result: {text}"
    );
    wait_for_env(&server, "response_create").await;
    assert!(responses.is_active());
}

/// Silent scheduling injects context without a kick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_tool_silent_injects_without_kick() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    let (mut responses, mut kick_pending, mut queue) =
        (ResponseTracker::default(), false, VecDeque::new());
    ctx.handle_agent_event(
        AgentEvent::CompleteTool {
            call_id: "c10".into(),
            result: "cache warmed".into(),
            scheduling: Scheduling::Silent,
        },
        &mut responses,
        &mut kick_pending,
        &mut queue,
    )
    .await
    .expect("agent event");

    wait_for_env(&server, "context_message").await;
    assert_never_recorded(&server, "response_create").await;
    assert!(!responses.is_active());
}

/// `AgentEvent::Message` forwards the `PipelineMessage` to the client
/// with no state change, no audio, no provider traffic (unless
/// `record_in_history`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_message_forwards_without_turn_or_state_change() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    let mut message = PipelineMessage::text("Your order shipped.", MessageRole::Assistant);
    message.kind = "card".to_string();
    message.payload = Some(serde_json::json!({ "order_id": "o-1" }));

    let (mut responses, mut kick_pending, mut queue) =
        (ResponseTracker::default(), false, VecDeque::new());
    ctx.handle_agent_event(
        AgentEvent::Message {
            message,
            record_in_history: false,
        },
        &mut responses,
        &mut kick_pending,
        &mut queue,
    )
    .await
    .expect("agent event");

    let msg = out_rx.recv().await.expect("client message");
    match msg {
        ServerMessage::Message(pm) => {
            assert_eq!(pm.kind, "card");
            assert_eq!(pm.text.as_deref(), Some("Your order shipped."));
        }
        other => panic!("expected Message, got {other:?}"),
    }
    assert!(
        out_rx.try_recv().is_err(),
        "no StateChanged/audio may accompany an out-of-turn message"
    );
    assert_never_recorded(&server, "context_message").await;
    assert_never_recorded(&server, "response_create").await;
    assert!(!responses.is_active());
}

/// Say with Interrupt cancels the in-flight response and delivers
/// immediately instead of queueing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn say_interrupt_cancels_and_speaks_now() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    let (mut responses, mut kick_pending, mut queue) = (active_responses(), false, VecDeque::new());
    ctx.handle_agent_event(
        AgentEvent::Say {
            text: "The build finished.".into(),
            scheduling: Scheduling::Interrupt,
        },
        &mut responses,
        &mut kick_pending,
        &mut queue,
    )
    .await
    .expect("agent event");

    assert!(queue.is_empty(), "Interrupt must not queue");
    wait_for_env(&server, "cancel").await;
    let env = wait_for_env(&server, "context_message").await;
    assert!(
        env.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .contains("The build finished.")
    );
    wait_for_env(&server, "response_create").await;
}

/// Unknown tool -> error output via the same completion path, so it
/// still gets the post-output kick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_tool_reports_error_through_agent_channel() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    let reg = ToolRegistry::new();
    let (agent_tx, mut agent_rx) = mpsc::channel(8);
    ctx.handle_provider_function_call("c2".into(), "nope".into(), "{}".into(), &reg, &agent_tx)
        .await;

    let ev = agent_rx.recv().await.expect("agent event");
    match &ev {
        AgentEvent::ToolDispatchDone {
            call_id, outcome, ..
        } => {
            assert_eq!(call_id, "c2");
            assert!(outcome.is_err());
        }
        other => panic!("expected ToolDispatchDone, got {other:?}"),
    }

    let (mut responses, mut kick_pending, mut queue) =
        (ResponseTracker::default(), false, VecDeque::new());
    ctx.handle_agent_event(ev, &mut responses, &mut kick_pending, &mut queue)
        .await
        .expect("agent event");
    let out = wait_for_env(&server, "function_call_output").await;
    assert!(
        out.get("result")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .contains("not registered")
    );
    wait_for_env(&server, "response_create").await;
}

// ── WS-2: post-audio chain runs in the realtime engine ──────────────
//
// Proves the realtime runner routes BOTH outbound audio paths -- provider
// audio deltas AND the end-of-utterance sentinel -- through the shared
// post-audio chain, so `audio_meta.extras` is populated identically to the
// cascade engine. Uses a generic marker processor (the same `AudioPostProcessor`
// trait a gRPC post-processor implements) rather than a live service, so the
// test stays hermetic while still exercising the exact call sites.
