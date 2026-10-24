use super::super::turn::{AnchorSource, ResponseTracker, SuppressGate};
use super::common::*;
use crate::agent::client_tools::{ClientToolCalls, ClientToolConfig};
use crate::realtime::ToolRegistry;
use crate::realtime::tests::mock::{MockRealtimeProtocol, MockServer};
use crate::realtime::{RealtimeClient, RealtimeProtocol};
use crate::transport::ClientToolPhase;
use crate::transport::{MessageRole, PipelineState, ServerControl, ServerMessage};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

fn navigate_tool(speak_on_result: bool) -> ClientToolConfig {
    serde_json::from_value(serde_json::json!({
        "name": "navigate",
        "description": "Navigate the UI to a named screen.",
        "parameters": { "type": "object",
            "properties": { "screen": { "type": "string" } },
            "required": ["screen"] },
        "speak_on_result": speak_on_result
    }))
    .unwrap()
}

/// Allowlisted call → the user's buffered transcription flushes FIRST
/// (chat shows question → command in speaking order), then
/// `trigger_action` to the client + silent ack to the provider; no
/// spoken response, no server-tool dispatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_tool_call_emits_trigger_action_and_silent_ack() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    // The user's words that triggered the call, still buffered (a silent
    // tool-call turn never starts audio output, which is what normally
    // flushes them).
    s.pending_transcript = Some("open the settings screen".to_string());

    // A server tool of the same name must be shadowed by the client tool.
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(SleepTool));
    let (agent_tx, mut agent_rx) = mpsc::channel(8);

    agent_test_ctx!(s, client, out_tx, pe_tx, suppress)
        .handle_provider_function_call(
            "c9".into(),
            "navigate".into(),
            "{\"screen\":\"settings\"}".into(),
            &reg,
            &agent_tx,
        )
        .await;

    // First out: the user's transcription — before the command card.
    let msg = out_rx.try_recv().expect("user transcript message");
    match msg {
        ServerMessage::Message(m) => {
            assert_eq!(m.role, MessageRole::User);
            assert_eq!(m.text.as_deref(), Some("open the settings screen"));
        }
        other => panic!("expected user transcript first, got {other:?}"),
    }

    // Then the structured command with the provider's call_id for
    // correlation.
    let msg = out_rx.try_recv().expect("trigger_action message");
    match msg {
        ServerMessage::Message(m) => {
            assert_eq!(m.kind, "trigger_action");
            let payload = m.payload.expect("payload");
            assert_eq!(payload["call_id"], "c9");
            assert_eq!(payload["name"], "navigate");
            assert_eq!(payload["arguments"]["screen"], "settings");
        }
        other => panic!("expected Message, got {other:?}"),
    }

    // The provider gets the (only) function output — the silent ack.
    let out = wait_for_env(&server, "function_call_output").await;
    assert!(
        out.get("result")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .contains("dispatched_to_client")
    );
    // No response.create — the model must not narrate the action.
    assert_never_recorded(&server, "response_create").await;

    // Not a server tool dispatch: nothing on the agent channel, and the
    // call is registered for result correlation.
    assert!(agent_rx.try_recv().is_err(), "no ToolDispatchDone expected");
    assert_eq!(s.client_calls.len(), 1);
}

/// Completed result, silent default: context injected, still no kick;
/// a duplicate Completed is a no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_tool_result_silent_injects_context_without_kick() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;

    let ev = ctx
        .resolve_client_tool_result(
            "c1",
            ClientToolPhase::Completed,
            Some(&serde_json::json!({"ok": true})),
            false,
        )
        .expect("completed resolves to an event");
    let (mut responses, mut kick_pending, mut queue) =
        (ResponseTracker::default(), false, VecDeque::new());
    ctx.handle_agent_event(ev, &mut responses, &mut kick_pending, &mut queue)
        .await
        .expect("agent event");

    let env = wait_for_env(&server, "context_message").await;
    let text = env.get("text").and_then(|x| x.as_str()).unwrap_or_default();
    assert!(text.contains("navigate"), "context names the tool: {text}");
    assert!(
        text.contains("\"ok\":true"),
        "context carries the result: {text}"
    );
    assert_never_recorded(&server, "response_create").await;

    // Duplicate Completed after the first is dropped.
    assert!(
        ctx.resolve_client_tool_result("c1", ClientToolPhase::Completed, None, false)
            .is_none()
    );
    // Unknown ids are dropped too.
    assert!(
        ctx.resolve_client_tool_result("nope", ClientToolPhase::Completed, None, false)
            .is_none()
    );
}

