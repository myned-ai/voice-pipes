use super::*;
use crate::streaming::StreamingConfig;

#[tokio::test]
async fn small_pool_serves_many_concurrent_streams_by_releasing_slots_between_steps() {
    let sets = (0..2)
        .map(|_| SessionBackend::Fake {
            delay: std::time::Duration::ZERO,
            tokens: vec![1, 2],
        })
        .collect();
    let pool = Arc::new(SessionPool::new(sets, "Nemotron session sets").expect("pool"));
    let mut vocab = HashMap::new();
    vocab.insert(1, "\u{2581}hello".to_string());
    vocab.insert(2, "\u{2581}nemotron".to_string());
    let stt = Arc::new(NemotronStt {
        pool,
        metadata: Arc::new(ExportMetadata {
            streaming_config: StreamingConfig {
                left: 70,
                chunk: 10,
                right: 7,
            },
            cache_init_shapes: HashMap::new(),
        }),
        vocab: Arc::new(vocab),
        initial_cache: CacheTensors::default(),
        initial_decoder_state: DecoderState {
            inputs: Vec::new(),
            output_names: Vec::new(),
            prev_token: DEFAULT_BLANK_ID,
        },
        model_dir: PathBuf::from("models/nemotron"),
        threads: DEFAULT_THREADS,
        blank_id: DEFAULT_BLANK_ID,
        max_tokens_per_step: DEFAULT_MAX_TOKENS_PER_STEP,
        pool_size: 2,
        core: Arc::new(ComponentCore::new("stt:nemotron", ComponentKind::Stt)),
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

            // Send several small 20ms chunks (320 samples each) that do not
            // reach the ~685ms chunk threshold yet; no pool slot is held.
            for _ in 0..3 {
                audio_tx
                    .send(AudioChunk {
                        samples: vec![100i16; 320],
                        is_last: false,
                    })
                    .await
                    .expect("send partial chunk");
            }
            // All 16 streams are simultaneously active before any finishes.
            barrier.wait().await;
            audio_tx
                .send(AudioChunk {
                    samples: vec![100i16; 1600],
                    is_last: true,
                })
                .await
                .expect("send final chunk");

            let final_res = res_rx.recv().await.expect("final result");
            assert!(final_res.is_final);
            assert_eq!(final_res.text, "hello nemotron");
            cancel.cancel();
            stream_task.await.expect("join").expect("stream ok");
        }));
    }
    for h in handles {
        h.await.expect("session task completed");
    }
}
