#![expect(
    clippy::needless_continue,
    reason = "test event-drain loops use explicit continue arms"
)]
use crate::agent::AgentEvent;
use crate::config::PipelineConfig;
use crate::pipeline::{AudioChunk, TurnMetricsSnapshot, TurnState, new_turn_state};
use crate::transport::direct::channel_pair;
use crate::transport::serve::runner::context::ClassicSessionContext;
use crate::transport::serve::turn::TurnTracker;
use crate::transport::serve::{TransportState, run_session};
use crate::transport::session_store::{SessionStore, SessionStoreConfig};
use crate::transport::{
    ClientControl, ClientMessage, PipelineState, ServerControl, ServerMessage, SessionConfig,
};
use crate::transport::{MessageRole, PipelineMessage};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Build a minimal `TransportState` with mock providers for testing.
pub(super) fn test_state() -> Arc<TransportState> {
    test_state_full(true, true, None)
}

/// Like [`test_state`] but lets a test omit the STT and/or TTS provider to
/// exercise the classic-runner precondition.
pub(super) fn test_state_opts(with_stt: bool, with_tts: bool) -> Arc<TransportState> {
    test_state_full(with_stt, with_tts, None)
}

/// Full-control variant: also takes the `[agent]` settings so idle
/// re-engagement tests can configure short timeouts.
pub(super) fn test_state_full(
    with_stt: bool,
    with_tts: bool,
    agent: Option<crate::config::AgentSettings>,
) -> Arc<TransportState> {
    test_state_customised(with_stt, with_tts, agent, |_| {})
}

/// [`test_state_full`] with a hook that can adjust the state (and, via
/// `Arc::get_mut`, its config) before it is shared.
pub(super) fn test_state_customised(
    with_stt: bool,
    with_tts: bool,
    agent: Option<crate::config::AgentSettings>,
    customise: impl FnOnce(&mut TransportState),
) -> Arc<TransportState> {
    // Minimal mock STT that echoes "hello" for any input.
    struct MockStt;

    #[async_trait::async_trait]
    impl crate::stt::SttProvider for MockStt {
        async fn transcribe(
            &self,
            _audio: &crate::audio::AudioFrame,
        ) -> Result<crate::stt::TranscriptionResult, crate::stt::SttError> {
            Ok(crate::stt::TranscriptionResult::new("hello", true).with_confidence(Some(0.99)))
        }

        async fn transcribe_stream(
            &self,
            _req: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<crate::stt::TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), crate::stt::SttError> {
            let mut got_audio = false;
            while let Some(chunk) = audio_rx.recv().await {
                if !chunk.samples.is_empty() {
                    got_audio = true;
                }
                if chunk.is_last {
                    break;
                }
            }
            if got_audio {
                let _ = tx
                    .send(
                        crate::stt::TranscriptionResult::new("hello", true)
                            .with_confidence(Some(0.99)),
                    )
                    .await;
            }
            // Keep result_tx alive until audio_rx closes so the
            // stt_task silence-commit timer has time to fire.
            while audio_rx.recv().await.is_some() {}
            Ok(())
        }

        fn expected_sample_rate(&self) -> u32 {
            16000
        }

        fn name(&self) -> &'static str {
            "mock-stt"
        }

        fn capabilities(&self) -> HashSet<crate::stt::SttCapability> {
            HashSet::new()
        }
    }

    // Minimal mock TTS that returns a fixed audio chunk.
    struct MockTts;

    #[async_trait::async_trait]
    impl crate::tts::TtsProvider for MockTts {
        async fn synthesize(
            &self,
            _text: &str,
        ) -> Result<crate::audio::AudioFrame, crate::tts::TtsError> {
            Ok(crate::audio::AudioFrame {
                samples: vec![100; 1600],
                sample_rate: 16000,
                channels: 1,
            })
        }

        fn sample_rate(&self) -> u32 {
            16000
        }

        fn name(&self) -> &'static str {
            "mock-tts"
        }
    }

    // Minimal mock text processor (echo).
    struct EchoProcessor;

    #[async_trait::async_trait]
    impl crate::text::TextProcessor for EchoProcessor {
        async fn process(&self, input: &str) -> Result<String, crate::text::TextProcessorError> {
            Ok(input.to_string())
        }

        fn name(&self) -> &'static str {
            "echo"
        }
    }

    let config = PipelineConfig {
        pipeline: crate::config::PipelineSettings::default(),
        speculative: crate::config::SpeculativeConfig::default(),
        stt: None,
        tts: None,
        output: None,
        vad: None,
        grading: None,
        turn_detection: Some(crate::config::TurnDetectionConfig {
            mode: "silence".into(),
            silence_timeout_ms: 200,
            options: std::collections::HashMap::default(),
        }),
        barge_in: None,
        adaptive_quality: crate::config::AdaptiveQualityConfig::default(),
        transport: None,
        text_processor: vec![],
        audio_post_processor: vec![],
        realtime_provider: None,
        auth: None,
        agent,
        client_tool: vec![],
        client_tools_path: None,
    };

    let mut state = TransportState {
        config: Arc::new(config),
        stt: with_stt.then(|| Arc::new(MockStt) as Arc<dyn crate::stt::SttProvider>),
        tts: with_tts.then(|| Arc::new(MockTts) as Arc<dyn crate::tts::TtsProvider>),
        processors: Arc::new(vec![Box::new(EchoProcessor)]),
        audio_post_processors: Arc::new(vec![]),
        annotators: Arc::new(vec![]),
        active_sessions: Arc::new(AtomicUsize::new(0)),
        max_sessions: 16,
        ready: AtomicBool::new(true),
        shutdown: tokio_util::sync::CancellationToken::new(),
        turn_complete_tx: None,
        session_event_tx: None,
        session_store: Arc::new(SessionStore::new(SessionStoreConfig::default())),
        pipeline_event_tx: tokio::sync::broadcast::channel(16).0,
        param_registry: Arc::new(crate::api::param_store::ParamRegistry::new()),
        component_registry: Arc::new(crate::component::ComponentRegistry::new()),
        client_tools: Arc::new(vec![]),
    };
    customise(&mut state);
    Arc::new(state)
}

