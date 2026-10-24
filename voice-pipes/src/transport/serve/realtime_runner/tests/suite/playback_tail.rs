use super::super::turn::SuppressGate;
use super::common::*;
use crate::realtime::tests::mock::{MockRealtimeProtocol, MockServer};
use crate::realtime::{RealtimeClient, RealtimeProtocol};
use crate::transport::{ServerControl, ServerMessage};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn speech_started_during_playback_tail_flushes_client() {
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
        complete_spoken_turn(&mut ctx).await;
    };
    let armed_utt = s
        .playback_tail
        .as_ref()
        .expect("a spoken turn must arm the playback tail")
        .utterance_id();
    assert!(drain_clear_audio(&mut out_rx).is_empty());

    // User speaks while the client is still playing the completed turn.
    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_provider_input_speech_started().await.unwrap();
    };

    assert_eq!(
        drain_clear_audio(&mut out_rx),
        vec![armed_utt],
        "the tail barge-in must clear the client's buffered audio"
    );
    assert!(suppress.is_suppressed(), "agent audio suppressed");
    assert_eq!(
        suppress.armed_for(),
        Some(armed_utt),
        "the gate is armed for the utterance the client was told to drop"
    );
    assert!(s.playback_tail.is_none(), "tail consumed");
    let pending = s
        .pending_interruption
        .as_ref()
        .expect("tail must enter the PlaybackReport reconciliation flow");
    assert_eq!(pending.utterance_id, armed_utt);
    assert!(pending.audio_emitted_ms >= 1000, "1s of audio was emitted");

    // The client's PlaybackReport then truncates through the normal flow.
    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_client_playback_report(armed_utt, 400, None)
            .await
            .unwrap();
    };
    assert!(
        s.pending_interruption.is_none(),
        "report consumed the pending"
    );
    let truncated = {
        let mut found = None;
        while let Ok(msg) = out_rx.try_recv() {
            if let ServerMessage::Control(ServerControl::TextTruncated { utterance_id, .. }) = msg {
                found = Some(utterance_id);
            }
        }
        found
    };
    assert_eq!(
        truncated,
        Some(armed_utt),
        "TextTruncated must reach the client"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playback_complete_disarms_the_tail() {
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
        complete_spoken_turn(&mut ctx).await;
        let utt = ctx.playback_tail.as_ref().unwrap().utterance_id();
        ctx.handle_client_playback_complete(utt).unwrap();
        ctx.handle_provider_input_speech_started().await.unwrap();
    };

    assert!(s.playback_tail.is_none());
    assert!(
        drain_clear_audio(&mut out_rx).is_empty(),
        "speech after natural playback end is a new turn, not a barge-in"
    );
    assert!(s.pending_interruption.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_audio_turn_arms_no_tail() {
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
        // Tool-call leg shape: a response that never emits audio.
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_provider_response_started().await.unwrap();
        ctx.handle_provider_text_delta("planning text".into())
            .await
            .unwrap();
        ctx.handle_provider_response_completed().await.unwrap();
    };

    assert!(
        s.playback_tail.is_none(),
        "nothing is playing after a zero-audio turn — no barge-in window"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_tail_is_not_a_barge_in() {
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
        complete_spoken_turn(&mut ctx).await;
    };
    // Clients that never report playback must not leave an eternal
    // barge-in window: force the wall-clock bound to expire.
    s.playback_tail = s.playback_tail.take().map(|t| t.with_max_live_ms(0));
    tokio::time::sleep(Duration::from_millis(10)).await;
    drain_clear_audio(&mut out_rx);

    {
        let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
        ctx.handle_provider_input_speech_started().await.unwrap();
    };

    assert!(
        drain_clear_audio(&mut out_rx).is_empty(),
        "expired tail must not flush"
    );
    assert!(s.pending_interruption.is_none());
    assert!(s.playback_tail.is_none(), "expired tail is discarded");
}

// ── Honest S2S latency anchors ──
