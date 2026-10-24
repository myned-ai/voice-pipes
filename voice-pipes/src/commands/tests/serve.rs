use super::*;

fn cfg(bind: &str, advertise: Option<&str>) -> api::types::WorkerConfig {
    api::types::WorkerConfig {
        debug_bind: bind.to_string(),
        advertise_address: advertise.map(str::to_string),
        ..Default::default()
    }
}

/// A port another process serves on `::1` is not taken on `127.0.0.1` beside
/// it: clients resolving `localhost` to `::1` would reach the other process.
#[tokio::test]
async fn a_port_served_on_another_loopback_address_is_skipped() {
    let other = std::net::TcpListener::bind("[::1]:0")
        .expect("IPv6 loopback is needed for this test; bind [::1]:0 failed");
    let port = other.local_addr().unwrap().port();

    let (_listener, bound) = try_bind("127.0.0.1", port, "test").await.unwrap();
    assert_ne!(bound, port, "bound beside another process serving [::1]:{port}");
    drop(other);
}

#[tokio::test]
async fn a_free_port_is_bound_as_configured() {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let (_listener, bound) = try_bind("127.0.0.1", port, "test").await.unwrap();
    assert_eq!(bound, port);
}

/// Docker case: the bind address (0.0.0.0) is meaningless to a
/// containerized orchestrator — the explicit advertise address must
/// win, so proxied live edits reach the worker (502 regression).
#[test]
fn advertise_address_overrides_bind() {
    assert_eq!(
        registered_debug_address(&cfg("0.0.0.0", Some("worker-custom:8085")), 9999),
        "worker-custom:8085"
    );
}

/// Two containerised workers publishing different host ports: without
/// an explicit transport address both register `0.0.0.0:8800` and every
/// dispatch lands on the same host port (observed: the Croatian worker's
/// dispatch opened the Gemini worker).
#[test]
fn public_transport_address_is_registered_separately_from_bind() {
    let mut c = cfg("0.0.0.0", None);
    c.transport_advertise_address = Some("localhost:8801".into());
    assert_eq!(
        registered_public_transport_address(&c, 8800).as_deref(),
        Some("localhost:8801")
    );
    c.transport_advertise_address = Some("localhost".into());
    assert_eq!(
        registered_public_transport_address(&c, 8800).as_deref(),
        Some("localhost:8800")
    );
    // Unset / blank: no public address — dispatch derives the URL from
    // the internal bind address as before.
    c.transport_advertise_address = Some("  ".into());
    assert_eq!(registered_public_transport_address(&c, 8800), None);
    c.transport_advertise_address = None;
    assert_eq!(registered_public_transport_address(&c, 8800), None);
}

#[test]
fn bare_advertise_host_gets_actual_port() {
    assert_eq!(
        registered_debug_address(&cfg("0.0.0.0", Some("worker-custom")), 8085),
        "worker-custom:8085"
    );
}

/// The heartbeat's capacity must track live sessions, because the
/// orchestrator gates dispatch on `active_sessions >= max_concurrent`.
#[test]
fn worker_capacity_reports_live_sessions() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let live = AtomicUsize::new(0);
    assert_eq!(worker_capacity(10, &live).active_sessions, 0);

    live.store(3, Ordering::Relaxed);
    let cap = worker_capacity(10, &live);
    assert_eq!(cap.active_sessions, 3);
    assert_eq!(cap.max_concurrent, 10);

    // At capacity the orchestrator must be able to see it and skip us.
    live.store(10, Ordering::Relaxed);
    let cap = worker_capacity(10, &live);
    assert!(
        cap.active_sessions >= cap.max_concurrent,
        "a saturated worker must report as saturated, or dispatch keeps loading it"
    );
}