/// `speak_on_result = true` → the completion becomes a spoken
/// confirmation: context message + response kick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_tool_result_speak_on_result_kicks_response() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(true));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_provider_function_call("c2".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;

    let ev = ctx
        .resolve_client_tool_result("c2", ClientToolPhase::Completed, None, false)
        .expect("event");
    let (mut responses, mut kick_pending, mut queue) =
        (ResponseTracker::default(), false, VecDeque::new());
    ctx.handle_agent_event(ev, &mut responses, &mut kick_pending, &mut queue)
        .await
        .expect("agent event");

    wait_for_env(&server, "context_message").await;
    wait_for_env(&server, "response_create").await;
    assert!(
        responses.is_active(),
        "spoken confirmation opened a response"
    );
}

/// Beyond the in-flight cap, new client tool calls are ack'd with an
/// overloaded error instead of growing the map.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_tool_calls_beyond_cap_ack_overloaded() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    s.client_calls = ClientToolCalls::new(1, Duration::from_mins(1));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        for id in ["c1", "c2"] {
            ctx.handle_provider_function_call(
                id.into(),
                "navigate".into(),
                "{}".into(),
                &reg,
                &agent_tx,
            )
            .await;
        }
    }

    // Only the first call reached the client.
    assert!(out_rx.try_recv().is_ok(), "first command forwarded");
    assert!(
        out_rx.try_recv().is_err(),
        "second command must be rejected"
    );
    assert_eq!(s.client_calls.len(), 1);

    // Both calls were ack'd (one output per call_id); the second with
    // the overloaded error.
    for _ in 0..40 {
        let outputs: Vec<String> = server
            .recorded()
            .iter()
            .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("function_call_output"))
            .filter_map(|v| v.get("result").and_then(|r| r.as_str()).map(String::from))
            .collect();
        if outputs.len() == 2 {
            assert!(outputs.iter().any(|o| o.contains("dispatched_to_client")));
            assert!(outputs.iter().any(|o| o.contains("overloaded")));
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("expected 2 function outputs: {:?}", server.recorded());
}

/// Gemini native-audio transcribes the model's "planned speech" around a
/// tool call even when NO audio is produced ("Let me check" with
/// `audio_duration` 0). That unspoken text must never reach the client:
/// not as streamed transcript deltas and not via the EOU `full_text`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_audio_tool_call_turn_drops_phantom_transcript() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_provider_response_started().await.unwrap();
    // The provider streams the transcription of unspoken planned speech…
    ctx.handle_provider_text_delta("Let me check".to_string())
        .await
        .unwrap();
    // …then the function call, and the turn ends with zero audio.
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;
    ctx.handle_provider_response_completed().await.unwrap();

    // The client sees the trigger_action command and the EOU sentinel —
    // but never the phantom "Let me check" (no transcript message, no
    // full_text on the sentinel).
    let mut got_trigger = false;
    let mut got_eou = false;
    while let Ok(msg) = out_rx.try_recv() {
        match msg {
            ServerMessage::Message(pm) if pm.kind == "trigger_action" => got_trigger = true,
            ServerMessage::Message(pm) if pm.kind == "text" => {
                panic!("phantom transcript leaked to the client: {:?}", pm.text)
            }
            ServerMessage::Audio(c) if c.is_end_of_utterance => {
                assert!(
                    c.full_text.is_none(),
                    "EOU full_text must be suppressed for a zero-audio tool-call turn"
                );
                got_eou = true;
            }
            _ => {}
        }
    }
    assert!(got_trigger, "trigger_action must still go out");
    assert!(got_eou, "EOU sentinel must still close the turn");
}

