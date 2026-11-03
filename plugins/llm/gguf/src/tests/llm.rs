use super::backend::LlmBackendError;
use super::backend::tests::{FakeBackend, word_id, word_tokenizer};
use super::*;
use chat_template::ChatMlTemplate;

/// A backend scripted to emit `words` and then end the turn, with the
/// tokenizer that decodes them.
fn scripted(words: &[&str]) -> (Tokenizer, FakeBackend) {
    let tokenizer = word_tokenizer(words);
    let mut seq: Vec<u32> = words.iter().map(|w| word_id(&tokenizer, w)).collect();
    seq.push(word_id(&tokenizer, "<|im_end|>"));
    let backend = FakeBackend::new(seq, tokenizer.get_vocab_size(true));
    (tokenizer, backend)
}

#[tokio::test]
async fn fake_backend_batch_process() {
    let (tokenizer, backend) = scripted(&["Hello", "there."]);

    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let result = proc.process("test").await.unwrap();
    assert_eq!(result, "Hello there.");
}

#[tokio::test]
async fn fake_backend_stream_emits_end_fragment() {
    let (tokenizer, backend) = scripted(&["Hi."]);

    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    proc.process_stream("test", tx, cancel).await.unwrap();

    let mut got_end = false;
    while let Some(frag) = rx.recv().await {
        if frag.is_end_of_response {
            got_end = true;
        }
    }
    assert!(
        got_end,
        "stream must emit a fragment with is_end_of_response=true"
    );
}

