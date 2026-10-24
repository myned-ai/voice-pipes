use super::*;
use crate::pipeline::AudioChunk;

#[test]
fn chunk_duration_calculation() {
    let chunk = AudioChunk {
        samples: vec![0i16; 48000], // 1 second at 48kHz
        sample_rate: 48000,
        channels: 1,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    let dur = chunk_audio_duration(&chunk);
    assert_eq!(dur, Duration::from_secs(1));
}

#[test]
fn chunk_duration_half_second() {
    let chunk = AudioChunk {
        samples: vec![0i16; 8000], // 0.5s at 16kHz
        sample_rate: 16000,
        channels: 1,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    let dur = chunk_audio_duration(&chunk);
    assert_eq!(dur, Duration::from_millis(500));
}

#[test]
fn chunk_duration_empty() {
    let chunk = AudioChunk {
        samples: vec![],
        sample_rate: 48000,
        channels: 1,
        is_end_of_utterance: true,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    assert_eq!(chunk_audio_duration(&chunk), Duration::ZERO);
}

#[tokio::test]
async fn disabled_pacer_is_passthrough() {
    let (tx, rx) = mpsc::channel(16);
    let suppress = Arc::new(AtomicBool::new(false));
    let mut paced_rx = spawn_pacer(PacerConfig::disabled(), rx, suppress);

    // Send a control message.
    tx.send(ServerMessage::Control(
        crate::transport::ServerControl::StateChanged {
            state: crate::transport::PipelineState::Processing,
        },
    ))
    .await
    .unwrap();
    drop(tx);

    let msg = paced_rx.recv().await;
    assert!(msg.is_some());
}

/// StateChanged{Listening} passes through immediately -- NOT held behind audio.
///
/// Holding it was tried but causes a worse problem: for long responses the
/// audio queue holds tens of seconds of data, freezing the client's
/// utteranceId counter and breaking word-reveal and interruptions.
///
/// The spurious-onPlaybackStart side-effect (which motivated the sentinel
/// approach) is handled on the client by guarding against pipelineState !=
/// "speaking" before creating a placeholder bubble.
#[tokio::test]
async fn listening_passes_through_immediately() {
    use crate::pipeline::AudioChunk;
    use crate::transport::{PipelineState, ServerControl};

    let config = PacerConfig {
        enabled: true,
        pace_factor: 100.0,
        burst_ms: 0,
        max_buffer_ms: 300_000,
    };

    let (tx, rx) = mpsc::channel(16);
    let suppress = Arc::new(AtomicBool::new(false));
    let mut paced_rx = spawn_pacer(config, rx, suppress);

    // Enqueue audio so the pacer has buffered chunks.
    let audio_chunk = AudioChunk {
        samples: vec![0i16; 480], // 10ms at 48kHz
        sample_rate: 48000,
        channels: 1,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    tx.send(ServerMessage::Audio(audio_chunk)).await.unwrap();

    // StateChanged{Listening} arrives while audio is still buffered.
    tx.send(ServerMessage::Control(ServerControl::StateChanged {
        state: PipelineState::Listening,
    }))
    .await
    .unwrap();
    drop(tx);

    let mut received = Vec::new();
    while let Some(msg) = paced_rx.recv().await {
        match &msg {
            ServerMessage::Audio(_) => received.push("audio"),
            ServerMessage::Control(ServerControl::StateChanged {
                state: PipelineState::Listening,
            }) => received.push("listening"),
            _ => received.push("other"),
        }
    }

    // Listening arrives BEFORE the buffered audio -- this is intentional.
    // The client's pipelineState guard prevents the spurious placeholder.
    assert_eq!(
        received,
        vec!["listening", "audio"],
        "StateChanged{{Listening}} must pass through immediately (not held behind audio)"
    );
}

fn audio_100ms() -> ServerMessage {
    ServerMessage::Audio(AudioChunk {
        samples: vec![0i16; 1600], // 100 ms at 16 kHz
        sample_rate: 16_000,
        channels: 1,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 1,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    })
}

/// Release instants of every audio frame the pacer lets through.
async fn release_times(
    paced_rx: &mut mpsc::Receiver<ServerMessage>,
    expected: usize,
) -> Vec<Instant> {
    let mut times = Vec::new();
    while times.len() < expected {
        match tokio::time::timeout(Duration::from_secs(30), paced_rx.recv()).await {
            Ok(Some(ServerMessage::Audio(_))) => times.push(Instant::now()),
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    times
}

/// A stereo chunk holds half as many frames as samples.
#[test]
fn chunk_duration_accounts_for_channels() {
    let chunk = AudioChunk {
        samples: vec![0i16; 32_000], // 1 s of stereo at 16 kHz
        sample_rate: 16_000,
        channels: 2,
        is_end_of_utterance: false,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    assert_eq!(chunk_audio_duration(&chunk), Duration::from_secs(1));
}

/// After the burst is spent, a backlog drains at exactly the configured
/// pace: N chunks of 100 ms at 2x take (N - 1) * 50 ms from the first
/// release to the last.
#[tokio::test(start_paused = true)]
async fn backlog_drains_at_the_configured_pace() {
    let config = PacerConfig {
        enabled: true,
        pace_factor: 2.0,
        burst_ms: 0,
        max_buffer_ms: 300_000,
    };
    let (tx, rx) = mpsc::channel(64);
    let mut paced_rx = spawn_pacer(config, rx, Arc::new(AtomicBool::new(false)));

    let n = 10u32;
    for _ in 0..n {
        tx.send(audio_100ms()).await.unwrap();
    }
    let times = release_times(&mut paced_rx, n as usize).await;
    assert_eq!(times.len(), n as usize);
    let span = times[n as usize - 1] - times[0];
    let ideal = Duration::from_millis(50) * (n - 1);
    let tolerance = ideal.mul_f64(0.05);
    assert!(
        span.abs_diff(ideal) <= tolerance,
        "drained in {span:?}, expected {ideal:?} +/- 5 percent"
    );
}

/// A source that keeps sending must not postpone releases: chunks go out
/// on the pacing schedule while new ones arrive every 10 ms, instead of
/// the whole backlog waiting for the source to go quiet.
#[tokio::test(start_paused = true)]
async fn steady_input_does_not_delay_releases() {
    let config = PacerConfig {
        enabled: true,
        pace_factor: 2.0,
        burst_ms: 0,
        max_buffer_ms: 300_000,
    };
    let (tx, rx) = mpsc::channel(64);
    let mut paced_rx = spawn_pacer(config, rx, Arc::new(AtomicBool::new(false)));

    let producer = tokio::spawn(async move {
        for _ in 0..30 {
            tx.send(audio_100ms()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Keep the source open a while longer so nothing can be released
        // "because the source went quiet".
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(tx);
    });

    let times = release_times(&mut paced_rx, 6).await;
    assert_eq!(times.len(), 6);
    for pair in times.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap >= Duration::from_millis(45) && gap <= Duration::from_millis(55),
            "release cadence must stay at 50 ms under steady input, got {gap:?}"
        );
    }
    producer.abort();
}

/// The burst budget lets the first `burst_ms` of an utterance out at
/// once; pacing starts after it.
#[tokio::test(start_paused = true)]
async fn burst_releases_the_head_of_the_utterance_immediately() {
    let config = PacerConfig {
        enabled: true,
        pace_factor: 2.0,
        burst_ms: 300,
        max_buffer_ms: 300_000,
    };
    let (tx, rx) = mpsc::channel(64);
    let mut paced_rx = spawn_pacer(config, rx, Arc::new(AtomicBool::new(false)));
    for _ in 0..6 {
        tx.send(audio_100ms()).await.unwrap();
    }
    let times = release_times(&mut paced_rx, 6).await;
    assert!(
        times[2] - times[0] < Duration::from_millis(5),
        "burst is immediate"
    );
    assert!(
        times[5] - times[2] >= Duration::from_millis(140),
        "the remainder is paced at 50 ms per chunk"
    );
}