/// Gemini 3.x answers with a bare `toolCall` (no text or audio first) and
/// closes that turn right away; a silent client tool gets no follow-up at
/// all. The call opens a turn of its own that carries the user's words,
/// and its completion returns the client to Listening.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_tool_call_opens_a_turn_that_ends_listening() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_provider_input_transcript("Open the settings.".into(), false)
        .await
        .unwrap();
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;
    let turn = ctx.current_turn.as_ref().expect("the call opens a turn");
    assert!(turn.opened_by_tool_call && turn.saw_function_call);
    assert_eq!(turn.user_transcript.as_deref(), Some("Open the settings."));
    ctx.handle_provider_response_completed().await.unwrap();

    let mut states = Vec::new();
    let mut metrics_user = None;
    while let Ok(msg) = out_rx.try_recv() {
        match msg {
            ServerMessage::Control(ServerControl::StateChanged { state }) => states.push(state),
            ServerMessage::Control(ServerControl::TurnMetrics { snapshot }) => {
                metrics_user = Some(snapshot.utterance_id);
            }
            _ => {}
        }
    }
    assert_eq!(states.last(), Some(&PipelineState::Listening));
    assert!(
        metrics_user.is_some(),
        "the tool-call leg is a turn of its own"
    );
    assert!(s.current_turn.is_none());
    assert!(
        s.pending_transcript.is_none(),
        "the leg took the user's words"
    );
}

/// Gemini 3.x sends the input transcript in the same burst as a bare
/// `toolCall`. As a proxy it would book the whole wait as provider
/// endpointing, so the leg is anchored on vp's own end-of-speech stamp.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcript_arriving_with_a_tool_call_yields_to_the_observer_stamp() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.endpoint_observer = Some(test_observer());
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    feed_mic(&mut ctx, 400, 400).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    ctx.handle_provider_input_transcript("Open the settings.".into(), false)
        .await
        .unwrap();
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "vp_vad");
    assert!(extras.get("provider_endpointing_ms").is_none());
    assert!(extras["vp_speech_end_epoch_ms"].as_u64().unwrap() > 0);
}

/// A silent leg whose tool result will be spoken hands its anchor to the
/// spoken follow-up; a leg with nothing to speak about keeps it to itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spoken_tool_result_inherits_the_legs_anchor() {
    for (speak, inherits) in [(true, true), (false, false)] {
        let server = MockServer::start(vec![]).await;
        let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
        let mut client = RealtimeClient::connect(proto, 16_000)
            .await
            .expect("connect");
        let (out_tx, _out_rx) = mpsc::channel(100);
        let (pe_tx, _) = broadcast::channel(100);
        let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
        let mut s = CtxState::new();
        s.client_tools.register(navigate_tool(speak));
        let reg = ToolRegistry::new();
        let (agent_tx, _agent_rx) = mpsc::channel(8);

        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_provider_input_transcript("Open the settings.".into(), false)
            .await
            .unwrap();
        ctx.handle_provider_function_call(
            "c1".into(),
            "navigate".into(),
            "{}".into(),
            &reg,
            &agent_tx,
        )
        .await;
        let leg_anchor = ctx.current_turn.as_ref().unwrap().ttfb_anchor;
        assert!(leg_anchor.is_some());
        ctx.handle_provider_response_completed().await.unwrap();
        assert_eq!(
            ctx.request_anchor.map(|a| a.instant),
            if inherits {
                leg_anchor.map(|a| a.instant)
            } else {
                None
            },
            "speak_on_result = {speak}"
        );
    }
}