/// Helper: send Configure and consume the `SessionReady` response.
pub(super) async fn handshake(client: &crate::transport::direct::DirectTransport) {
    handshake_with_rate(client, None).await;
}

/// Handshake with an optional explicit sample rate.
pub(super) async fn handshake_with_rate(
    client: &crate::transport::direct::DirectTransport,
    sample_rate: Option<u32>,
) {
    let mut cfg = SessionConfig::default();
    if let Some(rate) = sample_rate {
        cfg.sample_rate = rate;
        cfg.output_sample_rate = Some(rate);
    }
    // Send Configure so the server completes the handshake.
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(cfg)))
        .await
        .unwrap();

    // Consume SessionReady.
    let msg = client
        .recv_server()
        .await
        .unwrap()
        .expect("expected SessionReady");
    match msg {
        ServerMessage::Control(ServerControl::SessionReady(_)) => {}
        other => panic!("expected SessionReady, got {other:?}"),
    }
}

/// Drive a handshake with `cfg` and return the negotiated input rate.
pub(super) async fn negotiated_input_rate(state: &Arc<TransportState>, cfg: SessionConfig) -> u32 {
    let (client, server) = channel_pair();
    let state2 = Arc::clone(state);
    let handle = tokio::spawn(async move {
        run_session(Box::new(server), state2).await;
    });
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(cfg)))
        .await
        .unwrap();
    let rate = loop {
        match client
            .recv_server()
            .await
            .unwrap()
            .expect("expected a message")
        {
            ServerMessage::Control(ServerControl::SessionReady(info)) => {
                break info.negotiated.input_sample_rate;
            }
            _ => continue,
        }
    };
    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    rate
}

// ── agent-initiated turns ──

/// Text processor that records `inject_context` /
/// `append_assistant_message` calls so tests can assert the
/// history-side effects of agent events.
pub(super) struct CaptureProcessor {
    pub(super) injected: Arc<std::sync::Mutex<Vec<String>>>,
    pub(super) appended: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl crate::text::TextProcessor for CaptureProcessor {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "return type is fixed by the TextProcessor trait signature"
    )]
    fn name(&self) -> &str {
        "capture"
    }
    async fn process(&self, input: &str) -> Result<String, crate::text::TextProcessorError> {
        Ok(input.to_string())
    }
    async fn inject_context(&self, text: &str) {
        self.injected.lock().unwrap().push(text.to_string());
    }
    async fn append_assistant_message(&self, text: &str) {
        self.appended.lock().unwrap().push(text.to_string());
    }
}

