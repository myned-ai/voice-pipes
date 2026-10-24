use super::super::turn::{PendingInterruption, RealtimeTurn, RequestAnchor, ResponseTracker};
use crate::pipeline::TurnMetricsSnapshot;
use crate::realtime::tests::mock::MockServer;
use crate::realtime::{ConnectInfo, Modalities, RealtimeEvent, RealtimeProtocol, SessionConfig};
use crate::realtime::{ToolError, ToolHandler, ToolOutcome};
use crate::transport::{PipelineState, ServerControl, ServerMessage};
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug)]
pub(super) struct DummyProto {
    pub(super) url: String,
}
impl RealtimeProtocol for DummyProto {
    fn name(&self) -> &'static str {
        "dummy"
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
        Some(serde_json::json!({}))
    }
    fn build_cancel(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({}))
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
    fn parse_event(&self, _: &serde_json::Value) -> Vec<RealtimeEvent> {
        vec![]
    }
}

pub(super) fn noop_persist(_: TurnMetricsSnapshot, _: Option<String>) {}

/// A tracker that believes a provider response is currently streaming.
pub(super) fn active_responses() -> ResponseTracker {
    let mut responses = ResponseTracker::default();
    responses.on_created();
    responses
}

/// Owned per-test session state so each test can build a
/// `RealtimeSessionContext` without 15 loose `let mut` bindings.
pub(super) struct CtxState {
    pub(super) chunks_uplinked: u64,
    pub(super) chunks_downlinked: u64,
    pub(super) next_utt: u32,
    pub(super) current_turn: Option<RealtimeTurn>,
    pub(super) input_audio_ms: u64,
    pub(super) pending_interruption: Option<PendingInterruption>,
    pub(super) pending_audio: Vec<i16>,
    pub(super) pending_rate: u32,
    pub(super) pending_transcript: Option<String>,
    pub(super) pending_transcript_flushed: bool,
    pub(super) ignoring_stray: bool,
    pub(super) session_audio_ms: u64,
    pub(super) request_anchor: Option<RequestAnchor>,
    pub(super) endpoint_observer: Option<super::super::endpoint_observer::EndpointObserver>,
    pub(super) vp_speech_end: Option<super::super::turn::SpeechEndStamp>,
    pub(super) client_tools: crate::agent::client_tools::ClientToolIndex,
    pub(super) client_calls: crate::agent::client_tools::ClientToolCalls,
    pub(super) phantom_transcript_guard: bool,
    pub(super) last_flushed_user_transcript: Option<String>,
    pub(super) playback_tail: Option<super::super::turn::PlaybackTail>,
}

impl CtxState {
    pub(super) fn new() -> Self {
        Self {
            chunks_uplinked: 0,
            chunks_downlinked: 0,
            next_utt: 1,
            current_turn: None,
            input_audio_ms: 0,
            pending_interruption: None,
            pending_audio: vec![],
            pending_rate: 16_000,
            pending_transcript: None,
            pending_transcript_flushed: false,
            ignoring_stray: false,
            session_audio_ms: 0,
            request_anchor: None,
            endpoint_observer: None,
            vp_speech_end: None,
            client_tools: crate::agent::client_tools::ClientToolIndex::default(),
            client_calls: crate::agent::client_tools::ClientToolCalls::default(),
            phantom_transcript_guard: false,
            last_flushed_user_transcript: None,
            playback_tail: None,
        }
    }
}

macro_rules! agent_test_ctx {
    ($state:ident, $client:ident, $out_tx:ident, $pe_tx:ident, $suppress:ident) => {
        super::super::context::RealtimeSessionContext {
            session_id: "test",
            provider_label: "mock",
            provider_model: &None,
            client: &mut $client,
            out_tx: &$out_tx,
            post_processors: &[],
            observations: crate::transport::adaptive::ObservationSink::disconnected(),
            annotators: crate::pipeline::AnnotatorHubs::none(),
            session_recorder: None,
            suppress_audio: &$suppress,
            streaming_audit: &None,
            pe_tx: &$pe_tx,
            client_sample_rate: 16_000,
            output_sample_rate: 16_000,
            audio_chunks_uplinked: &mut $state.chunks_uplinked,
            audio_chunks_downlinked: &mut $state.chunks_downlinked,
            next_utterance_id: &mut $state.next_utt,
            current_turn: &mut $state.current_turn,
            input_audio_ms_acc: &mut $state.input_audio_ms,
            pending_interruption: &mut $state.pending_interruption,
            playback_tail: &mut $state.playback_tail,
            pending_input_audio: &mut $state.pending_audio,
            pending_input_rate: &mut $state.pending_rate,
            pending_user_transcript: &mut $state.pending_transcript,
            pending_user_transcript_flushed: &mut $state.pending_transcript_flushed,
            ignoring_stray_deltas: &mut $state.ignoring_stray,
            session_audio_emitted_ms: &mut $state.session_audio_ms,
            request_anchor: &mut $state.request_anchor,
            endpoint_observer: &mut $state.endpoint_observer,
            vp_speech_end: &mut $state.vp_speech_end,
            persist_turn: &noop_persist,
            pending_playback_health: Box::leak(Box::new(std::sync::Arc::new(
                parking_lot::Mutex::new(None),
            ))),
            client_tools: &$state.client_tools,
            client_calls: &mut $state.client_calls,
            phantom_transcript_guard: &mut $state.phantom_transcript_guard,
            last_flushed_user_transcript: &mut $state.last_flushed_user_transcript,
        }
    };
}

