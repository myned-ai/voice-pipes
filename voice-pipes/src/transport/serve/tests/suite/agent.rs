use super::common::*;
use crate::agent::{AgentEvent, Scheduling};
use crate::pipeline::metrics::TurnInitiator;
use crate::pipeline::{TurnState, get_turn_state};
use crate::transport::direct::channel_pair;
use crate::transport::serve::run_session;
use crate::transport::{ClientControl, ClientMessage, PipelineState, ServerControl, ServerMessage};
use crate::transport::{MessageRole, PipelineMessage};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Agent `Say` runs a full turn: Processing state, tracker-started
/// turn, TTS audio with a real utterance id, and a metrics snapshot
/// marked `initiator: agent` with no STT component.
#[tokio::test]
async fn agent_say_turn_runs_full_lifecycle() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.dispatch(AgentEvent::Say {
        text: "Your build finished successfully.".into(),
        scheduling: Scheduling::WhenIdle,
    })
    .await;

    // Turn opened: state machine and tracker both engaged.
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Processing);
    assert!(bed.tracker.current_cancel.is_some());
    let msg = bed.out_rx.recv().await.expect("state change");
    assert!(
        matches!(
            msg,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Processing
            })
        ),
        "expected StateChanged(Processing), got {msg:?}"
    );

    // TTS audio flows with the tracker's utterance id.
    let chunk = tokio::time::timeout(AGENT_TEST_TIMEOUT, bed.proc_audio_rx.recv())
        .await
        .expect("audio timed out")
        .expect("audio channel closed");
    assert_eq!(chunk.utterance_id, bed.tracker.current_utterance_id);

    // Pipeline snapshot: agent-initiated, no STT span, LLM+TTS present.
    let snapshot = tokio::time::timeout(AGENT_TEST_TIMEOUT, bed.metrics_rx.recv())
        .await
        .expect("metrics timed out")
        .expect("metrics channel closed");
    assert_eq!(snapshot.initiator, TurnInitiator::Agent);
    assert!(
        snapshot.components.iter().all(|c| c.kind != "stt"),
        "agent turn must not carry an STT component: {:?}",
        snapshot.components
    );
    assert!(snapshot.components.iter().any(|c| c.kind == "tts"));
    // The cascade engine reports the
    // streaming-phase `output` component whenever a turn produced audio.
    let output = snapshot
        .component("output")
        .expect("cascade turn with audio carries the `output` component");
    assert_eq!(output.kind, "output");
    assert!(output.extras["emit_rtf"].is_number());
    assert!(output.extras["source_rtf"].is_number());
    assert!(output.extras["chain_overhead_ms"].is_number());
    assert!(output.extras["audio_output_ms"].as_u64().unwrap_or(0) > 0);
}

/// While a turn is active, `WhenIdle` speech queues instead of
/// starting a competing turn.
#[tokio::test]
async fn agent_say_queues_while_turn_active() {
    let mut bed = AgentTestBed::new(vec![]);
    // Simulate an active user turn.
    bed.tracker.start_turn(42, CancellationToken::new());
    crate::pipeline::set_turn_state(&bed.turn_state, TurnState::Processing);

    bed.dispatch(AgentEvent::Say {
        text: "queued".into(),
        scheduling: Scheduling::WhenIdle,
    })
    .await;

    assert_eq!(bed.pending_agent.len(), 1, "event must queue while busy");
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Processing);
    assert!(
        bed.out_rx.try_recv().is_err(),
        "no frames may be emitted for a queued event"
    );
}

