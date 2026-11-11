use super::*;
use std::collections::BTreeMap;

/// The live shape: the previous question's "?" and the two spaces the
/// decoder puts after it open the next utterance.
#[test]
fn a_closing_mark_carried_from_the_previous_utterance_is_dropped() {
    assert_eq!(
        without_carried_punctuation("?  Can you take me to the settings page"),
        "Can you take me to the settings page"
    );
    assert_eq!(without_carried_punctuation("?"), "");
    assert_eq!(without_carried_punctuation(". , hello"), "hello");
}

#[test]
fn an_opening_mark_and_a_mark_inside_the_text_stay() {
    assert_eq!(
        without_carried_punctuation("¿Qué hora es?"),
        "¿Qué hora es?"
    );
    assert_eq!(without_carried_punctuation("Yes, I do."), "Yes, I do.");
    assert_eq!(without_carried_punctuation(""), "");
}

#[test]
fn inference_metrics_computes_effective_latency_and_uses_total_audio_time() {
    use crate::model::CHUNK_MS;

    // 1.0s speech + 0.4s tail silence = 1.4s total processed in 350ms wall time
    let total_audio_secs = 1.4;
    let inference_wall = Duration::from_millis(350);
    let chunks_run = 4;
    let m = InferenceMetrics::measured(total_audio_secs, inference_wall, chunks_run)
        .with_delay_floor_ms(u64::from(CHUNK_MS))
        .with_streaming_window_ms(Some(CHUNK_MS));

    assert_eq!(m.audio_duration_ms, Some(1400));
    assert_eq!(m.inference_count, Some(4));
    assert_eq!(m.algorithmic_delay_ms, Some(u64::from(CHUNK_MS)));
    assert_eq!(m.streaming_window_ms, Some(CHUNK_MS));
    assert!(m.rtfx.is_some());
    let rtfx = m.rtfx.unwrap();
    // 1.4 / 0.35 = 4.0
    assert!((rtfx - 4.0).abs() < 1e-4);
    // effective latency = delay * (1 + 1/rtfx) = 320 * (1 + 0.25) = 400
    assert_eq!(m.effective_latency_ms, Some(400));
}

#[tokio::test]
async fn small_pool_serves_many_concurrent_streams_by_releasing_slots_between_steps() {
    let sets = (0..2)
        .map(|_| SessionSlot::Fake {
            delay: Duration::ZERO,
            tokens: vec![2, 3],
        })
        .collect();
    let pool = Arc::new(SessionPool::new(sets, "Nemotron 3.5 session sets").expect("pool"));
    let mut prompt_dictionary = BTreeMap::new();
    prompt_dictionary.insert("auto".to_string(), 0);
    prompt_dictionary.insert("en-US".to_string(), 1);
    let mut chunk_ms_to_lookahead = BTreeMap::new();
    chunk_ms_to_lookahead.insert("320".to_string(), 3);
    let cfg = Arc::new(ModelConfig {
        base_model: "nvidia/nemotron-3.5-asr-streaming-0.6b".to_string(),
        vocab_size: 4,
        blank_token_id: 0,
        decoder_hidden_size: 4,
        num_decoder_layers: 2,
        max_symbols_per_step: 4,
        default_prompt_id: 0,
        prompt_dictionary,
        chunk_ms_to_lookahead,
        cache_shapes: BTreeMap::new(),
    });
    let tokens = Arc::new(Tokens {
        pieces: vec![
            "<unk>".into(),
            "<en-US>".into(),
            "\u{2581}hello".into(),
            "\u{2581}world".into(),
        ],
    });
    let stt = Arc::new(Nemotron35Stt {
        pool,
        cfg,
        tokens,
        primed_decoder: (vec![0.0; 8], vec![0.0; 8], vec![0.0; 4]),
        default_prompt: 0,
        default_locale: "auto".to_string(),
        core: Arc::new(ComponentCore::new("stt:nemotron-3.5", ComponentKind::Stt)),
    });

    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let stt = Arc::clone(&stt);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(8);
            let (res_tx, mut res_rx) = tokio::sync::mpsc::channel(8);
            let cancel = tokio_util::sync::CancellationToken::new();
            let cancel_stream = cancel.clone();
            let stream_task = tokio::spawn(async move {
                stt.transcribe_stream(
                    RecognitionRequest::default(),
                    audio_rx,
                    res_tx,
                    cancel_stream,
                )
                .await
            });

            // Sub-320ms chunks (320 samples = 20ms each) accumulate in per-stream
            // Engine without checking out a pool slot.
            for _ in 0..3 {
                audio_tx
                    .send(AudioChunk {
                        samples: vec![100i16; 320],
                        is_last: false,
                    })
                    .await
                    .expect("send partial chunk");
            }
            barrier.wait().await;

            audio_tx
                .send(AudioChunk {
                    samples: vec![100i16; 320],
                    is_last: true,
                })
                .await
                .expect("send final chunk");
            drop(audio_tx);

            let mut saw_final = false;
            while let Some(res) = res_rx.recv().await {
                if res.is_final {
                    assert_eq!(res.text, "hello world");
                    saw_final = true;
                    break;
                }
            }
            assert!(saw_final);
            stream_task.await.expect("join").expect("stream succeeds");
        }));
    }

    for h in handles {
        h.await.expect("concurrent stream task");
    }
}
