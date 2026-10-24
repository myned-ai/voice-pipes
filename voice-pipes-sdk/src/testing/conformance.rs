//! Shared checks for the streaming contract every STT provider must meet,
//! built in or plugin:
//! one `transcribe_stream` call serves the whole session, an `is_last` chunk
//! closes an utterance without ending the stream, and utterances are
//! numbered by `segment_index`.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::stt::{AudioChunk, SttError, TranscriptionResult};

/// How long to wait for each utterance's final.
const FINAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Drive `run` (a provider's streaming entry point) with two utterances on
/// one stream and check that each produces a final with the expected
/// segment index, then that the stream ends when the audio channel closes.
///
/// `utterance` builds the audio of one utterance; the last chunk it returns
/// is sent with `is_last = true`.
pub async fn two_utterances_on_one_stream<F, Fut>(run: F, utterance: fn() -> Vec<Vec<i16>>)
where
    F: FnOnce(
        mpsc::Receiver<AudioChunk>,
        mpsc::Sender<TranscriptionResult>,
        CancellationToken,
    ) -> Fut,
    Fut: Future<Output = Result<(), SttError>> + Send + 'static,
{
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(64);
    let (result_tx, mut result_rx) = mpsc::channel::<TranscriptionResult>(256);
    let handle = tokio::spawn(run(audio_rx, result_tx, CancellationToken::new()));

    for expected_segment in 0..2u32 {
        let chunks = utterance();
        let n = chunks.len();
        for (i, samples) in chunks.into_iter().enumerate() {
            audio_tx
                .send(AudioChunk {
                    samples,
                    is_last: i + 1 == n,
                })
                .await
                .expect("stream must still be reading");
        }
        let final_result = tokio::time::timeout(FINAL_TIMEOUT, async {
            loop {
                match result_rx.recv().await {
                    Some(r) if r.is_final => break Some(r),
                    Some(_) => {}
                    None => break None,
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("no final for utterance {expected_segment} within {FINAL_TIMEOUT:?}")
        })
        .unwrap_or_else(|| {
            panic!("stream ended before utterance {expected_segment} was finalised")
        });
        assert_eq!(
            final_result.segment_index, expected_segment,
            "utterance {expected_segment} must carry its own segment index"
        );
    }

    drop(audio_tx);
    let res = tokio::time::timeout(FINAL_TIMEOUT, handle)
        .await
        .expect("stream must end once the audio channel closes")
        .expect("stream task must not panic");
    assert!(res.is_ok(), "{res:?}");
}

/// Drive `run` with two utterances sent back to back, the second queued
/// before the first is finalised, as a slow decode or a quick speaker
/// leaves them, and check that each still gets its own final: a provider
/// that drains its queue must stop at `is_last`, or the second utterance's
/// audio is decoded as part of the first. `finals` is how long to wait for
/// both, which a local model on a debug build needs to be generous.
pub async fn back_to_back_utterances_stay_apart<F, Fut>(
    run: F,
    utterance: fn() -> Vec<Vec<i16>>,
    finals: Duration,
) where
    F: FnOnce(
        mpsc::Receiver<AudioChunk>,
        mpsc::Sender<TranscriptionResult>,
        CancellationToken,
    ) -> Fut,
    Fut: Future<Output = Result<(), SttError>> + Send + 'static,
{
    let (audio_tx, audio_rx) = mpsc::channel::<AudioChunk>(256);
    let (result_tx, mut result_rx) = mpsc::channel::<TranscriptionResult>(256);
    // Queue both utterances before the stream reads anything.
    for _ in 0..2 {
        let chunks = utterance();
        let n = chunks.len();
        for (i, samples) in chunks.into_iter().enumerate() {
            audio_tx
                .send(AudioChunk {
                    samples,
                    is_last: i + 1 == n,
                })
                .await
                .expect("the queue holds both utterances");
        }
    }
    let handle = tokio::spawn(run(audio_rx, result_tx, CancellationToken::new()));

    let deadline = tokio::time::Instant::now() + finals;
    let mut segments = Vec::new();
    let mut texts = Vec::new();
    while segments.len() < 2 {
        match tokio::time::timeout_at(deadline, result_rx.recv()).await {
            Ok(Some(r)) if r.is_final => {
                segments.push(r.segment_index);
                texts.push(r.text);
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(elapsed) => panic!(
                "two finals did not arrive ({elapsed} after {finals:?}); got segments \
                 {segments:?}: {texts:?}"
            ),
        }
    }
    assert_eq!(
        segments,
        [0, 1],
        "each utterance must be finalised on its own, in order: {texts:?}"
    );
    drop(audio_tx);
    let _ = tokio::time::timeout(finals, handle).await;
}