pub(super) async fn wait_for_env(server: &MockServer, kind: &str) -> serde_json::Value {
    for _ in 0..40 {
        if let Some(env) = server
            .recorded()
            .iter()
            .find(|v| v.get("type").and_then(|t| t.as_str()) == Some(kind))
            .cloned()
        {
            return env;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("envelope '{kind}' never recorded: {:?}", server.recorded());
}

pub(super) async fn assert_never_recorded(server: &MockServer, kind: &str) {
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !server
            .recorded()
            .iter()
            .any(|v| v.get("type").and_then(|t| t.as_str()) == Some(kind)),
        "envelope '{kind}' unexpectedly recorded: {:?}",
        server.recorded()
    );
}

/// A tool that takes long enough that the old inline-await dispatch
/// would visibly stall the select loop.
pub(super) struct SleepTool;

#[async_trait]
impl ToolHandler for SleepTool {
    fn name(&self) -> &'static str {
        "sleep"
    }
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "return type is fixed by the ToolHandler trait signature"
    )]
    fn description(&self) -> &str {
        "sleeps then returns"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn invoke(&self, _arguments: &str) -> Result<ToolOutcome, ToolError> {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(ToolOutcome::Done("{\"answer\":42}".to_string()))
    }
}

/// Drive one spoken turn to completion: response started, 1s of audio,
/// a transcript delta, response completed.
pub(super) async fn complete_spoken_turn(
    ctx: &mut super::super::context::RealtimeSessionContext<'_>,
) {
    ctx.handle_provider_response_started().await.unwrap();
    ctx.handle_provider_audio_delta(vec![0; 16_000], 16_000, None)
        .await
        .unwrap();
    ctx.handle_provider_text_delta("Hello there, how can I help?".into())
        .await
        .unwrap();
    ctx.handle_provider_response_completed().await.unwrap();
}

pub(super) fn drain_clear_audio(out_rx: &mut mpsc::Receiver<ServerMessage>) -> Vec<u32> {
    let mut cleared = Vec::new();
    while let Ok(msg) = out_rx.try_recv() {
        if let ServerMessage::Control(ServerControl::ClearAudio { utterance_id, .. }) = msg {
            cleared.push(utterance_id);
        }
    }
    cleared
}

pub(super) fn speaking_count(out_rx: &mut mpsc::Receiver<ServerMessage>) -> usize {
    let mut n = 0;
    while let Ok(msg) = out_rx.try_recv() {
        if matches!(
            msg,
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Speaking
            })
        ) {
            n += 1;
        }
    }
    n
}

pub(super) fn extras_of(
    snapshot: &crate::pipeline::TurnMetricsSnapshot,
) -> &serde_json::Map<String, serde_json::Value> {
    &snapshot
        .component("realtime")
        .expect("realtime component")
        .extras
}

pub(super) fn mic_chunk(samples: Vec<i16>) -> crate::pipeline::AudioChunk {
    crate::pipeline::AudioChunk {
        samples,
        sample_rate: 16_000,
        channels: 1,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    }
}

pub(super) fn test_observer() -> super::super::endpoint_observer::EndpointObserver {
    super::super::endpoint_observer::EndpointObserver::new(
        Box::new(crate::vad::segmenter::EnergyVad::new(200.0)),
        300,
        150,
    )
}

/// Feed `speech_ms` of loud audio then `silence_ms` of silence through the
/// client-audio path in 20 ms frames (what the browser does).
pub(super) async fn feed_mic(
    ctx: &mut super::super::context::RealtimeSessionContext<'_>,
    speech_ms: u64,
    silence_ms: u64,
) {
    for _ in 0..(speech_ms / 20) {
        ctx.handle_client_audio(mic_chunk(vec![3000; 320]))
            .await
            .unwrap();
    }
    for _ in 0..(silence_ms / 20) {
        ctx.handle_client_audio(mic_chunk(vec![0; 320]))
            .await
            .unwrap();
    }
}

pub(super) use agent_test_ctx;
