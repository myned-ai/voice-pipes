use super::*;
use chrono::Utc;
use tempfile::TempDir;

fn sample_suite() -> TestSuite {
    TestSuite {
        id: "test-suite-1".into(),
        name: "Basic Suite".into(),
        description: None,
        created_at: Utc::now(),
        scenarios: vec![TestScenario {
            id: "greeting".into(),
            name: "Simple Greeting".into(),
            input: ScenarioInput::AudioFile {
                path: "scenarios/greeting.wav".into(),
            },
            expected: Some(ScenarioExpected {
                transcript_contains: vec!["hello".into()],
                response_intent: Some("Greet the user".into()),
                max_latency_ms: Some(2000),
                golden_transcript: None,
                golden_response: None,
                ..Default::default()
            }),
        }],
    }
}

fn sample_experiment() -> Experiment {
    Experiment {
        id: "exp-001".into(),
        name: "STT Comparison".into(),
        description: Some("Compare providers".into()),
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        status: ExperimentStatus::Draft,
        test_suite_id: "test-suite-1".into(),
        variants: vec![
            Variant {
                id: "variant-a".into(),
                name: "AssemblyAI".into(),
                config_path: "variant-a.toml".into(),
                config_hash: "abc123".into(),
                tags: vec!["stt:assemblyai".into()],
            },
            Variant {
                id: "variant-b".into(),
                name: "Parakeet".into(),
                config_path: "variant-b.toml".into(),
                config_hash: "def456".into(),
                tags: vec!["stt:parakeet".into()],
            },
        ],
        repetitions: 3,
    }
}

// IDs used as path segments must reject traversal / separators.
#[test]
fn validate_id_accepts_slugs_rejects_traversal() {
    for ok in ["abc", "exp-123", "a_b_C9", "UUID-like-0000"] {
        assert!(
            validate_id("experiment", ok).is_ok(),
            "should accept {ok:?}"
        );
    }
    for bad in [
        "",
        "..",
        "../x",
        "a/b",
        "a\\b",
        "/etc/passwd",
        "C:\\x",
        "a.b",
        "a b",
        ".",
    ] {
        assert!(
            validate_id("experiment", bad).is_err(),
            "should reject {bad:?}"
        );
    }
}

#[tokio::test]
async fn store_rejects_traversal_ids_at_boundary() {
    let tmp = TempDir::new().unwrap();
    let store = BenchmarkStore::new(tmp.path().join("benchmarks"));
    assert!(store.load_experiment("../../etc").await.is_err());
    assert!(store.delete_suite("..\\..\\x").await.is_err());
    assert!(store.load_results("/absolute/path").await.is_err());
    assert!(store.scenario_audio_path("suite", "../evil").is_err());
}

#[tokio::test]
async fn round_trip_experiment() {
    let tmp = TempDir::new().unwrap();
    let store = BenchmarkStore::new(tmp.path().join("benchmarks"));

    let exp = sample_experiment();
    store.save_experiment(&exp).await.unwrap();

    let loaded = store.load_experiment("exp-001").await.unwrap().unwrap();
    assert_eq!(loaded.id, "exp-001");
    assert_eq!(loaded.variants.len(), 2);
    assert_eq!(loaded.repetitions, 3);
}

#[tokio::test]
async fn round_trip_suite() {
    let tmp = TempDir::new().unwrap();
    let store = BenchmarkStore::new(tmp.path().join("benchmarks"));

    let suite = sample_suite();
    store.save_suite(&suite).await.unwrap();

    let loaded = store.load_suite("test-suite-1").await.unwrap().unwrap();
    assert_eq!(loaded.id, "test-suite-1");
    assert_eq!(loaded.scenarios.len(), 1);
}

#[tokio::test]
async fn list_experiments_sorted() {
    let tmp = TempDir::new().unwrap();
    let store = BenchmarkStore::new(tmp.path().join("benchmarks"));

    let mut exp1 = sample_experiment();
    exp1.id = "exp-old".into();
    exp1.created_at = Utc::now() - chrono::Duration::hours(2);
    store.save_experiment(&exp1).await.unwrap();

    let mut exp2 = sample_experiment();
    exp2.id = "exp-new".into();
    store.save_experiment(&exp2).await.unwrap();

    let list = store.list_experiments().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, "exp-new"); // newest first
}