/// The queue of held-back agent speech is bounded: a flood of `Say`s
/// keeps the newest, drops the oldest, and never grows past the cap.
#[tokio::test]
async fn agent_speech_queue_is_capped_drop_oldest() {
    use crate::agent::MAX_PENDING_AGENT_EVENTS;
    let mut bed = AgentTestBed::new(vec![]);
    bed.tracker.start_turn(42, CancellationToken::new());
    crate::pipeline::set_turn_state(&bed.turn_state, TurnState::Processing);

    for i in 0..100 {
        bed.dispatch(AgentEvent::Say {
            text: format!("say {i}"),
            scheduling: Scheduling::WhenIdle,
        })
        .await;
    }
    assert_eq!(bed.pending_agent.len(), MAX_PENDING_AGENT_EVENTS);
    let texts: Vec<String> = bed
        .pending_agent
        .iter()
        .map(|ev| match ev {
            AgentEvent::Say { text, .. } => text.clone(),
            other => panic!("unexpected queued event {other:?}"),
        })
        .collect();
    assert_eq!(texts.first().map(String::as_str), Some("say 84"));
    assert_eq!(texts.last().map(String::as_str), Some("say 99"));

    // An interrupt re-queues at the head and still respects the cap.
    bed.dispatch(AgentEvent::Say {
        text: "urgent".into(),
        scheduling: Scheduling::Interrupt,
    })
    .await;
    assert!(bed.pending_agent.len() <= MAX_PENDING_AGENT_EVENTS);
    assert!(matches!(
        bed.pending_agent.front(),
        Some(AgentEvent::Say { text, .. }) if text == "urgent"
    ));
}

/// `Message` forwards to the client with no turn, no state change,
/// no audio; `record_in_history` reaches the processors as context.
#[tokio::test]
async fn agent_message_forwards_without_turn() {
    let injected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let appended = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut bed = AgentTestBed::new(vec![Box::new(CaptureProcessor {
        injected: Arc::clone(&injected),
        appended: Arc::clone(&appended),
    })]);

    let mut message = PipelineMessage::text("Order shipped.", MessageRole::Assistant);
    message.kind = "card".to_string();
    message.payload = Some(serde_json::json!({ "order_id": "o-1" }));
    bed.dispatch(AgentEvent::Message {
        message,
        record_in_history: true,
    })
    .await;

    let msg = bed.out_rx.recv().await.expect("client message");
    match msg {
        ServerMessage::Message(pm) => {
            assert_eq!(pm.kind, "card");
            assert_eq!(pm.text.as_deref(), Some("Order shipped."));
        }
        other => panic!("expected Message, got {other:?}"),
    }
    assert!(
        bed.out_rx.try_recv().is_err(),
        "no StateChanged/audio may accompany an out-of-turn message"
    );
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Listening);
    assert!(bed.tracker.current_cancel.is_none());
    let injected = injected.lock().unwrap();
    assert_eq!(injected.len(), 1);
    assert!(injected[0].contains("Order shipped."));
    assert!(appended.lock().unwrap().is_empty());
}

/// Silent scheduling records context only — nothing is spoken and
/// nothing reaches the client.
#[tokio::test]
async fn agent_silent_records_context_without_speaking() {
    let injected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let appended = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut bed = AgentTestBed::new(vec![Box::new(CaptureProcessor {
        injected: Arc::clone(&injected),
        appended: Arc::clone(&appended),
    })]);

    bed.dispatch(AgentEvent::CompleteTool {
        call_id: "c1".into(),
        result: "cache warmed".into(),
        scheduling: Scheduling::Silent,
    })
    .await;

    assert!(
        bed.out_rx.try_recv().is_err(),
        "Silent must not emit frames"
    );
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Listening);
    let injected = injected.lock().unwrap();
    assert_eq!(injected.len(), 1);
    assert!(injected[0].contains("cache warmed"));
}

/// Agent `Say` records the spoken text as an assistant message on
/// history-keeping processors at generation time.
#[tokio::test]
async fn agent_say_appends_assistant_history() {
    let injected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let appended = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut bed = AgentTestBed::new(vec![Box::new(CaptureProcessor {
        injected: Arc::clone(&injected),
        appended: Arc::clone(&appended),
    })]);

    bed.dispatch(AgentEvent::Say {
        text: "Reminder: your meeting starts in five minutes.".into(),
        scheduling: Scheduling::WhenIdle,
    })
    .await;

    let appended = appended.lock().unwrap();
    assert_eq!(appended.len(), 1);
    assert!(appended[0].contains("meeting starts"));
    assert!(injected.lock().unwrap().is_empty());
}

// ── Idle re-engagement: full-session agent-turn lifecycle ──
// These drive the ENTIRE loop path (idle countdown arm → agent
// channel arm → agent turn → wire frames), i.e. the deferred
// Full-session tests, with the idle timer as the
// in-session producer.

