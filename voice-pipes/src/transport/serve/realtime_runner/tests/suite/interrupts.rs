use super::super::turn::TerminalVerdict;
use super::super::turn::{AnchorSource, ResponseTracker, SuppressGate};
use super::common::*;
use crate::agent::{AgentEvent, Scheduling};
use crate::pipeline::TurnMetricsSnapshot;
use crate::realtime::tests::mock::{MockRealtimeProtocol, MockServer};
use crate::realtime::{RealtimeClient, RealtimeProtocol};
use crate::transport::ServerMessage;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

/// After the client cancelled and acknowledged the flush, the provider's
/// late `ResponseCancelled` finds no turn to flush and must leave the
/// audio gate alone. Re-arming it there used to mute every later
/// response: nothing was sent that the client would ever acknowledge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_provider_cancel_does_not_rearm_suppression() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();

    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_provider_response_started().await.unwrap();
        ctx.handle_provider_audio_delta(vec![0; 1600], 16_000, None)
            .await
            .unwrap();
        ctx.handle_client_cancel().await.unwrap();
    };
    assert_eq!(drain_clear_audio(&mut out_rx), vec![1]);
    assert!(suppress.is_suppressed());
    assert_eq!(suppress.armed_for(), Some(1));

    // The client acknowledges the flush.
    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_client_playback_complete(1).unwrap();
    };
    assert!(
        !suppress.is_suppressed(),
        "acknowledged flush releases the gate"
    );

    // The provider's cancel acknowledgement arrives afterwards.
    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_provider_response_cancelled().await.unwrap();
    };
    assert!(
        drain_clear_audio(&mut out_rx).is_empty(),
        "nothing to flush, so no ClearAudio"
    );
    assert!(
        !suppress.is_suppressed(),
        "a cancel with no turn to flush must not arm the gate"
    );

    // The next response plays.
    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_provider_response_started().await.unwrap();
        ctx.handle_provider_audio_delta(vec![0; 1600], 16_000, None)
            .await
            .unwrap();
    };
    assert!(!suppress.is_suppressed());
    let audio_frames = std::iter::from_fn(|| out_rx.try_recv().ok())
        .filter(|m| matches!(m, ServerMessage::Audio(c) if !c.samples.is_empty()))
        .count();
    assert_eq!(audio_frames, 1, "the new response's audio is forwarded");
}

/// A stale `PlaybackComplete` (for an older utterance than the one the
/// gate is armed for) must not release it; the matching or a later one
/// does. A new response releases it unconditionally.
#[test]
fn suppress_gate_release_rules() {
    let gate = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    gate.arm(7);
    assert!(gate.is_suppressed());
    assert!(!gate.release_for(6), "older acknowledgement is stale");
    assert!(gate.is_suppressed());
    assert!(gate.release_for(7));
    assert!(!gate.is_suppressed());

    gate.arm(8);
    assert!(gate.release_for(9), "a later acknowledgement covers it");

    gate.arm(10);
    gate.release();
    assert!(!gate.is_suppressed());
    assert_eq!(gate.armed_for(), None);
}

/// An agent `Interrupt` cancels the active response and requests a
/// replacement in one go. The cancelled response's terminal event still
/// arrives; it must not read as "the provider is idle", or the queued
/// agent work would fire a second `response.create` into the replacement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_interrupt_ignores_the_superseded_responses_terminal_event() {
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

    let mut responses = ResponseTracker::default();
    responses.on_started(Some("r1".into()));
    let mut kick_pending = false;
    let mut queue: VecDeque<AgentEvent> = VecDeque::new();
    queue.push_back(AgentEvent::Say {
        text: "queued for later".into(),
        scheduling: Scheduling::WhenIdle,
    });

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
    wait_for_env(&server, "cancel").await;
    wait_for_env(&server, "response_create").await;
    assert!(responses.is_active(), "the replacement is in flight");

    // The provider acknowledges the cancel of the OLD response.
    assert_eq!(
        responses.on_terminal(Some("r1")),
        super::super::turn::TerminalVerdict::Superseded
    );
    assert!(responses.is_active(), "the replacement is still running");
    // This is the runner loop's rule: queued work drains only when idle.
    if !responses.is_active() {
        let ev = queue.pop_front().unwrap();
        let _ = ctx
            .handle_agent_event(ev, &mut responses, &mut kick_pending, &mut queue)
            .await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let creates = server
        .recorded()
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("response_create"))
        .count();
    assert_eq!(creates, 1, "exactly one response.create for the interrupt");
    assert_eq!(
        queue.len(),
        1,
        "the WhenIdle event waits for the replacement"
    );

    // The replacement then runs its course.
    responses.on_started(Some("r2".into()));
    assert_eq!(
        responses.on_terminal(Some("r2")),
        super::super::turn::TerminalVerdict::Current
    );
    assert!(!responses.is_active());
}

