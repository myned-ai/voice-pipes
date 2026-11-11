use super::*;

#[tokio::test]
async fn local_fs_put_get_list() {
    let dir = std::env::temp_dir().join("vp_store_test");
    let _ = std::fs::remove_dir_all(&dir);

    let store = LocalFsAuditStore::new(dir.clone());
    store.put("hello.txt", b"world").await.unwrap();
    store
        .put("sub/nested.txt", b"nested content")
        .await
        .unwrap();

    // get
    let data = store.get("hello.txt").await.unwrap().unwrap();
    assert_eq!(data, b"world");

    // exists
    assert!(store.exists("hello.txt").await.unwrap());
    assert!(!store.exists("nope.txt").await.unwrap());

    // list root
    let blobs = store.list("").await.unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].key, "hello.txt");

    // list sub
    let blobs = store.list("sub/").await.unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].key, "sub/nested.txt");

    // get missing
    let missing = store.get("nope.txt").await.unwrap();
    assert!(missing.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// `put` lands whole or not at all: no temp file survives a successful
/// write, a write that cannot complete leaves nothing behind, and a
/// blob can be streamed back.
#[tokio::test]
async fn local_fs_put_is_atomic_and_streams_back() {
    let dir = std::env::temp_dir().join(format!("vp_store_atomic_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = LocalFsAuditStore::new(dir.clone());

    store.put("s/a.wav", &vec![1u8; 100_000]).await.unwrap();
    store.put("s/a.wav", &vec![2u8; 50_000]).await.unwrap();
    let names: Vec<String> = std::fs::read_dir(dir.join("s"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["a.wav".to_string()],
        "no temp files left: {names:?}"
    );
    assert_eq!(
        store.get("s/a.wav").await.unwrap().unwrap(),
        vec![2u8; 50_000]
    );

    // The target path is a directory: the rename cannot succeed and the
    // temp file must not be left behind.
    std::fs::create_dir_all(dir.join("s/blocked.txt")).unwrap();
    assert!(store.put("s/blocked.txt", b"partial").await.is_err());
    let leftovers: Vec<String> = std::fs::read_dir(dir.join("s"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            std::path::Path::new(n)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("tmp"))
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    use tokio::io::AsyncReadExt;
    let mut reader = store.open("s/a.wav").await.unwrap().expect("exists");
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await.unwrap();
    assert_eq!(buf.len(), 50_000);
    assert!(store.open("s/missing").await.unwrap().is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Records evicted from the in-memory window lose their file too, both
/// on push and when an oversized directory is loaded.
#[tokio::test]
async fn fs_session_store_evicts_files_with_records() {
    let dir = std::env::temp_dir().join(format!("vp_fs_evict_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let json_files = |sub: &str| -> usize {
        std::fs::read_dir(dir.join(sub)).map_or(0, |rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
    };

    let store = FsSessionStore::new(dir.clone(), 2);
    for i in 0..4 {
        let mut s = mk_session(&format!("s{i}"), SessionStatus::Completed);
        s.started_at = Utc::now() + chrono::Duration::seconds(i);
        store.push_session(s).await;
        let mut t = mk_turn(&format!("t{i}"), "s0", SessionStatus::Completed);
        t.started_at = Utc::now() + chrono::Duration::seconds(i);
        store.push(t).await;
    }
    // Eviction removes the file on a blocking task; give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(json_files("sessions"), 2);
    assert_eq!(json_files("turns"), 2);
    assert_eq!(store.sessions(None, 10, 0).await.len(), 2);
    drop(store);

    // Reloading with a smaller window trims records and files alike.
    let store = FsSessionStore::new(dir.clone(), 1);
    assert_eq!(store.sessions(None, 10, 0).await.len(), 1);
    assert_eq!(store.entries().await.len(), 1);
    assert_eq!(json_files("sessions"), 1);
    assert_eq!(json_files("turns"), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Traversal is rejected at the store boundary, not in
/// HTTP handlers.
#[tokio::test]
async fn local_fs_store_rejects_traversal_keys() {
    let dir = std::env::temp_dir().join("vp_store_traversal_test");
    let _ = std::fs::remove_dir_all(&dir);
    let store = LocalFsAuditStore::new(dir.clone());

    for bad in [
        "/etc/passwd",
        "C:\\Windows\\win.ini",
        "..\\secrets.txt",
        "../outside.txt",
        "a/../../outside.txt",
        "a/./b.txt",
        "a//b.txt",
        "",
    ] {
        assert!(store.put(bad, b"x").await.is_err(), "put accepted {bad:?}");
        assert!(store.get(bad).await.is_err(), "get accepted {bad:?}");
        assert!(store.exists(bad).await.is_err(), "exists accepted {bad:?}");
        assert!(
            store.append(bad, b"x").await.is_err(),
            "append accepted {bad:?}"
        );
    }

    // Legitimate nested keys keep working.
    store.put("session-1/001_mic.wav", b"ok").await.unwrap();
    assert_eq!(
        store.get("session-1/001_mic.wav").await.unwrap().unwrap(),
        b"ok"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn in_memory_session_store() {
    let store = InMemorySessionStore::new(3);

    for i in 0..5 {
        store
            .push(TurnRecord {
                id: format!("s-{i}"),
                session_id: "sess-1".into(),
                worker_id: "w1".into(),
                started_at: Utc::now(),
                completed_at: Some(Utc::now()),
                status: if i == 2 {
                    SessionStatus::Error
                } else {
                    SessionStatus::Completed
                },
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
            })
            .await;
    }

    assert_eq!(store.total_processed().await, 5);
    assert_eq!(store.errors_total().await, 1);

    // Only 3 entries kept (newest first)
    let entries = store.entries().await;
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].id, "s-4");
    assert_eq!(entries[2].id, "s-2");

    // Find
    assert!(store.find("s-4").await.is_some());
    assert!(store.find("s-0").await.is_none()); // evicted
}

fn mk_turn(id: &str, session: &str, status: SessionStatus) -> TurnRecord {
    TurnRecord {
        id: id.into(),
        session_id: session.into(),
        worker_id: "w1".into(),
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        status,
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

/// The same paged-and-filtered turn listing and the rolling-window query
/// must agree across every store, and neither may touch records outside
/// what it returns.
async fn turn_queries_agree(store: &dyn SessionStore) {
    // Pushed oldest first: the in-memory stores order by insertion (newest
    // push at the front), sqlite by `started_at`; inserting in time order
    // makes both agree on "newest first".
    let base = Utc::now();
    for i in (0..6).rev() {
        let mut t = mk_turn(
            &format!("t-{i}"),
            "s",
            if i % 3 == 0 {
                SessionStatus::Error
            } else {
                SessionStatus::Completed
            },
        );
        t.worker_id = if i % 2 == 0 {
            "w-even".into()
        } else {
            "w-odd".into()
        };
        t.started_at = base - chrono::Duration::seconds(i64::from(i) * 60);
        t.text_preview = Some(format!("Turn number {i}"));
        t.tags = if i == 4 { vec!["gold".into()] } else { vec![] };
        store.push(t).await;
    }

    // Window: the last 2.5 minutes hold turns 0, 1, 2.
    let recent = store
        .turns_since(base - chrono::Duration::seconds(150), None)
        .await;
    let mut ids: Vec<_> = recent.iter().map(|t| t.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["t-0", "t-1", "t-2"]);
    let recent_odd = store
        .turns_since(base - chrono::Duration::seconds(150), Some("w-odd"))
        .await;
    assert_eq!(recent_odd.len(), 1);
    assert_eq!(recent_odd[0].id, "t-1");

    // Column-only filter: SQL pushdown path on sqlite.
    let (page, total) = store
        .turns_page(
            &TurnFilter {
                status: Some(SessionStatus::Completed),
                ..Default::default()
            },
            2,
            0,
        )
        .await;
    assert_eq!(total, 4);
    assert_eq!(page.len(), 2);
    assert!(page.iter().all(|t| t.status == SessionStatus::Completed));
    assert_eq!(page[0].id, "t-1", "newest first");
    let (page2, _) = store
        .turns_page(
            &TurnFilter {
                status: Some(SessionStatus::Completed),
                ..Default::default()
            },
            2,
            2,
        )
        .await;
    assert_eq!(
        page2.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["t-4", "t-5"]
    );

    // JSON-field filters: text search, tag, exclude tag.
    let (found, total) = store
        .turns_page(
            &TurnFilter {
                search: Some("NUMBER 4".into()),
                ..Default::default()
            },
            10,
            0,
        )
        .await;
    assert_eq!((found.len(), total), (1, 1));
    assert_eq!(found[0].id, "t-4");
    let (tagged, _) = store
        .turns_page(
            &TurnFilter {
                tag: Some("gold".into()),
                worker_id: Some("w-even".into()),
                ..Default::default()
            },
            10,
            0,
        )
        .await;
    assert_eq!(
        tagged.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["t-4"]
    );
    let (_, without_gold) = store
        .turns_page(
            &TurnFilter {
                exclude_tag: Some("gold".into()),
                ..Default::default()
            },
            10,
            0,
        )
        .await;
    assert_eq!(without_gold, 5);

    // An offset past the end is an empty page with the right total.
    let (empty, total) = store.turns_page(&TurnFilter::default(), 10, 100).await;
    assert!(empty.is_empty());
    assert_eq!(total, 6);
}

#[tokio::test]
async fn in_memory_turn_queries() {
    let store = InMemorySessionStore::new(100);
    turn_queries_agree(&store).await;
}

#[tokio::test]
async fn fs_turn_queries() {
    let dir = std::env::temp_dir().join("vp_fs_turn_queries");
    let _ = std::fs::remove_dir_all(&dir);
    let store = FsSessionStore::new(dir.clone(), 100);
    turn_queries_agree(&store).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "session-sqlite")]
#[tokio::test]
async fn sqlite_turn_queries() {
    let path = std::env::temp_dir().join("vp_sqlite_turn_queries.db");
    rm_db(&path);
    let store = SqliteSessionStore::new(path.clone(), 100).unwrap();
    turn_queries_agree(&store).await;
    rm_db(&path);
}

fn mk_session(id: &str, status: SessionStatus) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        tenant_id: None,
        worker_id: "w1".into(),
        started_at: Utc::now(),
        ended_at: None,
        status,
        pipeline_name: Some("default".into()),
        pipeline_config_hash: Some("abc123".into()),
        stt_provider: None,
        tts_provider: None,
        input_sample_rate: Some(16_000),
        output_sample_rate: Some(24_000),
        tags: vec![],
        turn_count: 0,
    }
}

#[cfg(feature = "session-sqlite")]
fn rm_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[cfg(feature = "session-sqlite")]
#[tokio::test]
async fn sqlite_session_store_roundtrip() {
    let path = std::env::temp_dir().join("vp_sqlite_store_test.db");
    rm_db(&path);

    let store = SqliteSessionStore::new(path.clone(), 100).unwrap();

    // Sessions with strictly increasing start times.
    let base = Utc::now();
    for i in 0..3 {
        let mut s = mk_session(
            &format!("sess-{i}"),
            if i == 1 {
                SessionStatus::Error
            } else {
                SessionStatus::Completed
            },
        );
        s.started_at = base + chrono::Duration::seconds(i as i64);
        store.push_session(s).await;
    }

    // Newest first.
    let all = store.sessions(None, 10, 0).await;
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].id, "sess-2");

    // Status filter uses the indexed column.
    let errs = store.sessions(Some(SessionStatus::Error), 10, 0).await;
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].id, "sess-1");

    // Lookups.
    assert!(store.find_session("sess-0").await.is_some());
    assert!(store.find_session("nope").await.is_none());

    // Upsert by id updates in place rather than inserting a duplicate.
    let mut updated = mk_session("sess-0", SessionStatus::Completed);
    updated.turn_count = 7;
    store.push_session(updated).await;
    assert_eq!(store.sessions(None, 10, 0).await.len(), 3);
    assert_eq!(store.find_session("sess-0").await.unwrap().turn_count, 7);

    // Turns + derived counters.
    for i in 0..4 {
        let status = if i == 0 {
            SessionStatus::Error
        } else {
            SessionStatus::Completed
        };
        store
            .push(mk_turn(&format!("t-{i}"), "sess-0", status))
            .await;
    }
    assert_eq!(store.total_processed().await, 4);
    assert_eq!(store.errors_total().await, 1);
    assert_eq!(store.turns_for_session("sess-0").await.len(), 4);
    assert!(store.find("t-1").await.is_some());

    // Turn upsert: Error -> Completed drops the error count, not the total.
    store
        .push(mk_turn("t-0", "sess-0", SessionStatus::Completed))
        .await;
    assert_eq!(store.total_processed().await, 4);
    assert_eq!(store.errors_total().await, 0);

    // Reopen the file: data must survive the restart.
    drop(store);
    let reopened = SqliteSessionStore::new(path.clone(), 100).unwrap();
    assert_eq!(reopened.sessions(None, 10, 0).await.len(), 3);
    assert_eq!(reopened.total_processed().await, 4);

    drop(reopened);
    rm_db(&path);
}

fn mk_filter_session(
    id: &str,
    offset_mins: i64,
    status: SessionStatus,
    pipeline: &str,
    tags: &[&str],
    turns: u32,
) -> SessionRecord {
    let mut s = mk_session(id, status);
    s.started_at = Utc::now() + chrono::Duration::minutes(offset_mins);
    s.pipeline_name = Some(pipeline.into());
    s.pipeline_config_hash = Some(format!("hash-{id}"));
    s.tags = tags.iter().map(std::string::ToString::to_string).collect();
    s.turn_count = turns;
    s
}

/// Fixture shared by the SQL-pushdown and default-impl paging tests,
/// seeded oldest (`e`) to newest (`a`).
fn paging_fixture() -> Vec<SessionRecord> {
    vec![
        mk_filter_session("e", 0, SessionStatus::Error, "p1", &["benchmark", "x"], 4),
        mk_filter_session("d", 1, SessionStatus::Completed, "p2", &[], 1),
        mk_filter_session("c", 2, SessionStatus::InProgress, "p2", &["x"], 2),
        mk_filter_session("b", 3, SessionStatus::Completed, "p1", &[], 0),
        mk_filter_session("a", 4, SessionStatus::Completed, "p1", &["benchmark"], 3),
    ]
}

fn ids(v: &[SessionRecord]) -> Vec<String> {
    v.iter().map(|s| s.id.clone()).collect()
}

#[cfg(feature = "session-sqlite")]
#[tokio::test]
async fn sqlite_sessions_page_pushes_filters_into_sql() {
    let path = std::env::temp_dir().join("vp_sqlite_page_test.db");
    rm_db(&path);
    let store = SqliteSessionStore::new(path.clone(), 100).unwrap();
    for r in paging_fixture() {
        store.push_session(r).await;
    }

    // Unfiltered paging, newest first, with the grand total.
    let (page1, total) = store.sessions_page(&SessionFilter::default(), 2, 0).await;
    assert_eq!(
        (ids(&page1), total),
        (vec!["a".to_string(), "b".to_string()], 5)
    );
    let (page3, _) = store.sessions_page(&SessionFilter::default(), 2, 4).await;
    assert_eq!(ids(&page3), ["e"]);

    // The sessions-list default view: exclude benchmarks, hide empty.
    let f = SessionFilter {
        exclude_tag: Some("benchmark".into()),
        min_turns: Some(1),
        ..Default::default()
    };
    let (items, total) = store.sessions_page(&f, 50, 0).await;
    assert_eq!(
        (ids(&items), total),
        (vec!["c".to_string(), "d".to_string()], 2)
    );

    // Combined tag + pipeline + status.
    let f = SessionFilter {
        tag: Some("benchmark".into()),
        pipeline_name: Some("p1".into()),
        status: Some(SessionStatus::Error),
        ..Default::default()
    };
    let (items, total) = store.sessions_page(&f, 50, 0).await;
    assert_eq!((ids(&items), total), (vec!["e".to_string()], 1));

    // Tag matching anchors on the whole JSON element: a prefix of a
    // real tag must not match.
    let f = SessionFilter {
        tag: Some("bench".into()),
        ..Default::default()
    };
    assert_eq!(store.sessions_page(&f, 50, 0).await.1, 0);

    // Hash prefix.
    let f = SessionFilter {
        pipeline_hash_prefix: Some("hash-c".into()),
        ..Default::default()
    };
    assert_eq!(ids(&store.sessions_page(&f, 50, 0).await.0), ["c"]);

    // COUNT-only usage (limit 0) still reports the total.
    let (items, total) = store.sessions_page(&SessionFilter::default(), 0, 0).await;
    assert!(items.is_empty());
    assert_eq!(total, 5);

    // Time-range: the fixture starts at +0min ("e") through +4min
    // ("a"), so a window covering +2min..+3min keeps exactly c and b.
    let f = SessionFilter {
        started_after: Some(Utc::now() + chrono::Duration::seconds(90)),
        started_before: Some(Utc::now() + chrono::Duration::seconds(210)),
        ..Default::default()
    };
    let (items, total) = store.sessions_page(&f, 50, 0).await;
    assert_eq!(
        (ids(&items), total),
        (vec!["b".to_string(), "c".to_string()], 2)
    );

    drop(store);
    rm_db(&path);
}

#[tokio::test]
async fn default_sessions_page_agrees_with_sql_pushdown() {
    let mem = InMemorySessionStore::new(10);
    for r in paging_fixture() {
        mem.push_session(r).await;
    }
    let f = SessionFilter {
        exclude_tag: Some("benchmark".into()),
        min_turns: Some(1),
        ..Default::default()
    };
    // Same filter + paging as the sqlite test, offset into page 2.
    let (items, total) = mem.sessions_page(&f, 1, 1).await;
    assert_eq!((ids(&items), total), (vec!["d".to_string()], 2));
    let (items, total) = mem.sessions_page(&SessionFilter::default(), 0, 0).await;
    assert!(items.is_empty());
    assert_eq!(total, 5);
}

#[tokio::test]
async fn reconcile_backfills_orphaned_captures() {
    let dir = std::env::temp_dir().join("vp_reconcile_test");
    let _ = std::fs::remove_dir_all(&dir);

    let sess_dir = dir.join("sess-orphan");
    std::fs::create_dir_all(&sess_dir).unwrap();
    std::fs::write(
        sess_dir.join("metadata.json"),
        serde_json::json!({
            "id": "sess-orphan",
            "start_time": "2026-06-30T12:00:00Z",
            "pipeline_name": "default",
            "tags": ["smoke"],
            "input_sample_rate": 16_000,
        })
        .to_string(),
    )
    .unwrap();
    // Two completed turns on disk.
    std::fs::write(sess_dir.join("000_turn_resolved.json"), "{}").unwrap();
    std::fs::write(sess_dir.join("001_turn_resolved.json"), "{}").unwrap();
    // A directory without metadata.json is not a session capture.
    std::fs::create_dir_all(dir.join("not-a-session")).unwrap();

    let store = InMemorySessionStore::new(100);
    let recovered = reconcile_sessions_from_captures(&store, &dir).await;
    assert_eq!(recovered, 1);

    let s = store.find_session("sess-orphan").await.unwrap();
    assert_eq!(s.turn_count, 2);
    assert_eq!(s.pipeline_name.as_deref(), Some("default"));
    assert_eq!(s.input_sample_rate, Some(16_000));
    assert!(s.tags.iter().any(|t| t == "recovered"));
    assert!(s.tags.iter().any(|t| t == "smoke"));

    // Idempotent: an already-indexed session is left untouched.
    let again = reconcile_sessions_from_captures(&store, &dir).await;
    assert_eq!(again, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn appended_records_are_whole_and_in_order() {
    // Each append opens its own handle. If one returned before its bytes were
    // written, the next would race it, and on a filesystem without atomic
    // appends a record would land across part of another.
    let dir = std::env::temp_dir().join("vp_store_append_order_test");
    let _ = std::fs::remove_dir_all(&dir);
    let store = LocalFsAuditStore::new(dir.clone());

    let mut expected = String::new();
    for i in 0..400 {
        // Unequal lengths, so an overlap cannot hide behind equal-sized records.
        let line = format!("{{\"n\":{i},\"pad\":\"{}\"}}\n", "x".repeat(i % 97));
        store
            .append("s/events.jsonl", line.as_bytes())
            .await
            .unwrap();
        expected.push_str(&line);
    }

    let written = store.get("s/events.jsonl").await.unwrap().unwrap();
    assert_eq!(String::from_utf8(written).unwrap(), expected);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn session_filter_isolates_by_tenant() {
    let store = InMemorySessionStore::new(100);

    let mut s1 = mk_session("sess-1", SessionStatus::Completed);
    s1.tenant_id = Some(voice_pipes_sdk::TenantId::new("tenant-alpha").unwrap());
    store.push_session(s1).await;

    let mut s2 = mk_session("sess-2", SessionStatus::Completed);
    s2.tenant_id = Some(voice_pipes_sdk::TenantId::new("tenant-beta").unwrap());
    store.push_session(s2).await;

    let s3 = mk_session("sess-3", SessionStatus::Completed);
    store.push_session(s3).await;

    // Filter by tenant-alpha
    let filter_alpha = SessionFilter {
        tenant_id: Some(voice_pipes_sdk::TenantId::new("tenant-alpha").unwrap()),
        ..Default::default()
    };
    let (items_alpha, total_alpha) = store.sessions_page(&filter_alpha, 50, 0).await;
    assert_eq!(total_alpha, 1);
    assert_eq!(items_alpha.first().map(|s| s.id.as_str()), Some("sess-1"));

    // Filter by tenant-beta
    let filter_beta = SessionFilter {
        tenant_id: Some(voice_pipes_sdk::TenantId::new("tenant-beta").unwrap()),
        ..Default::default()
    };
    let (items_beta, total_beta) = store.sessions_page(&filter_beta, 50, 0).await;
    assert_eq!(total_beta, 1);
    assert_eq!(items_beta.first().map(|s| s.id.as_str()), Some("sess-2"));

    // Filter with no tenant_id: sees all 3
    let filter_all = SessionFilter::default();
    let (_, total_all) = store.sessions_page(&filter_all, 50, 0).await;
    assert_eq!(total_all, 3);
}

#[cfg(feature = "session-sqlite")]
#[tokio::test]
async fn sqlite_session_filter_isolates_by_tenant() {
    let path =
        std::env::temp_dir().join(format!("vp_tenant_sqlite_test_{}.db", std::process::id()));
    rm_db(&path);
    let store = SqliteSessionStore::new(path.clone(), 100).unwrap();

    let mut s1 = mk_session("sess-1", SessionStatus::Completed);
    s1.tenant_id = Some(voice_pipes_sdk::TenantId::new("tenant-alpha").unwrap());
    store.push_session(s1).await;

    let mut s2 = mk_session("sess-2", SessionStatus::Completed);
    s2.tenant_id = Some(voice_pipes_sdk::TenantId::new("tenant-beta").unwrap());
    store.push_session(s2).await;

    // Query tenant-alpha
    let filter_alpha = SessionFilter {
        tenant_id: Some(voice_pipes_sdk::TenantId::new("tenant-alpha").unwrap()),
        ..Default::default()
    };
    let (items_alpha, total_alpha) = store.sessions_page(&filter_alpha, 50, 0).await;
    assert_eq!(total_alpha, 1);
    assert_eq!(items_alpha.first().map(|s| s.id.as_str()), Some("sess-1"));

    // Query tenant-beta
    let filter_beta = SessionFilter {
        tenant_id: Some(voice_pipes_sdk::TenantId::new("tenant-beta").unwrap()),
        ..Default::default()
    };
    let (items_beta, total_beta) = store.sessions_page(&filter_beta, 50, 0).await;
    assert_eq!(total_beta, 1);
    assert_eq!(items_beta.first().map(|s| s.id.as_str()), Some("sess-2"));

    rm_db(&path);
}
