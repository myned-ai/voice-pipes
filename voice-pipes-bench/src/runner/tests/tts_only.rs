use super::*;

/// First entry is cold; subsequent entries are warm. The aggregator
/// must surface both means separately and treat the cold entry as the
/// scenario-level cold flag.
#[test]
fn tts_only_aggregator_splits_cold_and_warm() {
    use voice_pipes::transport::{AudioRef, TtsOnlyMetrics};
    let scenario = TestScenario {
        id: "tts-corpus".into(),
        name: "tts-corpus".into(),
        input: ScenarioInput::TtsOnlyCorpus { entries: vec![] },
        expected: None,
    };
    let entries = vec![
        TtsCorpusEntry {
            id: "e1".into(),
            text: "hello".into(),
            language: None,
            bucket: TtsLengthBucket::Short,
            class: TtsContentClass::Plain,
        },
        TtsCorpusEntry {
            id: "e2".into(),
            text: "world".into(),
            language: None,
            bucket: TtsLengthBucket::Short,
            class: TtsContentClass::Plain,
        },
        TtsCorpusEntry {
            id: "e3".into(),
            text: "again".into(),
            language: None,
            bucket: TtsLengthBucket::Short,
            class: TtsContentClass::Plain,
        },
    ];
    let make = |id: &str, synth_ms: u64, cold: bool| TtsOnlyEntryOutcome {
        session_id: Some("s".into()),
        metrics: Some(TtsOnlyMetrics {
            ttfb_ms: Some(synth_ms / 2),
            synthesis_ms: Some(synth_ms),
            audio_duration_ms: Some(1000),
            sample_rate: Some(22_050),
            chars: Some(5),
            rtf: Some(synth_ms as f32 / 1000.0),
            provider: Some("piper".into()),
            cold_start: cold,
            error: None,
            voice: None,
            peak_amplitude: Some(3300),
            speech_ratio: Some(0.4),
        }),
        audio_ref: Some(AudioRef {
            path: format!("{id}.wav"),
            format: "wav".into(),
            sample_rate: 22_050,
            channels: 1,
            duration_ms: 1000,
        }),
        error: None,
        score: None,
    };
    let outcomes = vec![
        make("e1", 800, true),
        make("e2", 100, false),
        make("e3", 200, false),
    ];
    let result = aggregate_tts_only_outcomes(&scenario, &entries, &outcomes);
    assert_eq!(result.status, ScenarioRunStatus::Passed);
    assert_eq!(result.repetitions_failed, None);
    assert!(result.metrics.tts_cold_start, "cold flag must propagate");
    assert_eq!(result.metrics.tts_provider.as_deref(), Some("piper"));
    // Mean synthesis = (800 + 100 + 200) / 3 ~= 366.67
    assert!((result.metrics.tts_synthesis_ms.unwrap() - 366.667).abs() < 0.1);
    // Mean TTFB = (400 + 50 + 100) / 3 ~= 183.33
    assert!((result.metrics.tts_ttfb_ms.unwrap() - 183.333).abs() < 0.1);
    assert_eq!(result.session_ids, vec!["s".to_string()]);
}

