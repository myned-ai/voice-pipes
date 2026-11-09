//! A speech-to-speech provider registered from outside the engine, through
//! the public API alone, as a plugin crate would: its name resolves, a
//! second provider under a taken name is refused, and a whole session runs
//! on it. A loopback WebSocket stands in for the vendor; the session takes
//! the caller's audio up, and the provider's transcript, tool call and
//! spoken reply come back down to the transport. A call the provider
//! withdraws is not answered, and a session moves to a new connection,
//! resumed, when the provider closes the old one.

#![cfg(feature = "realtime")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use voice_pipes::config::PipelineConfig;
use voice_pipes::pipeline::AudioChunk;
use voice_pipes::sdk::s2s::codec::{decode_pcm16_b64, encode_pcm16_b64};
use voice_pipes::sdk::s2s::{
    CloseClass, ConnectInfo, Modalities, RealtimeConfig, RealtimeEvent, RealtimeFactory,
    RealtimeProtocol, SessionConfig, default_classify_close,
};
use voice_pipes::sdk::secrets::Secret;
use voice_pipes::sdk::{BuildContext, ConfigError};
use voice_pipes::transport::direct::{DirectTransport, channel_pair};
use voice_pipes::transport::serve::{TransportState, run_session};
use voice_pipes::transport::session_store::{SessionStore, SessionStoreConfig};
use voice_pipes::transport::{ClientControl, ClientMessage, ServerControl, ServerMessage};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const RATE: u32 = 16_000;

// ── The plugin ──────────────────────────────────────────────

/// A wire mapper for the loopback vendor below: one JSON object per frame,
/// its kind in `type`.
struct LoopbackProtocol {
    url: String,
}

impl RealtimeProtocol for LoopbackProtocol {
    fn name(&self) -> &'static str {
        "loopback"
    }
    fn native_input_sample_rate(&self) -> u32 {
        RATE
    }
    fn native_output_sample_rate(&self) -> u32 {
        RATE
    }
    fn connect_info(&self) -> anyhow::Result<ConnectInfo> {
        Ok(ConnectInfo::new(self.url.clone()))
    }
    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value> {
        let tools: Vec<&str> = cfg.tools.iter().map(|t| t.name.as_str()).collect();
        let resume = cfg.resume_token.as_ref().map(Secret::expose);
        vec![serde_json::json!({ "type": "session", "tools": tools, "resume": resume })]
    }
    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value {
        serde_json::json!({ "type": "audio", "data": b64_pcm })
    }
    fn build_response_create(&self, _m: Modalities) -> Option<serde_json::Value> {
        None
    }
    fn build_text_message(&self, _text: &str) -> Option<serde_json::Value> {
        None
    }
    fn build_cancel(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "cancel" }))
    }
    fn build_function_call_output(
        &self,
        call_id: &str,
        _name: &str,
        result: &str,
        _silent: bool,
    ) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "tool_result", "call_id": call_id, "result": result }))
    }
    fn classify_close(&self, code: Option<u16>, reason: &str) -> CloseClass {
        if reason.contains("no such session") {
            CloseClass::Unresumable
        } else {
            default_classify_close(code, reason)
        }
    }
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent> {
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        match v["type"].as_str() {
            Some("ready") => vec![RealtimeEvent::SessionUpdated],
            Some("transcript") => vec![RealtimeEvent::InputTranscript {
                text: text("text"),
                is_final: true,
            }],
            Some("tool_call") => vec![RealtimeEvent::function_call(
                text("call_id"),
                text("name"),
                text("arguments"),
            )],
            Some("started") => vec![RealtimeEvent::ResponseStarted { response_id: None }],
            Some("text") => vec![RealtimeEvent::ResponseTextDelta { text: text("text") }],
            Some("audio") => decode_pcm16_b64(&text("data"), RATE)
                .map(|samples| {
                    vec![RealtimeEvent::ResponseAudioDelta {
                        samples,
                        sample_rate: RATE,
                        alignment: None,
                    }]
                })
                .unwrap_or_default(),
            Some("done") => vec![RealtimeEvent::ResponseCompleted { response_id: None }],
            Some("cancel_calls") => vec![RealtimeEvent::FunctionCallsCancelled {
                call_ids: v["ids"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|id| id.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            }],
            Some("resumable") => vec![RealtimeEvent::SessionResumable {
                token: Secret::new(text("token")),
            }],
            Some("go_away") => vec![RealtimeEvent::GoingAway { time_left: None }],
            _ => Vec::new(),
        }
    }
}

