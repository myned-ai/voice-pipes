use super::*;
use std::collections::HashSet;
use voice_pipes::audio::AudioFrame;
use voice_pipes::audio::wav_writer::write_wav;
use voice_pipes::stt::{
    AudioChunk, RecognitionRequest, SttCapability, SttError, TranscriptionResult,
};

#[test]
fn wer_identical_is_zero() {
    assert_eq!(compute_wer("hello world", "hello world"), 0.0);
}

#[test]
fn wer_one_substitution() {
    // ref=2 words, 1 substitution -> 0.5
    assert!((compute_wer("hello world", "hello there") - 0.5).abs() < 1e-9);
}

#[test]
fn wer_case_insensitive() {
    assert_eq!(compute_wer("Hello World", "hello world"), 0.0);
}

#[test]
fn wer_empty_ref_with_hyp_is_one() {
    assert_eq!(compute_wer("", "anything"), 1.0);
    assert_eq!(compute_wer("", ""), 0.0);
}

#[test]
fn wer_full_substitution() {
    assert!((compute_wer("a b c", "x y z") - 1.0).abs() < 1e-9);
}

#[test]
fn cer_one_char_substitution() {
    // 5 chars, 1 sub -> 0.2
    assert!((compute_cer("hello", "jello") - 0.2).abs() < 1e-9);
}

#[test]
fn cer_handles_empty() {
    assert_eq!(compute_cer("", ""), 0.0);
    assert_eq!(compute_cer("", "x"), 1.0);
}

/// A canned STT that returns a fixed transcript regardless of input
/// audio -- sufficient for exercising the round-trip plumbing.
struct CannedStt {
    transcript: String,
}

#[async_trait]
impl SttProvider for CannedStt {
    async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        Ok(TranscriptionResult::new(self.transcript.clone(), true)
            .with_confidence(Some(1.0))
            .with_language(Some("en".into()))
            .with_is_end_of_turn(Some(true))
            .with_speech_started(true))
    }

    async fn transcribe_stream(
        &self,
        _request: RecognitionRequest,
        _audio_rx: tokio::sync::mpsc::Receiver<AudioChunk>,
        _tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        unimplemented!("canned stt streaming")
    }

    fn expected_sample_rate(&self) -> u32 {
        16_000
    }

    fn name(&self) -> &'static str {
        "canned"
    }

    fn capabilities(&self) -> HashSet<SttCapability> {
        HashSet::new()
    }
}

#[tokio::test]
async fn round_trip_scorer_reads_wav_and_scores() {
    let dir = std::env::temp_dir().join("vp_scoring_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.wav");
    let frame = AudioFrame {
        samples: vec![0i16; 1600],
        sample_rate: 16_000,
        channels: 1,
    };
    write_wav(&frame, &path).unwrap();

    let stt: Arc<dyn SttProvider> = Arc::new(CannedStt {
        transcript: "hello there".into(),
    });
    let scorer = SttRoundTripScorer::new(stt);
    let score = scorer.score("hello world", &path).await.unwrap();

    assert_eq!(score.transcript, "hello there");
    // 2 ref words, 1 substitution -> WER = 0.5
    assert!((score.wer - 0.5).abs() < 1e-9);
    // CER comparing "hello world" vs "hello there" -- non-zero.
    assert!(score.cer > 0.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn round_trip_scorer_propagates_stt_error() {
    struct FailingStt;
    #[async_trait]
    impl SttProvider for FailingStt {
        async fn transcribe(&self, _audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
            Err(SttError::InferenceFailed("boom".into()))
        }
        async fn transcribe_stream(
            &self,
            _r: RecognitionRequest,
            _rx: tokio::sync::mpsc::Receiver<AudioChunk>,
            _tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            unimplemented!()
        }
        fn expected_sample_rate(&self) -> u32 {
            16_000
        }
        fn name(&self) -> &'static str {
            "failing"
        }
        fn capabilities(&self) -> HashSet<SttCapability> {
            HashSet::new()
        }
    }

    let dir = std::env::temp_dir().join("vp_scoring_err_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.wav");
    let frame = AudioFrame {
        samples: vec![0i16; 1600],
        sample_rate: 16_000,
        channels: 1,
    };
    write_wav(&frame, &path).unwrap();

    let scorer = SttRoundTripScorer::new(Arc::new(FailingStt));
    let err = scorer.score("hello", &path).await.unwrap_err();
    assert!(err.to_string().contains("stt round-trip failed"));

    let _ = std::fs::remove_dir_all(&dir);
}
