#![expect(
    clippy::too_many_lines,
    reason = "scenario tests read as linear scripts; splitting them into helpers hides the flow"
)]
use super::super::turn::{AlignedWord, PendingInterruption, SuppressGate};
use super::common::*;
use crate::pipeline::TurnMetricsSnapshot;
use crate::realtime::{
    ConnectInfo, Modalities, RealtimeClient, RealtimeEvent, RealtimeProtocol, SessionConfig,
};
use crate::transport::{MessageRole, PipelineMessage, PipelineState, ServerControl, ServerMessage};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::accept_async;

/// Off-path annotators: provider audio deltas are
/// teed after the inline chain, batched by the per-session hub, and come
/// back as `ServerMessage::Annotation` frames on the session channel in
/// utterance-offset space — with the audio messages themselves untouched and
/// the `annotator:<name>` component available for the turn snapshot.
#[tokio::test]
async fn provider_audio_is_teed_to_annotators_and_annotations_reach_out_tx() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn RealtimeProtocol> = Box::new(DummyProto { url });
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 1;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = None;
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = None;
    let mut pending_transcript_flushed = false;
    let mut session_audio_ms = 0;

    let persist_turn = |_, _| {};
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    // Built-in `level` annotator in annotate mode, no warm-up (keeps the
    // channel free of utterance-0 traffic for this assertion).
    let spec = crate::pipeline::AnnotatorSpec {
        annotator: Arc::new(crate::pipeline::annotate::LevelAnnotator),
        config: crate::config::AnnotatorConfig {
            warmup: false,
            ..crate::config::AnnotatorConfig::default()
        },
    };
    let hubs = crate::pipeline::AnnotatorHubs::spawn(
        std::slice::from_ref(&spec),
        "test",
        out_tx.clone(),
        16000,
    );

    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: &hubs,
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut None,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    ctx.handle_provider_response_started().await.unwrap();
    // Two 100 ms deltas: the first alone already exceeds `first_batch_ms`
    // (80 ms), so the hub releases a [0, 100) segment right away.
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16000, None)
        .await
        .unwrap();
    ctx.handle_provider_audio_delta(vec![1000; 1600], 16000, None)
        .await
        .unwrap();

    let mut audio_frames = 0;
    let mut annotation = None;
    for _ in 0..20 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), out_rx.recv())
            .await
            .expect("a message within 2 s")
            .expect("channel open");
        match msg {
            ServerMessage::Audio(_) => audio_frames += 1,
            ServerMessage::Annotation(a) => {
                annotation = Some(a);
                break;
            }
            _ => {}
        }
    }
    let a = annotation.expect("an audio_annotation frame reached the session channel");
    assert_eq!(a.kind, "audio_level");
    assert_eq!(a.utterance_id, 1);
    assert_eq!((a.start_ms, a.duration_ms), (0, 100));
    assert_eq!(a.extras["fps"], crate::pipeline::annotate::LEVEL_FPS);
    // The audio itself was forwarded untouched and ahead of the annotation.
    assert!(audio_frames >= 1, "audio frames precede the annotation");

    // Per-turn metrics for the snapshot.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let comps = hubs.turn_components(1);
    assert_eq!(comps.len(), 1);
    assert_eq!(comps[0].component_id, "annotator:level");
    assert_eq!(comps[0].kind, "annotator");
    assert!(comps[0].extras["segments"].as_u64().unwrap_or(0) >= 1);
    assert!(comps[0].extras["annotations"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(comps[0].extras["breaker_open"], false);
}

/// Alignment on audio deltas extends the turn's aligned-word ledger
/// with session-cumulative end times and rides out to the client as a
/// word-timed caption.
#[tokio::test]
async fn alignment_extends_ledger_and_emits_word_timed_captions() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn RealtimeProtocol> = Box::new(DummyProto { url });
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 1;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = None;
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = None;
    let mut pending_transcript_flushed = false;
    let mut session_audio_ms = 0;

    let persist_turn = |_, _| {};
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut None,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    ctx.handle_provider_response_started().await.unwrap();
    let wt = |word: &str, offset_ms: u32, duration_ms: u32| crate::pipeline::WordTiming {
        word: word.to_string(),
        offset_ms,
        duration_ms,
    };
    // Chunk 1: 100 ms of audio, two words.
    ctx.handle_provider_audio_delta(
        vec![0; 1600],
        16000,
        Some(vec![wt("Alpha", 0, 40), wt("beta", 50, 30)]),
    )
    .await
    .unwrap();
    // Chunk 2: another 100 ms, one word — chunk-relative offset 10.
    ctx.handle_provider_audio_delta(vec![0; 1600], 16000, Some(vec![wt("gamma", 10, 40)]))
        .await
        .unwrap();
    // Ledger: session-cumulative word-end times (chunk 2 shifted by
    // the 100 ms of audio that preceded it).
    let turn = ctx.current_turn.as_ref().unwrap();
    assert_eq!(
        turn.aligned_words,
        vec![
            AlignedWord {
                word: "Alpha".to_string(),
                end_ms: 40
            },
            AlignedWord {
                word: "beta".to_string(),
                end_ms: 80
            },
            AlignedWord {
                word: "gamma".to_string(),
                end_ms: 150
            },
        ]
    );

    // Captions: each aligned chunk carries its words + fragment index.
    let mut captions = Vec::new();
    while let Ok(msg) = out_rx.try_recv() {
        if let ServerMessage::Audio(chunk) = msg {
            captions.push(chunk.caption.expect("aligned chunk must carry a caption"));
        }
    }
    assert_eq!(captions.len(), 2);
    assert_eq!(captions[0].text, "Alpha beta");
    assert_eq!(captions[0].fragment_idx, 0);
    assert_eq!(captions[0].word_timings.as_ref().map(Vec::len), Some(2));
    assert_eq!(captions[1].text, "gamma");
    assert_eq!(captions[1].fragment_idx, 1);
}

