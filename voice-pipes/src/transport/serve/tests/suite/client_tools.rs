use super::common::*;
use crate::agent::{AgentEvent, Scheduling};
use crate::pipeline::metrics::TurnInitiator;
use crate::pipeline::{TurnState, get_turn_state};
use crate::transport::ClientToolPhase;
use crate::transport::{PipelineState, ServerControl, ServerMessage};
use std::sync::Arc;

fn navigate_tool(speak_on_result: bool) -> crate::agent::client_tools::ClientToolConfig {
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

/// An allowlisted `ClientToolCall` routes a `trigger_action` command
/// to the client with no turn, no TTS, and no state change; the call
/// registers for result correlation.
#[tokio::test]
async fn client_tool_call_routes_trigger_action_without_turn() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.client_tools.register(navigate_tool(false));

    bed.dispatch(AgentEvent::ClientToolCall {
        call_id: "call-1".into(),
        name: "navigate".into(),
        arguments: serde_json::json!({"screen": "settings"}),
    })
    .await;

    // The command went out — and nothing else did.
    let msg = bed.out_rx.try_recv().expect("trigger_action forwarded");
    match msg {
        ServerMessage::Message(m) => {
            assert_eq!(m.kind, "trigger_action");
            let payload = m.payload.expect("payload");
            assert_eq!(payload["call_id"], "call-1");
            assert_eq!(payload["name"], "navigate");
            assert_eq!(payload["arguments"]["screen"], "settings");
        }
        other => panic!("expected Message, got {other:?}"),
    }
    assert!(
        bed.out_rx.try_recv().is_err(),
        "no StateChanged/audio may accompany a client tool command"
    );
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Listening);
    assert!(bed.tracker.current_cancel.is_none(), "no turn opened");
    assert_eq!(bed.client_calls.len(), 1, "call registered");
}

/// A call for a tool that is not on the allowlist is rejected: no
/// command to the client, a silent context note so the model learns
/// the action did not happen.
#[tokio::test]
async fn client_tool_call_unknown_tool_injects_failure_note() {
    let injected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut bed = AgentTestBed::new(vec![Box::new(CaptureProcessor {
        injected: Arc::clone(&injected),
        appended: Arc::new(std::sync::Mutex::new(Vec::new())),
    })]);

    bed.dispatch(AgentEvent::ClientToolCall {
        call_id: "call-x".into(),
        name: "not_declared".into(),
        arguments: serde_json::json!({}),
    })
    .await;

    assert!(bed.out_rx.try_recv().is_err(), "no command may be sent");
    assert!(bed.client_calls.is_empty());
    let notes = injected.lock().unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("not_declared"));
    assert!(notes[0].contains("no such client-side tool"));
}

/// Completed result, silent default: fenced context injected into
/// the processors, no turn, no speech; a duplicate Completed is a
/// no-op.
#[tokio::test]
async fn client_tool_result_silent_injects_context_only() {
    let injected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut bed = AgentTestBed::new(vec![Box::new(CaptureProcessor {
        injected: Arc::clone(&injected),
        appended: Arc::new(std::sync::Mutex::new(Vec::new())),
    })]);
    bed.client_tools.register(navigate_tool(false));
    bed.dispatch(AgentEvent::ClientToolCall {
        call_id: "call-1".into(),
        name: "navigate".into(),
        arguments: serde_json::json!({"screen": "settings"}),
    })
    .await;
    let _ = bed.out_rx.try_recv(); // drain the trigger_action

    let ev = bed
        .resolve(
            "call-1",
            ClientToolPhase::Completed,
            Some(&serde_json::json!({"ok": true})),
            false,
        )
        .expect("completed resolves to an event");
    bed.dispatch(ev).await;

    let notes = injected.lock().unwrap().clone();
    assert_eq!(notes.len(), 1, "one silent context injection");
    assert!(notes[0].contains("navigate"));
    assert!(notes[0].contains("\"ok\":true"));
    assert!(notes[0].contains("not instructions"), "result is fenced");
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Listening);
    assert!(
        bed.out_rx.try_recv().is_err(),
        "silent result emits nothing"
    );

    // Duplicate Completed and unknown ids are dropped.
    assert!(
        bed.resolve("call-1", ClientToolPhase::Completed, None, false)
            .is_none()
    );
    assert!(
        bed.resolve("nope", ClientToolPhase::Completed, None, false)
            .is_none()
    );
}

