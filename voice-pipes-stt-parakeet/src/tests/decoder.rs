use super::*;

#[test]
fn argmax_basic() {
    assert_eq!(argmax(&[0.1, 0.9, 0.3]), Some(1));
    assert_eq!(argmax(&[0.5]), Some(0));
    assert_eq!(argmax(&[-1.0, -0.5, -2.0]), Some(1));
}

#[test]
fn argmax_empty() {
    assert_eq!(argmax(&[]), None);
}

// ── Greedy walk with scripted logits ─────────────────────

const VOCAB: usize = 4;
const BLANK: usize = 3;
/// Duration head with five classes: advance 0..=4 frames.
const DURATIONS: usize = 5;

fn empty_state() -> DecoderState {
    DecoderState {
        states_1: vec![],
        states_1_shape: vec![0],
        states_2: vec![],
        states_2_shape: vec![0],
    }
}

fn logits_for(token: usize, duration: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; VOCAB + DURATIONS];
    v[token] = 1.0;
    v[VOCAB + duration] = 1.0;
    v
}

/// Answers each `(frame, prev_token)` query from a table; anything not
/// scripted is a blank that advances one frame.
struct Scripted {
    table: HashMap<(usize, i64), (usize, usize)>,
    calls: Vec<(usize, i64)>,
}

impl Step for Scripted {
    fn step(
        &mut self,
        frame: usize,
        prev_token: i64,
        state: DecoderState,
    ) -> Result<(Vec<f32>, DecoderState), SttError> {
        self.calls.push((frame, prev_token));
        let (token, duration) = self
            .table
            .get(&(frame, prev_token))
            .copied()
            .unwrap_or((BLANK, 1));
        Ok((logits_for(token, duration), state))
    }
}

fn params(max_tokens_per_step: usize) -> GreedyParams {
    GreedyParams {
        vocab_size: VOCAB,
        blank_idx: BLANK,
        max_tokens_per_step,
    }
}

/// A non-blank token's duration moves the walk by that many frames; a
/// zero duration keeps it on the frame; a blank always moves at least one.
#[test]
fn greedy_walk_applies_durations_to_emitted_tokens() {
    let blank = BLANK as i64;
    let mut s = Scripted {
        table: HashMap::from([
            // frame 0: token 0 covering 2 frames -> next step at frame 2
            ((0, blank), (0, 2)),
            // frame 2: token 1 with duration 0 -> stay, then blank for 3
            ((2, 0), (1, 0)),
            ((2, 1), (BLANK, 3)),
            // frame 5: blank with duration 0 must still advance by one
            ((5, 1), (BLANK, 0)),
            // frame 6: token 2 covering 4 frames -> reaches t_max = 10
            ((6, 1), (2, 4)),
        ]),
        calls: vec![],
    };
    let tokens = greedy_decode(&mut s, empty_state(), 10, &params(10)).unwrap();
    let got: Vec<(u32, usize)> = tokens.iter().map(|t| (t.token_id, t.frame_index)).collect();
    assert_eq!(got, vec![(0, 0), (1, 2), (2, 6)]);
    assert_eq!(s.calls, vec![(0, blank), (2, 0), (2, 1), (5, 1), (6, 1)]);
}

/// Token logits with no usable value (a broken joint output) are an error,
/// not token 0 emitted as speech.
#[test]
fn greedy_walk_refuses_logits_with_no_usable_value() {
    struct NanJoint;
    impl Step for NanJoint {
        fn step(
            &mut self,
            _frame: usize,
            _prev: i64,
            state: DecoderState,
        ) -> Result<(Vec<f32>, DecoderState), SttError> {
            let mut v = vec![f32::NAN; VOCAB];
            v.extend(std::iter::repeat_n(0.0, DURATIONS));
            Ok((v, state))
        }
    }
    let err = greedy_decode(&mut NanJoint, empty_state(), 3, &params(4)).unwrap_err();
    assert!(matches!(err, SttError::InferenceFailed(_)), "{err:?}");
}

/// A frame that keeps producing zero-duration tokens is left after the
/// per-frame cap, so the walk always terminates.
#[test]
fn greedy_walk_leaves_a_frame_after_the_token_cap() {
    struct AlwaysToken;
    impl Step for AlwaysToken {
        fn step(
            &mut self,
            _frame: usize,
            _prev: i64,
            state: DecoderState,
        ) -> Result<(Vec<f32>, DecoderState), SttError> {
            Ok((logits_for(2, 0), state))
        }
    }
    let tokens = greedy_decode(&mut AlwaysToken, empty_state(), 3, &params(4)).unwrap();
    assert_eq!(tokens.len(), 12, "four tokens on each of three frames");
    assert_eq!(tokens[3].frame_index, 0);
    assert_eq!(tokens[4].frame_index, 1);
    assert_eq!(tokens[11].frame_index, 2);
}