/// The Croatian Pocket-TTS failure: the worker reports a successful
/// synthesis whose audio is near-silent noise. Timing metrics cannot see
/// it; the speech stats must flag the entry, count it, and fail the
/// scenario — without confusing it with a provider error.
#[test]
fn silent_audio_counts_as_a_failed_entry() {
    use voice_pipes::transport::{AudioRef, TtsOnlyMetrics};
    let scenario = TestScenario {
        id: "hr".into(),
        name: "hr".into(),
        input: ScenarioInput::TtsOnlyCorpus { entries: vec![] },
        expected: None,
    };
    let entry = |id: &str| TtsCorpusEntry {
        id: id.into(),
        text: "Moj najdraži izum je sustav izmjenične struje.".into(),
        language: Some("hr".into()),
        bucket: TtsLengthBucket::Short,
        class: TtsContentClass::Plain,
    };
    let entries = vec![entry("a"), entry("b"), entry("c")];
    let make = |peak: u16, ratio: f32| TtsOnlyEntryOutcome {
        session_id: Some("s".into()),
        metrics: Some(TtsOnlyMetrics {
            ttfb_ms: Some(900),
            synthesis_ms: Some(6000),
            audio_duration_ms: Some(7240),
            sample_rate: Some(24_000),
            chars: Some(46),
            rtf: Some(0.83),
            provider: Some("pocket-http".into()),
            cold_start: false,
            error: None,
            voice: None,
            peak_amplitude: Some(peak),
            speech_ratio: Some(ratio),
        }),
        audio_ref: Some(AudioRef {
            path: "x.wav".into(),
            format: "wav".into(),
            sample_rate: 24_000,
            channels: 1,
            duration_ms: 7240,
        }),
        error: None,
        score: None,
    };
    // Measured values: silent run (peak 282, ratio 0), good runs (peak
    // > 3000, ratio ≈ 0.4), and an old worker without the fields.
    let mut legacy = make(0, 0.0);
    legacy.metrics.as_mut().unwrap().peak_amplitude = None;
    legacy.metrics.as_mut().unwrap().speech_ratio = None;
    let outcomes = vec![make(282, 0.0), make(3300, 0.41), legacy];

    let result = aggregate_tts_only_outcomes(&scenario, &entries, &outcomes);
    assert_eq!(result.status, ScenarioRunStatus::Failed);
    assert_eq!(result.metrics.tts_silent_entries, Some(1));
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap()
            .contains("1/3 entries silent"),
        "{:?}",
        result.error_message
    );
    // Not a provider error: `tts_error` stays clear.
    assert!(result.metrics.tts_error.is_none());
    let e = result.tts_entries.as_ref().unwrap();
    assert!(e[0].silent);
    assert_eq!(e[0].peak_amplitude, Some(282));
    assert!(!e[1].silent);
    assert!((e[1].speech_ratio.unwrap() - 0.41).abs() < 1e-6);
    assert!(!e[2].silent, "legacy workers are never flagged");
}

/// All entries fail -> scenario is Error and `tts_error` is populated.
#[test]
fn tts_only_aggregator_marks_all_failed_as_error() {
    let scenario = TestScenario {
        id: "tts-corpus".into(),
        name: "tts-corpus".into(),
        input: ScenarioInput::TtsOnlyCorpus { entries: vec![] },
        expected: None,
    };
    let entries = vec![TtsCorpusEntry {
        id: "e1".into(),
        text: "hello".into(),
        language: None,
        bucket: TtsLengthBucket::Short,
        class: TtsContentClass::Plain,
    }];
    let outcomes = vec![TtsOnlyEntryOutcome {
        session_id: Some("s".into()),
        metrics: None,
        audio_ref: None,
        error: Some("boom".into()),
        score: None,
    }];
    let result = aggregate_tts_only_outcomes(&scenario, &entries, &outcomes);
    assert_eq!(result.status, ScenarioRunStatus::Error);
    assert_eq!(result.repetitions_failed, Some(1));
    assert!(
        result
            .metrics
            .tts_error
            .as_deref()
            .unwrap()
            .contains("boom")
    );
    assert!(result.metrics.tts_synthesis_ms.is_none());
}

/// When per-entry `score` is populated, the aggregator must surface
/// mean WER/CER and a `quality_score = 1 - wer` in the scoring slot.
#[test]
fn tts_only_aggregator_folds_round_trip_scores() {
    use crate::scoring::TtsScore;
    use voice_pipes::transport::{AudioRef, TtsOnlyMetrics};
    let scenario = TestScenario {
        id: "tts-corpus".into(),
        name: "tts-corpus".into(),
        input: ScenarioInput::TtsOnlyCorpus { entries: vec![] },
        expected: None,
    };
    let entries = vec![
        TtsCorpusEntry {
            id: "e1".into(),
            text: "hello world".into(),
            language: None,
            bucket: TtsLengthBucket::Short,
            class: TtsContentClass::Plain,
        },
        TtsCorpusEntry {
            id: "e2".into(),
            text: "again please".into(),
            language: None,
            bucket: TtsLengthBucket::Short,
            class: TtsContentClass::Plain,
        },
    ];
    let mk = |id: &str, wer: f64| TtsOnlyEntryOutcome {
        session_id: Some("s".into()),
        metrics: Some(TtsOnlyMetrics {
            ttfb_ms: Some(50),
            synthesis_ms: Some(100),
            audio_duration_ms: Some(1000),
            sample_rate: Some(22_050),
            chars: Some(11),
            rtf: Some(0.1),
            provider: Some("piper".into()),
            cold_start: false,
            error: None,
            voice: None,
            peak_amplitude: Some(3300),
            speech_ratio: Some(0.4),
        }),
        audio_ref: Some(AudioRef {
            path: format!("{id}.wav"),
            format: "wav".into(),
            sample_rate: 22_050,
            channels: 1,
            duration_ms: 1000,
        }),
        error: None,
        score: Some(TtsScore {
            wer,
            cer: wer / 2.0,
            transcript: "stub".into(),
        }),
    };
    let outcomes = vec![mk("e1", 0.2), mk("e2", 0.4)];
    let result = aggregate_tts_only_outcomes(&scenario, &entries, &outcomes);
    let scoring = result.scoring.expect("scoring populated");
    // Mean WER = 0.3 -> quality_score = 0.7.
    assert!((scoring.wer.unwrap() - 0.3).abs() < 1e-9);
    assert!((scoring.quality_score.unwrap() - 0.7).abs() < 1e-9);
    assert!((scoring.cer.unwrap() - 0.15).abs() < 1e-9);
    assert!(scoring.expectations_met);
}

