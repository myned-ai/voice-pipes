use super::*;
use crate::chat_template::ChatMlTemplate;

/// Scripted behaviour for the wrapped inner processor.
enum InnerBehavior {
    /// `process` succeeds with this response.
    Ok(String),
    /// `process`/`process_stream` fail immediately with no output.
    Err,
    /// `process_stream` emits these fragments, then returns Err.
    StreamThenErr(Vec<String>),
}

struct MockInner {
    behavior: InnerBehavior,
}

#[async_trait]
impl TextProcessor for MockInner {
    fn name(&self) -> &'static str {
        "mock-inner"
    }

    async fn process(&self, _input: &str) -> Result<String, TextProcessorError> {
        match &self.behavior {
            InnerBehavior::Ok(r) => Ok(r.clone()),
            InnerBehavior::Err => Err(TextProcessorError::new("mock-inner", "boom")),
            InnerBehavior::StreamThenErr(_) => {
                panic!("process() called on a streaming behaviour")
            }
        }
    }

    async fn process_stream(
        &self,
        _input: &str,
        tx: mpsc::Sender<TextFragment>,
        _cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let frags = match &self.behavior {
            InnerBehavior::StreamThenErr(frags) => frags,
            InnerBehavior::Err => {
                return Err(TextProcessorError::new("mock-inner", "boom"));
            }
            InnerBehavior::Ok(r) => {
                let _ = tx
                    .send(TextFragment::new(r.clone()).with_is_end_of_response(true))
                    .await;
                return Ok(());
            }
        };
        for f in frags {
            let _ = tx.send(TextFragment::new(f.clone())).await;
        }
        Err(TextProcessorError::new("mock-inner", "stream boom"))
    }
}

fn wrapper(behavior: InnerBehavior) -> ConversationMemoryWrapper {
    ConversationMemoryWrapper::new(
        Box::new(MockInner { behavior }),
        Box::new(ChatMlTemplate),
        "system",
        None,
    )
}

/// The prompt trace carries a count and hash by default; the full
/// message list only on request.
#[tokio::test]
async fn llm_prompt_trace_is_hashed_unless_full_prompts() {
    let (tx, mut rx) = mpsc::channel(8);
    let w = ConversationMemoryWrapper::new(
        Box::new(MockInner {
            behavior: InnerBehavior::Ok("reply".into()),
        }),
        Box::new(ChatMlTemplate),
        "system",
        Some(tx.clone()),
    );
    w.process("secret question").await.unwrap();
    let ev = rx.recv().await.expect("trace event");
    assert_eq!(ev.event_type, "llm_prompt");
    let dump = ev.payload.to_string();
    assert!(!dump.contains("secret question"), "{dump}");
    assert_eq!(ev.payload["messages_count"], 2);
    assert!(
        ev.payload["prompt_sha256"]
            .as_str()
            .is_some_and(|h| h.len() == 32)
    );

    let (tx, mut rx) = mpsc::channel(8);
    let w = ConversationMemoryWrapper::new(
        Box::new(MockInner {
            behavior: InnerBehavior::Ok("reply".into()),
        }),
        Box::new(ChatMlTemplate),
        "system",
        Some(tx),
    )
    .with_full_prompts(true);
    w.process("secret question").await.unwrap();
    let ev = rx.recv().await.expect("trace event");
    assert_eq!(ev.event_type, "llm_prompt");
    assert!(ev.payload.to_string().contains("secret question"));
}

#[tokio::test]
async fn process_error_rolls_back_user_turn() {
    let w = wrapper(InnerBehavior::Err);
    assert!(w.process("hello").await.is_err());
    assert!(
        w.history.read().is_empty(),
        "failed turn must not leave a dangling user message"
    );
}

#[tokio::test]
async fn process_success_appends_user_and_assistant() {
    let w = wrapper(InnerBehavior::Ok("hi there".into()));
    assert_eq!(w.process("hello").await.unwrap(), "hi there");
    let h = w.history.read();
    assert_eq!(h.len(), 2);
    assert_eq!(
        (h[0].role.as_str(), h[0].content.as_str()),
        ("user", "hello")
    );
    assert_eq!(
        (h[1].role.as_str(), h[1].content.as_str()),
        ("assistant", "hi there")
    );
}