/// `OpenAI` shape: `speech_stopped` anchors the request, `response.created`
/// opens the turn, the function call arrives inside it. The call opens no
/// second turn; a spoken result still inherits the provider's own anchor,
/// and the next `speech_stopped` replaces an inherited anchor that was never
/// used.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_tool_call_stays_in_its_response_turn() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(true));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.note_user_speech_end();
    ctx.handle_provider_response_started().await.unwrap();
    let response_turn = ctx.current_turn.as_ref().unwrap().utterance_id;
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;
    let turn = ctx.current_turn.as_ref().unwrap();
    assert_eq!(turn.utterance_id, response_turn, "no second turn");
    assert!(!turn.opened_by_tool_call);
    let anchor = turn.ttfb_anchor.expect("speech_stopped anchor");
    assert_eq!(anchor.source, AnchorSource::ProviderSpeechStopped);

    ctx.handle_provider_response_completed().await.unwrap();
    assert_eq!(ctx.request_anchor.map(|a| a.instant), Some(anchor.instant));

    tokio::time::sleep(Duration::from_millis(5)).await;
    ctx.note_user_speech_end();
    assert!(
        ctx.request_anchor
            .is_some_and(|a| a.instant > anchor.instant),
        "new speech replaces the unused inherited anchor"
    );
}

/// A completion with no turn outside a tool-call leg is a late duplicate
/// (the turn was already finished or interrupted) and changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turnless_completion_outside_tool_leg_sends_nothing() {
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
    ctx.handle_provider_response_completed().await.unwrap();
    assert!(out_rx.try_recv().is_err(), "no message expected");
}

/// A spoken tool-call turn keeps its transcript: once audio flows, the
/// withheld deltas flush and later ones stream straight through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spoken_tool_call_turn_keeps_transcript() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_provider_response_started().await.unwrap();
    ctx.handle_provider_text_delta("On it".to_string())
        .await
        .unwrap();
    // Audio arrives: the withheld transcript must flush.
    ctx.handle_provider_audio_delta(vec![0i16; 1600], 16_000, None)
        .await
        .unwrap();
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;
    ctx.handle_provider_response_completed().await.unwrap();

    let mut got_text = false;
    let mut got_full_text = false;
    while let Ok(msg) = out_rx.try_recv() {
        match msg {
            ServerMessage::Message(pm) if pm.kind == "text" => {
                assert_eq!(pm.text.as_deref(), Some("On it"));
                got_text = true;
            }
            ServerMessage::Audio(c) if c.is_end_of_utterance => {
                got_full_text = c.full_text.as_deref() == Some("On it");
            }
            _ => {}
        }
    }
    assert!(got_text, "spoken transcript must reach the client");
    assert!(got_full_text, "EOU full_text kept for spoken turns");
}

/// Gemini's known duplicate-response quirk: after a tool call, the model
/// can emit its unspoken "planned speech" AGAIN in a follow-up response
/// that carries no function call of its own — and the call itself can
/// arrive before any turn exists. The session-scoped phantom guard must
/// drop both zero-audio transcripts; the next audibly-spoken turn
/// disarms it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phantom_guard_covers_pre_turn_calls_and_duplicate_responses() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    // Function call arrives FIRST — no turn exists yet to flag.
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;

    // Turn 1: zero-audio transcript created AFTER the call.
    ctx.handle_provider_text_delta("Let me check".to_string())
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();

    // Turn 2: the duplicate follow-up — text-only, NO function call.
    ctx.handle_provider_text_delta("Let me check".to_string())
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();

    // Turn 3: the model audibly speaks — guard disarms, transcript flows.
    ctx.handle_provider_text_delta("Anything else?".to_string())
        .await
        .unwrap();
    ctx.handle_provider_audio_delta(vec![0i16; 1600], 16_000, None)
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();

    let mut agent_texts: Vec<String> = Vec::new();
    while let Ok(msg) = out_rx.try_recv() {
        if let ServerMessage::Message(pm) = msg
            && pm.kind == "text"
            && pm.role == MessageRole::Assistant
        {
            agent_texts.push(pm.text.unwrap_or_default());
        }
    }
    assert_eq!(
        agent_texts,
        vec!["Anything else?".to_string()],
        "both phantom 'Let me check' turns must be dropped; the spoken turn flows"
    );
}