// ── Worker resolution (regression coverage) ─────────────

use voice_pipes::api::types::{ComponentSnapshot, WorkerCapacity};

fn mk_worker(
    id: &str,
    address: &str,
    status: WorkerStatus,
    active: u32,
    max: u32,
    tags: &[&str],
    transport: Option<&str>,
) -> WorkerRecord {
    WorkerRecord {
        id: id.to_string(),
        address: address.to_string(),
        registered_at: Utc::now(),
        last_heartbeat: Utc::now(),
        status,
        capacity: WorkerCapacity {
            max_concurrent: max,
            active_sessions: active,
        },
        pipeline_config_hash: String::new(),
        pipeline_name: None,
        version: "test".into(),
        sessions_processed: 0,
        errors_total: 0,
        tags: tags.iter().map(|s| (*s).to_string()).collect(),
        transport_address: transport.map(std::string::ToString::to_string),
        public_transport_address: None,
        pipeline_config: None,
        available_configs: Vec::new(),
        components: Vec::<ComponentSnapshot>::new(),
        aggregate_health: None,
    }
}

fn fleet(workers: Vec<WorkerRecord>) -> Arc<DashMap<String, WorkerRecord>> {
    let map: DashMap<String, WorkerRecord> = DashMap::new();
    for w in workers {
        map.insert(w.id.clone(), w);
    }
    Arc::new(map)
}

#[test]
fn worker_ws_url_swaps_zero_bind_for_loopback() {
    let w = mk_worker("w", "0.0.0.0:8080", WorkerStatus::Active, 0, 1, &[], None);
    assert_eq!(worker_ws_url(&w), "ws://127.0.0.1:8080/ws");
}

#[test]
fn worker_ws_url_wildcard_transport_uses_advertised_host() {
    // Containerized fleet: debug API advertised as a compose service name,
    // transport registered with its wildcard bind. The runner must dial the
    // service name, not loopback (which is the orchestrator's own container).
    let w = mk_worker(
        "w",
        "worker-s2s-openai:8085",
        WorkerStatus::Active,
        0,
        1,
        &[],
        Some("0.0.0.0:8800"),
    );
    assert_eq!(worker_ws_url(&w), "ws://worker-s2s-openai:8800/ws");
}

#[test]
fn worker_ws_url_prefers_transport_address() {
    let w = mk_worker(
        "w",
        "0.0.0.0:8080",
        WorkerStatus::Active,
        0,
        1,
        &[],
        Some("10.0.0.5:9000"),
    );
    assert_eq!(worker_ws_url(&w), "ws://10.0.0.5:9000/ws");
}

#[test]
fn worker_ws_url_uses_address_when_no_transport() {
    let w = mk_worker(
        "w",
        "192.168.1.10:7000",
        WorkerStatus::Active,
        0,
        1,
        &[],
        None,
    );
    assert_eq!(worker_ws_url(&w), "ws://192.168.1.10:7000/ws");
}

#[test]
fn resolve_worker_ws_filters_by_tags() {
    let workers = fleet(vec![
        mk_worker(
            "w1",
            "127.0.0.1:1",
            WorkerStatus::Active,
            0,
            1,
            &["a"],
            None,
        ),
        mk_worker(
            "w2",
            "127.0.0.1:2",
            WorkerStatus::Active,
            0,
            1,
            &["a", "b"],
            None,
        ),
    ]);
    let got = resolve_worker_ws(&workers, &["b".into()]);
    assert_eq!(got, Some("ws://127.0.0.1:2/ws".into()));
}