struct LoopbackFactory;

impl RealtimeFactory for LoopbackFactory {
    fn name(&self) -> &'static str {
        "loopback"
    }
    fn aliases(&self) -> &[&str] {
        &["loop"]
    }
    fn default_api_key_env(&self) -> &'static str {
        "LOOPBACK_API_KEY"
    }
    fn validate(&self, _cfg: &RealtimeConfig) -> Vec<ConfigError> {
        Vec::new()
    }
    fn build(
        &self,
        cfg: &RealtimeConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes::sdk::BuildError> {
        let url = cfg
            .options
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("no vendor url in the options"))?;
        Ok(Box::new(LoopbackProtocol {
            url: url.to_string(),
        }))
    }
}

fn loopback() -> Box<dyn RealtimeFactory> {
    Box::new(LoopbackFactory)
}

/// A second plugin answering to the first one's alias.
struct ShadowingFactory;

impl RealtimeFactory for ShadowingFactory {
    fn name(&self) -> &'static str {
        "loop"
    }
    fn default_api_key_env(&self) -> &'static str {
        "X"
    }
    fn validate(&self, _cfg: &RealtimeConfig) -> Vec<ConfigError> {
        Vec::new()
    }
    fn build(
        &self,
        _cfg: &RealtimeConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes::sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed("never built"))
    }
}

fn shadowing() -> Box<dyn RealtimeFactory> {
    Box::new(ShadowingFactory)
}

/// How long the server-side tool takes: long enough for the provider to
/// withdraw the call while it runs.
const LOOKUP_TAKES: Duration = Duration::from_millis(200);

/// A server-side tool that answers after [`LOOKUP_TAKES`].
struct SlowLookup;

#[async_trait::async_trait]
impl voice_pipes::realtime::ToolHandler for SlowLookup {
    fn name(&self) -> &'static str {
        "lookup"
    }
    fn description(&self) -> &'static str {
        "Look something up."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn invoke(
        &self,
        _arguments: &str,
    ) -> Result<voice_pipes::realtime::ToolOutcome, voice_pipes::realtime::ToolError> {
        tokio::time::sleep(LOOKUP_TAKES).await;
        Ok(voice_pipes::realtime::ToolOutcome::Done("found".into()))
    }
}

fn tools(_handle: voice_pipes::agent::AgentSessionHandle) -> voice_pipes::realtime::ToolRegistry {
    let mut registry = voice_pipes::realtime::ToolRegistry::new();
    registry.register(Arc::new(SlowLookup));
    registry
}

/// The registries are process-wide; every test registers through here once.
fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        voice_pipes::realtime::register_realtime_factory(loopback)
            .expect("the first registration succeeds");
        voice_pipes::realtime::register_tool_registry_factory(tools);
    });
}

// ── The vendor ──────────────────────────────────────────────

/// Frames the vendor received, in order, each with the index of the
/// connection it came on as `conn`.
type Recorded = Arc<parking_lot::Mutex<Vec<serde_json::Value>>>;