/// With an idle timeout configured and zero user input, the session
/// must speak a full agent-initiated turn: `StateChanged(Processing)`,
/// audio, and a `TurnMetrics` snapshot marked `initiator: agent`.
#[tokio::test]
async fn idle_prompt_runs_full_agent_turn_over_the_wire() {
    let state = test_state_full(true, true, Some(idle_settings(150, 1)));
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    // Send NO user input. Drain frames until TurnMetrics arrives.
    let mut saw_processing = false;
    let mut saw_audio = false;
    let mut found_metrics = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(10) {
        match next_idle_frame(&client, std::time::Duration::from_secs(3)).await {
            IdleFrame::Processing => saw_processing = true,
            IdleFrame::Audio => saw_audio = true,
            IdleFrame::Metrics(snapshot) => {
                assert_eq!(
                    snapshot.initiator,
                    crate::pipeline::metrics::TurnInitiator::Agent,
                    "idle prompt turn must be agent-initiated"
                );
                assert!(
                    snapshot.components.iter().all(|c| c.kind != "stt"),
                    "agent turn must carry no STT component"
                );
                found_metrics = true;
                break;
            }
            IdleFrame::Other => {}
            IdleFrame::Timeout | IdleFrame::Closed => break,
        }
    }
    assert!(saw_processing, "idle prompt never opened a turn");
    assert!(saw_audio, "idle prompt produced no audio");
    assert!(found_metrics, "idle prompt turn emitted no TurnMetrics");

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

/// Regression: the idle countdown must re-arm after a NORMALLY
/// completed turn. `current_cancel` used to keep holding the
/// finished turn's token until the next turn started, so every
/// "no active turn" check stayed false after the first turn and
/// idle prompts only ever fired on a virgin session.
#[tokio::test]
async fn idle_prompt_fires_after_a_completed_turn() {
    let state = test_state_full(true, true, Some(idle_settings(200, 1)));
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    // Drive one normal (user) text turn to completion.
    client
        .send_client(ClientMessage::Message(PipelineMessage::text(
            "hello there",
            MessageRole::User,
        )))
        .await
        .unwrap();
    let mut utterance_id = 0u32;
    let start = std::time::Instant::now();
    let mut user_turn_done = false;
    while start.elapsed() < std::time::Duration::from_secs(10) {
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.recv_server()).await {
            Ok(Ok(Some(ServerMessage::Audio(c)))) => {
                if c.utterance_id != 0 {
                    utterance_id = c.utterance_id;
                }
            }
            Ok(Ok(Some(ServerMessage::Control(ServerControl::TurnMetrics { snapshot })))) => {
                assert_eq!(
                    snapshot.initiator,
                    crate::pipeline::metrics::TurnInitiator::User,
                    "first turn is user-initiated"
                );
                user_turn_done = true;
                break;
            }
            Ok(Ok(Some(_))) => {}
            _ => break,
        }
    }
    assert!(user_turn_done, "user turn never completed");

    // Finalize playback so the session returns to Listening.
    client
        .send_client(ClientMessage::Control(ClientControl::PlaybackComplete {
            utterance_id,
        }))
        .await
        .unwrap();

    // Now go quiet: the idle prompt must fire as an agent turn.
    let mut idle_fired = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(10) {
        match next_idle_frame(&client, std::time::Duration::from_secs(3)).await {
            IdleFrame::Metrics(snapshot) => {
                assert_eq!(
                    snapshot.initiator,
                    crate::pipeline::metrics::TurnInitiator::Agent,
                    "post-turn idle prompt must be agent-initiated"
                );
                idle_fired = true;
                break;
            }
            IdleFrame::Timeout | IdleFrame::Closed => break,
            _ => {}
        }
    }
    assert!(
        idle_fired,
        "idle prompt never fired after a completed turn (current_cancel held the dead token?)"
    );

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

/// The prompt budget caps per silence stretch: with `max_prompts` = 1,
/// exactly one agent turn fires and then the session stays quiet.
#[tokio::test]
async fn idle_prompts_are_capped_per_silence_stretch() {
    let state = test_state_full(true, true, Some(idle_settings(120, 1)));
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    // First prompt: drain until its TurnMetrics.
    let start = std::time::Instant::now();
    let mut prompt_fired = false;
    while start.elapsed() < std::time::Duration::from_secs(8) {
        match next_idle_frame(&client, std::time::Duration::from_secs(2)).await {
            IdleFrame::Metrics(_) => {
                prompt_fired = true;
                break;
            }
            IdleFrame::Timeout | IdleFrame::Closed => break,
            _ => {}
        }
    }
    assert!(prompt_fired, "first idle prompt should fire");

    // Then: several timeout windows of silence must produce no
    // second turn (budget exhausted, no user speech to reset it).
    let quiet_deadline = std::time::Instant::now() + std::time::Duration::from_millis(700);
    while std::time::Instant::now() < quiet_deadline {
        match next_idle_frame(&client, std::time::Duration::from_millis(200)).await {
            IdleFrame::Processing => panic!("second idle prompt fired past the cap"),
            IdleFrame::Closed => break,
            _ => {}
        }
    }

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}

/// A session processor whose agent never answers an idle prompt: it
/// registers an idle handler that waits forever, and echoes turns.
struct StallingIdleProcessor;

struct NeverAnswers;

#[async_trait::async_trait]
impl crate::agent::UserIdleHandler for NeverAnswers {
    async fn on_user_idle(&self, _idle_ms: u64, _prompt_count: u32) -> bool {
        std::future::pending().await
    }
}

#[async_trait::async_trait]
impl crate::text::TextProcessor for StallingIdleProcessor {
    fn name(&self) -> &'static str {
        "idle-stall-test"
    }
    async fn process(&self, input: &str) -> Result<String, crate::text::TextProcessorError> {
        Ok(input.to_string())
    }
    async fn process_stream(
        &self,
        input: &str,
        tx: tokio::sync::mpsc::Sender<crate::text::TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), crate::text::TextProcessorError> {
        let _ = tx
            .send(crate::text::TextFragment::new(input).with_is_end_of_response(true))
            .await;
        Ok(())
    }
    fn bind_session(&self, session: voice_pipes_sdk::agent::SessionHandle) {
        if let Some(handle) = crate::agent::AgentSessionHandle::from_session(&session) {
            handle.register_idle_handler(Arc::new(NeverAnswers));
        }
    }
}