/// Interim phases (Started/Progress) are telemetry only: the call
/// stays in flight and nothing is injected.
#[tokio::test]
async fn client_tool_result_progress_is_telemetry_only() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.client_tools.register(navigate_tool(false));
    bed.dispatch(AgentEvent::ClientToolCall {
        call_id: "call-1".into(),
        name: "navigate".into(),
        arguments: serde_json::json!({}),
    })
    .await;
    let _ = bed.out_rx.try_recv();

    assert!(
        bed.resolve("call-1", ClientToolPhase::Started, None, false)
            .is_none()
    );
    assert!(
        bed.resolve("call-1", ClientToolPhase::Progress, None, false)
            .is_none()
    );
    assert_eq!(bed.client_calls.len(), 1, "call still in flight");
}

/// `speak_on_result = true` → the completion runs a full generated
/// confirmation turn (agent-initiated lifecycle).
#[tokio::test]
async fn client_tool_result_speak_runs_confirmation_turn() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.client_tools.register(navigate_tool(true));
    bed.dispatch(AgentEvent::ClientToolCall {
        call_id: "call-1".into(),
        name: "navigate".into(),
        arguments: serde_json::json!({}),
    })
    .await;
    let _ = bed.out_rx.try_recv();

    let ev = bed
        .resolve("call-1", ClientToolPhase::Completed, None, false)
        .expect("event");
    match &ev {
        AgentEvent::Generate { scheduling, .. } => {
            assert_eq!(*scheduling, Scheduling::WhenIdle);
        }
        other => panic!("expected Generate, got {other:?}"),
    }
    bed.dispatch(ev).await;

    // Full agent-turn lifecycle: Processing state + tracked turn.
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Processing);
    assert!(bed.tracker.current_cancel.is_some());
    let msg = bed.out_rx.recv().await.expect("state change");
    assert!(matches!(
        msg,
        ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Processing
        })
    ));
    let snapshot = tokio::time::timeout(AGENT_TEST_TIMEOUT, bed.metrics_rx.recv())
        .await
        .expect("metrics timed out")
        .expect("metrics channel closed");
    assert_eq!(snapshot.initiator, TurnInitiator::Agent);
}

/// A session LLM as a plugin would write it: asked to open the settings
/// screen it raises a `navigate` client-tool call through its session
/// handle and says nothing; asked anything else (the confirmation turn,
/// generated from the injected result) it answers in prose. It counts its
/// calls so the test can see the confirmation turn reach it.
struct WireStubLlm {
    session: parking_lot::Mutex<Option<voice_pipes_sdk::agent::SessionHandle>>,
}

static WIRE_STUB_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[async_trait::async_trait]
impl crate::text::TextProcessor for WireStubLlm {
    fn name(&self) -> &'static str {
        "wire-stub-llm"
    }
    async fn process(&self, input: &str) -> Result<String, crate::text::TextProcessorError> {
        Ok(input.to_string())
    }
    async fn process_stream(
        &self,
        input: &str,
        tx: tokio::sync::mpsc::Sender<crate::text::TextFragment>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), crate::text::TextProcessorError> {
        WIRE_STUB_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if input.contains("open the settings screen") {
            let session = self
                .session
                .lock()
                .clone()
                .expect("bound before the first turn");
            session
                .client_tool_call(voice_pipes_sdk::agent::ClientToolCall::new(
                    "call-wire-1",
                    "navigate",
                    serde_json::json!({ "screen": "settings" }),
                ))
                .await
                .expect("session live");
            let _ = tx
                .send(crate::text::TextFragment::new("").with_is_end_of_response(true))
                .await;
        } else {
            let _ = tx
                .send(
                    crate::text::TextFragment::new("Done, you are on the settings screen.")
                        .with_is_end_of_response(true),
                )
                .await;
        }
        Ok(())
    }
    fn bind_session(&self, session: voice_pipes_sdk::agent::SessionHandle) {
        *self.session.lock() = Some(session);
    }
}

struct WireStubLlmFactory;

impl crate::text::TextProcessorFactory for WireStubLlmFactory {
    fn name(&self) -> &'static str {
        "wire-stub-llm"
    }
    fn validate(
        &self,
        _cfg: &crate::config::TextProcessorConfig,
    ) -> Vec<crate::config::ConfigError> {
        vec![]
    }
    fn build(
        &self,
        _cfg: &crate::config::TextProcessorConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn crate::text::TextProcessor>, voice_pipes_sdk::BuildError> {
        Ok(Box::new(WireStubLlm {
            session: parking_lot::Mutex::new(None),
        }))
    }
}

