use super::super::turn::{AnchorSource, RealtimeTurn, RequestAnchor, SpeechEndStamp, SuppressGate};
use super::common::*;
use crate::realtime::tests::mock::{MockRealtimeProtocol, MockServer};
use crate::realtime::{RealtimeClient, RealtimeProtocol};
use crate::transport::{MessageRole, PipelineMessage};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc};

/// Gemini shape: input transcript (proxy anchor) -> output transcript ->
/// audio. `ttfb_ms` waits for the audio; the transcript only stamps
/// `first_text_ms`; `speaking` is announced exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ttfb_stamps_on_first_audio_not_first_transcript() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_provider_input_transcript("Who are you?".into(), false)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    ctx.handle_provider_text_delta("I ".into()).await.unwrap();
    let turn = ctx
        .current_turn
        .as_ref()
        .expect("turn created by the text delta");
    assert!(turn.ttfb_ms.is_none(), "text must not stamp ttfb");
    assert!(turn.first_text_ms.is_some_and(|ms| ms >= 25));
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    assert_eq!(speaking_count(&mut out_rx), 1, "speaking announced once");
    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    let ttfb = extras["ttfb_ms"].as_u64().unwrap();
    let first_text = extras["first_text_ms"].as_u64().unwrap();
    assert!(
        ttfb >= first_text + 35,
        "ttfb {ttfb} must trail first text {first_text}"
    );
    assert_eq!(extras["ttfb_anchor_source"], "input_transcript_proxy");
    assert!(extras.get("user_speech_end_epoch_ms").is_some());
    assert!(extras.get("vp_speech_end_epoch_ms").is_none());
    assert!(extras.get("provider_endpointing_ms").is_none());
    assert!(
        extras.get("input_speech_dbfs").is_none(),
        "no observer, no input metering"
    );
}

/// `OpenAI` shape with the vp observer on: the observer stamps the true end
/// of speech on the uplink, the provider's `InputSpeechStopped` lands later
/// and anchors TTFB; the gap is `provider_endpointing_ms`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_measures_provider_endpointing_against_provider_stop() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.endpoint_observer = Some(test_observer());
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    feed_mic(&mut ctx, 400, 400).await;
    assert!(
        ctx.vp_speech_end.is_some(),
        "observer stamped the end of speech"
    );
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    ctx.note_user_speech_end();
    ctx.handle_provider_response_started().await.unwrap();
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();
    assert!(ctx.vp_speech_end.is_none(), "stamp consumed by the turn");

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "provider_speech_stopped");
    assert!(extras["vp_speech_end_epoch_ms"].as_u64().unwrap() > 0);
    let endpointing = extras["provider_endpointing_ms"].as_u64().unwrap();
    assert!(
        (55..2_000).contains(&endpointing),
        "endpointing {endpointing}"
    );
    // The anchor is the provider's, so ttfb is measured from the stop event.
    assert!(extras["ttfb_ms"].as_u64().unwrap() < endpointing);
    // Input audio quality rides with the stamp:
    // 3000/32768 speech, digital-silence hangover as the floor.
    assert_eq!(extras["input_speech_dbfs"], -20.8);
    assert_eq!(extras["input_peak_dbfs"], -20.8);
    assert_eq!(extras["input_noise_dbfs"], -100.0);
    assert_eq!(extras["input_snr_db"], 79.2);
    assert_eq!(extras["input_clipped_ratio"], 0.0);
    assert_eq!(extras["input_speech_ms"], 400);
    assert_eq!(extras["input_level_source"], "vp_energy");
}

