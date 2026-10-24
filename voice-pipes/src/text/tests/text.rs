use super::*;

struct UpperCase;

#[async_trait]
impl TextProcessor for UpperCase {
    fn name(&self) -> &'static str {
        "upper-case"
    }
    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        Ok(input.to_uppercase())
    }
}

struct Prefix(String);

#[async_trait]
impl TextProcessor for Prefix {
    fn name(&self) -> &'static str {
        "prefix"
    }
    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        Ok(format!("{}{}", self.0, input))
    }
}

struct Failing;

#[async_trait]
impl TextProcessor for Failing {
    fn name(&self) -> &'static str {
        "failing"
    }
    async fn process(&self, _input: &str) -> Result<String, TextProcessorError> {
        Err(TextProcessorError::new("failing", "intentional failure"))
    }
}

#[tokio::test]
async fn empty_chain_passes_through() {
    let processors: Vec<Box<dyn TextProcessor>> = vec![];
    let result = run_chain(&processors, "hello").await.unwrap();
    assert_eq!(result, "hello");
}

#[tokio::test]
async fn single_processor() {
    let processors: Vec<Box<dyn TextProcessor>> = vec![Box::new(UpperCase)];
    let result = run_chain(&processors, "hello world").await.unwrap();
    assert_eq!(result, "HELLO WORLD");
}

#[tokio::test]
async fn chain_order_matters() {
    let processors: Vec<Box<dyn TextProcessor>> =
        vec![Box::new(UpperCase), Box::new(Prefix(">> ".to_string()))];
    let result = run_chain(&processors, "hello").await.unwrap();
    assert_eq!(result, ">> HELLO");
}

#[tokio::test]
async fn chain_stops_on_error() {
    let processors: Vec<Box<dyn TextProcessor>> = vec![Box::new(Failing), Box::new(UpperCase)];
    let err = run_chain(&processors, "hello").await.unwrap_err();
    assert_eq!(err.processor, "failing");
}

#[test]
fn error_display() {
    let err = TextProcessorError::new("test", "boom");
    assert_eq!(err.to_string(), "text processor \"test\": boom");
}

// ── Streaming tests ─────────────────────────────────────

#[tokio::test]
async fn echo_process_stream_sends_one_fragment() {
    let echo = EchoProcessor;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    echo.process_stream("hello", tx, CancellationToken::new())
        .await
        .unwrap();

    let frag = rx.recv().await.unwrap();
    assert_eq!(frag.text, "hello");
    assert!(frag.is_end_of_response);
    assert!(rx.recv().await.is_none()); // channel closed
}

#[tokio::test]
async fn run_chain_stream_empty_chain() {
    let processors: Vec<Box<dyn TextProcessor>> = vec![];
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    run_chain_stream(&processors, "pass through", tx, CancellationToken::new())
        .await
        .unwrap();

    let frag = rx.recv().await.unwrap();
    assert_eq!(frag.text, "pass through");
    assert!(frag.is_end_of_response);
}

#[tokio::test]
async fn run_chain_stream_single_processor() {
    let processors: Vec<Box<dyn TextProcessor>> = vec![Box::new(UpperCase)];
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    run_chain_stream(&processors, "hello", tx, CancellationToken::new())
        .await
        .unwrap();

    let frag = rx.recv().await.unwrap();
    assert_eq!(frag.text, "HELLO");
    assert!(frag.is_end_of_response);
}

#[tokio::test]
async fn run_chain_stream_multi_processor_batches_intermediate() {
    // UpperCase runs batch, then Prefix streams (via default impl)
    let processors: Vec<Box<dyn TextProcessor>> =
        vec![Box::new(UpperCase), Box::new(Prefix(">> ".to_string()))];
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    run_chain_stream(&processors, "hello", tx, CancellationToken::new())
        .await
        .unwrap();

    let frag = rx.recv().await.unwrap();
    assert_eq!(frag.text, ">> HELLO");
    assert!(frag.is_end_of_response);
}

/// A mock processor that streams multiple sentence fragments.
struct MultiSentenceProcessor;

#[async_trait]
impl TextProcessor for MultiSentenceProcessor {
    fn name(&self) -> &'static str {
        "multi-sentence"
    }
    async fn process(&self, _input: &str) -> Result<String, TextProcessorError> {
        Ok("First sentence. Second sentence. Third.".to_string())
    }
    async fn process_stream(
        &self,
        _input: &str,
        tx: mpsc::Sender<TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let _ = tx
            .send(TextFragment::new("First sentence.".to_string()))
            .await;
        let _ = tx
            .send(TextFragment::new("Second sentence.".to_string()))
            .await;
        let _ = tx
            .send(TextFragment::new("Third.".to_string()).with_is_end_of_response(true))
            .await;
        Ok(())
    }
}

#[tokio::test]
async fn run_chain_stream_multi_fragment() {
    let processors: Vec<Box<dyn TextProcessor>> = vec![Box::new(MultiSentenceProcessor)];
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    run_chain_stream(&processors, "ignored", tx, CancellationToken::new())
        .await
        .unwrap();

    let f1 = rx.recv().await.unwrap();
    assert_eq!(f1.text, "First sentence.");
    assert!(!f1.is_end_of_response);

    let f2 = rx.recv().await.unwrap();
    assert_eq!(f2.text, "Second sentence.");
    assert!(!f2.is_end_of_response);

    let f3 = rx.recv().await.unwrap();
    assert_eq!(f3.text, "Third.");
    assert!(f3.is_end_of_response);
}

// ── Factory registry tests ──────────────────────────────

