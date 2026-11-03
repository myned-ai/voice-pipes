use super::common::*;
use crate::agent::{AgentEvent, Scheduling};
use crate::pipeline::{TurnMetricsSnapshot, TurnState, get_turn_state, new_turn_state};
use crate::transport::serve::runner::context::ClassicSessionContext;
use crate::transport::{PipelineState, ServerControl, ServerMessage};
use std::sync::Arc;
use tokio::sync::mpsc;

/// The user talks over the tail the client is still playing, after
/// the server finished the turn and reported its metrics. Only that
/// tail is cut: no second `TurnMetrics`, the session aggregate keeps
/// one turn, and the handler returns at once instead of waiting out
/// the metrics grace period on an empty channel.
#[tokio::test]
async fn late_interrupt_after_turn_end_emits_no_second_metrics() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.fake_completed_turn(1);
    assert_eq!(bed.tracker.session_aggregator.turns, 1);

    let started = std::time::Instant::now();
    bed.barge_in().await;
    assert!(
        started.elapsed() < std::time::Duration::from_millis(50),
        "late barge-in must not wait for metrics: {:?}",
        started.elapsed()
    );
    assert_eq!(bed.control_trace(), vec!["clear_audio:server_vad"]);
    assert_eq!(bed.tracker.session_aggregator.turns, 1);
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Draining);
    assert!(bed.tracker.current_cancel.is_none());

    // A client Cancel arriving on top of that has nothing left to
    // interrupt either.
    bed.cancel().await;
    assert_eq!(bed.control_trace(), vec!["clear_audio:client_cancel"]);
    assert_eq!(bed.tracker.session_aggregator.turns, 1);

    // The client's acknowledgement still closes the turn normally.
    bed.playback_complete(1).await;
    assert_eq!(bed.control_trace(), vec!["state_changed:listening"]);
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Listening);
}

/// A client `Cancel` while the completed turn's tail plays also cuts
/// only the tail.
#[tokio::test]
async fn client_cancel_in_draining_after_completion_emits_only_clear_audio() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.fake_completed_turn(4);
    bed.cancel().await;
    assert_eq!(bed.control_trace(), vec!["clear_audio:client_cancel"]);
    assert_eq!(bed.tracker.session_aggregator.turns, 1);
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Draining);
}

/// `PlaybackComplete` while the pipeline is still producing audio is
/// ignored: the turn keeps running and its metrics stay its own. Once
/// the end-of-utterance chunk has gone out it is honoured.
#[tokio::test]
async fn playback_complete_mid_turn_is_ignored() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.fake_turn(5, TurnState::Speaking);

    bed.playback_complete(5).await;
    assert!(
        bed.control_trace().is_empty(),
        "no frame for a premature ack"
    );
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Speaking);
    assert!(bed.tracker.current_cancel.is_some(), "turn still active");
    assert!(bed.tracker.stale_playback_reason(5).is_none());

    bed.tracker.eou_sent = true;
    crate::pipeline::set_turn_state(&bed.turn_state, TurnState::Draining);
    bed.playback_complete(5).await;
    assert_eq!(bed.control_trace(), vec!["state_changed:listening"]);
    assert!(bed.tracker.current_cancel.is_none());
}

/// A snapshot left on the metrics channel by an abandoned turn is
/// discarded when the next turn starts, so that turn collects its own.
#[tokio::test]
async fn stale_metrics_are_dropped_when_a_new_turn_starts() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.metrics_tx
        .try_send(TurnMetricsSnapshot {
            utterance_id: 999,
            ..Default::default()
        })
        .unwrap();

    bed.dispatch(AgentEvent::Say {
        text: "fresh turn".into(),
        scheduling: Scheduling::WhenIdle,
    })
    .await;

    let snapshot = tokio::time::timeout(AGENT_TEST_TIMEOUT, bed.metrics_rx.recv())
        .await
        .expect("metrics timed out")
        .expect("metrics channel closed");
    assert_ne!(snapshot.utterance_id, 999, "stale snapshot must be gone");
    assert!(
        snapshot.components.iter().any(|c| c.kind == "tts"),
        "the new turn's own snapshot is collected: {:?}",
        snapshot.components
    );
}

// ── State-machine verdicts on the wire ──

/// The client is told about a state only when the machine entered it:
/// a rejected trigger sends nothing, an idempotent repeat sends
/// nothing, a real move sends one frame.
#[tokio::test]
async fn rejected_transition_emits_no_state_changed() {
    use crate::transport::serve::gateway::state_machine::TurnTrigger;
    use crate::transport::serve::turn::transition;

    let turn_state = new_turn_state();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMessage>(8);

    assert_eq!(
        transition("t", &turn_state, &out_tx, TurnTrigger::PlaybackFinalized).await,
        None
    );
    assert!(out_rx.try_recv().is_err(), "rejected: no frame");
    assert_eq!(get_turn_state(&turn_state), TurnState::Listening);

    assert_eq!(
        transition("t", &turn_state, &out_tx, TurnTrigger::UtteranceCommitted).await,
        Some(TurnState::Processing)
    );
    assert!(matches!(
        out_rx.try_recv(),
        Ok(ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Processing
        }))
    ));

    assert_eq!(
        transition("t", &turn_state, &out_tx, TurnTrigger::UtteranceCommitted).await,
        None
    );
    assert!(out_rx.try_recv().is_err(), "no-op repeat: no frame");
}