#[tokio::test]
async fn test_realtime_turn_text_pacing_and_user_buffering() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn RealtimeProtocol> = Box::new(DummyProto { url });
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 1;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = None;
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = None;
    let mut pending_transcript_flushed = false;
    let mut session_audio_ms = 0;

    let persist_turn = |_, _| {};
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut None,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    // 1. is_final = false -> buffered
    ctx.handle_provider_input_transcript("hello".to_string(), false)
        .await
        .unwrap();
    assert_eq!(ctx.pending_user_transcript.as_deref(), Some("hello"));
    assert!(out_rx.try_recv().is_err());

    // 2. is_final = true -> flushed
    ctx.handle_provider_input_transcript("hello world".to_string(), true)
        .await
        .unwrap();
    let msg = out_rx.recv().await.unwrap();
    if let ServerMessage::Message(pm) = msg {
        assert_eq!(pm.role, MessageRole::User);
        assert_eq!(pm.text.as_deref(), Some("hello world"));
    } else {
        panic!("expected user text");
    }

    // 2.5. Provide response started and audio to create turn
    ctx.handle_provider_response_started().await.unwrap();
    ctx.handle_provider_audio_delta(vec![0; 1600], 16000, None)
        .await
        .unwrap();

    // Read the StateChanged(Speaking) message sent by audio delta
    let msg = out_rx.recv().await.unwrap();
    if let ServerMessage::Control(ServerControl::StateChanged { state }) = msg {
        assert_eq!(state, PipelineState::Speaking);
    } else {
        panic!("expected speaking");
    }

    // Read the Audio chunk that handle_provider_audio_delta sent
    let msg = out_rx.recv().await.unwrap();
    if let ServerMessage::Audio(_) = msg {
        // Ok
    } else {
        panic!("expected audio");
    }

    // 3. text delta -> estimated_text_ms advanced
    ctx.handle_provider_text_delta("agent".to_string())
        .await
        .unwrap();
    let msg = out_rx.recv().await.unwrap();
    if let ServerMessage::Message(pm) = msg {
        assert_eq!(pm.role, MessageRole::Assistant);
        assert_eq!(pm.text.as_deref(), Some("agent"));
        assert_eq!(
            pm.payload
                .unwrap()
                .get("audio_ms")
                .unwrap()
                .as_u64()
                .unwrap(),
            0
        );
    } else {
        panic!("expected assistant text");
    }

    let turn = ctx.current_turn.as_ref().unwrap();
    let expected = (5 * 1000) / 15;
    assert_eq!(turn.estimated_text_ms, expected);

    // 4. cancellation mid-stream -> state Listening, buffered text retained
    ctx.handle_provider_response_cancelled().await.unwrap();
    assert!(ctx.pending_interruption.is_some());
    let pi = ctx.pending_interruption.as_ref().unwrap();
    assert_eq!(pi.full_text, "agent");

    // Should emit ClearAudio first
    let msg = out_rx.recv().await.unwrap();
    if let ServerMessage::Control(ServerControl::ClearAudio { .. }) = msg {
        // ok
    } else {
        panic!("expected ClearAudio, got {msg:?}");
    }

    // Then the client-facing turn_metrics frame for the interrupted turn
    // (parity with cascade's emit_turn_complete).
    let msg = out_rx.recv().await.unwrap();
    if let ServerMessage::Control(ServerControl::TurnMetrics { snapshot }) = msg {
        assert!(snapshot.interrupted);
    } else {
        panic!("expected turn_metrics, got {msg:?}");
    }

    // Then StateChanged Listening
    let msg = out_rx.recv().await.unwrap();
    if let ServerMessage::Control(ServerControl::StateChanged { state }) = msg {
        assert_eq!(state, PipelineState::Listening);
    } else {
        panic!("expected listening, got {msg:?}");
    }
}

