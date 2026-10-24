use super::*;
use crate::audit::store::LocalFsAuditStore;
use crate::audit::test_support::{WRITE_TIMEOUT, file_count, wait_until};

#[tokio::test]
async fn tap_before_writes_pre_only() {
    let dir = std::env::temp_dir().join("voice_pipes_tap_before");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let tap = AuditTap::spawn(store, "test", TapSide::Before);
    let frame = AudioFrame {
        samples: vec![1000i16; 100],
        sample_rate: 22050,
        channels: 1,
    };
    let out = tap.resample(frame, 16000).unwrap();
    assert_eq!(out.sample_rate, 16000);

    // The tap writes from a spawned task; wait for it rather than
    // guessing a fixed delay (see `audit::test_support`).
    wait_until(WRITE_TIMEOUT, || file_count(&dir) >= 1).await;

    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    assert!(
        files.iter().any(|f| f.contains("_pre_")),
        "should have pre file: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.contains("_post_")),
        "should NOT have post file: {files:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn tap_after_writes_post_only() {
    let dir = std::env::temp_dir().join("voice_pipes_tap_after");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let tap = AuditTap::spawn(store, "test", TapSide::After);
    let frame = AudioFrame {
        samples: vec![1000i16; 100],
        sample_rate: 22050,
        channels: 1,
    };
    let _ = tap.resample(frame, 16000).unwrap();

    // The tap writes from a spawned task; wait for it rather than
    // guessing a fixed delay (see `audit::test_support`).
    wait_until(WRITE_TIMEOUT, || file_count(&dir) >= 1).await;

    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    assert!(
        !files.iter().any(|f| f.contains("_pre_")),
        "should NOT have pre file: {files:?}"
    );
    assert!(
        files.iter().any(|f| f.contains("_post_")),
        "should have post file: {files:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn tap_both_writes_pre_and_post() {
    let dir = std::env::temp_dir().join("voice_pipes_tap_both");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let tap = AuditTap::spawn(store, "resamp", TapSide::Both);
    let frame = AudioFrame {
        samples: vec![1000i16; 100],
        sample_rate: 22050,
        channels: 1,
    };
    let _ = tap.resample(frame, 16000).unwrap();

    // The tap writes from a spawned task; wait for it rather than
    // guessing a fixed delay (see `audit::test_support`).
    wait_until(WRITE_TIMEOUT, || file_count(&dir) >= 2).await;

    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    assert!(
        files.iter().any(|f| f.contains("_pre_")),
        "should have pre file: {files:?}"
    );
    assert!(
        files.iter().any(|f| f.contains("_post_")),
        "should have post file: {files:?}"
    );
    assert_eq!(files.len(), 2, "should have exactly 2 files: {files:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn tap_sequential_numbering() {
    let dir = std::env::temp_dir().join("voice_pipes_tap_seq");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let tap = AuditTap::spawn(store, "s", TapSide::Before);
    for _ in 0..3 {
        let frame = AudioFrame {
            samples: vec![0i16; 100],
            sample_rate: 44100,
            channels: 1,
        };
        let _ = tap.resample(frame, 16000).unwrap();
    }

    // The tap writes from a spawned task; wait for it rather than
    // guessing a fixed delay (see `audit::test_support`).
    wait_until(WRITE_TIMEOUT, || file_count(&dir) >= 3).await;

    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    files.sort();
    assert_eq!(files.len(), 3);
    assert!(files[0].starts_with("0000_"));
    assert!(files[1].starts_with("0001_"));
    assert!(files[2].starts_with("0002_"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn tap_identity_no_resample_still_records() {
    let dir = std::env::temp_dir().join("voice_pipes_tap_identity");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let tap = AuditTap::spawn(store, "id", TapSide::Both);
    let frame = AudioFrame {
        samples: vec![500i16; 50],
        sample_rate: 16000,
        channels: 1,
    };
    let out = tap.resample(frame, 16000).unwrap();
    assert_eq!(out.sample_rate, 16000);

    // The tap writes from a spawned task; wait for it rather than
    // guessing a fixed delay (see `audit::test_support`).
    wait_until(WRITE_TIMEOUT, || file_count(&dir) >= 2).await;

    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    assert_eq!(
        files.len(),
        2,
        "pre and post should both be written even for identity: {files:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