#[tokio::test]
async fn process_error_preserves_prior_history_alternation() {
    let w = wrapper(InnerBehavior::Err);
    {
        let mut h = w.history.write();
        h.push(Message {
            role: "user".into(),
            content: "u1".into(),
        });
        h.push(Message {
            role: "assistant".into(),
            content: "a1".into(),
        });
    };
    assert!(w.process("u2").await.is_err());
    let h = w.history.read();
    // u2 rolled back; prior pair intact, last entry is assistant.
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].content, "u1");
    assert_eq!(h[1].content, "a1");
}

#[tokio::test]
async fn process_stream_partial_then_error_keeps_heard_prefix() {
    let w = wrapper(InnerBehavior::StreamThenErr(vec![
        "Once ".into(),
        "upon ".into(),
    ]));
    let (tx, _rx) = mpsc::channel::<TextFragment>(16);
    let res = w
        .process_stream("tell a story", tx, CancellationToken::new())
        .await;
    assert!(res.is_err());
    let h = w.history.read();
    // Heard prefix retained as the assistant turn to keep alternation.
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].role, "user");
    assert_eq!(h[1].role, "assistant");
    assert_eq!(h[1].content, "Once upon ");
}

/// History is a rolling window — after N turns the message count
/// stays bounded and the oldest turns evict in order.
#[tokio::test]
async fn history_window_stays_bounded_and_evicts_oldest() {
    let w = wrapper(InnerBehavior::Ok("reply".into())).with_history_max_turns(2);

    for i in 0..5 {
        w.process(&format!("u{i}")).await.unwrap();
    }

    let h = w.history.read();
    assert_eq!(h.len(), 4, "window must hold at most 2 turn pairs");
    // Oldest turns (u0..u2) evicted; u3/u4 retained in order.
    assert_eq!((h[0].role.as_str(), h[0].content.as_str()), ("user", "u3"));
    assert_eq!(h[1].role, "assistant");
    assert_eq!((h[2].role.as_str(), h[2].content.as_str()), ("user", "u4"));
    assert_eq!(h[3].role, "assistant");
}

/// Context turns whose generation fails leave their context entry in
/// history; the window still stays bounded across many of them.
#[tokio::test]
async fn failing_context_turns_keep_history_bounded() {
    let w = wrapper(InnerBehavior::Err).with_history_max_turns(2);
    for i in 0..1000 {
        let (tx, _rx) = mpsc::channel::<TextFragment>(4);
        let _ = w
            .process_context_stream(&format!("event {i}"), tx, CancellationToken::new())
            .await;
    }
    let h = w.history.read();
    assert!(h.len() <= 4, "history grew to {} entries", h.len());
    assert!(h.iter().all(|m| m.role == "system"));
    assert_eq!(h.last().map(|m| m.content.as_str()), Some("event 999"));
}

/// The window is bounded in bytes as well as turns: a few very long
/// entries evict the oldest even when the turn count is under the cap.
#[tokio::test]
async fn history_window_is_bounded_in_bytes() {
    let w = wrapper(InnerBehavior::Ok("reply".into())).with_history_max_turns(50);
    let big = "x".repeat(MAX_HISTORY_BYTES / 2);
    for _ in 0..4 {
        w.process(&big).await.unwrap();
    }
    let h = w.history.read();
    let bytes: usize = h.iter().map(|m| m.content.len()).sum();
    assert!(bytes <= MAX_HISTORY_BYTES, "{bytes} bytes retained");
    assert_eq!(h.len(), 2, "one big pair fits; older pairs evicted");
    assert_eq!(h[0].role, "user");
}