/// The regression: capacity was derived by filtering the turn-history store
/// for `InProgress`, but only `Completed` turns are ever recorded there, so
/// it always read 0 no matter how busy the worker was. Turn history and
/// live capacity are different things and must not be conflated again.
#[test]
fn worker_capacity_is_independent_of_turn_history() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let live = AtomicUsize::new(2);
    let history: VecDeque<crate::api::types::TurnRecord> = VecDeque::new();

    // However many completed turns pile up, capacity reflects live sessions.
    assert_eq!(worker_capacity(10, &live).active_sessions, 2);
    assert!(
        history.is_empty(),
        "history is not an input to capacity at all"
    );

    live.store(0, Ordering::Relaxed);
    assert_eq!(
        worker_capacity(10, &live).active_sessions,
        0,
        "and it returns to 0 when the sessions end"
    );
}

#[test]
fn unset_or_blank_advertise_falls_back_to_bind() {
    assert_eq!(
        registered_debug_address(&cfg("0.0.0.0", None), 8085),
        "0.0.0.0:8085"
    );
    assert_eq!(
        registered_debug_address(&cfg("127.0.0.1", Some("  ")), 8085),
        "127.0.0.1:8085"
    );
}

fn mk_turn(id: &str) -> crate::api::types::TurnRecord {
    crate::api::types::TurnRecord {
        id: id.into(),
        session_id: "s1".into(),
        worker_id: String::new(),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        status: crate::api::types::SessionStatus::Completed,
        total_ms: Some(100),
        stt_provider: None,
        tts_provider: None,
        text_preview: None,
        otel_trace_id: None,
        error: None,
        detail: None,
        pipeline_name: None,
        pipeline_config_hash: None,
        tags: vec![],
        events: vec![],
    }
}

/// A standalone worker (no orchestrator) must still record completed turns
/// locally: `GET /debug/sessions` reads this store, and until it was
/// drained the endpoint returned `total: 0` forever while the telemetry
/// channel filled and warned on every turn.
#[tokio::test]
async fn turn_recorder_stores_records_without_an_orchestrator() {
    let store = Arc::new(parking_lot::RwLock::new(VecDeque::new()));
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    spawn_turn_recorder(Arc::clone(&store), None, "w1".into(), rx, None);

    tx.send(mk_turn("t1")).await.unwrap();
    tx.send(mk_turn("t2")).await.unwrap();
    drop(tx);

    // Let the recorder task drain.
    for _ in 0..50 {
        if store.read().len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let ids: Vec<String> = store.read().iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids, vec!["t1".to_string(), "t2".to_string()]);
}