#[test]
fn text_factory_names_are_unique() {
    let factories = text_processor_factories();
    let names: Vec<&str> = factories.iter().map(|f| f.name()).collect();
    let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate text factory names: {names:?}"
    );
}

#[test]
fn text_factory_includes_echo() {
    let factories = text_processor_factories();
    let names: Vec<&str> = factories.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"echo"), "missing echo factory");
}

// ── Plugin processors: every hook reaches them ──────────────

/// A plugin processor that records what the framework hands it and, when
/// asked to answer from context, calls the first client tool it was given.
#[derive(Default)]
struct HookRecorder {
    session: parking_lot::Mutex<Option<voice_pipes_sdk::agent::SessionHandle>>,
    tools: parking_lot::Mutex<Vec<voice_pipes_sdk::agent::ClientToolSpec>>,
    context: std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
}

#[async_trait]
impl voice_pipes_sdk::TextProcessor for HookRecorder {
    fn name(&self) -> &'static str {
        "hook-recorder"
    }

    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        Ok(input.to_string())
    }

    async fn process_context_stream(
        &self,
        context: &str,
        tx: mpsc::Sender<voice_pipes_sdk::TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let session = self.session.lock().clone().expect("bound before use");
        let tool = self.tools.lock()[0].name.clone();
        session
            .client_tool_call(voice_pipes_sdk::agent::ClientToolCall::new(
                "call-1",
                &tool,
                serde_json::json!({ "screen": context }),
            ))
            .await
            .expect("session live");
        let _ = tx
            .send(
                voice_pipes_sdk::TextFragment::new(format!("from context: {context}"))
                    .with_is_end_of_response(true),
            )
            .await;
        Ok(())
    }

    fn bind_session(&self, session: voice_pipes_sdk::agent::SessionHandle) {
        *self.session.lock() = Some(session);
    }

    fn configure_client_tools(&self, tools: Vec<voice_pipes_sdk::agent::ClientToolSpec>) {
        *self.tools.lock() = tools;
    }

    async fn inject_context(&self, text: &str) {
        self.context.lock().push(text.to_string());
    }
}

/// The session handle, the client tools and the agent hooks reach a plugin
/// processor as the runner hands them over, and its client-tool call arrives
/// on the session's agent channel as the built-in processors' do.
#[tokio::test]
async fn a_plugin_processor_gets_the_session_and_its_tool_calls_reach_the_agent_channel() {
    let recorder = HookRecorder::default();
    let context = std::sync::Arc::clone(&recorder.context);
    let processor: Box<dyn TextProcessor> = Box::new(recorder);
    let (agent_tx, mut agent_rx) = mpsc::channel(4);
    let agent = crate::agent::AgentSessionHandle::new(agent_tx).with_session_id("s-9");
    processor.bind_session(agent.session_handle());
    processor.configure_client_tools(vec![
        crate::agent::client_tools::ClientToolConfig {
            name: "navigate".into(),
            description: "Go to a screen.".into(),
            parameters: serde_json::json!({ "type": "object" }),
            speak_on_result: false,
        }
        .spec(),
    ]);

    let (tx, mut rx) = mpsc::channel(4);
    processor
        .process_context_stream("home", tx, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(rx.recv().await.unwrap().text, "from context: home");
    match agent_rx.recv().await {
        Some(crate::agent::AgentEvent::ClientToolCall {
            call_id,
            name,
            arguments,
        }) => {
            assert_eq!(call_id, "call-1");
            assert_eq!(name, "navigate");
            assert_eq!(arguments, serde_json::json!({ "screen": "home" }));
        }
        other => panic!("expected a client tool call, got {other:?}"),
    }

    processor
        .inject_context("the user is on the home screen")
        .await;
    assert_eq!(*context.lock(), vec!["the user is on the home screen"]);
    // Idle re-engagement stays with the framework: a plugin cannot speak,
    // so nothing claims the prompt.
    assert!(!agent.offer_user_idle(30_000, 1).await);
    // A plugin's handle carries the engine's own only for the engine to read.
    assert_eq!(
        crate::agent::AgentSessionHandle::from_session(&agent.session_handle())
            .map(|h| h.session_id().to_string()),
        Some("s-9".to_string())
    );
}

/// A plugin may link the engine crate (to register itself), so it can name
/// `AgentSessionHandle`; asking its session handle for one still yields
/// nothing. The engine handle is stored as a type private to the engine,
/// so `Say`, `Cancel` and idle registration stay out of a plugin's reach.
#[test]
fn a_plugin_cannot_recover_the_engine_handle_from_its_session() {
    let (agent_tx, _agent_rx) = mpsc::channel(1);
    let session = crate::agent::AgentSessionHandle::new(agent_tx)
        .with_session_id("s-10")
        .session_handle();
    assert!(
        session
            .host_data::<crate::agent::AgentSessionHandle>()
            .is_none()
    );
}

/// A processor whose session has ended learns it from the handle instead of
/// the call vanishing.
#[tokio::test]
async fn a_plugin_processor_learns_when_its_session_has_ended() {
    let (agent_tx, agent_rx) = mpsc::channel(1);
    drop(agent_rx);
    let handle = crate::agent::AgentSessionHandle::new(agent_tx).session_handle();
    let sent = handle
        .client_tool_call(voice_pipes_sdk::agent::ClientToolCall::new(
            "c",
            "navigate",
            serde_json::Value::Null,
        ))
        .await;
    assert_eq!(sent, Err(voice_pipes_sdk::agent::SessionEnded));
}
