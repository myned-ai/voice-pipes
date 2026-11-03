use super::*;
use crate::backend::tests::{FakeBackend, word_tokenizer};
use crate::chat_template::ChatMlTemplate;

#[test]
fn prefill_returns_token_count() {
    let tokenizer = word_tokenizer(&[]);

    let vocab_size = tokenizer.get_vocab_size(true);
    // The fake backend just needs to handle one forward call.
    let mut backend = FakeBackend::new(vec![1; 256], vocab_size);

    let template = ChatMlTemplate;
    let system = "You are a helpful assistant.";

    let tokens = prefill_system_prompt(&mut backend, &tokenizer, &template, system).unwrap();

    assert!(
        !tokens.is_empty(),
        "prefill should produce at least one token"
    );
    // Verify save_cache was called (FakeBackend tracks this).
    assert!(
        backend.snapshot_saved(),
        "save_cache must be called after prefill"
    );
}