/// Owned per-test session state for building a
/// `ClassicSessionContext` without the transport loop.
pub(super) struct AgentTestBed {
    pub(super) state: Arc<TransportState>,
    pub(super) tracker: TurnTracker,
    pub(super) suppress_audio: Arc<AtomicBool>,
    pub(super) out_tx: mpsc::Sender<ServerMessage>,
    pub(super) out_rx: mpsc::Receiver<ServerMessage>,
    pub(super) audio_tx: mpsc::Sender<AudioChunk>,
    pub(super) metrics_tx: mpsc::Sender<TurnMetricsSnapshot>,
    pub(super) metrics_rx: mpsc::Receiver<TurnMetricsSnapshot>,
    pub(super) proc_audio_tx: mpsc::Sender<AudioChunk>,
    pub(super) proc_audio_rx: mpsc::Receiver<AudioChunk>,
    pub(super) turn_state: crate::pipeline::SharedTurnState,
    pub(super) session_processors: Arc<Vec<Box<dyn crate::text::TextProcessor>>>,
    pub(super) pe_tx: tokio::sync::broadcast::Sender<crate::pipeline::events::PipelineEvent>,
    pub(super) audio_ledgers:
        std::collections::HashMap<u32, Arc<parking_lot::Mutex<crate::pipeline::AudioLedger>>>,
    pub(super) barge_in_sent_at: Option<std::time::Instant>,
    pub(super) tts_session: Arc<tokio::sync::Mutex<Option<Box<dyn crate::tts::TtsSession>>>>,
    pub(super) cancel_watch_tx: tokio::sync::watch::Sender<CancellationToken>,
    pub(super) post_processors: Arc<Vec<Box<dyn crate::pipeline::AudioPostProcessor>>>,
    pub(super) tts_cold_start: Arc<AtomicBool>,
    pub(super) pending_agent: std::collections::VecDeque<AgentEvent>,
    pub(super) client_tools: crate::agent::client_tools::ClientToolIndex,
    pub(super) client_calls: crate::agent::client_tools::ClientToolCalls,
    /// Set by a test that wants the real tee points exercised; `None`
    /// leaves every handler on the path it takes in production without
    /// recording configured.
    pub(super) session_recorder: Option<crate::audit::SessionRecorder>,
    pub(super) text_utt_tx: mpsc::Sender<crate::pipeline::Utterance>,
    pub(super) text_utt_rx: mpsc::Receiver<crate::pipeline::Utterance>,
}

/// Build a `ClassicSessionContext` over the bed's owned state.
macro_rules! classic_ctx {
    ($bed:expr) => {
        ClassicSessionContext {
            session_id: "agent-test",
            tracker: &mut $bed.tracker,
            suppress_audio: &$bed.suppress_audio,
            out_tx: &$bed.out_tx,
            annotators: crate::pipeline::AnnotatorHubs::none(),
            turn_detector: None,
            session_recorder: $bed.session_recorder.as_ref(),
            observations: crate::transport::adaptive::ObservationSink::disconnected(),
            audio_tx: &$bed.audio_tx,
            metrics_rx: &mut $bed.metrics_rx,
            proc_audio_rx: &mut $bed.proc_audio_rx,
            turn_state: &$bed.turn_state,
            session_processors: &$bed.session_processors,
            streaming_audit: &None,
            pe_tx: &$bed.pe_tx,
            stt_name: "mock-stt",
            tts_name: "mock-tts",
            client_sample_rate: 16000,
            negotiated_output: Some(16000),
            audio_ledgers: &mut $bed.audio_ledgers,
            barge_in_sent_at: &mut $bed.barge_in_sent_at,
            tts_session: Arc::clone(&$bed.tts_session),
            spec_manager: None,
            client_tools: &$bed.client_tools,
            client_calls: &mut $bed.client_calls,
        }
    };
}

impl AgentTestBed {
    pub(super) fn new(processors: Vec<Box<dyn crate::text::TextProcessor>>) -> Self {
        let (out_tx, out_rx) = mpsc::channel(100);
        let (audio_tx, _audio_rx_unused) = mpsc::channel(8);
        let (metrics_tx, metrics_rx) = mpsc::channel(4);
        let (proc_audio_tx, proc_audio_rx) = mpsc::channel(32);
        let (cancel_watch_tx, _) = tokio::sync::watch::channel(CancellationToken::new());
        let (text_utt_tx, text_utt_rx) = mpsc::channel(4);
        Self {
            state: test_state(),
            tracker: TurnTracker::new(),
            suppress_audio: Arc::new(AtomicBool::new(false)),
            out_tx,
            out_rx,
            audio_tx,
            metrics_tx,
            metrics_rx,
            proc_audio_tx,
            proc_audio_rx,
            turn_state: new_turn_state(),
            session_processors: Arc::new(processors),
            pe_tx: tokio::sync::broadcast::channel(64).0,
            audio_ledgers: std::collections::HashMap::default(),
            barge_in_sent_at: None,
            tts_session: Arc::new(tokio::sync::Mutex::new(None)),
            cancel_watch_tx,
            post_processors: Arc::new(vec![]),
            tts_cold_start: Arc::new(AtomicBool::new(false)),
            pending_agent: std::collections::VecDeque::default(),
            client_tools: crate::agent::client_tools::ClientToolIndex::default(),
            client_calls: crate::agent::client_tools::ClientToolCalls::default(),
            session_recorder: None,
            text_utt_tx,
            text_utt_rx,
        }
    }