/// A window that would start with an assistant message (odd history
/// after a restore) is trimmed back to a user-first alternation.
#[test]
fn eviction_restores_user_first_alternation() {
    let mut h = vec![
        Message {
            role: "assistant".into(),
            content: "orphan".into(),
        },
        Message {
            role: "user".into(),
            content: "u1".into(),
        },
        Message {
            role: "assistant".into(),
            content: "a1".into(),
        },
        Message {
            role: "user".into(),
            content: "u2".into(),
        },
        Message {
            role: "assistant".into(),
            content: "a2".into(),
        },
    ];
    ConversationMemoryWrapper::evict_oldest_turns(&mut h, 2);
    assert_eq!(h.len(), 4);
    assert_eq!(h[0].role, "user");
    assert_eq!(h[0].content, "u1");
}

#[tokio::test]
async fn process_stream_immediate_error_rolls_back_user_turn() {
    let w = wrapper(InnerBehavior::StreamThenErr(vec![]));
    let (tx, _rx) = mpsc::channel::<TextFragment>(16);
    let res = w
        .process_stream("hello", tx, CancellationToken::new())
        .await;
    assert!(res.is_err());
    assert!(
        w.history.read().is_empty(),
        "stream that produced nothing must not leave a dangling user message"
    );
}

// ── Agent-initiated history entries ──────────────────────

#[tokio::test]
async fn process_context_stream_records_system_then_assistant_no_user() {
    let w = wrapper(InnerBehavior::Ok("The task finished fine.".into()));
    let (tx, _rx) = mpsc::channel::<TextFragment>(16);
    w.process_context_stream(
        "background task 7 completed: ok",
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let h = w.history.read();
    assert_eq!(h.len(), 2);
    assert_eq!(
        h[0].role, "system",
        "context must be a system entry, not a fake user turn"
    );
    assert!(h[0].content.contains("background task 7"));
    assert_eq!(h[1].role, "assistant");
    assert!(!h.iter().any(|m| m.role == "user"));
}

#[tokio::test]
async fn agent_history_entries_do_not_corrupt_window() {
    let w = wrapper(InnerBehavior::Ok("ok".into())).with_history_max_turns(2);
    // A normal turn, an agent Say, injected context, another turn.
    w.process("u1").await.unwrap();
    w.append_assistant_message("proactive reminder").await;
    w.inject_context("silent context note").await;
    w.process("u2").await.unwrap();
    // 6 entries > cap (2 pairs = 4 msgs) -> eviction ran; the
    // retained window must start with a user message even with
    // agent entries adjacent at the boundary. (Scoped read so the
    // guard drops before truncate_history takes the write lock.)
    let (len, first_role, last_role) = {
        let h = w.history.read();
        (
            h.len(),
            h.first().unwrap().role.clone(),
            h.last().unwrap().role.clone(),
        )
    };
    assert!(len <= 4);
    assert_eq!(first_role, "user");
    assert_eq!(last_role, "assistant");
    // Truncation still finds the last assistant entry.
    assert!(w.truncate_history("truncated").is_some());
    assert_eq!(
        w.history.read().last().unwrap().content,
        "truncated... [interrupted by user]"
    );
}

#[tokio::test]
async fn append_assistant_message_is_barge_in_truncatable() {
    let w = wrapper(InnerBehavior::Ok("unused".into()));
    w.append_assistant_message("Your build finished successfully.")
        .await;
    // Simulate barge-in after "Your build".
    let original = w.truncate_history("Your build");
    assert_eq!(
        original.as_deref(),
        Some("Your build finished successfully.")
    );
    let h = w.history.read();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].content, "Your build... [interrupted by user]");
}