/// A bare toolCall with no response turn must CONSUME the flushed user
/// transcription: without that, the user's next utterance appends onto
/// the leftover buffer and re-emits both sentences as one merged bubble
/// attributed to the next turn (observed live on Gemini: "open settings"
/// + the follow-up question became one turn-4 transcript).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_tool_call_consumes_flushed_transcript() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    // User speaks; Gemini emits a bare toolCall (no response wrapper).
    ctx.handle_provider_input_transcript("Open the settings.".into(), false)
        .await
        .unwrap();
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;

    // The user speaks AGAIN before the model replies.
    ctx.handle_provider_input_transcript("Which screen did you open?".into(), false)
        .await
        .unwrap();
    // The reply turn starts with audio; it must adopt ONLY the new words.
    ctx.handle_provider_audio_delta(vec![0i16; 1600], 16_000, None)
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();

    let mut user_texts: Vec<String> = Vec::new();
    while let Ok(msg) = out_rx.try_recv() {
        if let ServerMessage::Message(pm) = msg
            && pm.role == MessageRole::User
        {
            user_texts.push(pm.text.unwrap_or_default());
        }
    }
    assert_eq!(
        user_texts,
        vec![
            "Open the settings.".to_string(),
            "Which screen did you open?".to_string(),
        ],
        "each utterance must surface once, in speaking order — never merged"
    );
}

/// `OpenAI`'s `input_audio_transcription.completed` routinely lags a fast
/// tool-call turn: deltas arrive early (adopted + flushed by the
/// dispatch), the turn completes (resetting the per-turn flushed flag),
/// and THEN the completed event re-delivers the same transcript. Without
/// the session dedupe memory this surfaced the same user bubble twice
/// under a fresh utterance id (observed live: turns 2+3 with identical
/// transcripts). A genuine repeat after new speech must still flow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_transcription_final_is_not_flushed_twice() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.client_tools.register(navigate_tool(false));
    let reg = ToolRegistry::new();
    let (agent_tx, _agent_rx) = mpsc::channel(8);

    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);

    // Deltas arrive early; the response turn adopts them.
    ctx.handle_provider_input_transcript("Show me the settings page?".into(), false)
        .await
        .unwrap();
    ctx.handle_provider_response_started().await.unwrap();
    // Tool call: the dispatch flushes the user's words (bubble #1).
    ctx.handle_provider_function_call("c1".into(), "navigate".into(), "{}".into(), &reg, &agent_tx)
        .await;
    // The tool-call turn completes (zero audio; resets the flushed flag).
    ctx.handle_provider_response_completed().await.unwrap();

    // The provider's transcription.completed arrives LATE, re-delivering
    // the identical transcript — must be dropped, not re-flushed.
    ctx.handle_provider_input_transcript("Show me the settings page?".into(), true)
        .await
        .unwrap();

    let mut user_texts: Vec<String> = Vec::new();
    while let Ok(msg) = out_rx.try_recv() {
        if let ServerMessage::Message(pm) = msg
            && pm.role == MessageRole::User
        {
            user_texts.push(pm.text.unwrap_or_default());
        }
    }
    assert_eq!(
        user_texts,
        vec!["Show me the settings page?".to_string()],
        "the transcript must surface exactly once"
    );

    // New speech clears the memory: the user genuinely repeating the
    // same sentence must surface again.
    ctx.handle_provider_input_speech_started().await.unwrap();
    ctx.handle_provider_input_transcript("Show me the settings page?".into(), true)
        .await
        .unwrap();
    let mut repeat = Vec::new();
    while let Ok(msg) = out_rx.try_recv() {
        if let ServerMessage::Message(pm) = msg
            && pm.role == MessageRole::User
        {
            repeat.push(pm.text.unwrap_or_default());
        }
    }
    assert_eq!(
        repeat,
        vec!["Show me the settings page?".to_string()],
        "a genuine repeat after new speech must still flow"
    );
}

// ── Post-completion playback tail (barge-in during client playback) ──
//
// S2S providers deliver audio much faster than realtime: `response.done`
// closes the server-side turn while the client still has seconds of
// audio queued. These tests pin that a `speech_started` in that window
// flushes client playback and re-enters the PendingInterruption →
// PlaybackReport flow instead of silently no-oping (the "barge-in does
// nothing with openai-realtime" bug).
