//! Replay a captured session's audio through the stream path, cut where the
//! VAD cut it, to see what each utterance decodes to with its context.
//!
//! Ignored by default: it wants a capture that is not in the repository.
//!
//!     NEMOTRON35_REPLAY="path/to/004_mic_raw.wav:2544" \
//!         cargo test -p voice-pipes-stt-nemotron35 --release --test replay -- --ignored --nocapture
//!
//! The value is `wav:boundary_ms[,boundary_ms...]`; each boundary ends an
//! utterance, and the rest of the file is the last one. `NEMOTRON35_LOCALE`
//! picks the locale (default `en-US`).

use std::collections::HashMap;
use std::path::PathBuf;

use voice_pipes_sdk::{AudioChunk, RecognitionRequest, SttConfig, TranscriptionResult};
use voice_pipes_stt_nemotron35::factory;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs NEMOTRON35_REPLAY pointing at a capture"]
async fn replay_a_capture_split_at_the_vad_boundaries() {
    let spec = std::env::var("NEMOTRON35_REPLAY").expect("NEMOTRON35_REPLAY=wav:ms[,ms]");
    let (wav, cuts) = spec.rsplit_once(':').expect("wav:boundary_ms");
    let cuts: Vec<usize> = cuts
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().expect("boundary in ms"))
        .collect();
    let locale = std::env::var("NEMOTRON35_LOCALE").unwrap_or_else(|_| "en-US".into());
    let model_dir = std::env::var_os("NEMOTRON35_MODEL_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../voice-pipes/models/nemotron-3.5"),
        PathBuf::from,
    );

    let pcm: Vec<i16> = hound::WavReader::open(wav)
        .expect("wav")
        .samples::<i16>()
        .map(|s| s.unwrap())
        .collect();
    let provider = factory()
        .build(
            &SttConfig::new("nemotron-3.5")
                .with_model_dir(Some(model_dir))
                .with_options(HashMap::from([
                    ("pool_size".to_string(), serde_json::json!(1)),
                    ("language".to_string(), serde_json::json!(locale)),
                ])),
            &voice_pipes_sdk::BuildContext::new(),
        )
        .expect("build");

    let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TranscriptionResult>(64);
    let mut bounds: Vec<usize> = cuts.iter().map(|ms| ms * 16).collect();
    bounds.push(pcm.len());
    let feeder = tokio::spawn(async move {
        let mut start = 0;
        for end in bounds {
            let end = end.min(pcm.len());
            let seg = &pcm[start..end];
            let pieces: Vec<&[i16]> = seg.chunks(1600).collect();
            let n = pieces.len();
            for (i, piece) in pieces.into_iter().enumerate() {
                audio_tx
                    .send(AudioChunk {
                        samples: piece.to_vec(),
                        is_last: i + 1 == n,
                    })
                    .await
                    .unwrap();
            }
            start = end;
        }
    });
    let collector = tokio::spawn(async move {
        let mut out = Vec::new();
        while let Some(r) = rx.recv().await {
            if r.is_final {
                let m = r.inference_metrics.as_ref();
                eprintln!(
                    "  final {} chunks_run={:?} audio_ms={:?}",
                    r.segment_index,
                    m.and_then(|m| m.inference_count),
                    m.and_then(|m| m.audio_duration_ms)
                );
                out.push((r.segment_index, r.text));
            } else {
                eprintln!(
                    "  interim {} at {:.2}s: {:?}",
                    r.segment_index,
                    r.end_time.map_or(0.0, |t| t.as_secs_f64()),
                    r.text
                );
            }
        }
        out
    });
    provider
        .transcribe_stream(
            RecognitionRequest::default()
                .with_request_id("replay".into())
                .with_language(Some(locale))
                .with_sample_rate(16_000),
            audio_rx,
            tx,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("stream");
    feeder.await.unwrap();
    for (i, text) in collector.await.unwrap() {
        eprintln!("utterance {i}: {text:?}");
    }
}
