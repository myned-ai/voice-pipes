use super::*;
use crate::transport::PipelineMessage;

fn text(t: &str) -> ClientMessage {
    ClientMessage::Message(PipelineMessage::text(
        t,
        crate::transport::MessageRole::User,
    ))
}

fn text_of(m: &ClientMessage) -> String {
    match m {
        ClientMessage::Message(pm) => pm.text.clone().unwrap_or_default(),
        other => panic!("unexpected {other:?}"),
    }
}

/// Frames a client sent during component setup come out first, in
/// arrival order, followed by the live stream — the benchmark runner's
/// `tts_only_synthesize` (and a playground user's first typed message)
/// must survive the setup window instead of being discarded.
#[tokio::test]
async fn held_frames_are_replayed_before_live_ones() {
    let (live_tx, live_rx) = mpsc::channel::<ClientMessage>(8);
    let held = vec![text("held-1"), text("held-2")];
    let mut rx = replay_held_frames(live_rx, held);
    live_tx.send(text("live-1")).await.unwrap();
    drop(live_tx);

    let mut seen = Vec::new();
    while let Some(m) = rx.recv().await {
        seen.push(text_of(&m));
    }
    assert_eq!(seen, vec!["held-1", "held-2", "live-1"]);
}

#[tokio::test]
async fn nothing_held_keeps_the_live_receiver() {
    let (live_tx, live_rx) = mpsc::channel::<ClientMessage>(8);
    let mut rx = replay_held_frames(live_rx, Vec::new());
    live_tx.send(text("live-1")).await.unwrap();
    drop(live_tx);
    assert_eq!(text_of(&rx.recv().await.unwrap()), "live-1");
    assert!(rx.recv().await.is_none());
}