// ── Control-plane traces of the interrupt paths ──

/// Early barge: the user speaks while the LLM is still thinking. No
/// audio ever went out, so no `ClearAudio`; the server resolves the
/// Draining → Listening handshake itself.
#[tokio::test]
async fn early_barge_control_trace() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.fake_turn(1, TurnState::Processing);
    bed.barge_in().await;
    assert_eq!(
        bed.control_trace(),
        vec![
            "turn_metrics:interrupted=true",
            "state_changed:draining",
            "state_changed:listening",
        ]
    );
}

/// Late barge: the user speaks over audio that is still being
/// produced. The client flushes, the interrupted turn is reported
/// once, and the turn waits in Draining for the client's ack.
#[tokio::test]
async fn late_barge_control_trace() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.fake_turn(2, TurnState::Speaking);
    bed.barge_in().await;
    assert_eq!(
        bed.control_trace(),
        vec![
            "clear_audio:server_vad",
            "turn_metrics:interrupted=true",
            "state_changed:draining",
        ]
    );
}

/// Typed text while the assistant is speaking interrupts exactly like
/// a cancel (through the same path), then injects the new utterance.
#[tokio::test]
async fn text_interrupt_control_trace() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.fake_turn(3, TurnState::Speaking);
    bed.text_message("actually, stop").await;
    assert_eq!(
        bed.control_trace(),
        vec![
            "clear_audio:text_interrupt",
            "turn_metrics:interrupted=true",
            "state_changed:draining",
        ]
    );
    let injected = bed.text_utt_rx.try_recv().expect("utterance injected");
    assert_eq!(injected.text, "actually, stop");
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Draining);
}

/// Typed text while the previous turn's tail is still playing: the
/// tail is cut without a second metrics snapshot, and the next turn
/// may start straight from Draining.
#[tokio::test]
async fn text_in_draining_starts_the_next_turn_control_trace() {
    let mut bed = AgentTestBed::new(vec![]);
    bed.fake_completed_turn(6);
    bed.text_message("and another thing").await;
    assert_eq!(bed.control_trace(), vec!["clear_audio:text_interrupt"]);
    let injected = bed.text_utt_rx.try_recv().expect("utterance injected");

    // The runner would now hand the injected utterance to
    // `handle_utterance`; drive that step directly.
    {
        let mut ctx = classic_ctx!(bed);
        ctx.handle_utterance(
            injected,
            &bed.cancel_watch_tx,
            &bed.proc_audio_tx,
            &bed.post_processors,
            &None,
            &bed.metrics_tx,
            &bed.tts_cold_start,
            &bed.state,
        )
        .await;
    };
    assert_eq!(bed.control_trace(), vec!["state_changed:processing"]);
    assert_eq!(get_turn_state(&bed.turn_state), TurnState::Processing);
    assert!(bed.tracker.current_cancel.is_some());
    // Let the spawned pipeline task finish before the bed is dropped.
    let _ = tokio::time::timeout(AGENT_TEST_TIMEOUT, bed.metrics_rx.recv()).await;
}

/// Recording processor for verifying `truncate_history` calls on interrupt paths.
struct TruncationRecordingProcessor {
    truncated_with: Arc<parking_lot::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl crate::text::TextProcessor for TruncationRecordingProcessor {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "return type is fixed by the TextProcessor trait signature"
    )]
    fn name(&self) -> &str {
        "truncation-recorder"
    }
    async fn process(&self, input: &str) -> Result<String, crate::text::TextProcessorError> {
        Ok(input.to_string())
    }
    fn truncate_history(&self, played_text: &str) -> Option<String> {
        self.truncated_with.lock().push(played_text.to_string());
        Some("Unheard assistant response".to_string())
    }
}

#[tokio::test]
async fn early_barge_in_truncates_unheard_llm_history() {
    let truncated_with = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let proc = TruncationRecordingProcessor {
        truncated_with: Arc::clone(&truncated_with),
    };
    let mut bed = AgentTestBed::new(vec![Box::new(proc)]);
    bed.fake_turn(1, TurnState::Processing);
    bed.barge_in().await;

    assert_eq!(*truncated_with.lock(), vec![String::new()]);
    assert_eq!(
        bed.control_trace(),
        vec![
            "turn_metrics:interrupted=true",
            "state_changed:draining",
            "state_changed:listening",
        ]
    );
}

#[tokio::test]
async fn playback_report_with_zero_ms_truncates_unheard_llm_history() {
    let truncated_with = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let proc = TruncationRecordingProcessor {
        truncated_with: Arc::clone(&truncated_with),
    };
    let mut bed = AgentTestBed::new(vec![Box::new(proc)]);
    bed.fake_turn(2, TurnState::Speaking);
    let mut ledger = crate::pipeline::AudioLedger::default();
    ledger.push(
        "Unheard assistant response".into(),
        1200,
        0,
        19_200,
        16_000,
        None,
    );
    bed.audio_ledgers
        .insert(2, Arc::new(parking_lot::Mutex::new(ledger)));

    bed.barge_in().await;
    let _ = bed.control_trace();

    classic_ctx!(bed).handle_playback_report(2, 0, None).await;
    assert_eq!(*truncated_with.lock(), vec![String::new()]);
    assert_eq!(bed.control_trace(), vec!["text_truncated:''"]);
}