/// Without a duration head every blank advances one frame and tokens
/// stay on their frame until the cap.
#[test]
fn greedy_walk_without_duration_head_steps_one_frame_per_blank() {
    struct NoDurations;
    impl Step for NoDurations {
        fn step(
            &mut self,
            frame: usize,
            prev_token: i64,
            state: DecoderState,
        ) -> Result<(Vec<f32>, DecoderState), SttError> {
            let mut v = vec![0.0f32; VOCAB];
            let token = if frame == 1 && prev_token == BLANK as i64 {
                1
            } else {
                BLANK
            };
            v[token] = 1.0;
            Ok((v, state))
        }
    }
    let tokens = greedy_decode(&mut NoDurations, empty_state(), 3, &params(10)).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].frame_index, 1);
}

#[test]
fn greedy_walk_rejects_short_logits() {
    struct Short;
    impl Step for Short {
        fn step(
            &mut self,
            _frame: usize,
            _prev: i64,
            state: DecoderState,
        ) -> Result<(Vec<f32>, DecoderState), SttError> {
            Ok((vec![0.0; 2], state))
        }
    }
    let err = greedy_decode(&mut Short, empty_state(), 1, &params(10)).unwrap_err();
    assert!(matches!(err, SttError::InferenceFailed(_)), "{err}");
}