/// The store is a bounded ring: a long-lived worker must not grow without
/// limit, and the oldest turn is the one to lose.
#[tokio::test]
async fn turn_recorder_evicts_oldest_beyond_the_cap() {
    let store = Arc::new(parking_lot::RwLock::new(VecDeque::new()));
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    spawn_turn_recorder(Arc::clone(&store), None, "w1".into(), rx, None);

    let total = LOCAL_TURN_HISTORY + 5;
    for i in 0..total {
        tx.send(mk_turn(&format!("t{i}"))).await.unwrap();
    }
    drop(tx);

    // Wait for the LAST record, not merely for the cap: the store reaches
    // the cap while records are still arriving, so polling on length alone
    // samples the ring mid-fill.
    let last = format!("t{}", total - 1);
    for _ in 0..100 {
        if store.read().back().is_some_and(|r| r.id == last) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let store = store.read();
    assert_eq!(
        store.len(),
        LOCAL_TURN_HISTORY,
        "capped at the history size"
    );
    assert_eq!(
        store.front().unwrap().id,
        "t5",
        "the five oldest were evicted"
    );
    assert_eq!(store.back().unwrap().id, format!("t{}", total - 1));
}

/// The transport task ending is the orderly case only while a shutdown
/// was requested; at any other time it ends the worker, and so does a
/// listener error whenever it comes.
#[tokio::test]
async fn a_transport_that_ends_is_fatal_unless_the_worker_is_shutting_down() {
    let clean = || tokio::spawn(async { Ok::<(), std::io::Error>(()) });
    assert!(transport_task_outcome(clean().await, true).is_ok());
    let err = transport_task_outcome(clean().await, false).expect_err("not shutting down");
    assert!(err.to_string().contains("stopped"), "{err:#}");

    let failed = tokio::spawn(async { Err(std::io::Error::other("bind lost")) });
    let err = transport_task_outcome(failed.await, true).expect_err("a listener error");
    assert!(format!("{err:#}").contains("bind lost"), "{err:#}");
}

/// An idle worker runs its recogniser once per interval so the model stays
/// resident; a worker with a session open leaves the recogniser to that
/// session, and the loop ends with the worker's shutdown.
#[tokio::test(start_paused = true)]
async fn an_idle_worker_keeps_its_recogniser_warm_until_shutdown() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingStt(AtomicUsize);
    #[async_trait::async_trait]
    impl stt::SttProvider for CountingStt {
        async fn transcribe(
            &self,
            _audio: &crate::audio::AudioFrame,
        ) -> Result<stt::TranscriptionResult, stt::SttError> {
            Ok(stt::TranscriptionResult::new("", true))
        }
        async fn warm_up(&self) -> Result<(), stt::SttError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn transcribe_stream(
            &self,
            _req: stt::RecognitionRequest,
            mut audio_rx: tokio::sync::mpsc::Receiver<stt::AudioChunk>,
            _tx: tokio::sync::mpsc::Sender<stt::TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), stt::SttError> {
            while audio_rx.recv().await.is_some() {}
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "counting"
        }
        fn capabilities(&self) -> std::collections::HashSet<stt::SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let provider = Arc::new(CountingStt(AtomicUsize::new(0)));
    let active = Arc::new(AtomicUsize::new(0));
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(keep_warm(
        Arc::clone(&provider) as Arc<dyn stt::SttProvider>,
        Arc::clone(&active),
        std::time::Duration::from_secs(300),
        shutdown.clone(),
    ));

    tokio::time::sleep(std::time::Duration::from_secs(301)).await;
    assert_eq!(
        provider.0.load(Ordering::SeqCst),
        1,
        "one warm-up per idle interval"
    );

    active.store(1, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
    assert_eq!(
        provider.0.load(Ordering::SeqCst),
        1,
        "a session in progress is left alone"
    );

    active.store(0, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    assert_eq!(provider.0.load(Ordering::SeqCst), 2);

    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("the loop ends with the shutdown")
        .unwrap();
}

/// An interval of zero is the switch off.
#[tokio::test]
async fn keep_warm_with_a_zero_interval_returns_at_once() {
    struct Never;
    #[async_trait::async_trait]
    impl stt::SttProvider for Never {
        async fn transcribe(
            &self,
            _audio: &crate::audio::AudioFrame,
        ) -> Result<stt::TranscriptionResult, stt::SttError> {
            Ok(stt::TranscriptionResult::new("", true))
        }
        async fn warm_up(&self) -> Result<(), stt::SttError> {
            panic!("a disabled keep-warm never warms")
        }
        async fn transcribe_stream(
            &self,
            _req: stt::RecognitionRequest,
            mut audio_rx: tokio::sync::mpsc::Receiver<stt::AudioChunk>,
            _tx: tokio::sync::mpsc::Sender<stt::TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), stt::SttError> {
            while audio_rx.recv().await.is_some() {}
            Ok(())
        }
        fn expected_sample_rate(&self) -> u32 {
            16000
        }
        fn name(&self) -> &'static str {
            "never"
        }
        fn capabilities(&self) -> std::collections::HashSet<stt::SttCapability> {
            std::collections::HashSet::new()
        }
    }
    keep_warm(
        Arc::new(Never),
        Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        std::time::Duration::ZERO,
        tokio_util::sync::CancellationToken::new(),
    )
    .await;
}