#[tokio::test]
async fn fake_backend_cancel_stops_generation() {
    // A long run of ordinary words that cancellation must cut short.
    let tokenizer = tiny_wordlevel_tokenizer(200);
    let seq: Vec<u32> = (0..200).map(|i| 3 + i).collect();
    let backend = FakeBackend::new(seq, tokenizer.get_vocab_size(true));

    let config = LlmConfig {
        max_tokens: 500,
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (tx, mut rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    cancel.cancel(); // cancel immediately

    proc.process_stream("test", tx, cancel).await.unwrap();

    // Should get at most the final flush fragment
    let mut count = 0;
    while rx.recv().await.is_some() {
        count += 1;
    }
    assert!(
        count <= 1,
        "cancelled stream should emit at most 1 fragment, got {count}"
    );
}

#[test]
fn repeat_penalty_suppresses_recent_tokens() {
    let logits = candle_core::Tensor::from_vec(
        vec![1.0f32, 2.0, 3.0, 4.0, 5.0],
        5,
        &candle_core::Device::Cpu,
    )
    .unwrap();

    // Penalise token 4 (logit 5.0 > 0, so divide by 2.0)
    let adjusted = LlmProcessor::apply_repeat_penalty(&logits, &[4], 2.0).unwrap();
    let data: Vec<f32> = adjusted.to_vec1().unwrap();
    assert!(
        (data[4] - 2.5).abs() < 1e-5,
        "5.0 / 2.0 = 2.5, got {}",
        data[4]
    );
    assert!(
        (data[0] - 1.0).abs() < 1e-5,
        "untouched token should stay 1.0"
    );
}

#[test]
fn repeat_penalty_one_is_noop() {
    let logits =
        candle_core::Tensor::from_vec(vec![1.0f32, 2.0, 3.0], 3, &candle_core::Device::Cpu)
            .unwrap();

    let adjusted = LlmProcessor::apply_repeat_penalty(&logits, &[1], 1.0).unwrap();
    let data: Vec<f32> = adjusted.to_vec1().unwrap();
    assert!((data[1] - 2.0).abs() < 1e-5);
}

#[test]
fn repeat_penalty_negative_logits_multiply() {
    let logits =
        candle_core::Tensor::from_vec(vec![-2.0f32, 1.0], 2, &candle_core::Device::Cpu).unwrap();

    // Negative logits get multiplied (made more negative)
    let adjusted = LlmProcessor::apply_repeat_penalty(&logits, &[0], 2.0).unwrap();
    let data: Vec<f32> = adjusted.to_vec1().unwrap();
    assert!(
        (data[0] - (-4.0)).abs() < 1e-5,
        "-2.0 * 2.0 = -4.0, got {}",
        data[0]
    );
}

#[tokio::test]
async fn fake_backend_stream_tracks_first_sentence_chars() {
    let (tokenizer, backend) = scripted(&["Hello", "world.", "This", "is", "test."]);

    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let (audit_tx, mut audit_rx) = mpsc::channel(32);

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap()
    .with_audit_tx(Some(audit_tx));

    let (tx, mut rx) = mpsc::channel(32);
    let cancel = CancellationToken::new();
    proc.process_stream("test", tx, cancel).await.unwrap();

    // Consume output stream
    while rx.recv().await.is_some() {}

    // Verify audit log has the timing event with first_sentence_chars
    let mut found_metric = false;
    while let Ok(event) = audit_rx.try_recv() {
        if event.event_type == "llm_detail"
            && let Some(chars) = event.payload.get("first_sentence_chars")
        {
            let val = chars
                .as_u64()
                .expect("first_sentence_chars must be a number");
            assert!(val > 0, "first_sentence_chars must be greater than zero");
            found_metric = true;
        }
    }
    assert!(
        found_metric,
        "must find first_sentence_chars in llm_detail trace event"
    );
}

#[test]
fn unknown_device_errors() {
    let result = LlmProcessor::resolve_device("tpu");
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("tpu"));
}

/// Byte-level BPE tokens are not individually valid UTF-8 — a
/// multi-byte character split across two tokens must stream through
/// without U+FFFD corruption (a naive per-token decode garbles it).
#[tokio::test]
async fn stream_multibyte_grapheme_across_tokens_no_corruption() {
    use tokenizers::models::bpe::BpeBuilder;
    use tokenizers::pre_tokenizers::byte_level::ByteLevel;

    // GPT-2 byte-level alphabet: "é" (bytes 0xC3 0xA9) is represented as
    // the two visible chars "Ã" and "©" — here as two separate tokens,
    // so neither token alone decodes to valid UTF-8.
    let vocab: Vec<(String, u32)> = vec![
        ("<unk>".into(), 0),
        ("Ã".into(), 1),
        ("©".into(), 2),
        (".".into(), 3),
        ("<|im_end|>".into(), 4),
    ];
    let bpe = BpeBuilder::new()
        .vocab_and_merges(
            vocab
                .into_iter()
                .collect::<tokenizers::models::bpe::Vocab>(),
            vec![],
        )
        .unk_token("<unk>".into())
        .build()
        .expect("bpe build");
    let mut tokenizer = Tokenizer::new(bpe);
    tokenizer.with_decoder(Some(ByteLevel::default()));

    // Emit the two halves of "é", then ".", then EOS.
    let backend = FakeBackend::new(vec![1, 2, 3, 4], 5);

    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (tx, mut rx) = mpsc::channel(32);
    proc.process_stream(".", tx, CancellationToken::new())
        .await
        .unwrap();

    let mut streamed = String::new();
    while let Some(frag) = rx.recv().await {
        streamed.push_str(&frag.text);
    }
    assert!(
        !streamed.contains('\u{FFFD}'),
        "streamed text contains U+FFFD: {streamed:?}"
    );
    assert_eq!(streamed, "é.");
}

/// The incremental `decode_stream` path stays correct over a longer
/// sequence with several multi-byte reconstructions — no dropped,
/// duplicated, or corrupted pieces as the token count grows.
#[tokio::test]
async fn stream_repeated_multibyte_sequence_no_corruption() {
    use tokenizers::models::bpe::BpeBuilder;
    use tokenizers::pre_tokenizers::byte_level::ByteLevel;

    let vocab: Vec<(String, u32)> = vec![
        ("<unk>".into(), 0),
        ("Ã".into(), 1),
        ("©".into(), 2),
        (".".into(), 3),
        ("<|im_end|>".into(), 4),
    ];
    let bpe = BpeBuilder::new()
        .vocab_and_merges(
            vocab
                .into_iter()
                .collect::<tokenizers::models::bpe::Vocab>(),
            vec![],
        )
        .unk_token("<unk>".into())
        .build()
        .expect("bpe build");
    let mut tokenizer = Tokenizer::new(bpe);
    tokenizer.with_decoder(Some(ByteLevel::default()));

    // "é" "é" "é" "." then EOS — each "é" spans two tokens.
    let backend = FakeBackend::new(vec![1, 2, 1, 2, 1, 2, 3, 4], 5);
    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };
    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (tx, mut rx) = mpsc::channel(32);
    proc.process_stream(".", tx, CancellationToken::new())
        .await
        .unwrap();

    let mut streamed = String::new();
    while let Some(frag) = rx.recv().await {
        streamed.push_str(&frag.text);
    }
    assert!(!streamed.contains('\u{FFFD}'), "corruption: {streamed:?}");
    assert_eq!(streamed, "ééé.");
}

