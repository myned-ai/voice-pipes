use super::super::turn::SuppressGate;
use super::common::*;
use crate::realtime::tests::mock::{MockRealtimeProtocol, MockServer};
use crate::realtime::{RealtimeClient, RealtimeProtocol};
use crate::transport::ServerMessage;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{broadcast, mpsc};

struct MarkerProc;

#[async_trait]
impl crate::pipeline::AudioPostProcessor for MarkerProc {
    fn name(&self) -> &'static str {
        "marker"
    }
    async fn process(
        &self,
        chunk: &mut crate::pipeline::AudioChunk,
        _event_tx: &mpsc::Sender<ServerMessage>,
    ) -> Result<(), crate::pipeline::AudioPostProcessorError> {
        chunk.extras = Some(serde_json::json!({
            "marker": chunk.utterance_id,
            "eou": chunk.is_end_of_utterance,
        }));
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_runs_post_audio_chain_on_deltas_and_eou() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();

    // Inject a real chain -- `agent_test_ctx!` hardcodes an empty one.
    let procs: Vec<Box<dyn crate::pipeline::AudioPostProcessor>> = vec![Box::new(MarkerProc)];
    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "mock",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: procs.as_slice(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
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
        persist_turn: &noop_persist,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &s.client_tools,
        client_calls: &mut s.client_calls,
        phantom_transcript_guard: &mut s.phantom_transcript_guard,
        last_flushed_user_transcript: &mut s.last_flushed_user_transcript,
    };

    // A provider turn: response started -> audio delta -> completed (EOU).
    ctx.handle_provider_response_started().await.unwrap();
    ctx.handle_provider_audio_delta(vec![0i16; 1600], 16_000, None)
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();

    // Every audio frame forwarded to the transport must carry the marker
    // extras the chain attached -- on the delta and on the EOU sentinel.
    let mut audio_frames = Vec::new();
    while let Ok(msg) = out_rx.try_recv() {
        if let ServerMessage::Audio(chunk) = msg {
            audio_frames.push(chunk);
        }
    }

    assert!(
        audio_frames.len() >= 2,
        "expected >=1 audio delta + EOU sentinel, got {}",
        audio_frames.len()
    );
    assert!(
        audio_frames.iter().all(|c| c.extras.is_some()),
        "every realtime audio frame must carry post-audio extras"
    );
    let delta = audio_frames
        .iter()
        .find(|c| !c.is_end_of_utterance)
        .expect("an audio-delta chunk");
    assert_eq!(delta.extras.as_ref().unwrap()["eou"], false);
    let eou = audio_frames
        .iter()
        .find(|c| c.is_end_of_utterance)
        .expect("the EOU sentinel");
    assert_eq!(eou.extras.as_ref().unwrap()["eou"], true);
}

// ── Client-side tools ──────────────
//
// The provider model calls a tool on the per-session client allowlist:
// the runner emits `trigger_action` to the client, acks the provider
// silently (one output per call_id, NO response kick), and later maps
// the client's `ClientToolResult` onto the agent-event machinery.