    /// Dispatch one agent event through a freshly-built context.
    pub(super) async fn dispatch(&mut self, event: AgentEvent) {
        let mut ctx = classic_ctx!(self);
        ctx.handle_agent_event(
            event,
            &mut self.pending_agent,
            &self.cancel_watch_tx,
            &self.proc_audio_tx,
            &self.post_processors,
            &None,
            &self.metrics_tx,
            &self.tts_cold_start,
            &self.state,
        )
        .await;
    }

    /// Resolve one client tool result through a freshly-built context.
    pub(super) fn resolve(
        &mut self,
        call_id: &str,
        phase: crate::transport::ClientToolPhase,
        result: Option<&serde_json::Value>,
        speak: bool,
    ) -> Option<AgentEvent> {
        let mut ctx = classic_ctx!(self);
        ctx.resolve_client_tool_result(call_id, phase, result, speak)
    }

    /// Server-VAD barge-in on the active turn.
    pub(super) async fn barge_in(&mut self) {
        classic_ctx!(self).handle_server_barge_in().await;
    }

    /// Client `Cancel`.
    pub(super) async fn cancel(&mut self) {
        classic_ctx!(self).handle_cancel().await;
    }

    pub(super) async fn playback_complete(&mut self, utterance_id: u32) {
        classic_ctx!(self)
            .handle_playback_complete(utterance_id)
            .await;
    }

    /// Typed text from the client (the text-interrupt path when a turn
    /// is active).
    pub(super) async fn text_message(&mut self, text: &str) {
        let msg = PipelineMessage::text(text, MessageRole::User);
        let tx = self.text_utt_tx.clone();
        classic_ctx!(self).handle_pipeline_message(msg, &tx).await;
    }

    /// Pretend a turn is in flight: tracker engaged, state as given,
    /// and a pipeline snapshot already queued so the interrupt paths
    /// collect it without waiting out the grace period.
    pub(super) fn fake_turn(&mut self, utterance_id: u32, state: TurnState) {
        self.tracker
            .start_turn(utterance_id, CancellationToken::new());
        crate::pipeline::set_turn_state(&self.turn_state, state);
        self.metrics_tx
            .try_send(TurnMetricsSnapshot::default())
            .expect("metrics slot");
    }

    /// Pretend a turn ran to completion on the server: metrics emitted,
    /// end-of-utterance forwarded, waiting in Draining for the client's
    /// `PlaybackComplete`.
    pub(super) fn fake_completed_turn(&mut self, utterance_id: u32) {
        self.tracker
            .start_turn(utterance_id, CancellationToken::new());
        crate::pipeline::set_turn_state(&self.turn_state, TurnState::Draining);
        self.tracker.eou_sent = true;
        self.tracker.metrics_emitted = true;
        self.tracker
            .session_aggregator
            .record(&TurnMetricsSnapshot::default());
    }

    /// Control-plane trace of everything emitted so far.
    pub(super) fn control_trace(&mut self) -> Vec<String> {
        let mut frames = Vec::new();
        while let Ok(msg) = self.out_rx.try_recv() {
            frames.push(msg);
        }
        crate::transport::serve::gateway::golden::control_trace(&frames)
    }
}

// ── Interrupts after the turn already completed ──

pub(super) const AGENT_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) fn idle_settings(timeout_ms: u64, max_prompts: u32) -> crate::config::AgentSettings {
    crate::config::AgentSettings {
        endpoint: None,
        idle_timeout_ms: Some(timeout_ms),
        idle_max_prompts: max_prompts,
        idle_message: "Are you still there?".into(),
    }
}

/// One drained frame from the idle-test sessions.
pub(super) enum IdleFrame {
    Processing,
    Audio,
    Metrics(TurnMetricsSnapshot),
    Other,
    /// No frame within the timeout (expected during silence).
    Timeout,
    Closed,
}

/// Receive one frame with a timeout, classified for the idle tests.
pub(super) async fn next_idle_frame(
    client: &crate::transport::direct::DirectTransport,
    timeout: std::time::Duration,
) -> IdleFrame {
    match tokio::time::timeout(timeout, client.recv_server()).await {
        Ok(Ok(Some(ServerMessage::Control(ServerControl::StateChanged {
            state: PipelineState::Processing,
        })))) => IdleFrame::Processing,
        Ok(Ok(Some(ServerMessage::Audio(_)))) => IdleFrame::Audio,
        Ok(Ok(Some(ServerMessage::Control(ServerControl::TurnMetrics { snapshot })))) => {
            IdleFrame::Metrics(snapshot)
        }
        Ok(Ok(Some(_))) => IdleFrame::Other,
        Ok(Ok(None) | Err(_)) => IdleFrame::Closed,
        Err(_elapsed) => IdleFrame::Timeout,
    }
}

pub(super) use classic_ctx;