/// OpenAI-style S2S flow: provider VAD stop (`InputSpeechStopped` →
/// `note_user_speech_end`) followed by response audio. The persisted
/// `TurnMetricsReady` snapshot must carry both the anchored `ttfb_ms`
/// and the `user_speech_end_epoch_ms` wall clock the admin session
/// details screen needs to compute perceived E2E.
#[tokio::test]
async fn speech_end_anchor_flows_into_turn_metrics_snapshot() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn RealtimeProtocol> = Box::new(DummyProto { url });
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, mut pe_rx) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 1;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = None;
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = None;
    let mut pending_transcript_flushed = false;
    let mut session_audio_ms = 0;
    let mut request_anchor = None;

    let persist_turn = |_, _| {};
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut request_anchor,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    // Turn 1: user speech ends (provider VAD) before the response starts.
    ctx.note_user_speech_end();
    ctx.handle_provider_audio_delta(vec![0; 1600], 16000, None)
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();

    let mut snapshots = Vec::new();
    while let Ok(pe) = pe_rx.try_recv() {
        if let crate::pipeline::events::PipelineEvent::TurnMetricsReady { metrics, .. } = pe {
            snapshots.push(metrics);
        }
    }
    assert_eq!(snapshots.len(), 1);
    let extras = &snapshots[0].components[0].extras;
    assert!(
        extras
            .get("user_speech_end_epoch_ms")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|v| v > 1_577_836_800_000),
        "anchored turn must persist the speech-end wall clock, got extras: {extras:?}"
    );
    assert!(extras.get("ttfb_ms").is_some());

    // Turn 2: response with no preceding speech-end signal — the wall
    // clock must be absent so the reader never anchors E2E on a
    // turn-creation timestamp.
    ctx.handle_provider_audio_delta(vec![0; 1600], 16000, None)
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();

    let mut snapshots = Vec::new();
    while let Ok(pe) = pe_rx.try_recv() {
        if let crate::pipeline::events::PipelineEvent::TurnMetricsReady { metrics, .. } = pe {
            snapshots.push(metrics);
        }
    }
    assert_eq!(snapshots.len(), 1);
    assert!(
        !snapshots[0].components[0]
            .extras
            .contains_key("user_speech_end_epoch_ms")
    );
}