#[tokio::test]
async fn truncate_history_zero_ms_and_structured_json_and_prior_turn_protection() {
    let w = wrapper(InnerBehavior::Ok("Hello world, how are you?".into()));
    w.process("hi").await.unwrap();

    // 0ms played replaces unheard response with `[interrupted by user]`.
    assert_eq!(
        w.truncate_history("").as_deref(),
        Some("Hello world, how are you?")
    );
    assert_eq!(
        w.history.read().last().unwrap().content,
        "[interrupted by user]"
    );
    // Repeat truncation on same turn is a no-op.
    assert_eq!(w.truncate_history(""), None);

    // Structured JSON turn stays valid JSON and appends a system note.
    w.append_assistant_message("{\"reply\":\"Speaking a long answer\"}")
        .await;
    assert_eq!(
        w.truncate_history("").as_deref(),
        Some("{\"reply\":\"Speaking a long answer\"}")
    );
    let h = w.history.read().clone();
    assert_eq!(h[h.len() - 2].role, "assistant");
    assert_eq!(
        h[h.len() - 2].content,
        "{\"reply\":\"Speaking a long answer\"}"
    );
    assert_eq!(h.last().unwrap().role, "system");
    assert_eq!(h.last().unwrap().content, "[interrupted by user]");

    // If a new turn fails before producing an assistant response, calling
    // `truncate_history("")` must NOT clobber the previous turn's assistant message.
    let w_err = wrapper(InnerBehavior::Err);
    w_err.append_assistant_message("Previous turn reply").await;
    assert!(w_err.process("new user turn that fails").await.is_err());
    assert_eq!(w_err.truncate_history(""), None);
    assert_eq!(
        w_err.history.read().last().unwrap().content,
        "Previous turn reply"
    );
}

/// Every hook the wrapper has no use for reaches the processor it wraps: a
/// wrapped processor warms up, reports its model and device, and receives
/// the session and its client tools as if it stood alone.
#[tokio::test]
async fn the_wrapper_passes_the_session_hooks_through() {
    #[derive(Default)]
    struct Seen {
        warmed: std::sync::atomic::AtomicBool,
        session: parking_lot::Mutex<Option<String>>,
        tools: parking_lot::Mutex<Vec<String>>,
    }
    struct Hooked(Arc<Seen>);
    #[async_trait]
    impl TextProcessor for Hooked {
        fn name(&self) -> &'static str {
            "hooked"
        }
        async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
            Ok(input.to_string())
        }
        async fn warm_up(&self) -> Result<(), TextProcessorError> {
            self.0
                .warmed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn model_identity(&self) -> Option<voice_pipes_sdk::component::ModelIdentity> {
            Some(voice_pipes_sdk::component::ModelIdentity::from_config(
                "inner-model",
            ))
        }
        async fn device_info(&self) -> Option<voice_pipes_sdk::component::DeviceInfo> {
            Some(voice_pipes_sdk::component::DeviceInfo::config("cuda", None))
        }
        fn bind_session(&self, session: voice_pipes_sdk::agent::SessionHandle) {
            *self.0.session.lock() = Some(session.session_id().to_string());
        }
        fn configure_client_tools(&self, tools: Vec<voice_pipes_sdk::agent::ClientToolSpec>) {
            *self.0.tools.lock() = tools.into_iter().map(|t| t.name).collect();
        }
    }

    let seen = Arc::new(Seen::default());
    let w = ConversationMemoryWrapper::new(
        Box::new(Hooked(Arc::clone(&seen))),
        Box::new(ChatMlTemplate),
        "system",
        None,
    );
    w.warm_up().await.unwrap();
    assert!(seen.warmed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        w.model_identity().await.map(|m| m.id),
        Some("inner-model".into())
    );
    assert_eq!(w.device_info().await.map(|d| d.kind), Some("cuda".into()));

    #[derive(Debug)]
    struct NoSink;
    #[async_trait::async_trait]
    impl voice_pipes_sdk::SessionSink for NoSink {
        async fn client_tool_call(
            &self,
            _call: voice_pipes_sdk::ClientToolCall,
        ) -> Result<(), voice_pipes_sdk::SessionEnded> {
            Ok(())
        }
    }
    w.bind_session(voice_pipes_sdk::SessionHandle::new(Arc::new(NoSink), "s-1"));
    assert_eq!(seen.session.lock().as_deref(), Some("s-1"));
    w.configure_client_tools(vec![voice_pipes_sdk::agent::ClientToolSpec::new(
        "navigate",
        "go somewhere",
        serde_json::json!({ "type": "object" }),
    )]);
    assert_eq!(*seen.tools.lock(), vec!["navigate".to_string()]);
}