/// A backend whose every forward pass stalls, simulating slow candle
/// inference on CPU.
struct SlowBackend {
    inner: FakeBackend,
    delay: std::time::Duration,
}

impl LlmBackend for SlowBackend {
    fn forward(
        &mut self,
        tokens: &[u32],
        pos: usize,
    ) -> Result<candle_core::Tensor, LlmBackendError> {
        std::thread::sleep(self.delay);
        self.inner.forward(tokens, pos)
    }

    fn save_cache(&mut self) -> usize {
        self.inner.save_cache()
    }

    fn reset_cache(&mut self) {
        self.inner.reset_cache();
    }
}

/// Build a minimal in-memory `WordLevel` tokenizer, so no test needs a
/// model file.
/// Vocab: `<unk>`=0, `<|im_end|>`=1 (`ChatML` EOS), "test"=2, then
/// `w0..wN` from 3.
fn tiny_wordlevel_tokenizer(n_words: u32) -> Tokenizer {
    use tokenizers::models::wordlevel::WordLevel;

    let mut vocab = vec![
        ("<unk>".to_string(), 0),
        ("<|im_end|>".to_string(), 1),
        ("test".to_string(), 2),
    ];
    for i in 0..n_words {
        vocab.push((format!("w{i}"), 3 + i));
    }
    let model = WordLevel::builder()
        // `.collect()` because the builder takes tokenizers' own
        // AHashMap vocab type.
        .vocab(vocab.into_iter().collect())
        .unk_token("<unk>".to_string())
        .build()
        .expect("wordlevel build");
    Tokenizer::new(model)
}

/// Generation must run on the blocking pool, not inline on the tokio
/// worker. On this single-threaded test runtime an
/// inline generation loop would freeze the ticker task completely; via
/// `spawn_blocking` the ticker keeps making progress while the slow
/// backend grinds through its forward passes.
#[tokio::test]
async fn slow_inference_does_not_block_runtime() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let n_words = 20u32;
    let tokenizer = tiny_wordlevel_tokenizer(n_words);
    let eos_id = tokenizer.token_to_id("<|im_end|>").unwrap();

    // ~20 forward passes at 20 ms each ≈ 400 ms of "inference".
    let mut seq: Vec<u32> = (0..n_words).map(|i| 3 + i).collect();
    seq.push(eos_id);

    let vocab_size = tokenizer.get_vocab_size(true);
    let backend = SlowBackend {
        inner: FakeBackend::new(seq, vocab_size),
        delay: std::time::Duration::from_millis(20),
    };

    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let ticks = Arc::new(AtomicU64::new(0));
    let ticker = tokio::spawn({
        let ticks = std::sync::Arc::clone(&ticks);
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                ticks.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    let (tx, mut rx) = mpsc::channel(64);
    proc.process_stream("test", tx, CancellationToken::new())
        .await
        .unwrap();
    ticker.abort();
    while rx.recv().await.is_some() {}

    let observed = ticks.load(Ordering::Relaxed);
    assert!(
        observed >= 10,
        "concurrent task starved during generation: only {observed} ticks"
    );
}

/// Time to first token counts the prompt ingest. Measured from generation
/// alone it read ~0 ms while a CPU spent seconds ingesting the prompt, and
/// the session report charged that wait to TTS.
#[tokio::test]
async fn time_to_first_token_includes_the_prompt_ingest() {
    let tokenizer = tiny_wordlevel_tokenizer(4);
    let eos_id = tokenizer.token_to_id("<|im_end|>").unwrap();
    let vocab_size = tokenizer.get_vocab_size(true);
    let ingest = std::time::Duration::from_millis(60);
    // The first forward pass is the prompt ingest; its logits give the first
    // token, so nothing but the ingest precedes it.
    let backend = SlowBackend {
        inner: FakeBackend::new(vec![3, 4, eos_id], vocab_size),
        delay: ingest,
    };
    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };
    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (tx, mut rx) = mpsc::channel(16);
    proc.process_stream("test", tx, CancellationToken::new())
        .await
        .unwrap();
    let first = rx.recv().await.expect("a fragment");
    let ttft = first.first_token_ms.expect("the first fragment carries TTFT");
    assert!(
        ttft >= ingest.as_secs_f64() * 1000.0,
        "TTFT {ttft:.1} ms leaves out the {} ms prompt ingest",
        ingest.as_millis()
    );
}