#[test]
fn response_tracker_classifies_terminal_events() {
    // Ids: a terminal event for a response other than the active one is
    // stale, the active one's is current.
    let mut t = ResponseTracker::default();
    t.on_started(Some("a".into()));
    assert_eq!(t.on_terminal(Some("zzz")), TerminalVerdict::Stale);
    assert!(t.is_active());
    assert_eq!(t.on_terminal(Some("a")), TerminalVerdict::Current);
    assert!(!t.is_active());

    // Interrupt with ids: the superseded id is consumed once.
    let mut t = ResponseTracker::default();
    t.on_started(Some("a".into()));
    t.note_superseded();
    t.on_created();
    assert_eq!(t.on_terminal(Some("a")), TerminalVerdict::Superseded);
    assert!(t.is_active());
    t.on_started(Some("b".into()));
    assert_eq!(t.on_terminal(Some("a")), TerminalVerdict::Stale);
    assert_eq!(t.on_terminal(Some("b")), TerminalVerdict::Current);

    // Interrupt without ids: the next terminal event is the superseded
    // response's, the one after that ends the replacement.
    let mut t = ResponseTracker::default();
    t.on_created();
    t.note_superseded();
    t.on_created();
    assert_eq!(t.on_terminal(None), TerminalVerdict::Superseded);
    assert!(t.is_active());
    assert_eq!(t.on_terminal(None), TerminalVerdict::Current);
    assert!(!t.is_active());

    // Nothing active: an interrupt supersedes nothing.
    let mut t = ResponseTracker::default();
    t.note_superseded();
    t.on_created();
    assert_eq!(t.on_terminal(None), TerminalVerdict::Current);

    // Inferred completion and reconnect clear everything.
    let mut t = ResponseTracker::default();
    t.on_started(Some("a".into()));
    t.note_superseded();
    t.finish_inferred();
    assert!(!t.is_active());
    assert_eq!(t.on_terminal(Some("a")), TerminalVerdict::Current);
}

/// The record of a client-cancelled turn carries that turn's own user
/// words, not whatever the session-level pending buffer happens to hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_cancel_persists_the_interrupted_turns_own_transcript() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let persisted: Arc<std::sync::Mutex<Vec<Option<String>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&persisted);
    let persist = move |_: TurnMetricsSnapshot, user: Option<String>| {
        sink.lock().unwrap().push(user);
    };

    macro_rules! ctx_with_persist {
        () => {
            super::super::context::RealtimeSessionContext {
                session_id: "test",
                provider_label: "mock",
                provider_model: &None,
                client: &mut client,
                out_tx: &out_tx,
                post_processors: &[],
                observations: crate::transport::adaptive::ObservationSink::disconnected(),
                annotators: crate::pipeline::AnnotatorHubs::none(),
                session_recorder: None,
                suppress_audio: &suppress,
                streaming_audit: &None,
                pe_tx: &pe_tx,
                client_sample_rate: 16_000,
                output_sample_rate: 16_000,
                audio_chunks_uplinked: &mut s.chunks_uplinked,
                audio_chunks_downlinked: &mut s.chunks_downlinked,
                next_utterance_id: &mut s.next_utt,
                current_turn: &mut s.current_turn,
                input_audio_ms_acc: &mut s.input_audio_ms,
                pending_interruption: &mut s.pending_interruption,
                playback_tail: &mut s.playback_tail,
                pending_input_audio: &mut s.pending_audio,
                pending_input_rate: &mut s.pending_rate,
                pending_user_transcript: &mut s.pending_transcript,
                pending_user_transcript_flushed: &mut s.pending_transcript_flushed,
                ignoring_stray_deltas: &mut s.ignoring_stray,
                session_audio_emitted_ms: &mut s.session_audio_ms,
                request_anchor: &mut s.request_anchor,
                endpoint_observer: &mut s.endpoint_observer,
                vp_speech_end: &mut s.vp_speech_end,
                persist_turn: &persist,
                pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(
                    parking_lot::Mutex::new(None),
                ))),
                client_tools: &s.client_tools,
                client_calls: &mut s.client_calls,
                phantom_transcript_guard: &mut s.phantom_transcript_guard,
                last_flushed_user_transcript: &mut s.last_flushed_user_transcript,
            }
        };
    }

    {
        let mut ctx = ctx_with_persist!();
        ctx.handle_provider_input_transcript("book a flight".into(), true)
            .await
            .unwrap();
        ctx.handle_provider_response_started().await.unwrap();
        ctx.handle_provider_audio_delta(vec![0; 1600], 16_000, None)
            .await
            .unwrap();
    };
    assert_eq!(
        s.current_turn
            .as_ref()
            .and_then(|t| t.user_transcript.as_deref()),
        Some("book a flight")
    );
    // Something unrelated lands in the session buffer meanwhile.
    s.pending_transcript = Some("stale words".into());
    {
        let mut ctx = ctx_with_persist!();
        ctx.handle_client_cancel().await.unwrap();
    };
    assert_eq!(
        persisted.lock().unwrap().as_slice(),
        &[Some("book a flight".to_string())]
    );
}

/// Anchor precedence: a transcript-proxy anchor only ever replaces a
/// weaker one. The provider's own speech-stopped instant survives the
/// input-transcript deltas that follow it; the client's speech-end is
/// upgraded by the proxy; a later proxy replaces an earlier proxy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcript_proxy_anchor_does_not_replace_a_better_anchor() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();

    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.note_user_speech_end();
        ctx.handle_provider_input_transcript("book".into(), false)
            .await
            .unwrap();
        ctx.handle_provider_input_transcript("book a flight".into(), true)
            .await
            .unwrap();
    };
    assert_eq!(
        s.request_anchor.map(|a| a.source),
        Some(AnchorSource::ProviderSpeechStopped)
    );

    s.request_anchor = None;
    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_client_timing_report(0, crate::transport::TimingEventKind::SpeechEnd, 1);
        assert_eq!(
            ctx.request_anchor.map(|a| a.source),
            Some(AnchorSource::ClientSpeechEnd)
        );
        ctx.handle_provider_input_transcript("hello".into(), false)
            .await
            .unwrap();
        let first = ctx.request_anchor.expect("proxy anchor");
        assert_eq!(first.source, AnchorSource::InputTranscriptProxy);
        tokio::time::sleep(Duration::from_millis(5)).await;
        ctx.handle_provider_input_transcript(" there".into(), false)
            .await
            .unwrap();
        let second = ctx.request_anchor.expect("proxy anchor");
        assert!(
            second.instant > first.instant,
            "a later transcript delta moves the proxy anchor"
        );
    }
}
