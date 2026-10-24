use super::*;
use async_trait::async_trait;
use std::sync::atomic::AtomicUsize;

/// Scripted annotator: records every command and replies per `mode`.
struct FakeAnnotator {
    mode: &'static str,
    commands: Arc<Mutex<Vec<AnnotatorCommand>>>,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl AudioAnnotator for FakeAnnotator {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn run(
        &self,
        _session_id: String,
        mut commands: mpsc::Receiver<AnnotatorCommand>,
        out: mpsc::Sender<AudioAnnotation>,
    ) -> Result<(), super::super::AnnotatorError> {
        self.runs.fetch_add(1, Ordering::Relaxed);
        while let Some(cmd) = commands.recv().await {
            self.commands.lock().push(cmd.clone());
            match (self.mode, cmd) {
                // Answer every segment (and the warm-up) immediately.
                ("echo", AnnotatorCommand::Segment(seg)) => {
                    let _ = out
                        .send(AudioAnnotation {
                            utterance_id: seg.utterance_id,
                            start_ms: seg.start_ms,
                            duration_ms: seg.duration_ms(),
                            kind: "echo".into(),
                            extras: serde_json::json!({ "n": seg.samples.len() }),
                        })
                        .await;
                }
                ("echo", AnnotatorCommand::Warmup { .. }) => {
                    let _ = out
                        .send(AudioAnnotation {
                            utterance_id: WARMUP_UTTERANCE_ID,
                            start_ms: 0,
                            duration_ms: 0,
                            kind: "echo".into(),
                            extras: serde_json::json!({}),
                        })
                        .await;
                }
                // Dies on the first segment.
                ("die", AnnotatorCommand::Segment(_)) => {
                    return Err(super::super::AnnotatorError::new("fake", "boom"));
                }
                // "silent" (never answers -> timeouts / breaker) and
                // everything else: consume and stay quiet.
                _ => {}
            }
        }
        Ok(())
    }
}

fn spec(
    mode: &'static str,
    cfg: AnnotatorConfig,
) -> (
    AnnotatorSpec,
    Arc<Mutex<Vec<AnnotatorCommand>>>,
    Arc<AtomicUsize>,
) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let runs = Arc::new(AtomicUsize::new(0));
    let annotator = Arc::new(FakeAnnotator {
        mode,
        commands: Arc::clone(&commands),
        runs: Arc::clone(&runs),
    });
    (
        AnnotatorSpec {
            annotator,
            config: cfg,
        },
        commands,
        runs,
    )
}

fn cfg() -> AnnotatorConfig {
    AnnotatorConfig {
        batch_ms: 160,
        first_batch_ms: 80,
        max_in_flight: 2,
        timeout_ms: 100,
        warmup: false,
        breaker_failures: 3,
        breaker_cooldown_ms: 200,
    }
}

/// 40 ms of 24 kHz mono.
fn chunk(utt: u32, eou: bool) -> AudioChunk {
    AudioChunk {
        samples: vec![100; 960],
        sample_rate: 24_000,
        channels: 1,
        is_end_of_utterance: eou,
        utterance_id: utt,
        ..AudioChunk::default()
    }
}

async fn next_annotation(rx: &mut mpsc::Receiver<ServerMessage>) -> Option<AudioAnnotation> {
    let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .ok()??;
    match msg {
        ServerMessage::Annotation(a) => Some(a),
        other => panic!("expected annotation, got {other:?}"),
    }
}