#[tokio::test]
async fn test_realtime_text_mode_interruption_fix() {
    let (out_tx, _out_rx) = mpsc::channel(100);
    let (pe_tx, _pe_rx) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 1;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = None;
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = None;
    let mut pending_transcript_flushed = false;
    let mut session_audio_ms = 0;

    let persist_turn = |_, _| {};
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = tokio_tungstenite::accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn crate::realtime::RealtimeProtocol> = Box::new(DummyProto { url });
    let mut client = crate::realtime::RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let mut ctx = crate::transport::serve::realtime_runner::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut None,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    // 1. Agent starts talking
    ctx.handle_provider_response_started().await.unwrap();
    ctx.handle_provider_text_delta("hello".to_string())
        .await
        .unwrap();

    // 2. User interrupts
    ctx.handle_client_cancel().await.unwrap();

    // 3. Ensure ignoring_stray_deltas is set and further deltas are ignored
    assert!(*ctx.ignoring_stray_deltas);

    // This delta should NOT crash or push a message
    ctx.handle_provider_text_delta(" world".to_string())
        .await
        .unwrap();

    // We should NOT get "hello world" since it's ignoring deltas
    assert!(ctx.current_turn.is_none());

    // 4. Provider response cancelled arrives
    ctx.handle_provider_response_cancelled().await.unwrap();

    // 5. ignoring_stray_deltas should be false now
    assert!(!*ctx.ignoring_stray_deltas);
}

#[tokio::test]
async fn test_realtime_text_mode_stuck_in_processing_fix() {
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 1;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = None;
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = Some("my user text".to_string());
    let mut pending_transcript_flushed = true;
    let mut session_audio_ms = 0;

    let persist_turn = |_, _| {};
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = tokio_tungstenite::accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn crate::realtime::RealtimeProtocol> = Box::new(DummyProto { url });
    let mut client = crate::realtime::RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut None,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    // Agent replies in text mode. It skips ResponseStarted and goes straight to text delta.
    ctx.handle_provider_text_delta("hello".to_string())
        .await
        .unwrap();

    // Validate that the turn correctly consumed the pending_user_transcript.
    let turn = ctx.current_turn.as_ref().unwrap();
    assert_eq!(turn.user_transcript.as_deref(), Some("my user text"));
    assert!(ctx.pending_user_transcript.is_none());

    // Agent finishes turn
    ctx.handle_provider_response_completed().await.unwrap();

    // Must emit state=Listening (because pending_user_transcript is None)
    let mut got_audio = false;
    let mut got_listening = false;
    while let Ok(msg) = out_rx.try_recv() {
        match msg {
            ServerMessage::Audio(pm) => {
                assert!(pm.is_end_of_utterance);
                got_audio = true;
            }
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Listening,
            }) => {
                got_listening = true;
            }
            _ => {}
        }
    }
    assert!(got_audio, "expected audio");
    assert!(got_listening, "expected listening");
}

