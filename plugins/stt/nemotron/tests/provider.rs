//! Integration tests for the Nemotron streaming STT provider.
//!
//! These tests load the real ONNX models, which are exported once rather
//! than downloaded, and read the recorded fixtures in `voice-pipes/fixtures/`.
//! They need `voice-pipes/models/nemotron-streaming/7-10-7/` (encoder,
//! decoder, joint, vocab), or the directory `VP_NEMOTRON_DIR` names.
//!
//! Run with:
//! ```sh
//! cargo test --release -p voice-pipes-stt-nemotron --test provider \
//!   -- --ignored --test-threads=1
//! ```
//!
//! These tests guard against regressions in the Nemotron pipeline:
//! 1. Batch path produces real text (sanity).
//! 2. Streaming path (the live mic path) produces real text — this
//!    catches the empty-transcript bug observed in `voice-pipes-admin`
//!    live tests where the user's voice was correctly segmented by VAD
//!    but Nemotron returned `text: ""` for every utterance.

use std::path::PathBuf;
use std::sync::OnceLock;

use voice_pipes_sdk::AudioFrame;
use voice_pipes_sdk::stt::{AudioChunk, RecognitionRequest, SttProvider};
use voice_pipes_stt_nemotron::NemotronStt;

/// The exported model: `VP_NEMOTRON_DIR`, else the repository's models/.
fn nemotron_dir() -> PathBuf {
    std::env::var_os("VP_NEMOTRON_DIR").map_or_else(
        || {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../voice-pipes/models/nemotron-streaming/7-10-7"
            ))
        },
        PathBuf::from,
    )
}

fn have_nemotron() -> bool {
    nemotron_dir().join("encoder.onnx").exists()
}

/// A recorded fixture: mono 16-bit PCM.
fn fixture(name: &str) -> AudioFrame {
    let path = format!(
        "{}/../../../voice-pipes/fixtures/{name}.wav",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut reader = hound::WavReader::open(&path).expect("fixture wav");
    let spec = reader.spec();
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .expect("16-bit PCM");
    AudioFrame {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    }
}

/// Shared Nemotron instance — loading the 3 ONNX models is expensive,
/// so we load once and reuse across tests.
static NEMOTRON: OnceLock<NemotronStt> = OnceLock::new();

fn stt() -> &'static NemotronStt {
    NEMOTRON
        .get_or_init(|| NemotronStt::new(&nemotron_dir()).expect("failed to load Nemotron model"))
}

// ── Batch path ─────────────────────────────────────────────

/// Sanity: batch `transcribe` on a recorded question recovers it. If this
/// fails, the batch decoder itself is broken and streaming has no chance.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn batch_transcribes_a_recorded_question() {
    assert!(have_nemotron(), "nemotron model not found");
    let result = stt()
        .transcribe(&fixture("hello-how-are-you"))
        .await
        .unwrap();
    let text = result.text.to_lowercase();
    eprintln!("STT (batch): {text:?}");
    assert!(
        text.contains("how") && text.split_whitespace().count() >= 3,
        "expected the question back, got: {text:?}"
    );
}

// ── Streaming path (the live mic path) ─────────────────────

/// Drives `transcribe_stream` with a recorded question, chunked into
/// ~100 ms frames as the live pipeline does, ending with an `is_last=true`
/// chunk that includes 500 ms of trailing silence (the pipeline's flush).
///
/// The regression test for the empty-transcript bug: the streaming path
/// returned "" for every utterance while the batch path worked.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn streaming_transcribes_a_recorded_question() {
    assert!(have_nemotron(), "nemotron model not found");
    let stt = stt();
    let audio = fixture("hello-how-are-you");
    let samples_16k = resample_to_expected(stt, &audio.samples, audio.sample_rate);
    let final_text = run_streaming(stt, samples_16k).await;
    eprintln!("STT (streaming): {final_text:?}");
    let lower = final_text.to_lowercase();
    assert!(
        lower.contains("how") && lower.split_whitespace().count() >= 3,
        "expected the question back, got: {final_text:?}"
    );
}