#[test]
fn resolve_worker_ws_skips_dead_workers() {
    let workers = fleet(vec![mk_worker(
        "w",
        "127.0.0.1:1",
        WorkerStatus::Dead,
        0,
        1,
        &[],
        None,
    )]);
    assert!(resolve_worker_ws(&workers, &[]).is_none());
}

#[test]
fn resolve_worker_ws_skips_draining_workers() {
    let workers = fleet(vec![mk_worker(
        "w",
        "127.0.0.1:1",
        WorkerStatus::Draining,
        0,
        1,
        &[],
        None,
    )]);
    assert!(resolve_worker_ws(&workers, &[]).is_none());
}

#[test]
fn resolve_worker_ws_skips_full_workers() {
    let workers = fleet(vec![mk_worker(
        "w",
        "127.0.0.1:1",
        WorkerStatus::Active,
        3,
        3,
        &[],
        None,
    )]);
    assert!(resolve_worker_ws(&workers, &[]).is_none());
}

#[test]
fn resolve_worker_ws_prefers_least_loaded() {
    let workers = fleet(vec![
        mk_worker("busy", "127.0.0.1:1", WorkerStatus::Active, 2, 4, &[], None),
        mk_worker("idle", "127.0.0.1:2", WorkerStatus::Active, 0, 4, &[], None),
    ]);
    let got = resolve_worker_ws(&workers, &[]).expect("a worker");
    assert_eq!(got, "ws://127.0.0.1:2/ws");
}

#[test]
fn resolve_with_pin_bypasses_tag_requirements() {
    // Pinned worker has *no* tags, but caller asks for tag "b".
    // Pin must win.
    let workers = fleet(vec![mk_worker(
        "pinned",
        "127.0.0.1:1",
        WorkerStatus::Active,
        0,
        1,
        &[],
        None,
    )]);
    let got = resolve_worker_ws_with_pin(&workers, &["b".into()], Some("pinned"));
    assert_eq!(got, Some("ws://127.0.0.1:1/ws".into()));
}

#[test]
fn resolve_with_pin_rejects_unknown_worker_id() {
    let workers = fleet(vec![mk_worker(
        "real",
        "127.0.0.1:1",
        WorkerStatus::Active,
        0,
        1,
        &[],
        None,
    )]);
    let got = resolve_worker_ws_with_pin(&workers, &[], Some("ghost"));
    assert!(got.is_none(), "unknown pin must NOT fall back to auto-pick");
}

#[test]
fn resolve_with_pin_rejects_inactive_pinned_worker() {
    let workers = fleet(vec![mk_worker(
        "pinned",
        "127.0.0.1:1",
        WorkerStatus::Draining,
        0,
        1,
        &[],
        None,
    )]);
    assert!(resolve_worker_ws_with_pin(&workers, &[], Some("pinned")).is_none());
}

#[test]
fn resolve_with_pin_rejects_full_pinned_worker() {
    let workers = fleet(vec![mk_worker(
        "pinned",
        "127.0.0.1:1",
        WorkerStatus::Active,
        2,
        2,
        &[],
        None,
    )]);
    assert!(resolve_worker_ws_with_pin(&workers, &[], Some("pinned")).is_none());
}

#[test]
fn resolve_with_pin_none_falls_back_to_tag_selection() {
    let workers = fleet(vec![
        mk_worker(
            "w1",
            "127.0.0.1:1",
            WorkerStatus::Active,
            0,
            1,
            &["a"],
            None,
        ),
        mk_worker(
            "w2",
            "127.0.0.1:2",
            WorkerStatus::Active,
            0,
            1,
            &["b"],
            None,
        ),
    ]);
    let got = resolve_worker_ws_with_pin(&workers, &["b".into()], None);
    assert_eq!(got, Some("ws://127.0.0.1:2/ws".into()));
}

#[test]
fn runner_config_default_has_no_pinned_worker() {
    let cfg = RunnerConfig::default();
    assert!(
        cfg.target_worker_id.is_none(),
        "default RunnerConfig must not pin a worker"
    );
}
