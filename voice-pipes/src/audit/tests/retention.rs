use super::*;

fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vp_retention_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn session(root: &Path, name: &str, age: Duration, now: SystemTime) -> PathBuf {
    let d = root.join(name);
    std::fs::create_dir_all(&d).unwrap();
    for f in ["metadata.json", "000_mic_raw.wav", "events.jsonl"] {
        let p = d.join(f);
        std::fs::write(&p, b"0123456789").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(now - age)
            .unwrap();
    }
    d
}

#[test]
fn removes_only_expired_session_dirs() {
    let now = SystemTime::now();
    let root = fresh_dir("basic");
    let old = session(&root, "sess-old", days(10), now);
    let young = session(&root, "sess-young", days(2), now);
    // Old but not a session capture (no metadata.json): must survive.
    let other = root.join("not-a-session");
    std::fs::create_dir_all(&other).unwrap();
    let stray = other.join("keep.txt");
    std::fs::write(&stray, b"x").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&stray)
        .unwrap()
        .set_modified(now - days(30))
        .unwrap();
    // A stray file at the root is never touched either.
    std::fs::write(root.join("README"), b"x").unwrap();

    let report = sweep_expired_captures_at(&root, 7, now);

    assert!(!old.exists(), "10-day-old session must be removed");
    assert!(young.exists(), "2-day-old session must be kept");
    assert!(
        other.exists() && stray.exists(),
        "non-session dir must be kept"
    );
    assert!(root.join("README").exists());
    assert_eq!(
        (report.removed, report.kept, report.skipped),
        (1, 1, 1),
        "{report:?}"
    );
    assert_eq!(report.bytes_freed, 30);
    assert!(report.errors.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn newest_file_decides_age_not_oldest() {
    // A session whose recording started 10 days ago but received a file
    // yesterday is still live from the operator's point of view.
    let now = SystemTime::now();
    let root = fresh_dir("newest");
    let d = session(&root, "sess-long", days(10), now);
    let late = d.join("005_turn_resolved.json");
    std::fs::write(&late, b"{}").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&late)
        .unwrap()
        .set_modified(now - days(1))
        .unwrap();

    let report = sweep_expired_captures_at(&root, 7, now);
    assert!(d.exists());
    assert_eq!(report.kept, 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// A session whose `metadata.json` never landed still leaves audio and
/// an events log; it is a capture and ages out like any other. A
/// directory with neither is not touched.
#[test]
fn metadata_less_capture_dirs_are_still_swept() {
    let now = SystemTime::now();
    let root = fresh_dir("nometa");
    let audio_only = root.join("sess-audio");
    std::fs::create_dir_all(&audio_only).unwrap();
    let events_only = root.join("sess-events");
    std::fs::create_dir_all(&events_only).unwrap();
    let unrelated = root.join("photos");
    std::fs::create_dir_all(&unrelated).unwrap();
    for p in [
        audio_only.join("000_mic_raw.wav"),
        events_only.join("events.jsonl"),
        unrelated.join("holiday.jpg"),
    ] {
        std::fs::write(&p, b"0123456789").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(now - days(30))
            .unwrap();
    }

    let report = sweep_expired_captures_at(&root, 7, now);
    assert!(!audio_only.exists(), "audio-only capture must be removed");
    assert!(!events_only.exists(), "events-only capture must be removed");
    assert!(unrelated.exists(), "unrelated directory must survive");
    assert_eq!((report.removed, report.skipped), (2, 1), "{report:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Symbolic links are never followed: a link at the top level is not a
/// candidate, and a link inside a capture neither inflates its size nor
/// gets its target removed.
#[test]
fn symlinks_are_skipped_not_followed() {
    let now = SystemTime::now();
    let root = fresh_dir("symlink");
    let keep = fresh_dir("symlink_target");
    let precious = keep.join("precious.txt");
    std::fs::write(&precious, vec![b'x'; 1000]).unwrap();

    // A capture that links back to the root (a cycle) and to the
    // precious tree.
    let sess = session(&root, "sess-old", days(30), now);
    #[cfg(unix)]
    let link = |target: &Path, at: &Path| std::os::unix::fs::symlink(target, at);
    #[cfg(windows)]
    let link = |target: &Path, at: &Path| std::os::windows::fs::symlink_dir(target, at);
    if link(&root, &sess.join("cycle")).is_err() || link(&keep, &sess.join("out")).is_err() {
        // Creating links needs a privilege this environment lacks.
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&keep);
        return;
    }
    // A top-level link to the precious tree, old by any measure.
    link(&keep, &root.join("linked-session")).unwrap();

    let report = sweep_expired_captures_at(&root, 7, now);
    assert!(!sess.exists(), "the old capture is removed");
    assert!(precious.exists(), "a linked tree is never removed");
    assert_eq!(report.removed, 1, "{report:?}");
    assert_eq!(
        report.bytes_freed, 30,
        "linked content does not count towards freed bytes"
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&keep);
}

#[test]
fn zero_days_disables_and_missing_dir_is_fine() {
    let now = SystemTime::now();
    let root = fresh_dir("zero");
    let old = session(&root, "sess-old", days(400), now);
    assert_eq!(
        sweep_expired_captures_at(&root, 0, now),
        SweepReport::default()
    );
    assert!(old.exists());
    let _ = std::fs::remove_dir_all(&root);

    let missing = root.join("does-not-exist");
    assert_eq!(
        sweep_expired_captures_at(&missing, 7, now),
        SweepReport::default()
    );
}

#[test]
fn sweeps_tenant_partitioned_captures() {
    let now = SystemTime::now();
    let root = fresh_dir("tenant_retention");

    // Tenant A: has one expired session and one young session
    let tenant_a = root.join("tenant-a");
    std::fs::create_dir_all(&tenant_a).unwrap();
    let old_a = session(&tenant_a, "sess-old-a", days(14), now);
    let young_a = session(&tenant_a, "sess-young-a", days(2), now);

    // Tenant B: has only expired sessions
    let tenant_b = root.join("tenant-b");
    std::fs::create_dir_all(&tenant_b).unwrap();
    let old_b = session(&tenant_b, "sess-old-b", days(20), now);

    let report = sweep_expired_captures_at(&root, 7, now);

    assert!(
        !old_a.exists(),
        "expired session in tenant A must be removed"
    );
    assert!(young_a.exists(), "young session in tenant A must be kept");
    assert!(
        tenant_a.exists(),
        "tenant A dir must survive since it has young sessions"
    );

    assert!(
        !old_b.exists(),
        "expired session in tenant B must be removed"
    );
    assert!(
        !tenant_b.exists(),
        "tenant B dir must be removed when all sessions are purged"
    );

    assert_eq!(report.removed, 2);
    assert_eq!(report.kept, 1);
    let _ = std::fs::remove_dir_all(&root);
}