/// Gemini 3.x shape: the whole input transcript arrives in the same burst
/// as the first reply. As a proxy it would book the entire wait as
/// provider endpointing and leave TTFB at ~0, so vp's own end-of-speech
/// stamp anchors the turn and no endpointing figure is reported.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcript_arriving_with_the_reply_yields_to_the_observer_stamp() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.endpoint_observer = Some(test_observer());
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    feed_mic(&mut ctx, 400, 400).await;
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    ctx.handle_provider_input_transcript("Tell me a story.".into(), false)
        .await
        .unwrap();
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "vp_vad");
    assert!(
        extras.get("provider_endpointing_ms").is_none(),
        "a reply-time transcript measures no endpointing"
    );
    assert!(extras["vp_speech_end_epoch_ms"].as_u64().unwrap() > 0);
    assert!(
        extras["ttfb_ms"].as_u64().unwrap() >= 115,
        "ttfb counts from the end of speech, not from the transcript"
    );
}

/// Gemini 2.5 shape: the transcript streams well before the reply, so it
/// still anchors TTFB and the gap to vp's stamp is the provider's
/// endpointing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcript_well_before_the_reply_stays_the_anchor() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.endpoint_observer = Some(test_observer());
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    feed_mic(&mut ctx, 400, 400).await;
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    ctx.handle_provider_input_transcript("Tell me a story.".into(), false)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "input_transcript_proxy");
    let endpointing = extras["provider_endpointing_ms"].as_u64().unwrap();
    assert!(
        (55..450).contains(&endpointing),
        "endpointing {endpointing}"
    );
    assert!(extras["ttfb_ms"].as_u64().unwrap() >= 495);
}

/// A pause longer than the observer's hangover splits one utterance into
/// two stamps; the turn reports the pooled input stats of both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_utterance_pools_input_audio_on_the_turn() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.endpoint_observer = Some(test_observer());
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    feed_mic(&mut ctx, 400, 400).await;
    feed_mic(&mut ctx, 200, 400).await;
    ctx.handle_provider_input_transcript("What is your name?".into(), false)
        .await
        .unwrap();
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["input_speech_ms"], 600);
    assert_eq!(extras["input_speech_dbfs"], -20.8);
}

/// Segments pool only when they are close enough to be one utterance.
#[test]
fn speech_end_absorbs_earlier_segment_only_when_close() {
    let segment = || {
        let mut m = crate::audio::InputAudioMeter::new("vp_energy");
        m.feed(&[3000; 3200], 16_000, true); // 200 ms
        m.take()
    };
    let now = Instant::now();
    let at = |ago_ms: u64, input_audio: Option<crate::audio::InputAudioStats>| SpeechEndStamp {
        instant: now
            .checked_sub(std::time::Duration::from_millis(ago_ms))
            .unwrap(),
        epoch_ms: 1,
        input_audio,
    };
    // Second segment starts ~500 ms after the first ended: one utterance.
    let mut late = at(0, segment());
    late.absorb_earlier(&at(700, segment()));
    assert_eq!(late.input_audio.unwrap().speech_ms(), 400);
    // An earlier exchange 5 s back stays separate.
    let mut late = at(0, segment());
    late.absorb_earlier(&at(5_000, segment()));
    assert_eq!(late.input_audio.unwrap().speech_ms(), 200);
    // Nothing to pool from a stamp without stats.
    let mut late = at(0, segment());
    late.absorb_earlier(&at(100, None));
    assert_eq!(late.input_audio.unwrap().speech_ms(), 200);
}

/// Provider faster than our hangover: the stop event arrives before the
/// observer has declared the end. The stamp still lands on the open turn
/// because it precedes the provider anchor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_observer_stamp_attaches_to_open_turn_when_it_precedes_anchor() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.endpoint_observer = Some(test_observer());
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    // Speech ended, hangover not yet met.
    feed_mic(&mut ctx, 400, 100).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    // Provider stop before our declaration.
    ctx.note_user_speech_end();
    ctx.handle_provider_response_started().await.unwrap();
    // Hangover completes during the response.
    feed_mic(&mut ctx, 0, 400).await;
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "provider_speech_stopped");
    assert!(extras.get("vp_speech_end_epoch_ms").is_some());
    assert!(extras["provider_endpointing_ms"].as_u64().unwrap() >= 15);
}