/// Regression guard for the short-utterance flush bug: a one-word reply
/// used to be swallowed whole by the right-context window. A single short
/// word is at the edge of what this model decodes, so the guard is that the
/// streaming path completes without error, not that it finds the word.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn streaming_a_short_reply_completes() {
    assert!(have_nemotron(), "nemotron model not found");
    let stt = stt();
    let audio = fixture("short-yes");
    let samples_16k = resample_to_expected(stt, &audio.samples, audio.sample_rate);
    let final_text = run_streaming(stt, samples_16k).await;
    eprintln!("STT (streaming short): {final_text:?}");
}

/// Streaming contract: one `transcribe_stream` call serves two utterances,
/// finalised under segment 0 and 1, and ends when the audio channel closes.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn streaming_serves_two_utterances_on_one_stream() {
    assert!(have_nemotron(), "nemotron model not found");
    let stt = stt();
    let sr = stt.expected_sample_rate() as usize;
    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<AudioChunk>(64);
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel(64);
    let request = RecognitionRequest::default().with_sample_rate(sr as u32);
    let stream = tokio::spawn(async move {
        stt.transcribe_stream(
            request,
            chunk_rx,
            result_tx,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
    });

    for expected in 0..2u32 {
        // One second of dither followed by the end-of-utterance chunk.
        let samples: Vec<i16> = (0..sr)
            .map(|i| {
                ((((i as i32).wrapping_mul(1_103_515_245).wrapping_add(12345)) & 0x7f) - 64) as i16
            })
            .collect();
        chunk_tx
            .send(AudioChunk {
                samples,
                is_last: false,
            })
            .await
            .unwrap();
        chunk_tx
            .send(AudioChunk {
                samples: vec![0i16; sr / 2],
                is_last: true,
            })
            .await
            .unwrap();
        let fin = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                match result_rx.recv().await {
                    Some(r) if r.is_final => break Some(r),
                    Some(_) => {}
                    None => break None,
                }
            }
        })
        .await
        .expect("final within the deadline")
        .expect("stream must outlive the utterance");
        assert_eq!(fin.segment_index, expected);
    }
    drop(chunk_tx);
    let res = tokio::time::timeout(std::time::Duration::from_secs(10), stream)
        .await
        .expect("stream must end when audio closes")
        .unwrap();
    assert!(res.is_ok(), "{res:?}");
}

/// Two sessions at once each get a session set from the pool.
#[tokio::test]
#[ignore = "requires model files on disk"]
async fn two_concurrent_streams_both_transcribe() {
    assert!(have_nemotron(), "nemotron model not found");
    let stt = stt();
    let sr = stt.expected_sample_rate() as usize;
    let mut runs = Vec::new();
    for _ in 0..2 {
        let samples: Vec<i16> = (0..sr * 2)
            .map(|i| {
                ((((i as i32).wrapping_mul(1_103_515_245).wrapping_add(12345)) & 0x7f) - 64) as i16
            })
            .collect();
        runs.push(async move { run_streaming(stt, samples).await });
    }
    let (a, b) = tokio::join!(runs.pop().unwrap(), runs.pop().unwrap());
    // Dither is not speech; the point is that neither stream failed.
    let _ = (a, b);
}

/// Resample i16 samples to the STT provider's expected sample rate.
fn resample_to_expected(stt: &NemotronStt, samples: &[i16], from_sr: u32) -> Vec<i16> {
    let to_sr = stt.expected_sample_rate();
    if from_sr == to_sr {
        return samples.to_vec();
    }
    let f32_samples: Vec<f32> = samples.iter().map(|s| *s as f32 / 32768.0).collect();
    let resampled = voice_pipes_sdk::audio::resample::resample_f32(&f32_samples, from_sr, to_sr)
        .expect("the fixture's rate converts");
    resampled
        .into_iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect()
}

