use super::*;
use futures_util::stream;

#[expect(
    clippy::unnecessary_wraps,
    reason = "builds items for a stream whose item type is the socket's `Result`"
)]
fn text_msg(v: serde_json::Value) -> Result<Message, tokio_tungstenite::tungstenite::Error> {
    Ok(Message::Text(v.to_string().into()))
}

fn turn_metrics_env(initiator: &str, total_ms: u64) -> serde_json::Value {
    serde_json::json!({
        "type": "control",
        "payload": {
            "action": "turn_metrics",
            "snapshot": {
                "utterance_id": 1,
                "total_ms": total_ms,
                "interrupted": false,
                "initiator": initiator,
                "components": []
            }
        }
    })
}

fn assistant_text_env(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "payload": { "kind": "text", "role": "assistant", "text": text }
    })
}

/// An agent-initiated `turn_metrics` must NOT complete the scripted
/// turn (the phantom-turn class) — it is recorded separately
/// and the collector keeps waiting for the user-initiated one.
#[tokio::test]
async fn agent_turn_metrics_do_not_complete_scripted_turn() {
    let frames = vec![
        text_msg(turn_metrics_env("agent", 111)),
        text_msg(assistant_text_env("the real reply")),
        text_msg(turn_metrics_env("user", 222)),
    ];
    let mut stream = stream::iter(frames);
    let outcome = collect_outcome(
        &mut stream,
        None,
        Duration::from_secs(5),
        Duration::from_millis(200),
        Instant::now(),
        true,
    )
    .await
    .expect("collect");

    assert!(outcome.turn_metrics_seen, "user turn must complete");
    assert_eq!(outcome.agent_snapshots.len(), 1);
    assert_eq!(outcome.agent_snapshots[0].total_ms, 111);
    // e2e comes from the USER snapshot, not the agent one.
    assert_eq!(outcome.e2e_latency_ms, Some(222.0));
}

fn state_env(state: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "control",
        "payload": { "action": "state_changed", "state": state }
    })
}

/// A realtime session emits `state_changed: listening` when the provider
/// session finishes opening — often after the client already submitted
/// its text turn. That startup echo must NOT complete a text-only turn
/// (it used to, making the runner fire the next scripted turn into the
/// provider's still-active response). Only a `listening` seen after
/// `processing` demarcates the turn.
#[tokio::test]
async fn startup_listening_does_not_complete_text_turn() {
    let frames = vec![
        text_msg(state_env("listening")), // provider-session-open echo
        text_msg(state_env("processing")),
        text_msg(assistant_text_env("the reply")),
        text_msg(state_env("listening")), // genuine end-of-turn
    ];
    let mut stream = stream::iter(frames);
    let outcome = collect_outcome(
        &mut stream,
        None,
        Duration::from_secs(5),
        Duration::from_millis(200),
        Instant::now(),
        true,
    )
    .await
    .expect("collect");

    // Early return on the startup echo would have missed the reply.
    assert_eq!(outcome.response_text.as_deref(), Some("the reply"));
}

/// The wire carries the reply on two channels (`audio_meta` captions +
/// assistant text deltas); providers that deliver their full turn text
/// as one delta (`ElevenLabs` `agent_response`) made the concatenation
/// self-interleave. The server snapshot's `llm_response_text` is
/// authoritative and must win over the accumulated parts.
#[tokio::test]
async fn snapshot_text_beats_interleaved_caption_concatenation() {
    let metrics_env = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "turn_metrics",
            "snapshot": {
                "utterance_id": 1,
                "total_ms": 900,
                "interrupted": false,
                "llm_response_text": "The capital of France is Paris.",
                "components": []
            }
        }
    });
    let caption = |t: &str| serde_json::json!({ "type": "audio_meta", "payload": { "text": t } });
    let frames = vec![
        text_msg(caption("The capital of France")),
        text_msg(assistant_text_env("The capital of France is Paris.")),
        text_msg(caption("is Paris.")),
        text_msg(metrics_env),
    ];
    let mut stream = stream::iter(frames);
    let outcome = collect_outcome(
        &mut stream,
        None,
        Duration::from_secs(5),
        Duration::from_millis(200),
        Instant::now(),
        true,
    )
    .await
    .expect("collect");

    assert_eq!(
        outcome.response_text.as_deref(),
        Some("The capital of France is Paris.")
    );
}

/// The `turn_metrics` envelope trails the terminal `listening` signal
/// (`TurnCompleted` -> listening -> `TurnMetricsReady`). The collector must
/// linger for it instead of returning at `listening` -- otherwise the
/// turn reports `completed: false` and the late envelope bleeds into
/// the next scripted turn's collect.
#[tokio::test]
async fn trailing_turn_metrics_after_listening_are_collected() {
    let frames = vec![
        text_msg(state_env("processing")),
        text_msg(assistant_text_env("the reply")),
        text_msg(state_env("listening")),
        text_msg(turn_metrics_env("user", 333)),
    ];
    let mut stream = stream::iter(frames);
    let outcome = collect_outcome(
        &mut stream,
        None,
        Duration::from_secs(5),
        Duration::from_millis(200),
        Instant::now(),
        true,
    )
    .await
    .expect("collect");

    assert!(
        outcome.turn_metrics_seen,
        "trailing snapshot must be drained"
    );
    assert_eq!(outcome.e2e_latency_ms, Some(333.0));
}

/// Realtime S2S snapshots carry a single `realtime` component;
/// its ttfb/audio numbers must reach the outcome.
#[tokio::test]
async fn realtime_component_kind_feeds_outcome_metrics() {
    let env = serde_json::json!({
        "type": "control",
        "payload": {
            "action": "turn_metrics",
            "snapshot": {
                "utterance_id": 3,
                "total_ms": 900,
                "interrupted": false,
                "components": [{
                    "component_id": "realtime",
                    "kind": "realtime",
                    "started_at_ms": 0,
                    "provider": "openai",
                    "extras": { "ttfb_ms": 480, "audio_output_ms": 2500 }
                }]
            }
        }
    });
    let mut stream = stream::iter(vec![text_msg(env)]);
    let outcome = collect_outcome(
        &mut stream,
        None,
        Duration::from_secs(5),
        Duration::from_millis(200),
        Instant::now(),
        true,
    )
    .await
    .expect("collect");

    assert_eq!(outcome.ttfb_ms, Some(480.0));
    assert_eq!(outcome.tts_ttfb_ms, Some(480.0));
    assert_eq!(outcome.tts_audio_duration_s, Some(2.5));
    assert_eq!(outcome.tts_provider.as_deref(), Some("openai"));
}

/// Follow-up collection: counts agent turn completions and gathers
/// the assistant text spoken in the window.
#[tokio::test]
async fn followup_collection_counts_agent_turns_and_text() {
    let frames = vec![
        text_msg(assistant_text_env("Good news — ")),
        text_msg(assistant_text_env("your order shipped.")),
        text_msg(turn_metrics_env("agent", 300)),
    ];
    let mut stream = stream::iter(frames);
    let followups = collect_agent_followups(&mut stream, 1, Duration::from_secs(5)).await;
    assert_eq!(followups.seen, 1);
    assert_eq!(
        followups.text.as_deref(),
        Some("Good news — your order shipped.")
    );
}

/// A closed stream ends the follow-up wait with what was seen.
#[tokio::test]
async fn followup_collection_stops_on_stream_end() {
    let mut stream = stream::iter(Vec::new());
    let followups = collect_agent_followups(&mut stream, 2, Duration::from_secs(5)).await;
    assert_eq!(followups.seen, 0);
    assert!(followups.text.is_none());
}