/// Full-session cascade wire test (the plan's acceptance flow, over a real
/// session loop, with a session LLM registered as a plugin):
/// typed input → the LLM reports a client-tool call and says nothing
/// → the client receives `trigger_action` with NO speech → its
/// `ClientToolResult { speak: true }` produces a spoken
/// confirmation turn generated from the injected result context.
#[tokio::test]
async fn cascade_client_tool_round_trip_over_the_wire() {
    use crate::transport::direct::channel_pair;
    use crate::transport::serve::{TransportState, run_session};
    use crate::transport::{ClientControl, ClientMessage, MessageRole, PipelineMessage};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::text::register_text_processor_factory(|| Box::new(WireStubLlmFactory))
            .expect("test name is free");
    });
    let calls_before = WIRE_STUB_CALLS.load(std::sync::atomic::Ordering::SeqCst);

    // A state whose session LLM is the stub, with `navigate` declared as
    // a client tool.
    let base = test_state();
    let mut config = (*base.config).clone();
    config.text_processor = vec![crate::config::TextProcessorConfig::new("wire-stub-llm")];
    let state = Arc::new(TransportState {
        config: Arc::new(config),
        stt: base.stt.clone(),
        tts: base.tts.clone(),
        processors: Arc::clone(&base.processors),
        audio_post_processors: Arc::clone(&base.audio_post_processors),
        annotators: Arc::clone(&base.annotators),
        active_sessions: Arc::new(AtomicUsize::new(0)),
        max_sessions: base.max_sessions,
        ready: AtomicBool::new(true),
        shutdown: tokio_util::sync::CancellationToken::new(),
        turn_complete_tx: None,
        session_event_tx: None,
        session_store: Arc::clone(&base.session_store),
        pipeline_event_tx: base.pipeline_event_tx.clone(),
        param_registry: Arc::clone(&base.param_registry),
        component_registry: Arc::clone(&base.component_registry),
        client_tools: Arc::new(vec![navigate_tool(false)]),
    });

    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    // Typed text input triggers the LLM turn.
    client
        .send_client(ClientMessage::Message(PipelineMessage::text(
            "open the settings screen",
            MessageRole::User,
        )))
        .await
        .unwrap();

    // The command arrives on the socket, silently: no Speaking state
    // may precede it — the tool-call turn is prose-free.
    // Like any real client, ack the (empty) end-of-utterance frame
    // with PlaybackComplete so the turn finishes Draining → Listening.
    let deadline = std::time::Duration::from_secs(10);
    let mut seen: Vec<String> = Vec::new();
    let mut trigger: Option<PipelineMessage> = None;
    let mut eou_utterance: Option<u32> = None;
    while trigger.is_none() || eou_utterance.is_none() {
        let msg = tokio::time::timeout(deadline, client.recv_server())
            .await
            .expect("timed out waiting for trigger_action + EOU")
            .expect("recv failed")
            .expect("stream ended");
        match msg {
            ServerMessage::Message(m) if m.kind == "trigger_action" => trigger = Some(m),
            ServerMessage::Audio(c) => {
                assert!(
                    c.samples.is_empty(),
                    "client tool turn must not produce audio"
                );
                if c.is_end_of_utterance {
                    eou_utterance = Some(c.utterance_id);
                }
            }
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Speaking,
            }) => panic!("client tool turn must not speak; saw: {seen:?}"),
            other => seen.push(format!("{other:?}")),
        }
    }
    let payload = trigger.unwrap().payload.expect("payload");
    assert_eq!(payload["call_id"], "call-wire-1");
    assert_eq!(payload["name"], "navigate");
    assert_eq!(payload["arguments"]["screen"], "settings");
    client
        .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
            utterance_id: eou_utterance.unwrap(),
        }))
        .await
        .unwrap();

    // The client completes the call and asks for a spoken
    // confirmation ("client decides" affordance): a full generated
    // turn must follow — the LLM is called again with the result
    // context and its prose is synthesized.
    client
        .send_client(ClientMessage::Control(ClientControl::ClientToolResult {
            call_id: "call-wire-1".into(),
            phase: ClientToolPhase::Completed,
            result: Some(serde_json::json!({"screen": "settings"})),
            speak: true,
        }))
        .await
        .unwrap();

    let mut saw_speaking = false;
    let mut saw_audio = false;
    let start = std::time::Instant::now();
    while start.elapsed() < deadline && !(saw_speaking && saw_audio) {
        let msg = tokio::time::timeout(deadline, client.recv_server())
            .await
            .expect("timed out waiting for spoken confirmation")
            .expect("recv failed")
            .expect("stream ended");
        match msg {
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Speaking,
            }) => saw_speaking = true,
            ServerMessage::Audio(c) if !c.samples.is_empty() => saw_audio = true,
            _ => {}
        }
    }
    assert!(saw_speaking, "confirmation turn must reach Speaking");
    assert!(saw_audio, "confirmation turn must produce audio");
    assert!(
        WIRE_STUB_CALLS.load(std::sync::atomic::Ordering::SeqCst) >= calls_before + 2,
        "the confirmation turn must have called the LLM again"
    );

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}