/// A backend that counts forward passes, so tests can assert generation
/// stopped early instead of grinding to `max_tokens`.
struct CountingBackend {
    inner: FakeBackend,
    forwards: Arc<std::sync::atomic::AtomicUsize>,
}

impl LlmBackend for CountingBackend {
    fn forward(
        &mut self,
        tokens: &[u32],
        pos: usize,
    ) -> Result<candle_core::Tensor, LlmBackendError> {
        self.forwards
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.forward(tokens, pos)
    }

    fn save_cache(&mut self) -> usize {
        self.inner.save_cache()
    }

    fn reset_cache(&mut self) {
        self.inner.reset_cache();
    }
}

/// Vocab: `<unk>`=0, `<|im_end|>`=1, "test"=2, "hello."=3 — every
/// generated "hello." token ends a sentence, so the stream emits a
/// fragment per token (exercising the send path continuously).
fn sentence_per_token_tokenizer() -> Tokenizer {
    use tokenizers::models::wordlevel::WordLevel;

    let vocab = vec![
        ("<unk>".to_string(), 0),
        ("<|im_end|>".to_string(), 1),
        ("test".to_string(), 2),
        ("hello.".to_string(), 3),
    ];
    let model = WordLevel::builder()
        .vocab(vocab.into_iter().collect())
        .unk_token("<unk>".to_string())
        .build()
        .expect("wordlevel build");
    Tokenizer::new(model)
}

/// A dropped fragment receiver must stop generation promptly — a send
/// path that discards the closed-channel error would grind on to
/// `max_tokens` while holding the process-wide backend lock.
#[tokio::test]
async fn dropped_receiver_stops_generation_early() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let tokenizer = sentence_per_token_tokenizer();
    let vocab_size = tokenizer.get_vocab_size(true);

    // 200 sentence-ending tokens, then EOS: without the fix the loop
    // runs all 200 forwards; with it, the first failed send stops it.
    let mut seq = vec![3u32; 200];
    seq.push(1);

    let forwards = Arc::new(AtomicUsize::new(0));
    let backend = CountingBackend {
        inner: FakeBackend::new(seq, vocab_size),
        forwards: std::sync::Arc::clone(&forwards),
    };

    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let (tx, rx) = mpsc::channel(1);
    drop(rx); // consumer gone before generation starts

    proc.process_stream("test", tx, CancellationToken::new())
        .await
        .unwrap();

    let observed = forwards.load(Ordering::Relaxed);
    assert!(
        observed < 20,
        "generation kept running after the consumer dropped: {observed} forwards"
    );
}