#[tokio::test]
async fn batches_by_offset_and_flushes_on_eou() {
    let (s, commands, _) = spec("echo", cfg());
    let (out_tx, mut out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 24_000);
    // 5 x 40 ms: the first batch releases at 80 ms (first_batch_ms); the
    // remaining 120 ms accumulates until the 40 ms EOU chunk completes a
    // 160 ms batch, which is flushed as the end-of-utterance segment.
    for _ in 0..5 {
        hubs.offer(&chunk(1, false));
    }
    hubs.offer(&chunk(1, true));
    let a1 = next_annotation(&mut out_rx).await.unwrap();
    assert_eq!((a1.utterance_id, a1.start_ms, a1.duration_ms), (1, 0, 80));
    let a2 = next_annotation(&mut out_rx).await.unwrap();
    assert_eq!((a2.start_ms, a2.duration_ms), (80, 160));
    // A bare EOU sentinel (no samples) arriving right after a release
    // still flushes, as a zero-length end-of-utterance segment.
    let mut eou_only = chunk(2, true);
    eou_only.samples.clear();
    hubs.offer(&chunk(2, false));
    hubs.offer(&chunk(2, false)); // 80 ms -> released
    hubs.offer(&eou_only);
    let b1 = next_annotation(&mut out_rx).await.unwrap();
    assert_eq!((b1.utterance_id, b1.start_ms, b1.duration_ms), (2, 0, 80));
    let b2 = next_annotation(&mut out_rx).await.unwrap();
    assert_eq!((b2.start_ms, b2.duration_ms), (80, 0));
    // Segments in order; the EOU flag rides the last segment of each utterance.
    let cmds = commands.lock();
    let segs: Vec<&AudioSegment> = cmds
        .iter()
        .filter_map(|c| match c {
            AnnotatorCommand::Segment(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(segs.len(), 4);
    assert!(!segs[0].end_of_utterance && segs[1].end_of_utterance);
    assert!(!segs[2].end_of_utterance && segs[3].end_of_utterance);
    assert_eq!(segs[3].duration_ms(), 0);
    drop(cmds);
    let comps = hubs.turn_components(1);
    assert_eq!(comps.len(), 1);
    assert_eq!(comps[0].component_id, "annotator:fake");
    assert_eq!(comps[0].kind, "annotator");
    assert_eq!(comps[0].extras["segments"], 2);
    assert_eq!(comps[0].extras["annotations"], 2);
    assert_eq!(comps[0].extras["breaker_open"], false);
    // Collected once: a second collect is empty for that utterance.
    assert_eq!(hubs.turn_components(1)[0].extras["segments"], 0);
}

/// The downlink cost of the annotation
/// stream, which the fidelity ladder is calibrated on. Asserts the counter
/// matches exactly what was put on the wire rather than a magic number, so
/// it stays honest if the payload shape changes.
#[tokio::test]
async fn annotation_bytes_and_bitrate_are_recorded() {
    let (s, _commands, _) = spec("echo", cfg());
    let (out_tx, mut out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 24_000);
    // 80 ms releases the first batch; the EOU chunk flushes a 40 ms tail.
    hubs.offer(&chunk(1, false));
    hubs.offer(&chunk(1, false));
    hubs.offer(&chunk(1, true));

    let a1 = next_annotation(&mut out_rx).await.unwrap();
    let a2 = next_annotation(&mut out_rx).await.unwrap();
    let expected: u64 = [&a1, &a2]
        .iter()
        .map(|a| serde_json::to_string(&a.extras).unwrap().len() as u64)
        .sum();
    assert!(expected > 0, "the fake annotator emits a non-empty payload");

    let comps = hubs.turn_components(1);
    assert_eq!(comps[0].extras["annotations"], 2);
    assert_eq!(
        comps[0].extras["annotation_bytes"], expected,
        "counter must equal the bytes actually forwarded"
    );
    let kbps = comps[0].extras["annotation_kbps"]
        .as_f64()
        .expect("bitrate present once audio has been annotated");
    assert!(kbps > 0.0, "bitrate was {kbps}");
}

/// The bitrate denominator is the audio the annotations DESCRIBE, not the
/// audio emitted so far. Annotations trail the audio by the pipeline lag
/// (`lag_max_ms` was ~774 ms in the first real measurement), so using
/// `emitted` under-reported the bitrate several-fold — the whole point of
/// the metric is to compare honestly against the audio bitrate.
#[tokio::test]
async fn bitrate_denominator_is_annotated_span_not_emitted_audio() {
    let (s, _commands, _) = spec("echo", cfg());
    let (out_tx, mut out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 24_000);

    // 80 ms releases the first batch. Then pile on a further 400 ms of
    // audio *without* waiting, so `emitted` races far ahead of the single
    // annotation that describes only the first 80 ms.
    hubs.offer(&chunk(1, false));
    hubs.offer(&chunk(1, false));
    let a1 = next_annotation(&mut out_rx).await.unwrap();
    assert_eq!(a1.start_ms + a1.duration_ms, 80, "describes 80 ms of audio");
    for _ in 0..10 {
        hubs.offer(&chunk(1, false));
    }

    let comps = hubs.turn_components(1);
    let bytes = comps[0].extras["annotation_bytes"].as_u64().unwrap();
    let kbps = comps[0].extras["annotation_kbps"].as_f64().unwrap();
    // bits over the 80 ms actually described — not over the ~480 ms emitted.
    let expected = (bytes as f64 * 8.0) / 80.0;
    assert!(
        (kbps - expected).abs() < 0.001,
        "kbps {kbps} should be {expected} (bytes {bytes} over the 80 ms described)"
    );
}

/// A turn that annotated no audio reports no bitrate rather than dividing
/// by zero.
#[tokio::test]
async fn annotation_bitrate_absent_without_audio() {
    let m = TurnAnnotatorMetrics::default();
    assert!(m.annotation_kbps().is_none());
    let extras = serde_json::to_value(m.extras(false)).unwrap();
    assert!(extras.get("annotation_kbps").is_none());
    assert_eq!(extras["annotation_bytes"], 0);
}

#[tokio::test]
async fn warmup_output_is_discarded() {
    let mut c = cfg();
    c.warmup = true;
    let (s, commands, _) = spec("echo", c);
    let (out_tx, mut out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 16_000);
    hubs.offer(&chunk(1, true));
    let a = next_annotation(&mut out_rx).await.unwrap();
    assert_eq!(
        a.utterance_id, 1,
        "warm-up annotation (utterance 0) must not reach the client"
    );
    assert!(matches!(
        commands.lock().first(),
        Some(AnnotatorCommand::Warmup {
            sample_rate: 16_000,
            ..
        })
    ));
}

#[tokio::test]
async fn cancel_drops_pending_and_late_annotations() {
    let (s, commands, _) = spec("silent", cfg());
    let (out_tx, mut out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 24_000);
    hubs.offer(&chunk(7, false));
    hubs.offer(&chunk(7, false)); // 80 ms -> first segment sent
    hubs.offer(&chunk(7, false)); // accumulating
    hubs.cancel(7);
    hubs.offer(&chunk(7, false)); // after cancel: ignored
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cmds = commands.lock().clone();
    assert!(
        cmds.iter()
            .any(|c| matches!(c, AnnotatorCommand::Cancel { utterance_id: 7 }))
    );
    assert_eq!(
        cmds.iter()
            .filter(|c| matches!(c, AnnotatorCommand::Segment(_)))
            .count(),
        1,
        "nothing after the cancel is sent"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), out_rx.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn drop_oldest_when_tee_overflows() {
    let (s, _, _) = spec("silent", cfg());
    let (out_tx, _out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 24_000);
    // Push synchronously far past capacity before the hub task can run.
    for _ in 0..(TEE_CAPACITY + 10) {
        hubs.hubs[0].shared.push(TeeItem::Chunk {
            utterance_id: 2,
            samples: vec![0; 960],
            sample_rate: 24_000,
            channels: 1,
            end_of_utterance: false,
        });
    }
    assert!(hubs.hubs[0].shared.queue.lock().len() <= TEE_CAPACITY);
    let m = hubs.hubs[0].shared.metrics.lock();
    assert_eq!(m.per_utterance[&2].dropped_chunks, 10);
}

#[tokio::test(start_paused = true)]
async fn breaker_opens_after_timeouts_and_half_opens_after_cooldown() {
    let (s, commands, _) = spec("silent", cfg());
    let (out_tx, _out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 24_000);
    // Three utterances, one 80 ms segment each -> three timeouts. Yield
    // BEFORE advancing so the hub sends the segment at the current
    // (paused) instant and the advance is what makes it time out.
    for utt in 1..=3 {
        hubs.offer(&chunk(utt, false));
        hubs.offer(&chunk(utt, true));
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(150)).await;
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(30)).await;
    tokio::task::yield_now().await;
    assert!(
        hubs.hubs[0].shared.metrics.lock().breaker_open,
        "breaker open after 3 timeouts"
    );
    let sent_before = commands.lock().len();
    // While open: the utterance's single (merged) segment is skipped, not sent.
    hubs.offer(&chunk(4, false));
    hubs.offer(&chunk(4, true));
    tokio::task::yield_now().await;
    assert_eq!(commands.lock().len(), sent_before);
    assert_eq!(hubs.turn_components(4)[0].extras["skipped_segments"], 1);
    // After the cooldown one probe goes through (half-open).
    tokio::time::advance(Duration::from_millis(250)).await;
    tokio::task::yield_now().await;
    hubs.offer(&chunk(5, false));
    hubs.offer(&chunk(5, true));
    tokio::task::yield_now().await;
    assert!(commands.lock().len() > sent_before, "half-open probe sent");
}

#[tokio::test]
async fn stream_death_is_isolated_and_reopened() {
    let mut c = cfg();
    c.breaker_cooldown_ms = 50;
    let (s, _, runs) = spec("die", c);
    let (out_tx, _out_rx) = mpsc::channel(32);
    let hubs = AnnotatorHubs::spawn(&[s], "s1", out_tx, 24_000);
    hubs.offer(&chunk(1, false));
    hubs.offer(&chunk(1, true));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        runs.load(Ordering::Relaxed) >= 2,
        "stream reopened after the cooldown"
    );
    drop(hubs);
}

#[tokio::test]
async fn no_hubs_is_a_noop() {
    let hubs = AnnotatorHubs::none();
    assert!(hubs.is_empty());
    hubs.offer(&chunk(1, true));
    hubs.cancel(1);
    assert!(hubs.turn_components(1).is_empty());
}