/// An empty reply is not an assistant turn: the user message is rolled
/// back, as for a stream that produced nothing, so the history never holds
/// a turn the user did not hear.
#[tokio::test]
async fn an_empty_reply_leaves_no_turn_in_history() {
    let w = wrapper(InnerBehavior::Ok(String::new()));
    assert_eq!(w.process("hello").await.unwrap(), "");
    assert!(w.history.read().is_empty(), "{:?}", w.history.read());
}

/// A streamed reply of several sentences is stored as it was spoken, with
/// the space between sentences, and a barge-in keeps the part played.
#[tokio::test]
async fn a_streamed_reply_is_stored_with_its_spaces_and_truncates_to_the_played_part() {
    use crate::backend::tests::{FakeBackend, word_id, word_tokenizer};
    let words = ["Hello.", "How", "are", "you?"];
    let tokenizer = word_tokenizer(&words);
    let mut script: Vec<u32> = words.iter().map(|w| word_id(&tokenizer, w)).collect();
    script.push(word_id(&tokenizer, "<|im_end|>"));
    let backend = FakeBackend::new(script, tokenizer.get_vocab_size(true));
    let inner = crate::LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        crate::LlmConfig {
            prefill_cache: false,
            repeat_penalty: 1.0,
            ..Default::default()
        },
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();
    let w =
        ConversationMemoryWrapper::new(Box::new(inner), Box::new(ChatMlTemplate), "system", None);

    let (tx, mut rx) = mpsc::channel::<TextFragment>(16);
    w.process_stream("hi", tx, CancellationToken::new())
        .await
        .unwrap();
    let mut spoken = Vec::new();
    while let Ok(frag) = rx.try_recv() {
        spoken.push(frag.text);
    }
    assert!(spoken.len() >= 2, "one fragment per sentence: {spoken:?}");
    assert_eq!(
        w.history.read().last().map(|m| m.content.clone()),
        Some("Hello. How are you?".to_string()),
        "fragments {spoken:?}"
    );

    assert_eq!(
        w.truncate_history("Hello. How").as_deref(),
        Some("Hello. How are you?")
    );
    assert_eq!(
        w.history.read().last().unwrap().content,
        "Hello. How... [interrupted by user]"
    );
}

/// The regression: the window was capped in turns and bytes only, so a
/// history of a few long turns outgrew the model's context and every turn
/// after failed the same way. With a budget, the oldest turns go until the
/// prompt and a full reply fit, and the newest user message stays.
#[tokio::test]
async fn a_history_past_the_window_is_trimmed_to_fit() {
    struct Recording(Arc<parking_lot::Mutex<Vec<String>>>);
    #[async_trait]
    impl TextProcessor for Recording {
        fn name(&self) -> &'static str {
            "recording"
        }
        async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
            self.0.lock().push(input.to_string());
            Ok("ok".into())
        }
    }
    let mut tokenizer = crate::backend::tests::word_tokenizer(&[]);
    tokenizer.with_pre_tokenizer(Some(tokenizers::pre_tokenizers::whitespace::Whitespace {}));
    let count = |text: &str| {
        crate::chat_template::encode_prompt(&tokenizer, text)
            .unwrap()
            .len()
    };

    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let budget = TokenBudget {
        tokenizer: Arc::new(tokenizer.clone()),
        context: 200,
        max_tokens: Arc::new(std::sync::atomic::AtomicI64::new(50)),
    };
    let w = ConversationMemoryWrapper::new(
        Box::new(Recording(Arc::clone(&seen))),
        Box::new(ChatMlTemplate),
        "system",
        None,
    )
    .with_token_budget(budget);
    let long = "word ".repeat(60);
    {
        let mut h = w.history.write();
        for _ in 0..3 {
            h.push(Message {
                role: "user".into(),
                content: long.clone(),
            });
            h.push(Message {
                role: "assistant".into(),
                content: long.clone(),
            });
        }
    }

    w.process("latest question").await.unwrap();
    let sent = seen.lock()[0].clone();
    assert!(count(&sent) + 50 <= 200, "{} tokens sent", count(&sent));
    assert!(sent.contains("latest question"));
    let h = w.history.read();
    assert_eq!(h.first().map(|m| m.role.as_str()), Some("user"));
    assert!(h.len() < 8, "older turns dropped: {} left", h.len());
}