/// A stalled (alive but never-polling) consumer must not park the
/// generation thread forever inside the backend lock — cancelling the
/// turn has to unblock it.
#[tokio::test]
async fn stalled_consumer_cancel_unblocks_generation() {
    let tokenizer = sentence_per_token_tokenizer();
    let vocab_size = tokenizer.get_vocab_size(true);

    let mut seq = vec![3u32; 200];
    seq.push(1);
    let backend = FakeBackend::new(seq, vocab_size);

    let config = LlmConfig {
        prefill_cache: false,
        repeat_penalty: 1.0,
        ..Default::default()
    };

    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        config,
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    // Capacity-1 channel with a live receiver that never polls: the
    // first fragment fills the slot, the second parks the sender.
    let (tx, rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let cancel_task = cancel.clone();

    let handle = tokio::spawn(async move { proc.process_stream("test", tx, cancel_task).await });

    // Let generation reach the parked send, then cancel the turn.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    cancel.cancel();

    let res = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("generation must unblock promptly after cancel (not park in the lock)")
        .expect("generation task panicked");
    assert!(
        res.is_ok(),
        "cancelled turn should complete cleanly: {res:?}"
    );

    drop(rx);
}

/// Two sessions must share one loaded model.
///
/// Each session builds its own text processors so histories stay separate;
/// before the shared cache that meant re-reading the whole GGUF on every
/// connect, which on a CPU-bound container took long enough that the caller
/// spoke into a session that was not yet listening. Gated on a real model:
/// set `VP_TEST_GGUF` (and `VP_TEST_TOKENIZER` when it is not beside it).
#[test]
#[ignore = "needs a real GGUF; set VP_TEST_GGUF"]
fn a_second_session_reuses_the_loaded_model() {
    use voice_pipes_sdk::TextProcessorFactory;
    let Ok(model) = std::env::var("VP_TEST_GGUF") else {
        panic!("set VP_TEST_GGUF")
    };
    let mut options = std::collections::HashMap::new();
    options.insert("model".to_string(), serde_json::json!(model));
    if let Ok(tok) = std::env::var("VP_TEST_TOKENIZER") {
        options.insert("tokenizer_path".to_string(), serde_json::json!(tok));
    }
    let cfg = voice_pipes_sdk::TextProcessorConfig::new("llm").with_options(options);
    let chunking = voice_pipes_sdk::TextChunkingConfig::default();

    let first = std::time::Instant::now();
    let _a = crate::LlmFactory
        .build(
            &cfg,
            &voice_pipes_sdk::BuildContext::new().with_text_chunking(chunking.clone()),
        )
        .expect("first build loads the model");
    let cold = first.elapsed();

    let second = std::time::Instant::now();
    let _b = crate::LlmFactory
        .build(
            &cfg,
            &voice_pipes_sdk::BuildContext::new().with_text_chunking(chunking.clone()),
        )
        .expect("second build reuses it");
    let warm = second.elapsed();

    eprintln!("cold={cold:?} warm={warm:?}");
    assert!(
        warm * 5 < cold,
        "the second session should reuse the loaded weights, not reload them \
         (cold={cold:?}, warm={warm:?})"
    );
}

/// `prefill_cache` is a speed-up, not a different model: on a Llama-family
/// GGUF (the architecture that continues a batch at an offset), greedy
/// turns answer the same with it as without it, the second turn too, which
/// starts from the restored snapshot. Gated on a real model: set
/// `VP_TEST_GGUF` to a `ChatML` model such as `SmolLM2` (and
/// `VP_TEST_TOKENIZER` when its tokenizer is not beside it).
#[tokio::test]
#[ignore = "needs a real GGUF; set VP_TEST_GGUF"]
async fn prefill_cache_answers_as_without_it() {
    use voice_pipes_sdk::TextProcessorFactory;
    let Ok(model) = std::env::var("VP_TEST_GGUF") else {
        panic!("set VP_TEST_GGUF")
    };
    let system = "You answer in one short sentence.";
    let build = |prefill: bool| {
        let mut options = std::collections::HashMap::new();
        options.insert("model".to_string(), serde_json::json!(model));
        options.insert("chat_template".to_string(), serde_json::json!("chatml"));
        options.insert("system_prompt".to_string(), serde_json::json!(system));
        options.insert("temperature".to_string(), serde_json::json!(0.0));
        options.insert("max_tokens".to_string(), serde_json::json!(24));
        options.insert("prefill_cache".to_string(), serde_json::json!(prefill));
        if let Ok(tok) = std::env::var("VP_TEST_TOKENIZER") {
            options.insert("tokenizer_path".to_string(), serde_json::json!(tok));
        }
        crate::LlmFactory
            .build(
                &voice_pipes_sdk::TextProcessorConfig::new("llm").with_options(options),
                &voice_pipes_sdk::BuildContext::new(),
            )
            .expect("the model loads")
    };
    let prompt = ChatMlTemplate.format_messages(&[
        chat_template::Message {
            role: "system".into(),
            content: system.into(),
        },
        chat_template::Message {
            role: "user".into(),
            content: "What is the capital of France?".into(),
        },
    ]);
    let (plain, prefilled) = (build(false), build(true));
    for turn in 1..=2 {
        let want = plain.process(&prompt).await.expect("a plain turn");
        let got = prefilled.process(&prompt).await.expect("a prefilled turn");
        eprintln!("turn {turn}: {got:?}");
        assert!(
            !want.trim().is_empty(),
            "turn {turn}: the model said nothing"
        );
        assert_eq!(got, want, "turn {turn}: prefill_cache changed the answer");
    }
}

/// The store is built from the processor's own schema: it holds only the
/// fields the config gave it.
#[test]
fn the_store_built_from_the_schema_holds_the_given_fields() {
    use voice_pipes_sdk::TextProcessorFactory;
    use voice_pipes_sdk::params::AtomicParamStore;
    let cfg: voice_pipes_sdk::TextProcessorConfig =
        serde_json::from_value(serde_json::json!({ "type": "llm", "temperature": 0.5 }))
            .expect("a text-processor config");
    let store = AtomicParamStore::from_schema(&crate::LlmFactory.provider_schema(&cfg));
    assert_eq!(store.float("temperature"), Some(0.5));
    assert!(store.string("system_prompt").is_none());
    assert!(store.int("max_tokens").is_none());
}

/// Every wizard entry names its files through the model registry and writes
/// a config the factory accepts, naming no tokenizer path: the processor
/// finds it next to the model. 360M comes first, ahead of the other sizes.
#[test]
fn the_wizard_entries_write_configs_the_factory_accepts() {
    use voice_pipes_sdk::{TextProcessorFactory, WizardDeploy};
    let choices = crate::wizard_choices();
    let keys: Vec<_> = choices
        .iter()
        .map(|c| match &c.deploy {
            WizardDeploy::Registry { key } => key.as_str(),
            other => panic!("expected a registry entry, got {other:?}"),
        })
        .collect();
    assert_eq!(keys, ["smollm2-360m", "smollm2-1.7b", "smollm2-135m"]);
    assert!(choices.windows(2).all(|w| w[0].menu_rank < w[1].menu_rank));
    assert_eq!(
        choices[0].render_defaults("type", "llm").model_path.as_deref(),
        Some("models/smollm2-360m/SmolLM2-360M-Instruct-Q4_K_M.gguf")
    );
    for choice in &choices {
        let rendered = choice.render_defaults("type", "llm");
        assert!(!rendered.toml.contains("tokenizer"), "{}", rendered.toml);
        let table: toml::Table = rendered.toml.parse().expect("valid TOML");
        let cfg: voice_pipes_sdk::TextProcessorConfig =
            serde_json::from_value(serde_json::to_value(table).expect("JSON")).expect("a config");
        let errors = crate::LlmFactory.validate(&cfg);
        assert!(
            errors
                .iter()
                .all(|e| e.message.contains("not found") || e.message.contains("exist")),
            "only the not-yet-downloaded model may be missing: {errors:?}"
        );
    }
}

/// A key the processor never read (`tokenizer`, once shown in the docs in
/// place of `tokenizer_path`) is refused against its own key.
#[test]
fn an_unknown_option_is_refused() {
    use voice_pipes_sdk::TextProcessorFactory;
    let cfg = voice_pipes_sdk::TextProcessorConfig::new("llm").with_options(
        [
            ("model".to_string(), serde_json::json!("missing.gguf")),
            ("tokenizer".to_string(), serde_json::json!("tokenizer.json")),
            ("tokenizer_path".to_string(), serde_json::json!("t.json")),
        ]
        .into_iter()
        .collect(),
    );
    let fields: Vec<String> = crate::LlmFactory
        .validate(&cfg)
        .into_iter()
        .map(|e| e.field)
        .collect();
    assert!(
        fields.contains(&"text_processor.tokenizer".to_string()),
        "{fields:?}"
    );
    assert!(
        !fields.contains(&"text_processor.tokenizer_path".to_string()),
        "{fields:?}"
    );
}

/// A template or device the processor does not know is refused in
/// `validate`, not run as `ChatML` on the CPU: "gemma" is not "gemma3", and a
/// model prompted in the wrong format answers nonsense with no error.
#[test]
fn an_unknown_template_or_device_is_refused() {
    use voice_pipes_sdk::TextProcessorFactory;
    let cfg = |key: &str, value: &str| {
        voice_pipes_sdk::TextProcessorConfig::new("llm").with_options(
            [
                ("model".to_string(), serde_json::json!("missing.gguf")),
                (key.to_string(), serde_json::json!(value)),
            ]
            .into_iter()
            .collect(),
        )
    };
    for (key, value) in [
        ("chat_template", "gemma"),
        ("chat_template", "llama"),
        ("device", "tpu"),
    ] {
        let fields: Vec<String> = crate::LlmFactory
            .validate(&cfg(key, value))
            .into_iter()
            .map(|e| e.field)
            .collect();
        assert!(
            fields.contains(&format!("text_processor.{key}")),
            "{key}={value}: {fields:?}"
        );
    }
    for name in crate::chat_template::NAMES {
        let fields: Vec<String> = crate::LlmFactory
            .validate(&cfg("chat_template", name))
            .into_iter()
            .map(|e| e.field)
            .collect();
        assert!(
            !fields.contains(&"text_processor.chat_template".to_string()),
            "{name}"
        );
    }
}

/// A model that ends its turn at once replies with nothing. The prompt it
/// was given is chat-template markup and must never come back as the reply.
#[tokio::test]
async fn a_model_that_generates_nothing_replies_with_nothing() {
    let (tokenizer, backend) = scripted(&[]);
    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer,
        Box::new(ChatMlTemplate),
        LlmConfig {
            prefill_cache: false,
            repeat_penalty: 1.0,
            ..Default::default()
        },
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();
    let prompt = "<|im_start|>user\nhello<|im_end|>\n<|im_start|>assistant\n";
    assert_eq!(proc.process(prompt).await.unwrap(), "");
}

/// The regression: with `prefill_cache` on, the system prompt was prefilled
/// and its KV snapshot restored each turn, then the whole prompt was fed
/// again from position 0, which discards the snapshot. A prompt that starts
/// with the prefilled tokens is now fed from where they end.
#[tokio::test]
async fn a_prefilled_system_prompt_is_not_fed_again() {
    let mut tokenizer = word_tokenizer(&["be", "brief", "hi"]);
    tokenizer.with_pre_tokenizer(Some(tokenizers::pre_tokenizers::whitespace::Whitespace {}));
    let end = word_id(&tokenizer, "<|im_end|>");
    // One forward for the prefill, then the turn's first ends it.
    let backend = FakeBackend::new(vec![end, end], tokenizer.get_vocab_size(true));
    let calls = std::sync::Arc::clone(&backend.calls);
    let proc = LlmProcessor::new(
        Box::new(backend),
        tokenizer.clone(),
        Box::new(ChatMlTemplate),
        LlmConfig {
            prefill_cache: true,
            system_prompt: "be brief".into(),
            repeat_penalty: 1.0,
            ..Default::default()
        },
        voice_pipes_sdk::TextChunkingConfig::default(),
    )
    .unwrap();

    let prompt = ChatMlTemplate.format_messages(&[
        chat_template::Message {
            role: "system".into(),
            content: "be brief".into(),
        },
        chat_template::Message {
            role: "user".into(),
            content: "hi".into(),
        },
    ]);
    proc.process(&prompt).await.unwrap();

    let calls = calls.lock().clone();
    let (prefix, at) = &calls[0];
    assert_eq!(*at, 0, "the prefill starts the cache");
    let (turn, from) = &calls[1];
    assert_eq!(*from, prefix.len(), "the turn continues after the prefix");
    let all = crate::chat_template::encode_prompt(&tokenizer, &prompt).unwrap();
    assert_eq!([prefix.as_slice(), turn.as_slice()].concat(), all);
}

#[tokio::test]
async fn metric_conformance() {
    let run = |tx: tokio::sync::mpsc::Sender<voice_pipes_sdk::TextFragment>,
               _cancel: tokio_util::sync::CancellationToken| async move {
        let delay = std::time::Duration::from_millis(25);
        tokio::time::sleep(delay).await;

        let mut frag1 = voice_pipes_sdk::TextFragment::new("Hello");
        frag1.first_token_ms = Some(25.0);
        tx.send(frag1).await.unwrap();

        let mut frag2 = voice_pipes_sdk::TextFragment::new(" world");
        frag2.is_end_of_response = true;
        frag2.token_count = Some(2);
        tx.send(frag2).await.unwrap();
        Ok(())
    };

    let ttft = voice_pipes_sdk::testing::assert_llm_metrics_conformance(
        run,
        std::time::Duration::from_millis(25),
    )
    .await
    .unwrap();
    assert_eq!(ttft, 25.0);
}