#[test]
fn load_vocab_parses_lines() {
    let dir = std::env::temp_dir().join("vp_test_vocab");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("vocab.txt");
    std::fs::write(&path, "<blk> 0\n▁hello 1\nworld 2\n").unwrap();

    let vocab = load_vocab(&path).unwrap();
    assert_eq!(vocab.len(), 3);
    assert_eq!(vocab[&0], "<blk>");
    assert_eq!(vocab[&1], "▁hello");
    assert_eq!(vocab[&2], "world");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timestamps_empty_tokens() {
    let vocab = HashMap::new();
    let (text, words) = detokenize_with_timestamps_impl(&vocab, &[], 1.0);
    assert_eq!(text, "");
    assert!(words.is_empty());
}

#[test]
fn timestamps_single_word() {
    let mut vocab = HashMap::new();
    vocab.insert(1u32, "▁hello".to_string());

    let tokens = vec![DecodedToken {
        token_id: 1,
        frame_index: 0,
    }];
    let (text, words) = detokenize_with_timestamps_impl(&vocab, &tokens, 2.0);
    assert_eq!(text, "hello");
    assert_eq!(words.len(), 1);
    assert_eq!(words[0].word, "hello");
    assert_eq!(words[0].start, std::time::Duration::ZERO);
    assert_eq!(words[0].end, std::time::Duration::from_secs(2));
}

#[test]
fn timestamps_multi_word_boundaries() {
    let mut vocab = HashMap::new();
    vocab.insert(1u32, "▁He".to_string());
    vocab.insert(2, "llo".to_string());
    vocab.insert(3, "▁world".to_string());

    // "He" at frame 0, "llo" at frame 2 (same word), "world" at frame 10
    let tokens = vec![
        DecodedToken {
            token_id: 1,
            frame_index: 0,
        },
        DecodedToken {
            token_id: 2,
            frame_index: 2,
        },
        DecodedToken {
            token_id: 3,
            frame_index: 10,
        },
    ];
    let total = 3.0;
    let (text, words) = detokenize_with_timestamps_impl(&vocab, &tokens, total);

    assert_eq!(text, "Hello world");
    assert_eq!(words.len(), 2);

    // "Hello" starts at frame 0 -> 0.0s, ends at frame 10 -> 0.8s
    assert_eq!(words[0].word, "Hello");
    assert_eq!(words[0].start, std::time::Duration::ZERO);
    assert_eq!(words[0].end, std::time::Duration::from_millis(800));

    // "world" starts at frame 10 -> 0.8s, ends at total (3.0s)
    assert_eq!(words[1].word, "world");
    assert_eq!(words[1].start, std::time::Duration::from_millis(800));
    assert_eq!(words[1].end, std::time::Duration::from_secs(3));
}

#[test]
fn timestamps_subword_concatenation() {
    let mut vocab = HashMap::new();
    vocab.insert(1u32, "▁un".to_string());
    vocab.insert(2, "believ".to_string());
    vocab.insert(3, "able".to_string());

    let tokens = vec![
        DecodedToken {
            token_id: 1,
            frame_index: 0,
        },
        DecodedToken {
            token_id: 2,
            frame_index: 1,
        },
        DecodedToken {
            token_id: 3,
            frame_index: 3,
        },
    ];
    let (text, words) = detokenize_with_timestamps_impl(&vocab, &tokens, 1.0);

    assert_eq!(text, "unbelievable");
    assert_eq!(words.len(), 1);
    assert_eq!(words[0].word, "unbelievable");
}

#[test]
fn timestamps_frame_to_time_math() {
    // 1 encoder frame = 80ms (10ms mel * 8x downsample)
    let mut vocab = HashMap::new();
    vocab.insert(1u32, "▁a".to_string());
    vocab.insert(2, "▁b".to_string());

    let tokens = vec![
        DecodedToken {
            token_id: 1,
            frame_index: 0,
        },
        DecodedToken {
            token_id: 2,
            frame_index: 25,
        }, // 25 * 80ms = 2000ms
    ];
    let (_, words) = detokenize_with_timestamps_impl(&vocab, &tokens, 5.0);

    assert_eq!(words[0].start, std::time::Duration::ZERO);
    assert_eq!(words[0].end, std::time::Duration::from_secs(2)); // 25 * 0.08
    assert_eq!(words[1].start, std::time::Duration::from_secs(2));
    assert_eq!(words[1].end, std::time::Duration::from_secs(5));
}

// ── Blank frames must not move the prediction network ──────

/// A step whose state records how many inputs the prediction network has
/// consumed, as the real LSTM's does, and which only recognises a word when
/// that state has not drifted.
///
/// The existing `Scripted` step hands state back untouched, which is why it
/// never saw the fault this guards: a walk that advanced the state on every
/// blank frame looked identical to one that did not.
struct Consuming {
    /// Frame at which a word arrives, after a run of silence.
    word_at: usize,
    /// State seen at each call, so the test can say where it drifted.
    seen: Vec<f32>,
}

impl Step for Consuming {
    fn step(
        &mut self,
        frame: usize,
        _prev_token: i64,
        state: DecoderState,
    ) -> Result<(Vec<f32>, DecoderState), SttError> {
        let consumed = state.states_1.first().copied().unwrap_or(0.0);
        self.seen.push(consumed);
        // The joint only emits the word when the prediction network is where
        // it should be -- having consumed nothing, since nothing was said.
        let logits = if frame == self.word_at && consumed == 0.0 {
            logits_for(0, 1)
        } else {
            logits_for(BLANK, 1)
        };
        let next = DecoderState {
            states_1: vec![consumed + 1.0],
            states_1_shape: vec![1],
            states_2: vec![],
            states_2_shape: vec![0],
        };
        Ok((logits, next))
    }
}

/// The fault behind "Hello, how are you?" arriving as "o, how are you?".
///
/// The prediction network's state may only advance when a token is emitted.
/// Advancing it on blank frames too feeds the previous token in again on
/// every frame of silence, so a second of lead-in before speech leaves the
/// state dozens of steps from where it belongs; the joint then predicts
/// blanks through the first words while it recovers. The loss grows with the
/// silence -- measured on the probe: 0.4 s of lead-in transcribed
/// "Hello how are you?", 1.9 s transcribed "are you".
#[test]
fn a_run_of_blank_frames_does_not_move_the_prediction_network() {
    let initial = DecoderState {
        states_1: vec![0.0],
        states_1_shape: vec![1],
        states_2: vec![],
        states_2_shape: vec![0],
    };
    // Twenty frames of silence, then a word -- about 1.6 s at 80 ms a frame.
    let mut step = Consuming {
        word_at: 20,
        seen: vec![],
    };
    let tokens = greedy_decode(&mut step, initial, 30, &params(10)).unwrap();

    assert!(
        step.seen[..20].iter().all(|&c| c == 0.0),
        "the state must not move across blank frames; it drifted to {:?}",
        &step.seen[..20]
    );
    assert_eq!(
        tokens.len(),
        1,
        "the word after the silence must survive, got {} token(s)",
        tokens.len()
    );
    assert_eq!(tokens[0].frame_index, 20);
}