#[tokio::test]
async fn test_realtime_text_mode_double_bubble_fix() {
    let (out_tx, mut out_rx) = mpsc::channel(100);
    let (pe_tx, mut pe_rx) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 1;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = None;
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = None;
    let mut pending_transcript_flushed = false;
    let mut session_audio_ms = 0;

    let persist_turn = |_, _| {};
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = tokio_tungstenite::accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn crate::realtime::RealtimeProtocol> = Box::new(DummyProto { url });
    let mut client = crate::realtime::RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut None,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    // User sends typed text message
    let pm = PipelineMessage {
        kind: "text".into(),
        text: Some("hello".to_string()),
        payload: None,
        binary: None,
        role: MessageRole::User,
        turn_id: None,
        message_id: "client-id-1".into(),
    };
    ctx.handle_client_message(pm).await.unwrap();

    assert_eq!(ctx.pending_user_transcript.as_deref(), Some("hello"));
    assert!(*ctx.pending_user_transcript_flushed); // this prevents double bubble!

    // Verify UtteranceReceived was dispatched (used by session details)
    let pe_msg = pe_rx.recv().await.unwrap();
    if let crate::pipeline::events::PipelineEvent::UtteranceReceived { text, source, .. } = pe_msg {
        assert_eq!(text, "hello");
        assert_eq!(source, "Text");
    } else {
        panic!("expected utterance received");
    }

    // Next provider response should consume the text, but NOT emit stt- message
    ctx.handle_provider_text_delta("world".to_string())
        .await
        .unwrap();

    // We should get state=Processing but NOT the agent text yet: transcript
    // is withheld until the turn produces audio (or, for a genuine
    // text-only turn like this one, until turn completion) so unspoken
    // tool-call "planned speech" never reaches the chat.
    let mut got_processing = false;
    while let Ok(msg) = out_rx.try_recv() {
        match msg {
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Processing,
            }) => {
                got_processing = true;
            }
            ServerMessage::Message(pm) if pm.role == MessageRole::Assistant => {
                panic!("agent text must be withheld until audio or turn end");
            }
            _ => {}
        }
    }
    assert!(got_processing, "expected processing");

    // Turn completes with no audio and no function call: a genuine
    // text-only response — the withheld transcript flushes now.
    ctx.handle_provider_response_completed().await.unwrap();
    let mut got_agent_text = false;
    while let Ok(msg) = out_rx.try_recv() {
        match msg {
            ServerMessage::Message(pm) if pm.role == MessageRole::Assistant => {
                assert_eq!(pm.text.as_deref(), Some("world"));
                got_agent_text = true;
            }
            _ => {}
        }
    }
    assert!(got_agent_text, "expected agent text at turn completion");
}