/// No provider anchor at all (Gemini with no input transcript): the
/// observer's stamp becomes the anchor, source `vp_vad`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_stamp_is_the_anchor_when_provider_gives_none() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.endpoint_observer = Some(test_observer());
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    feed_mic(&mut ctx, 400, 400).await;
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "vp_vad");
    assert!(extras["ttfb_ms"].as_u64().unwrap() >= 35);
    assert!(extras.get("user_speech_end_epoch_ms").is_some());
    assert!(extras.get("vp_speech_end_epoch_ms").is_some());
    assert!(
        extras.get("provider_endpointing_ms").is_none(),
        "no provider anchor to measure against"
    );
}

/// The client's `speech_end` timing report is the last-resort anchor:
/// replaced when the provider speaks, kept when nothing better arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_speech_end_is_last_resort_anchor() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_client_timing_report(1, crate::transport::TimingEventKind::SpeechEnd, 1);
    assert_eq!(
        ctx.request_anchor.map(|a| a.source),
        Some(AnchorSource::ClientSpeechEnd)
    );
    ctx.note_user_speech_end();
    assert_eq!(
        ctx.request_anchor.map(|a| a.source),
        Some(AnchorSource::ProviderSpeechStopped)
    );
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();
    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    assert_eq!(
        extras_of(&snap)["ttfb_anchor_source"],
        "provider_speech_stopped"
    );

    let mut s2 = CtxState::new();
    let mut ctx = agent_test_ctx!(s2, client, out_tx, pe_tx, suppress);
    ctx.handle_client_timing_report(1, crate::transport::TimingEventKind::SpeechEnd, 1);
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();
    let snap = s2.current_turn.take().unwrap().finish(false, 0, "mock");
    assert_eq!(extras_of(&snap)["ttfb_anchor_source"], "client_speech_end");
}

/// A text submission anchors at the submit and ignores any observer stamp
/// (that speech belonged to something else).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_submit_anchor_ignores_observer_stamp() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    s.vp_speech_end = Some(SpeechEndStamp::now());
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_client_message(PipelineMessage {
        kind: "text".into(),
        text: Some("hello".into()),
        payload: None,
        binary: None,
        role: MessageRole::User,
        turn_id: None,
        message_id: "m1".into(),
    })
    .await
    .unwrap();
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "text_submit");
    assert!(extras.get("vp_speech_end_epoch_ms").is_none());
}

/// No anchor of any kind: the snapshot says so instead of shipping a
/// meaningless ~0 ms TTFB as if it were speech-end anchored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unanchored_turn_reports_turn_start_source() {
    let server = MockServer::start(vec![]).await;
    let proto: Box<dyn RealtimeProtocol> = Box::new(MockRealtimeProtocol::new(server.url()));
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut s = CtxState::new();
    let mut ctx = agent_test_ctx!(s, client, out_tx, pe_tx, suppress);
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16_000, None)
        .await
        .unwrap();

    let snap = s.current_turn.take().unwrap().finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert_eq!(extras["ttfb_anchor_source"], "turn_start");
    assert!(extras.get("user_speech_end_epoch_ms").is_none());
    assert!(extras.get("first_text_ms").is_none());
}

/// A stale observer stamp (an earlier exchange) must not be reported as
/// this turn's end of speech.
#[test]
fn stale_observer_stamp_is_dropped_at_finish() {
    let mut turn = RealtimeTurn::new(1, 0);
    turn.ttfb_anchor = Some(RequestAnchor::now(AnchorSource::InputTranscriptProxy));
    turn.vp_speech_end = Some(SpeechEndStamp {
        instant: Instant::now()
            .checked_sub(std::time::Duration::from_secs(30))
            .unwrap(),
        epoch_ms: 1,
        input_audio: None,
    });
    let snap = turn.finish(false, 0, "mock");
    let extras = extras_of(&snap);
    assert!(extras.get("vp_speech_end_epoch_ms").is_none());
    assert!(extras.get("provider_endpointing_ms").is_none());
    assert_eq!(extras["ttfb_anchor_source"], "input_transcript_proxy");
}

// ── audio gate and response bookkeeping across interrupts ──