#[tokio::test]
async fn delete_experiment_removes_directory() {
    let tmp = TempDir::new().unwrap();
    let store = BenchmarkStore::new(tmp.path().join("benchmarks"));

    let exp = sample_experiment();
    store.save_experiment(&exp).await.unwrap();
    assert!(store.load_experiment("exp-001").await.unwrap().is_some());

    let deleted = store.delete_experiment("exp-001").await.unwrap();
    assert!(deleted);
    assert!(store.load_experiment("exp-001").await.unwrap().is_none());
}

#[tokio::test]
async fn scenario_audio_round_trip() {
    let tmp = TempDir::new().unwrap();
    let store = BenchmarkStore::new(tmp.path().join("benchmarks"));

    let suite = sample_suite();
    store.save_suite(&suite).await.unwrap();

    let audio = b"RIFF....WAVEfmt "; // fake WAV header
    let path = store
        .save_scenario_audio("test-suite-1", "greeting", audio)
        .await
        .unwrap();
    assert!(path.exists());
    let data = tokio::fs::read(&path).await.unwrap();
    assert_eq!(data, audio);
}

/// Round-trip a `Conversation` scenario with two text turns and
/// per-turn expectations. Verifies the new variant serialises with a
/// stable tag and that all new `ScenarioExpected` fields survive.
#[test]
fn conversation_scenario_round_trip() {
    let suite = TestSuite {
        id: "convo-suite".into(),
        name: "convo".into(),
        description: None,
        created_at: Utc::now(),
        scenarios: vec![TestScenario {
            id: "c1".into(),
            name: "two turns".into(),
            input: ScenarioInput::Conversation {
                turns: vec![
                    ConversationTurn {
                        expect_followups: 0,
                        followup_timeout_ms: None,
                        id: "t1".into(),
                        input: TurnInput::Text {
                            text: "hello".into(),
                        },
                        expected: Some(TurnExpected {
                            transcript_contains: vec!["hello".into()],
                            ..Default::default()
                        }),
                    },
                    ConversationTurn {
                        expect_followups: 0,
                        followup_timeout_ms: None,
                        id: "t2".into(),
                        input: TurnInput::Audio {
                            path: "scenarios/followup.wav".into(),
                        },
                        expected: None,
                    },
                ],
            },
            expected: Some(ScenarioExpected {
                turns_completed: Some(2),
                max_latency_ms_per_turn: Some(3000),
                max_latency_drift_pct: Some(50.0),
                ..Default::default()
            }),
        }],
    };
    let json = serde_json::to_string(&suite).expect("serialise");
    let back: TestSuite = serde_json::from_str(&json).expect("deserialise");
    match &back.scenarios[0].input {
        ScenarioInput::Conversation { turns } => {
            assert_eq!(turns.len(), 2);
            assert_eq!(turns[0].id, "t1");
            match &turns[0].input {
                TurnInput::Text { text } => assert_eq!(text, "hello"),
                other @ TurnInput::Audio { .. } => panic!("expected text turn, got {other:?}"),
            }
            match &turns[1].input {
                TurnInput::Audio { path } => {
                    assert_eq!(path.to_string_lossy(), "scenarios/followup.wav");
                }
                other @ TurnInput::Text { .. } => panic!("expected audio turn, got {other:?}"),
            }
        }
        other => panic!("expected conversation, got {other:?}"),
    }
    let exp = back.scenarios[0].expected.as_ref().unwrap();
    assert_eq!(exp.turns_completed, Some(2));
    assert_eq!(exp.max_latency_ms_per_turn, Some(3000));
}