/// What the vendor does in answer to a frame.
enum Reply {
    Send(serde_json::Value),
    Close(u16, &'static str),
}

/// The vendor's answers to frame `frame` on connection `conn`, given
/// whether it is the first audio on that connection.
type Script = fn(conn: usize, frame: &serde_json::Value, first_audio: bool) -> Vec<Reply>;

/// Accept connections, numbered from 0, and answer each frame by `script`.
async fn start_vendor(script: Script) -> (String, Recorded) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("ws://{}", listener.local_addr().expect("addr"));
    let recorded: Recorded = Arc::default();
    let seen = Arc::clone(&recorded);
    tokio::spawn(async move {
        let connections = AtomicUsize::new(0);
        while let Ok((stream, _)) = listener.accept().await {
            let conn = connections.fetch_add(1, Ordering::SeqCst);
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                let mut heard = false;
                while let Some(Ok(msg)) = ws.next().await {
                    let Message::Text(text) = msg else { continue };
                    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    let first_audio = v["type"] == "audio" && !heard;
                    heard |= first_audio;
                    let replies = script(conn, &v, first_audio);
                    v["conn"] = conn.into();
                    seen.lock().push(v);
                    for reply in replies {
                        let sent = match reply {
                            Reply::Send(frame) => {
                                ws.send(Message::Text(frame.to_string().into())).await
                            }
                            Reply::Close(code, reason) => {
                                let _ = ws
                                    .close(Some(CloseFrame {
                                        code: code.into(),
                                        reason: reason.into(),
                                    }))
                                    .await;
                                // Read on until the client answers the
                                // close: a socket dropped with frames
                                // unread is reset, and a reset can discard
                                // what the client has received but not yet
                                // read, this close frame included.
                                while let Some(Ok(_)) = ws.next().await {}
                                return;
                            }
                        };
                        if sent.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    (url, recorded)
}

/// A spoken reply: the response opens, carries some audio, and closes.
fn spoken_reply() -> Vec<Reply> {
    let audio = encode_pcm16_b64(&vec![1_000_i16; 3_200]);
    vec![
        Reply::Send(serde_json::json!({ "type": "started" })),
        Reply::Send(serde_json::json!({ "type": "audio", "data": audio })),
        Reply::Send(serde_json::json!({ "type": "done" })),
    ]
}

/// Wait, within the test's bound, until the vendor has received a frame
/// matching `wanted`, and return the frames so far.
async fn wait_for_frame(
    recorded: &Recorded,
    wanted: impl Fn(&serde_json::Value) -> bool,
) -> Vec<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let frames = recorded.lock().clone();
        if frames.iter().any(&wanted) || tokio::time::Instant::now() >= deadline {
            return frames;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ── The session ─────────────────────────────────────────────

fn state(config: PipelineConfig) -> Arc<TransportState> {
    let navigate = serde_json::from_value(serde_json::json!({
        "name": "navigate",
        "description": "Navigate the UI to a named screen.",
        "parameters": { "type": "object",
            "properties": { "screen": { "type": "string" } },
            "required": ["screen"] },
    }))
    .expect("a client tool");
    Arc::new(TransportState {
        config: Arc::new(config),
        stt: None,
        tts: None,
        processors: Arc::new(vec![]),
        audio_post_processors: Arc::new(vec![]),
        annotators: Arc::new(vec![]),
        active_sessions: Arc::new(AtomicUsize::new(0)),
        max_sessions: 4,
        ready: AtomicBool::new(true),
        shutdown: tokio_util::sync::CancellationToken::new(),
        turn_complete_tx: None,
        session_event_tx: None,
        session_store: Arc::new(SessionStore::new(SessionStoreConfig::default())),
        pipeline_event_tx: tokio::sync::broadcast::channel(16).0,
        param_registry: Arc::new(voice_pipes::api::param_store::ParamRegistry::new()),
        component_registry: Arc::new(voice_pipes::component::ComponentRegistry::new()),
        client_tools: Arc::new(vec![navigate]),
    })
}

/// The pipeline config selecting the loopback provider, by its alias, at
/// the vendor at `url`.
fn config(url: &str) -> PipelineConfig {
    toml::from_str(&format!(
        "[realtime_provider]\nprovider = \"loop\"\n[realtime_provider.options]\nurl = \"{url}\"\n"
    ))
    .expect("config")
}

async fn next_message(client: &DirectTransport) -> ServerMessage {
    timeout(TEST_TIMEOUT, client.recv_server())
        .await
        .expect("timed out waiting for the session")
        .expect("transport error")
        .expect("transport closed")
}

/// Open a session on `config` and wait until it listens: audio sent while
/// the provider connection opens is dropped.
async fn open_session(config: PipelineConfig) -> (DirectTransport, tokio::task::JoinHandle<()>) {
    let (client, server) = channel_pair();
    let session = tokio::spawn(run_session(Box::new(server), state(config)));
    client
        .send_client(ClientMessage::Control(ClientControl::Configure(
            voice_pipes::transport::SessionConfig {
                sample_rate: RATE,
                ..Default::default()
            },
        )))
        .await
        .expect("configure");
    assert!(matches!(
        next_message(&client).await,
        ServerMessage::Control(ServerControl::SessionReady(_))
    ));
    while !matches!(
        next_message(&client).await,
        ServerMessage::Control(ServerControl::StateChanged {
            state: voice_pipes::transport::PipelineState::Listening
        })
    ) {}
    (client, session)
}

/// The caller says something.
async fn speak(client: &DirectTransport) {
    for _ in 0..5 {
        client
            .send_client(ClientMessage::Audio(AudioChunk {
                samples: vec![500; 1_600],
                sample_rate: RATE,
                channels: 1,
                ..Default::default()
            }))
            .await
            .expect("audio");
    }
}

/// Read the session's messages until one carries audio; return the
/// warnings and errors seen on the way.
async fn until_audio(client: &DirectTransport) -> Vec<String> {
    let mut errors = Vec::new();
    loop {
        match next_message(client).await {
            ServerMessage::Audio(chunk) if !chunk.samples.is_empty() => return errors,
            ServerMessage::Control(ServerControl::Error { message, .. }) => errors.push(message),
            _ => {}
        }
    }
}

#[test]
fn the_provider_resolves_by_name_and_alias_and_a_taken_name_is_refused() {
    register();
    for name in ["loopback", "loop"] {
        let f = voice_pipes::realtime::lookup_realtime_factory(name).expect("found");
        assert_eq!(f.name(), "loopback");
    }
    assert!(voice_pipes::realtime::register_realtime_factory(loopback).is_err());
    assert!(voice_pipes::realtime::register_realtime_factory(shadowing).is_err());
    let names: Vec<&str> = voice_pipes::realtime::realtime_factories()
        .iter()
        .map(|f| f.name())
        .collect();
    assert_eq!(names, ["loopback"], "a refused provider is not registered");
}

/// Acknowledge the session; on the caller's first audio, answer with a
/// transcript of it, a tool call, and a spoken reply, then call the slow
/// server-side tool and withdraw the call while it runs.
fn conversation(_conn: usize, frame: &serde_json::Value, first_audio: bool) -> Vec<Reply> {
    if frame["type"] == "session" {
        return vec![Reply::Send(serde_json::json!({ "type": "ready" }))];
    }
    if !first_audio {
        return Vec::new();
    }
    let mut replies = vec![
        Reply::Send(
            serde_json::json!({ "type": "transcript", "text": "open the settings screen" }),
        ),
        Reply::Send(serde_json::json!({
            "type": "tool_call",
            "call_id": "c1",
            "name": "navigate",
            "arguments": "{\"screen\":\"settings\"}",
        })),
    ];
    replies.extend(spoken_reply());
    replies.extend([
        Reply::Send(serde_json::json!({
            "type": "tool_call",
            "call_id": "c2",
            "name": "lookup",
            "arguments": "{}",
        })),
        Reply::Send(serde_json::json!({ "type": "cancel_calls", "ids": ["c2"] })),
    ]);
    replies
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_runs_on_a_provider_the_engine_never_saw() {
    register();
    let (url, recorded) = start_vendor(conversation).await;
    let config = config(&url);
    assert!(
        voice_pipes::config::validate(&config)
            .iter()
            .all(|e| !e.field.starts_with("realtime_provider")),
        "the plugin's name validates"
    );
    let (client, session) = open_session(config).await;

    // The caller speaks; the audio goes up to the vendor.
    speak(&client).await;

    // The transcript, the tool call and the spoken reply come back down.
    let (mut heard, mut tool_call, mut audio) = (false, false, false);
    while !(heard && tool_call && audio) {
        match next_message(&client).await {
            ServerMessage::Message(m) if m.kind == "trigger_action" => {
                let payload = m.payload.expect("payload");
                assert_eq!(payload["call_id"], "c1");
                assert_eq!(payload["arguments"]["screen"], "settings");
                tool_call = true;
            }
            ServerMessage::Message(m) if m.text.as_deref() == Some("open the settings screen") => {
                heard = true;
            }
            ServerMessage::Audio(chunk) if !chunk.samples.is_empty() => audio = true,
            _ => {}
        }
    }

    // The session declared the client tool, sent the caller's audio, and
    // acknowledged the tool call back to the vendor. The acknowledgement
    // is written after the call reaches the client, so it may still be on
    // its way.
    let frames = wait_for_frame(&recorded, |f| f["type"] == "tool_result").await;
    assert!(
        frames.iter().any(|f| f["type"] == "session"
            && f["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|t| t == "navigate"))),
        "{frames:?}"
    );
    assert!(frames.iter().any(|f| f["type"] == "audio"), "{frames:?}");
    assert!(
        frames
            .iter()
            .any(|f| f["type"] == "tool_result" && f["call_id"] == "c1"),
        "{frames:?}"
    );

    // The withdrawn call's tool finishes, and its result stays here.
    tokio::time::sleep(LOOKUP_TAKES * 3).await;
    let frames = recorded.lock().clone();
    assert!(
        !frames
            .iter()
            .any(|f| f["type"] == "tool_result" && f["call_id"] == "c2"),
        "{frames:?}"
    );
    session.abort();
}

/// Connection 0 issues a resumption token, answers the caller, and says
/// it is closing; a connection presenting the token is taken up as the
/// same session, and answers too.
fn closing_after_one_reply(
    conn: usize,
    frame: &serde_json::Value,
    first_audio: bool,
) -> Vec<Reply> {
    match (conn, frame["type"].as_str()) {
        (0, Some("session")) => vec![
            Reply::Send(serde_json::json!({ "type": "ready" })),
            Reply::Send(serde_json::json!({ "type": "resumable", "token": "t1" })),
        ],
        (0, Some("audio")) if first_audio => {
            let mut replies = spoken_reply();
            replies.push(Reply::Send(serde_json::json!({ "type": "go_away" })));
            replies
        }
        (_, Some("session")) if frame["resume"] == "t1" => {
            vec![Reply::Send(serde_json::json!({ "type": "ready" }))]
        }
        (_, Some("session")) => vec![Reply::Close(1008, "no such session")],
        (_, Some("audio")) if first_audio => spoken_reply(),
        _ => Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_connection_is_left_for_a_resumed_one() {
    register();
    let (url, recorded) = start_vendor(closing_after_one_reply).await;
    let (client, session) = open_session(config(&url)).await;

    speak(&client).await;
    let errors = until_audio(&client).await;
    assert!(errors.is_empty(), "{errors:?}");

    // Once the reply is over, the session opens a new connection with
    // the token, before the old one closes.
    let frames = wait_for_frame(&recorded, |f| f["type"] == "session" && f["conn"] == 1).await;
    let resumed = frames
        .iter()
        .find(|f| f["type"] == "session" && f["conn"] == 1)
        .unwrap_or_else(|| panic!("no second connection: {frames:?}"));
    assert_eq!(resumed["resume"], "t1", "{frames:?}");

    // The caller carries on, on the new connection, unaware of the move.
    speak(&client).await;
    let errors = until_audio(&client).await;
    assert!(errors.is_empty(), "the move is seamless: {errors:?}");
    let frames = recorded.lock().clone();
    assert!(
        frames
            .iter()
            .any(|f| f["type"] == "audio" && f["conn"] == 1),
        "{frames:?}"
    );
    session.abort();
}

/// Connection 0 issues a resumption token and drops on the caller's
/// first audio; connection 1 refuses the token; a fresh connection is
/// taken up and answers.
fn refusing_the_token(conn: usize, frame: &serde_json::Value, first_audio: bool) -> Vec<Reply> {
    match (conn, frame["type"].as_str()) {
        (0, Some("session")) => vec![
            Reply::Send(serde_json::json!({ "type": "ready" })),
            Reply::Send(serde_json::json!({ "type": "resumable", "token": "t1" })),
        ],
        (0, Some("audio")) if first_audio => vec![Reply::Close(1011, "going down")],
        (_, Some("session")) if frame["resume"].is_string() => {
            vec![Reply::Close(1008, "no such session")]
        }
        (_, Some("session")) => vec![Reply::Send(serde_json::json!({ "type": "ready" }))],
        (_, Some("audio")) if first_audio => spoken_reply(),
        _ => Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_resumption_starts_a_fresh_session() {
    register();
    let (url, recorded) = start_vendor(refusing_the_token).await;
    let (client, session) = open_session(config(&url)).await;

    speak(&client).await;
    let frames = wait_for_frame(&recorded, |f| f["type"] == "session" && f["conn"] == 2).await;
    let sessions: Vec<_> = frames
        .iter()
        .filter(|f| f["type"] == "session")
        .map(|f| (f["conn"].clone(), f["resume"].clone()))
        .collect();
    assert_eq!(
        sessions,
        [
            (0.into(), serde_json::Value::Null),
            (1.into(), "t1".into()),
            (2.into(), serde_json::Value::Null),
        ],
        "{frames:?}"
    );

    // The session lives on, fresh, and the caller is told.
    tokio::time::sleep(Duration::from_millis(200)).await;
    speak(&client).await;
    let errors = until_audio(&client).await;
    assert!(
        errors.iter().any(|e| e.contains("fresh session")),
        "{errors:?}"
    );
    session.abort();
}
