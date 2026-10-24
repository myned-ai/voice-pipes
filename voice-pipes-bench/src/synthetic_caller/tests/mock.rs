use super::*;

#[tokio::test]
async fn mock_caller_emits_turns_in_order() {
    let mut caller = MockCaller::from_turns(["one".into(), "two".into()]);
    caller.configure("ignored").await.unwrap();
    // First turn: transcript, audio, response_done.
    match caller.next_event().await {
        Some(CallerEvent::TranscriptDelta(t)) => assert_eq!(t, "one"),
        other => panic!("expected TranscriptDelta(one), got {other:?}"),
    }
    match caller.next_event().await {
        Some(CallerEvent::AudioDelta(a)) => assert!(a.is_empty()),
        other => panic!("expected AudioDelta, got {other:?}"),
    }
    match caller.next_event().await {
        Some(CallerEvent::ResponseDone) => {}
        other => panic!("expected ResponseDone, got {other:?}"),
    }
    // Second turn.
    match caller.next_event().await {
        Some(CallerEvent::TranscriptDelta(t)) => assert_eq!(t, "two"),
        other => panic!("expected TranscriptDelta(two), got {other:?}"),
    }
    // Drain.
    let _ = caller.next_event().await;
    let _ = caller.next_event().await;
    // Exhausted.
    assert!(caller.next_event().await.is_none());
}

#[tokio::test]
async fn mock_caller_cancel_drops_remaining_events_in_turn() {
    let mut caller = MockCaller::from_turns(["one".into(), "two".into()]);
    // Pull the first transcript, then cancel before consuming audio
    // / response_done.
    let _ = caller.next_event().await;
    caller.cancel().await.unwrap();
    // Next poll should advance to the second turn.
    match caller.next_event().await {
        Some(CallerEvent::TranscriptDelta(t)) => assert_eq!(t, "two"),
        other => panic!("expected TranscriptDelta(two), got {other:?}"),
    }
}
