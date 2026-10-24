use super::*;
use crate::audit::store::LocalFsAuditStore;

// ── Timeline placement ──────────────────────────────────────
//
// Placement is tested directly rather than through `record`, which stamps
// `Instant::now()` internally: asserting on offsets through the public path
// would be asserting on how fast the test machine ran.

fn track(t: &Timeline, side: Side) -> &[i16] {
    &t.channels[side.index()]
}

#[test]
fn places_a_chunk_at_its_offset_and_zero_fills_the_gap() {
    let mut t = Timeline::default();
    t.place(Side::User, &[7, 7, 7], 4, 0);
    assert_eq!(track(&t, Side::User), &[0, 0, 0, 0, 7, 7, 7]);
}

#[test]
fn a_later_chunk_appends_after_the_gap() {
    let mut t = Timeline::default();
    t.place(Side::User, &[1, 1], 0, 0);
    t.place(Side::User, &[2, 2], 5, 0);
    assert_eq!(track(&t, Side::User), &[1, 1, 0, 0, 0, 2, 2]);
}

#[test]
fn placement_never_moves_backwards_over_written_audio() {
    // Transport jitter can deliver a chunk whose arrival offset predates the
    // previous one; overwriting real audio would be worse than appending.
    let mut t = Timeline::default();
    t.place(Side::User, &[1, 1, 1, 1], 0, 0);
    t.place(Side::User, &[9, 9], 1, 0);
    assert_eq!(track(&t, Side::User), &[1, 1, 1, 1, 9, 9]);
}

#[test]
fn the_two_sides_are_independent() {
    let mut t = Timeline::default();
    t.place(Side::User, &[1, 1], 0, 0);
    t.place(Side::Agent, &[2, 2], 6, 0);
    assert_eq!(track(&t, Side::User), &[1, 1]);
    assert_eq!(track(&t, Side::Agent), &[0, 0, 0, 0, 0, 0, 2, 2]);
}

#[test]
fn the_cap_truncates_and_is_reported() {
    let mut t = Timeline::default();
    t.place(Side::User, &[1, 2, 3, 4, 5, 6], 0, 4);
    assert_eq!(track(&t, Side::User), &[1, 2, 3, 4]);
    assert!(t.dropped_past_cap);
}

#[test]
fn a_chunk_starting_past_the_cap_is_dropped_whole() {
    let mut t = Timeline::default();
    t.place(Side::User, &[1, 1], 10, 4);
    assert!(track(&t, Side::User).is_empty());
    assert!(t.dropped_past_cap);
}

#[test]
fn stereo_interleaves_and_pads_the_shorter_side() {
    let mut t = Timeline::default();
    t.place(Side::User, &[1, 1, 1], 0, 0);
    t.place(Side::Agent, &[2], 0, 0);
    let frame = t.snapshot_stereo();
    assert_eq!(frame.channels, 2);
    assert_eq!(frame.sample_rate, TARGET_SAMPLE_RATE);
    // user, agent, user, agent, ...
    assert_eq!(frame.samples, vec![1, 2, 1, 0, 1, 0]);
}

// ── Downmix / resample ──────────────────────────────────────

#[test]
fn stereo_input_is_downmixed_to_mono() {
    let out = to_mono_target(vec![10, 20, 30, 40], TARGET_SAMPLE_RATE, 2);
    assert_eq!(out, vec![15, 35]);
}

#[test]
fn mono_at_the_target_rate_passes_through_untouched() {
    let out = to_mono_target(vec![1, 2, 3], TARGET_SAMPLE_RATE, 1);
    assert_eq!(out, vec![1, 2, 3]);
}

#[test]
fn a_higher_rate_is_resampled_down() {
    let samples = vec![100i16; 4800];
    let out = to_mono_target(samples, 48_000, 1);
    // 48k -> 16k is a third of the samples, give or take filter edges.
    assert!(
        (1500..=1700).contains(&out.len()),
        "expected ~1600 samples, got {}",
        out.len()
    );
}

// ── End to end ──────────────────────────────────────────────

#[tokio::test]
async fn writes_one_stereo_file_with_both_sides() {
    let dir = std::env::temp_dir().join("voice_pipes_session_audio_test");
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));

    let rec = SessionRecorder::spawn(Arc::clone(&store) as Arc<dyn AuditStore>, "s1".into(), 60);
    rec.record(Side::User, &[500i16; 1600], TARGET_SAMPLE_RATE, 1);
    rec.record(Side::Agent, &[-500i16; 1600], TARGET_SAMPLE_RATE, 1);
    rec.finish().await;

    let blob = store
        .get("s1/session_audio.wav")
        .await
        .expect("store readable")
        .expect("session audio written");
    assert!(
        blob.len() > 44,
        "expected a WAV with samples, got {} bytes",
        blob.len()
    );
    // RIFF header, two channels, 16 kHz.
    assert_eq!(&blob[0..4], b"RIFF");
    assert_eq!(u16::from_le_bytes([blob[22], blob[23]]), 2);
    assert_eq!(
        u32::from_le_bytes([blob[24], blob[25], blob[26], blob[27]]),
        TARGET_SAMPLE_RATE
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_session_with_no_audio_writes_nothing() {
    let dir = std::env::temp_dir().join("voice_pipes_session_audio_empty_test");
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));

    let rec = SessionRecorder::spawn(Arc::clone(&store) as Arc<dyn AuditStore>, "s2".into(), 60);
    rec.finish().await;

    assert!(
        store
            .get("s2/session_audio.wav")
            .await
            .expect("store readable")
            .is_none(),
        "an empty session should not leave a zero-length WAV behind"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn empty_chunks_are_ignored() {
    let dir = std::env::temp_dir().join("voice_pipes_session_audio_empty_chunk_test");
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));

    let rec = SessionRecorder::spawn(Arc::clone(&store) as Arc<dyn AuditStore>, "s3".into(), 60);
    rec.record(Side::User, &[], TARGET_SAMPLE_RATE, 1);
    rec.record(Side::User, &[1, 2, 3], 0, 1); // rate 0 is not usable
    rec.finish().await;

    assert!(
        store
            .get("s3/session_audio.wav")
            .await
            .expect("store readable")
            .is_none()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dropping_every_handle_still_writes_the_recording() {
    // The session that is killed, crashes, or has its terminal closed never
    // reaches `finish`. Losing the whole recording in that case is how an
    // afternoon of collected conversation disappears, so the writer treats a
    // closed channel as a reason to save rather than to give up.
    let dir = std::env::temp_dir().join("voice_pipes_session_audio_drop_test");
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalFsAuditStore::new(dir.clone()));

    let rec = SessionRecorder::spawn(
        Arc::clone(&store) as Arc<dyn AuditStore>,
        "dropped".into(),
        60,
    );
    rec.record(Side::User, &[700i16; 1600], TARGET_SAMPLE_RATE, 1);
    // No finish(): the last handle simply goes away.
    drop(rec);

    let mut blob = None;
    for _ in 0..200 {
        if let Ok(Some(b)) = store.get("dropped/session_audio.wav").await {
            blob = Some(b);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let blob = blob.expect("a dropped recorder must still write what it had");
    assert_eq!(&blob[0..4], b"RIFF");
    assert!(blob.len() > 44);

    let _ = std::fs::remove_dir_all(&dir);
}
