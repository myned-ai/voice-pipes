use super::*;
use crate::audit::store::LocalFsAuditStore;
use crate::audit::test_support::{WRITE_TIMEOUT, wait_until};

#[tokio::test]
async fn streaming_audit_writes_utterance_files() {
    let dir = std::env::temp_dir().join("vp_streaming_audit_test");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let sink = StreamingAuditSink::spawn(store, "test-session".into(), 64);
    let session_dir = dir.join("test-session");

    // Mic raw
    sink.record(AuditEvent::MicRaw {
        utterance_id: 1,
        audio: AudioFrame {
            samples: vec![100; 1600],
            sample_rate: 48000,
            channels: 1,
        },
    });
    // STT result
    sink.record(AuditEvent::SttResult {
        utterance_id: 1,
        text: "hello world".into(),
        latency: Duration::from_millis(150),
    });
    // LLM input
    sink.record(AuditEvent::LlmInput {
        utterance_id: 1,
        text: "hello world".into(),
    });
    // LLM output
    sink.record(AuditEvent::LlmOutput {
        utterance_id: 1,
        text: "Hi there!".into(),
        spoken_text: Some("Hi there!".into()),
        tts_directives: Vec::new(),
        stripped_directives: Vec::new(),
        latency: Duration::from_millis(300),
        is_partial: false,
    });
    // TTS fragment
    sink.record(AuditEvent::TtsFragment {
        utterance_id: 1,
        fragment_idx: 0,
        text: "Hi there!".into(),
        spoken_text: Some("Hi there!".into()),
        audio: AudioFrame {
            samples: vec![500; 2205],
            sample_rate: 22050,
            channels: 1,
        },
    });
    // Summary
    sink.record(AuditEvent::UtteranceDone {
        utterance_id: 1,
        interrupted: false,
        terminated: false,
        stt_provider: "parakeet-tdt".into(),
        tts_provider: "piper".into(),
    });

    // Wait for the background writer rather than guessing a delay
    // (see `audit::test_support`). The turn record is written last, so
    // its presence implies the earlier artefacts, which the assertions
    // below still verify.
    wait_until(WRITE_TIMEOUT, || {
        session_dir.join("001_turn_resolved.json").exists()
    })
    .await;

    assert!(session_dir.join("001_mic_raw.wav").exists());
    assert!(session_dir.join("001_stt.json").exists());
    assert!(session_dir.join("001_llm_input.txt").exists());
    assert!(session_dir.join("001_llm_output.json").exists());
    assert!(session_dir.join("001_tts_frag_000.wav").exists());
    assert!(session_dir.join("001_tts_frag_000.txt").exists());
    assert!(session_dir.join("001_turn_resolved.json").exists());

    // Verify STT JSON content.
    let stt_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session_dir.join("001_stt.json")).unwrap())
            .unwrap();
    assert_eq!(stt_json["text"], "hello world");
    assert_eq!(stt_json["latency_ms"], 150);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A store that always fails: counts attempts so the test can see the
/// writer stop trying.
#[derive(Debug)]
struct FailingStore {
    attempts: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl crate::audit::store::AuditStore for FailingStore {
    async fn put(&self, _key: &str, _data: &[u8]) -> anyhow::Result<()> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::bail!("disk full")
    }
    async fn get(&self, _key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    async fn list(&self, _prefix: &str) -> anyhow::Result<Vec<crate::audit::StoredBlob>> {
        Ok(Vec::new())
    }
    async fn exists(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn append(&self, _key: &str, _data: &[u8]) -> anyhow::Result<()> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::bail!("disk full")
    }
}

/// After the failure cap the writer stops touching the store instead
/// of failing (and logging) once per event for the rest of the session.
#[tokio::test]
async fn writer_disables_itself_after_repeated_failures() {
    let store = Arc::new(FailingStore {
        attempts: std::sync::atomic::AtomicUsize::new(0),
    });
    let sink = StreamingAuditSink::spawn(
        Arc::clone(&store) as Arc<dyn crate::audit::store::AuditStore>,
        "failing".into(),
        128,
    );
    for i in 0..40u32 {
        sink.record(AuditEvent::LlmInput {
            utterance_id: i,
            text: "x".into(),
        });
    }
    wait_until(WRITE_TIMEOUT, || {
        store.attempts.load(std::sync::atomic::Ordering::SeqCst)
            >= MAX_CONSECUTIVE_WRITE_FAILURES as usize
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        store.attempts.load(std::sync::atomic::Ordering::SeqCst),
        MAX_CONSECUTIVE_WRITE_FAILURES as usize,
        "no store call past the failure cap"
    );
}

/// Past the session byte budget, audio artefacts are dropped while text
/// artefacts keep being written.
#[tokio::test]
async fn session_budget_drops_audio_but_keeps_text() {
    let dir = std::env::temp_dir().join(format!("vp_audit_budget_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    // Budget: three 4000-sample frames (8000 bytes each) plus the
    // small text artefacts written between them.
    let sink = StreamingAuditSink::spawn_with_budget(store, "budget".into(), 64, 8000 * 3 + 200);
    let session_dir = dir.join("budget");
    for i in 0..6u32 {
        sink.record(AuditEvent::MicRaw {
            utterance_id: i,
            audio: AudioFrame {
                samples: vec![1; 4000],
                sample_rate: 16000,
                channels: 1,
            },
        });
        sink.record(AuditEvent::SttResult {
            utterance_id: i,
            text: "still recorded".into(),
            latency: Duration::from_millis(1),
        });
    }
    wait_until(WRITE_TIMEOUT, || session_dir.join("005_stt.json").exists()).await;
    for i in 0..6u32 {
        assert!(session_dir.join(format!("{i:03}_stt.json")).exists());
    }
    let wavs = (0..6u32)
        .filter(|i| session_dir.join(format!("{i:03}_mic_raw.wav")).exists())
        .count();
    assert_eq!(wavs, 3, "audio stops at the budget");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `PlaybackHealth` audit event becomes `NNN_playback.json` with the
/// health JSON verbatim.
#[tokio::test]
async fn streaming_audit_writes_playback_health_artifact() {
    let dir = std::env::temp_dir().join("vp_streaming_audit_playback_test");
    let _ = std::fs::remove_dir_all(&dir);

    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));
    let sink = StreamingAuditSink::spawn(store, "test-session".into(), 64);
    let session_dir = dir.join("test-session");

    sink.record(AuditEvent::PlaybackHealth {
        utterance_id: 4,
        health: crate::api::types::PlaybackHealth {
            underrun_count: 2,
            total_silence_ms: 550,
            audio_ms: 300,
            heard_ms: 300,
            wall_ms: 850,
            gaps: vec![
                crate::transport::PlaybackGap {
                    at_audio_ms: 100,
                    silence_ms: 300,
                },
                crate::transport::PlaybackGap {
                    at_audio_ms: 200,
                    silence_ms: 250,
                },
            ],
            ..crate::api::types::PlaybackHealth::default()
        },
    });

    let path = session_dir.join("004_playback.json");
    wait_until(WRITE_TIMEOUT, || path.exists()).await;
    assert!(path.exists(), "004_playback.json must be written");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["underrun_count"], 2);
    assert_eq!(json["total_silence_ms"], 550);
    assert_eq!(json["gaps"][0]["at_audio_ms"], 100);
    assert_eq!(json["gaps"][1]["silence_ms"], 250);

    let _ = std::fs::remove_dir_all(&dir);
}