struct StallingIdleFactory;

impl crate::text::TextProcessorFactory for StallingIdleFactory {
    fn name(&self) -> &'static str {
        "idle-stall-test"
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
        Ok(Box::new(StallingIdleProcessor))
    }
}

/// The regression: the session loop awaited the idle handler, and an agent
/// that never answered froze the session. Now the configured message is
/// spoken once the bound passes, and the session keeps serving turns.
#[tokio::test]
async fn an_idle_handler_that_never_answers_does_not_freeze_the_session() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::text::register_text_processor_factory(|| Box::new(StallingIdleFactory))
            .expect("test name is free");
    });
    let state = test_state_customised(true, true, Some(idle_settings(150, 1)), |s| {
        Arc::make_mut(&mut s.config).text_processor =
            vec![crate::config::TextProcessorConfig::new("idle-stall-test")];
    });
    let (client, server) = channel_pair();
    let state2 = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    handshake(&client).await;

    // The idle prompt fires, the agent never answers, and the configured
    // message is spoken as an agent turn.
    let mut agent_turn = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(10) {
        match next_idle_frame(&client, std::time::Duration::from_secs(3)).await {
            IdleFrame::Metrics(snapshot) => {
                assert_eq!(snapshot.initiator, TurnInitiator::Agent);
                agent_turn = true;
                break;
            }
            IdleFrame::Timeout | IdleFrame::Closed => break,
            _ => {}
        }
    }
    assert!(agent_turn, "the configured idle message was never spoken");

    // The session still answers the user.
    client
        .send_client(ClientMessage::Message(PipelineMessage::text(
            "are you there",
            MessageRole::User,
        )))
        .await
        .unwrap();
    let mut user_turn = false;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(10) {
        match next_idle_frame(&client, std::time::Duration::from_secs(3)).await {
            IdleFrame::Metrics(snapshot) if snapshot.initiator == TurnInitiator::User => {
                user_turn = true;
                break;
            }
            IdleFrame::Timeout | IdleFrame::Closed => break,
            _ => {}
        }
    }
    assert!(user_turn, "the session stopped serving turns");

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
}
