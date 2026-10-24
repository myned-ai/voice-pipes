use super::*;

fn seg(samples: Vec<i16>, rate: u32) -> AudioSegment {
    AudioSegment {
        utterance_id: 3,
        start_ms: 160,
        samples,
        sample_rate: rate,
        channels: 1,
        end_of_utterance: false,
    }
}

#[test]
fn segment_duration_uses_frames_not_samples() {
    let mut s = seg(vec![0; 4800], 24_000);
    assert_eq!(s.duration_ms(), 200);
    s.channels = 2;
    assert_eq!(s.duration_ms(), 100);
    s.sample_rate = 0;
    assert_eq!(s.duration_ms(), 0);
}

#[test]
fn level_frames_track_amplitude() {
    // 25 fps at 1000 Hz -> 40-sample frames; two frames: silence, full scale.
    let mut samples = vec![0i16; 40];
    samples.extend(std::iter::repeat_n(i16::MAX, 40));
    let frames = LevelAnnotator::frames(&samples, 1000, 1);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], [0.0, 0.0]);
    assert!(frames[1][0] > 0.99 && frames[1][1] > 0.99);
}

#[test]
fn level_annotation_carries_offset_space_fields() {
    let a = LevelAnnotator::annotate(&seg(vec![1000; 2400], 24_000)).expect("annotation");
    assert_eq!(a.utterance_id, 3);
    assert_eq!(a.start_ms, 160);
    assert_eq!(a.duration_ms, 100);
    assert_eq!(a.kind, "audio_level");
    assert_eq!(a.extras["fps"], LEVEL_FPS);
    assert_eq!(a.extras["frames"].as_array().map(Vec::len), Some(3));
    assert!(LevelAnnotator::annotate(&seg(vec![], 24_000)).is_none());
}

#[tokio::test]
async fn level_annotator_streams_one_annotation_per_segment() {
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (out_tx, mut out_rx) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        LevelAnnotator
            .run("s".into(), cmd_rx, out_tx)
            .await
            .unwrap();
    });
    cmd_tx
        .send(AnnotatorCommand::Warmup {
            sample_rate: 24_000,
            channels: 1,
        })
        .await
        .unwrap();
    cmd_tx
        .send(AnnotatorCommand::Segment(seg(vec![500; 2400], 24_000)))
        .await
        .unwrap();
    cmd_tx
        .send(AnnotatorCommand::Cancel { utterance_id: 3 })
        .await
        .unwrap();
    drop(cmd_tx);
    let a = out_rx.recv().await.expect("one annotation");
    assert_eq!(a.utterance_id, 3);
    assert!(
        out_rx.recv().await.is_none(),
        "warmup/cancel produce nothing"
    );
    task.await.unwrap();
}