/// Round-trip a `BargeIn` scenario with `client_cancel` interrupt
/// and a follow-up turn.
#[test]
fn barge_in_scenario_round_trip() {
    let suite = TestSuite {
        id: "barge-suite".into(),
        name: "barge".into(),
        description: None,
        created_at: Utc::now(),
        scenarios: vec![TestScenario {
            id: "b1".into(),
            name: "cancel mid-response".into(),
            input: ScenarioInput::BargeIn {
                initial: ConversationTurn {
                    expect_followups: 0,
                    followup_timeout_ms: None,
                    id: "t1".into(),
                    input: TurnInput::Text {
                        text: "tell me a long story".into(),
                    },
                    expected: None,
                },
                interrupt: InterruptSpec {
                    mode: InterruptMode::ClientCancel,
                    fire_on: InterruptFireOn::PlaybackOffset { offset_ms: 200 },
                },
                follow_up: Some(ConversationTurn {
                    expect_followups: 0,
                    followup_timeout_ms: None,
                    id: "t2".into(),
                    input: TurnInput::Text {
                        text: "actually just say hi".into(),
                    },
                    expected: None,
                }),
            },
            expected: Some(ScenarioExpected {
                must_interrupt: true,
                interruption_trigger: Some(ExpectedTrigger::ClientCancel),
                max_barge_in_rtt_ms: Some(500),
                max_recovery_ms: Some(1500),
                follow_up_must_complete: true,
                ..Default::default()
            }),
        }],
    };
    let json = serde_json::to_string(&suite).expect("serialise");
    let back: TestSuite = serde_json::from_str(&json).expect("deserialise");
    match &back.scenarios[0].input {
        ScenarioInput::BargeIn {
            initial,
            interrupt,
            follow_up,
        } => {
            assert_eq!(initial.id, "t1");
            match &interrupt.mode {
                InterruptMode::ClientCancel => {}
                other => panic!("expected client_cancel, got {other:?}"),
            }
            match interrupt.fire_on {
                InterruptFireOn::PlaybackOffset { offset_ms } => {
                    assert_eq!(offset_ms, 200);
                }
                other => panic!("expected playback_offset, got {other:?}"),
            }
            assert!(follow_up.is_some());
        }
        other => panic!("expected barge_in, got {other:?}"),
    }
    let exp = back.scenarios[0].expected.as_ref().unwrap();
    assert!(exp.must_interrupt);
    assert_eq!(
        exp.interruption_trigger,
        Some(ExpectedTrigger::ClientCancel)
    );
    assert!(exp.follow_up_must_complete);
}

/// Load the on-disk fixtures shipped under `examples/benchmark/`.
/// Guards against accidental schema drift between the README/plan
/// examples and the Rust types.
#[test]
fn fixtures_parse_conversation_basic() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/benchmark/conversation-basic.json");
    let data =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let suite: TestSuite = serde_json::from_str(&data).expect("parse fixture");
    assert_eq!(suite.scenarios.len(), 1);
    match &suite.scenarios[0].input {
        ScenarioInput::Conversation { turns } => assert_eq!(turns.len(), 2),
        other => panic!("expected conversation, got {other:?}"),
    }
}

#[test]
fn fixtures_parse_barge_in_cancel() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/benchmark/barge-in-cancel.json");
    let data =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let suite: TestSuite = serde_json::from_str(&data).expect("parse fixture");
    assert_eq!(suite.scenarios.len(), 1);
    match &suite.scenarios[0].input {
        ScenarioInput::BargeIn { follow_up, .. } => assert!(follow_up.is_some()),
        other => panic!("expected barge_in, got {other:?}"),
    }
}

#[test]
fn fixtures_parse_barge_in_inject_audio() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/benchmark/barge-in-inject-audio.json");
    let data =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let suite: TestSuite = serde_json::from_str(&data).expect("parse fixture");
    assert_eq!(suite.scenarios.len(), 1);
    match &suite.scenarios[0].input {
        ScenarioInput::BargeIn { interrupt, .. } => match &interrupt.mode {
            crate::types::InterruptMode::InjectAudio { path } => {
                assert!(path.to_string_lossy().ends_with(".wav"));
            }
            other => panic!("expected inject_audio, got {other:?}"),
        },
        other => panic!("expected barge_in, got {other:?}"),
    }
}

#[test]
fn fixtures_parse_barge_in_inject_text() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/benchmark/barge-in-inject-text.json");
    let data =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let suite: TestSuite = serde_json::from_str(&data).expect("parse fixture");
    assert_eq!(suite.scenarios.len(), 1);
    match &suite.scenarios[0].input {
        ScenarioInput::BargeIn {
            interrupt,
            follow_up,
            ..
        } => {
            assert!(follow_up.is_none());
            match &interrupt.mode {
                crate::types::InterruptMode::InjectText { text } => {
                    assert!(text.to_lowercase().contains("stop"));
                }
                other => panic!("expected inject_text, got {other:?}"),
            }
        }
        other => panic!("expected barge_in, got {other:?}"),
    }
}

#[test]
fn fixtures_parse_synthetic_caller_mock() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/benchmark/synthetic-caller-mock.json");
    let data =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let suite: TestSuite = serde_json::from_str(&data).expect("parse fixture");
    assert_eq!(suite.scenarios.len(), 1);
    match &suite.scenarios[0].input {
        ScenarioInput::SyntheticCaller { caller } => {
            assert_eq!(caller.stop.max_turns, 4);
            match &caller.caller {
                crate::types::CallerKind::Mock { script } => {
                    assert!(
                        script
                            .to_string_lossy()
                            .contains("synthetic-caller-mock-script")
                    );
                }
                other => panic!("expected mock caller, got {other:?}"),
            }
        }
        other => panic!("expected synthetic_caller, got {other:?}"),
    }
}