/// The interrupted turn must be persisted (and its events emitted)
/// BEFORE the provider-side truncate call — a failing provider send used to
/// abort the handler after `pending_interruption` was consumed, so the turn
/// snapshot was never persisted and HistoryModified/BargeInResolved were
/// never emitted.
#[tokio::test]
async fn playback_report_persists_turn_before_provider_truncate() {
    #[derive(Debug)]
    struct OrderProto {
        url: String,
        order: Arc<Mutex<Vec<&'static str>>>,
    }
    impl RealtimeProtocol for OrderProto {
        fn name(&self) -> &'static str {
            "order"
        }
        fn native_input_sample_rate(&self) -> u32 {
            16000
        }
        fn native_output_sample_rate(&self) -> u32 {
            16000
        }
        fn connect_info(&self) -> anyhow::Result<ConnectInfo> {
            Ok(ConnectInfo::new(self.url.clone()))
        }
        fn build_session_update(&self, _: &SessionConfig) -> Vec<serde_json::Value> {
            vec![]
        }
        fn build_audio_append(&self, _: &str) -> serde_json::Value {
            serde_json::json!({})
        }
        fn build_response_create(&self, _: Modalities) -> Option<serde_json::Value> {
            None
        }
        fn build_text_message(&self, _: &str) -> Option<serde_json::Value> {
            None
        }
        fn build_cancel(&self) -> Option<serde_json::Value> {
            None
        }
        fn build_function_call_output(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: bool,
        ) -> Option<serde_json::Value> {
            None
        }
        fn build_item_truncate(&self, _: &str, _: u32) -> Option<serde_json::Value> {
            self.order.lock().unwrap().push("provider_truncate");
            Some(serde_json::json!({"type": "truncate"}))
        }
        fn parse_event(&self, _: &serde_json::Value) -> Vec<RealtimeEvent> {
            vec![]
        }
    }

    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mock");
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = accept_async(stream).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });

    let proto: Box<dyn RealtimeProtocol> = Box::new(OrderProto {
        url,
        order: Arc::clone(&order),
    });
    let mut client = RealtimeClient::connect(proto, 16_000)
        .await
        .expect("connect");

    let (out_tx, mut _out_rx) = mpsc::channel(100);
    let (pe_tx, _) = broadcast::channel(100);
    let suppress_audio = SuppressGate::new(Arc::new(AtomicBool::new(false)));
    let mut chunks_uplinked = 0;
    let mut chunks_downlinked = 0;
    let mut next_utt = 8;
    let mut current_turn = None;
    let mut input_audio_ms = 0;
    let mut pending_interruption = Some(PendingInterruption {
        utterance_id: 7,
        full_text: "hello world".to_string(),
        text_timeline: vec![(0, 11)],
        audio_emitted_ms: 3_000,
        item_id: Some("item-1".to_string()),
        start_audio_ms: 0,
        aligned_words: Vec::new(),
        snapshot: TurnMetricsSnapshot::default(),
        user_transcript: None,
    });
    let mut pending_audio = vec![];
    let mut pending_rate = 16000;
    let mut pending_transcript = None;
    let mut pending_transcript_flushed = false;
    let mut session_audio_ms = 0;

    let order_persist = Arc::clone(&order);
    let persist_turn = move |_, _| {
        order_persist.lock().unwrap().push("persist");
    };
    let client_tools_index = crate::agent::client_tools::ClientToolIndex::default();
    let mut client_calls = crate::agent::client_tools::ClientToolCalls::default();
    let mut phantom_transcript_guard = false;
    let mut last_flushed_user_transcript: Option<String> = None;

    let mut ctx = super::super::context::RealtimeSessionContext {
        session_id: "test",
        provider_label: "test_provider",
        provider_model: &None,
        client: &mut client,
        out_tx: &out_tx,
        post_processors: &[],
        observations: crate::transport::adaptive::ObservationSink::disconnected(),
        annotators: crate::pipeline::AnnotatorHubs::none(),
        session_recorder: None,
        suppress_audio: &suppress_audio,
        streaming_audit: &None,
        pe_tx: &pe_tx,
        client_sample_rate: 16000,
        output_sample_rate: 16000,
        audio_chunks_uplinked: &mut chunks_uplinked,
        audio_chunks_downlinked: &mut chunks_downlinked,
        next_utterance_id: &mut next_utt,
        current_turn: &mut current_turn,
        input_audio_ms_acc: &mut input_audio_ms,
        pending_interruption: &mut pending_interruption,
        playback_tail: &mut None,
        pending_input_audio: &mut pending_audio,
        pending_input_rate: &mut pending_rate,
        pending_user_transcript: &mut pending_transcript,
        pending_user_transcript_flushed: &mut pending_transcript_flushed,
        ignoring_stray_deltas: &mut false,
        session_audio_emitted_ms: &mut session_audio_ms,
        request_anchor: &mut None,
        endpoint_observer: &mut None,
        vp_speech_end: &mut None,
        persist_turn: &persist_turn,
        pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(parking_lot::Mutex::new(
            None,
        )))),
        client_tools: &client_tools_index,
        client_calls: &mut client_calls,
        phantom_transcript_guard: &mut phantom_transcript_guard,
        last_flushed_user_transcript: &mut last_flushed_user_transcript,
    };

    ctx.handle_client_playback_report(7, 1_000, Some("hello".to_string()))
        .await
        .expect("playback report");

    let recorded = order.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["persist", "provider_truncate"],
        "turn persistence must precede the fallible provider truncate call"
    );
    assert!(
        pending_interruption.is_none(),
        "pending interruption must be consumed"
    );
}

// ── agentic foundation ──
//
// These tests pin the non-blocking tool dispatch, the post-output
// response kick, and AgentEvent handling (Say / CompleteTool /
// Message / scheduling) against the recording mock provider.