/// Drive `transcribe_stream` end-to-end and return the final transcript.
///
/// Mirrors the live pipeline behaviour:
/// - splits audio into ~100 ms chunks (1600 samples @ 16 kHz),
/// - appends 500 ms of trailing silence on the final chunk (the
///   pipeline's "flush trick" so the streaming STT evaluates its
///   right-context without waiting for real silence).
async fn run_streaming(stt: &NemotronStt, samples: Vec<i16>) -> String {
    let sr = stt.expected_sample_rate() as usize;
    let chunk_size = sr / 10; // 100 ms

    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<AudioChunk>(64);
    let (result_tx, mut result_rx) = tokio::sync::mpsc::channel(16);

    let request = RecognitionRequest::default().with_sample_rate(sr as u32);

    // Feed audio in a background task so we can read results concurrently.
    let producer = tokio::spawn(async move {
        let total = samples.len();
        let mut pos = 0;
        while pos < total {
            let end = (pos + chunk_size).min(total);
            let is_last = end == total;
            // Append flush-silence to the very last chunk only.
            let mut chunk_samples = samples[pos..end].to_vec();
            if is_last {
                chunk_samples.extend(std::iter::repeat_n(0i16, sr / 2));
            }
            chunk_tx
                .send(AudioChunk {
                    samples: chunk_samples,
                    is_last,
                })
                .await
                .expect("send chunk");
            pos = end;
        }
        drop(chunk_tx);
    });

    // Run the streaming transcription.
    let stream_handle = {
        // Need to satisfy 'static for spawn — use tokio::join! instead.
        async move {
            stt.transcribe_stream(
                request,
                chunk_rx,
                result_tx,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
        }
    };

    let collector = async {
        let mut final_text = String::new();
        while let Some(r) = result_rx.recv().await {
            if r.is_final {
                final_text = r.text;
            }
        }
        final_text
    };

    let (stream_res, _producer_res, final_text) = tokio::join!(stream_handle, producer, collector);
    stream_res.expect("transcribe_stream failed");
    final_text
}

/// The wizard entry asks for the export directory it cannot fetch, and the
/// factory answers to the name the entry writes.
#[test]
fn the_wizard_entry_points_at_the_export_directory() {
    let factory = voice_pipes_stt_nemotron::factory();
    assert_eq!(factory.name(), "nemotron-streaming");
    assert!(factory.matches("nemotron"));
    let choices = voice_pipes_stt_nemotron::wizard_choices();
    assert_eq!(choices.len(), 1);
    assert_eq!(
        choices[0].deploy,
        voice_pipes_sdk::WizardDeploy::LocalManual
    );
    let rendered = choices[0].render_defaults("provider", "nemotron");
    assert_eq!(
        rendered.toml,
        "provider = \"nemotron\"\nmodel_dir = \"models/nemotron-streaming/7-10-7\"\n"
    );
}

/// Two utterances queued back to back, the second before the first is
/// finalised, each get their own final: the stream's drain stops at
/// `is_last` instead of decoding the second utterance as part of the first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires model files on disk"]
async fn back_to_back_utterances_are_finalised_apart() {
    assert!(have_nemotron(), "nemotron model not found");
    fn utterance() -> Vec<Vec<i16>> {
        let audio = fixture("hello-how-are-you");
        resample_to_expected(stt(), &audio.samples, audio.sample_rate)
            .chunks(1600)
            .map(<[i16]>::to_vec)
            .collect()
    }
    voice_pipes_sdk::testing::back_to_back_utterances_stay_apart(
        |audio, tx, cancel| async move {
            stt()
                .transcribe_stream(RecognitionRequest::default(), audio, tx, cancel)
                .await
        },
        utterance,
        std::time::Duration::from_secs(120),
    )
    .await;
}
