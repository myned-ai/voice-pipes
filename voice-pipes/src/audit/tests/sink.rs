use super::*;
use crate::audit::store::LocalFsAuditStore;
use crate::audit::test_support::{WRITE_TIMEOUT, file_count, wait_until};

#[tokio::test]
async fn audit_sink_writes_files() {
    let dir = std::env::temp_dir().join("voice_pipes_audit_test");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let sink = AuditSink::spawn(store, 16);

    let record = AuditRecord {
        timestamp: SystemTime::now(),
        request_id: "test-001".to_string(),
        input_text: "hello world".to_string(),
        audio: AudioFrame {
            samples: vec![0i16; 100],
            sample_rate: 22050,
            channels: 1,
        },
        provider: "mock".to_string(),
        latency: Duration::from_millis(42),
    };

    sink.record(record);

    // Wait for the background writer rather than guessing a delay
    // (see `audit::test_support`).
    wait_until(WRITE_TIMEOUT, || file_count(&dir) >= 2).await;

    // Verify files were created
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .collect();
    assert!(entries.len() >= 2, "expected .txt and .wav files");

    let has_txt = entries
        .iter()
        .any(|e| e.path().extension().is_some_and(|ext| ext == "txt"));
    let has_wav = entries
        .iter()
        .any(|e| e.path().extension().is_some_and(|ext| ext == "wav"));
    assert!(has_txt, "missing .txt audit file");
    assert!(has_wav, "missing .wav audit file");

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
}
