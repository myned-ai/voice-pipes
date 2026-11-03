use super::*;

/// Each `forward` call's tokens and position.
pub type ForwardCalls = std::sync::Arc<parking_lot::Mutex<Vec<(Vec<u32>, usize)>>>;

/// A deterministic fake backend for unit-testing `LlmProcessor` without a
/// real model.  Each `forward()` call returns the next token ID from a
/// pre-configured sequence encoded as one-hot logits.
pub struct FakeBackend {
    /// Pre-determined token sequence to emit.
    pub tokens: Vec<u32>,
    /// Vocabulary size (logit vector length).
    pub vocab_size: usize,
    /// Current position in `tokens`.
    pos: usize,
    /// Saved position snapshot (for `save_cache` / `reset_cache`).
    saved_pos: Option<usize>,
    /// Every `forward` call's tokens and position, shared so a test can
    /// read it after the backend moved into a processor.
    pub calls: ForwardCalls,
}

impl FakeBackend {
    pub fn new(tokens: Vec<u32>, vocab_size: usize) -> Self {
        Self {
            tokens,
            vocab_size,
            pos: 0,
            saved_pos: None,
            calls: std::sync::Arc::default(),
        }
    }

    /// Returns `true` if `save_cache()` has been called at least once.
    pub fn snapshot_saved(&self) -> bool {
        self.saved_pos.is_some()
    }
}

impl LlmBackend for FakeBackend {
    fn forward(&mut self, tokens: &[u32], pos: usize) -> Result<Tensor, LlmBackendError> {
        self.calls.lock().push((tokens.to_vec(), pos));
        let next = if self.pos < self.tokens.len() {
            self.tokens[self.pos]
        } else {
            0 // fallback -- should hit EOS before this
        };
        self.pos += 1;

        // Build a logits vector with a large value at `next` and zeros elsewhere
        let mut logits = vec![0.0f32; self.vocab_size];
        if (next as usize) < self.vocab_size {
            logits[next as usize] = 100.0;
        }

        Tensor::from_vec(logits, self.vocab_size, &candle_core::Device::Cpu).map_err(|e| {
            LlmBackendError {
                message: format!("fake tensor: {e}"),
                source: None,
            }
        })
    }

    fn continues_batches(&self) -> bool {
        true
    }

    fn save_cache(&mut self) -> usize {
        self.saved_pos = Some(self.pos);
        self.pos
    }

    fn reset_cache(&mut self) {
        self.pos = self.saved_pos.unwrap_or(0);
    }
}

#[test]
fn fake_backend_emits_sequence() {
    let mut backend = FakeBackend::new(vec![5, 10, 15], 100);
    let logits = backend.forward(&[0], 0).unwrap();
    // Token 5 should have the highest logit
    let data: Vec<f32> = logits.to_vec1().unwrap();
    assert!(data[5] > data[0]);

    let logits = backend.forward(&[5], 1).unwrap();
    let data: Vec<f32> = logits.to_vec1().unwrap();
    assert!(data[10] > data[0]);
}

#[test]
fn fake_backend_save_restore() {
    let mut backend = FakeBackend::new(vec![5, 10, 15, 20], 100);
    backend.forward(&[0], 0).unwrap(); // consumes pos 0 -> token 5
    backend.forward(&[5], 1).unwrap(); // consumes pos 1 -> token 10

    let saved = backend.save_cache();
    assert_eq!(saved, 2);

    backend.forward(&[10], 2).unwrap(); // pos 2 -> token 15
    backend.reset_cache(); // back to pos 2

    let logits = backend.forward(&[10], 2).unwrap();
    let data: Vec<f32> = logits.to_vec1().unwrap();
    assert!(data[15] > data[0], "after reset, should replay token 15");
}

#[test]
fn fake_backend_reset_without_save_clears() {
    let mut backend = FakeBackend::new(vec![5, 10], 100);
    backend.forward(&[0], 0).unwrap();
    backend.reset_cache(); // no save -> resets to 0

    let logits = backend.forward(&[0], 0).unwrap();
    let data: Vec<f32> = logits.to_vec1().unwrap();
    assert!(
        data[5] > data[0],
        "after reset, should start from beginning"
    );
}

/// An in-memory tokenizer whose vocabulary is `<unk>` (0), the `ChatML`
/// end token `<|im_end|>` (1), then `words` from 2 in order. Each word is
/// one token and decoding joins them with spaces, so a test that scripts
/// the backend's output by id knows exactly what text comes out, with no
/// model file on disk.
pub fn word_tokenizer(words: &[&str]) -> tokenizers::Tokenizer {
    use tokenizers::models::wordlevel::WordLevel;
    let vocab = ["<unk>", "<|im_end|>"]
        .iter()
        .chain(words)
        .zip(0u32..)
        .map(|(w, id)| ((*w).to_string(), id))
        .collect();
    let model = WordLevel::builder()
        .vocab(vocab)
        .unk_token("<unk>".to_string())
        .build()
        .expect("wordlevel build");
    tokenizers::Tokenizer::new(model)
}

/// The id [`word_tokenizer`] gives `word`.
pub fn word_id(tokenizer: &tokenizers::Tokenizer, word: &str) -> u32 {
    tokenizer
        .token_to_id(word)
        .expect("a word the tokenizer was built with")
}
