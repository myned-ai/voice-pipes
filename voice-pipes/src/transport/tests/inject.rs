use super::*;
use crate::audio::AudioFrame;
use crate::audio::wav_writer::write_wav;

fn make_test_wav(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("inject_test.wav");
    // 0.1s of silence at 16 kHz mono = 1600 samples
    let frame = AudioFrame {
        samples: vec![0i16; 1600],
        sample_rate: 16000,
        channels: 1,
    };
    write_wav(&frame, &path).unwrap();
    path
}

#[tokio::test]
async fn inject_sends_all_chunks() {
    let dir = std::env::temp_dir().join("vp_inject_test");
    std::fs::create_dir_all(&dir).unwrap();
    let wav_path = make_test_wav(&dir);

    let (tx, mut rx) = mpsc::channel(256);
    let result = inject_audio_file(&wav_path, &tx, 0.0).await.unwrap();

    // 0.1s at 16 kHz = 1600 samples, 20 ms chunks = 320 samples/chunk -> 5 chunks
    assert_eq!(result.chunks_sent, 5);
    assert!(result.audio_duration_s > 0.09 && result.audio_duration_s < 0.11);

    let mut total_samples = 0usize;
    let mut last_was_eou = false;
    while let Ok(chunk) = rx.try_recv() {
        total_samples += chunk.samples.len();
        last_was_eou = chunk.is_end_of_utterance;
    }
    assert_eq!(total_samples, 1600);
    assert!(last_was_eou, "last chunk should be end-of-utterance");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn inject_nonexistent_file_returns_error() {
    let (tx, _rx) = mpsc::channel(16);
    let result = inject_audio_file(Path::new("/nonexistent.wav"), &tx, 1.0).await;
    assert!(result.is_err());
}
