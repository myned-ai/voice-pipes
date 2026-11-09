use super::super::*;
use tokio::sync::mpsc;

/// The VAD filter must re-derive its 20 ms framing when the input rate
/// changes mid-session, and stamp the new rate on what it forwards.
#[tokio::test]
async fn vad_filter_reframes_on_sample_rate_change() {
    let (raw_tx, raw_rx) = mpsc::channel::<AudioChunk>(32);
    let (filtered_tx, mut filtered_rx) = mpsc::channel::<AudioChunk>(64);
    let vad = Box::new(crate::vad::segmenter::EnergyVad::new(10.0));
    let _handle = tokio::spawn(vad_filter_task(raw_rx, filtered_tx, vad, 100, 100, None));

    let send = |rate: u32, n: usize| {
        let tx = raw_tx.clone();
        async move {
            for _ in 0..n {
                tx.send(AudioChunk {
                    samples: vec![1000i16; (rate / 50) as usize], // 20 ms
                    sample_rate: rate,
                    channels: 1,
                    is_end_of_utterance: false,
                    ..Default::default()
                })
                .await
                .unwrap();
            }
        }
    };

    send(16_000, 4).await;
    let mut got_16k = 0;
    while let Ok(Some(c)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), filtered_rx.recv()).await
    {
        assert_eq!((c.sample_rate, c.samples.len()), (16_000, 320));
        got_16k += 1;
    }
    assert!(got_16k >= 3, "speech should have been forwarded at 16 kHz");

    // Rate switch: onset confirmation restarts, then everything forwarded
    // must be 20 ms frames stamped 48 kHz.
    send(48_000, 6).await;
    let mut got_48k = 0;
    while let Ok(Some(c)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), filtered_rx.recv()).await
    {
        assert_eq!(
            (c.sample_rate, c.samples.len()),
            (48_000, 960),
            "chunk after the rate switch still carries the old framing"
        );
        got_48k += 1;
    }
    assert!(got_48k >= 3, "speech should have been forwarded at 48 kHz");
}

#[tokio::test]
async fn test_vad_eou_samples_retained() {
    let (raw_tx, raw_rx) = mpsc::channel::<AudioChunk>(8);
    let (filtered_tx, mut filtered_rx) = mpsc::channel::<AudioChunk>(8);
    let vad = Box::new(crate::vad::segmenter::EnergyVad::new(10.0));

    let _handle = tokio::spawn(vad_filter_task(raw_rx, filtered_tx, vad, 100, 100, None));

    // Send 3 speech chunks to activate speech (320 samples each)
    for _ in 0..3 {
        raw_tx
            .send(AudioChunk {
                samples: vec![1000i16; 320],
                sample_rate: 16000,
                channels: 1,
                is_end_of_utterance: false,
                ..Default::default()
            })
            .await
            .unwrap();
    }

    // Drain active speech chunks
    let mut processed = 0;
    for _ in 0..10 {
        if let Ok(Some(chunk)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), filtered_rx.recv()).await
        {
            processed += chunk.samples.len();
        } else {
            break;
        }
    }
    assert!(processed > 0);

    // Send a final chunk containing samples AND is_end_of_utterance = true.
    raw_tx
        .send(AudioChunk {
            samples: vec![1000i16; 320],
            sample_rate: 16000,
            channels: 1,
            is_end_of_utterance: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut got_eou_samples = false;
    while let Ok(Some(chunk)) =
        tokio::time::timeout(std::time::Duration::from_millis(100), filtered_rx.recv()).await
    {
        if chunk.is_end_of_utterance {
            break;
        }
        if !chunk.samples.is_empty() {
            got_eou_samples = true;
        }
    }
    assert!(got_eou_samples, "VAD task discarded samples on EOU chunk");
}

/// Emoticons/pictographs must be stripped from TTS input (spoken
/// text) while words and spoken symbols ($, %, +) survive; an
/// emoji-only fragment must reduce to empty so the TTS task skips
/// it like any other empty fragment.
#[test]
fn strip_unspeakable_removes_emoji_keeps_speech() {
    use voice_pipes_sdk::strip_unspeakable;

    assert_eq!(strip_unspeakable("Great! 😊"), "Great!");
    assert_eq!(strip_unspeakable("Hi 😊 there"), "Hi there");
    assert_eq!(strip_unspeakable("👍👍"), "");
    // ZWJ sequences and variation selectors go with their emoji.
    assert_eq!(strip_unspeakable("Family: 👨‍👩‍👧 here"), "Family: here");
    assert_eq!(strip_unspeakable("❤️ you"), "you");
    // Regional-indicator flags.
    assert_eq!(strip_unspeakable("Flag 🇩🇪 test"), "Flag test");
    // Speakable symbols and plain text pass through untouched.
    assert_eq!(strip_unspeakable("Costs $5 + 5% tax"), "Costs $5 + 5% tax");
    assert_eq!(strip_unspeakable("No emoji at all."), "No emoji at all.");
}

/// The speech probe must go true at confirmed onset and back to
/// false once the end-of-utterance sentinel is emitted — it gates
/// the idle re-engagement countdown so the agent doesn't fire an
/// idle prompt while the user is mid-sentence (speech heard but
/// the turn not yet committed).
#[tokio::test]
async fn test_vad_speech_probe_tracks_utterance() {
    use std::sync::atomic::Ordering;

    let (raw_tx, raw_rx) = mpsc::channel::<AudioChunk>(64);
    let (filtered_tx, mut filtered_rx) = mpsc::channel::<AudioChunk>(64);
    let vad = Box::new(crate::vad::segmenter::EnergyVad::new(10.0));
    let probe = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let _handle = tokio::spawn(vad_filter_task(
        raw_rx,
        filtered_tx,
        vad,
        100,
        100,
        Some(std::sync::Arc::clone(&probe)),
    ));

    assert!(!probe.load(Ordering::Acquire), "probe must start false");

    // 3 speech chunks (320 samples = 20ms each) confirm onset.
    for _ in 0..3 {
        raw_tx
            .send(AudioChunk {
                samples: vec![1000i16; 320],
                sample_rate: 16000,
                channels: 1,
                is_end_of_utterance: false,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    // Once a forwarded chunk arrives, onset was confirmed.
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), filtered_rx.recv())
        .await
        .expect("timed out waiting for onset")
        .expect("filter channel closed");
    assert!(!first.is_end_of_utterance);
    assert!(
        probe.load(Ordering::Acquire),
        "probe must be true during active speech"
    );

    // Enough silence (100ms limit -> 5 chunks) ends the utterance.
    for _ in 0..8 {
        raw_tx
            .send(AudioChunk {
                samples: vec![0i16; 320],
                sample_rate: 16000,
                channels: 1,
                is_end_of_utterance: false,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    loop {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(1), filtered_rx.recv())
            .await
            .expect("timed out waiting for EOU sentinel")
            .expect("filter channel closed");
        if chunk.is_end_of_utterance {
            break;
        }
    }
    // The probe clears right after the sentinel send; poll briefly to
    // absorb the task-scheduling race.
    for _ in 0..100 {
        if !probe.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("probe still true after end-of-utterance sentinel");
}

/// The pre-roll must reach the recogniser, and its size must be what the
/// caller asked for.
///
/// A detector recognises a word slightly after it starts, so whatever is not
/// held back from before the onset is lost. At the 100 ms this was fixed at,
/// "hello, how are you" reached the recogniser as "o, how are you" and read
/// as a recogniser fault. The test drives silence into speech and counts what
/// comes out ahead of the onset.
#[tokio::test]
async fn pre_roll_keeps_the_configured_audio_from_before_the_onset() {
    async fn leading_silence_chunks(pre_roll_ms: u32) -> usize {
        let (raw_tx, raw_rx) = mpsc::channel::<AudioChunk>(256);
        let (filtered_tx, mut filtered_rx) = mpsc::channel::<AudioChunk>(256);
        // Threshold well above the "silence" level and below the speech one.
        let vad = Box::new(crate::vad::segmenter::EnergyVad::new(500.0));
        let _h = tokio::spawn(vad_filter_task(
            raw_rx,
            filtered_tx,
            vad,
            1000,
            pre_roll_ms,
            None,
        ));

        let send = |amp: i16, n: usize| {
            let tx = raw_tx.clone();
            async move {
                for _ in 0..n {
                    tx.send(AudioChunk {
                        samples: vec![amp; 320], // 20 ms at 16 kHz
                        sample_rate: 16_000,
                        channels: 1,
                        is_end_of_utterance: false,
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                }
            }
        };

        // A second of near-silence, then speech. Only the tail of the silence
        // should survive, and only as much of it as the pre-roll allows.
        send(10, 50).await;
        send(8000, 20).await;
        drop(raw_tx);

        let mut leading = 0usize;
        while let Some(c) = filtered_rx.recv().await {
            if c.samples.is_empty() {
                continue;
            }
            if c.samples[0].abs() < 500 {
                leading += 1;
            } else {
                break;
            }
        }
        leading
    }

    // 20 ms chunks, so 300 ms is 15 of them and 100 ms is 5.
    assert_eq!(
        leading_silence_chunks(300).await,
        15,
        "300 ms of pre-roll must survive the onset"
    );
    assert_eq!(
        leading_silence_chunks(100).await,
        5,
        "the pre-roll must be the size the caller asked for, not a fixed one"
    );
    // The default is what utterances actually get; pin it so a change is a
    // decision rather than a drift.
    assert_eq!(crate::config::DEFAULT_VAD_PRE_ROLL_MS, 300);
}

/// P2.4: Trailing VAD silence hangover frames must not reset `last_speech_instant`
/// in `SttLoop::on_chunk`. A 500 ms VAD hangover before the EOU sentinel commits
/// the turn must produce `endpointing_latency >= 500 ms`.
#[tokio::test]
async fn vad_hangover_is_included_in_endpointing_ms() {
    use crate::stt::{SttCapability, SttError, SttProvider, TranscriptionResult};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FinalOnLastChunkStt;

    #[async_trait]
    impl SttProvider for FinalOnLastChunkStt {
        async fn transcribe(
            &self,
            _audio: &crate::audio::AudioFrame,
        ) -> Result<TranscriptionResult, SttError> {
            Ok(TranscriptionResult::new("hangover test", true))
        }

        async fn transcribe_stream(
            &self,
            _request: crate::stt::RecognitionRequest,
            mut audio_rx: mpsc::Receiver<crate::stt::AudioChunk>,
            tx: mpsc::Sender<TranscriptionResult>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), SttError> {
            while let Some(chunk) = audio_rx.recv().await {
                if chunk.is_last {
                    let _ = tx
                        .send(
                            TranscriptionResult::new("hangover test", true)
                                .with_is_end_of_turn(Some(true)),
                        )
                        .await;
                    break;
                }
            }
            Ok(())
        }

        fn expected_sample_rate(&self) -> u32 {
            16_000
        }

        fn name(&self) -> &'static str {
            "final-on-last-stt"
        }

        fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
            std::collections::HashSet::new()
        }
    }

    let (raw_tx, raw_rx) = mpsc::channel::<AudioChunk>(64);
    let (filtered_tx, filtered_rx) = mpsc::channel::<AudioChunk>(64);
    let (utt_tx, mut utt_rx) = mpsc::channel::<Utterance>(4);

    let vad = Box::new(crate::vad::segmenter::EnergyVad::new(100.0));
    let _vad_handle = tokio::spawn(vad_filter_task(raw_rx, filtered_tx, vad, 500, 100, None));

    let stt: Arc<dyn SttProvider> = Arc::new(FinalOnLastChunkStt);
    let td = Arc::new(crate::stt::turn::SilenceTimeoutDetector::new(0));
    let _stt_handle = tokio::spawn(stt_task(
        filtered_rx,
        utt_tx,
        None,
        stt,
        td,
        None,
        None,
        0,
        None,
        None,
    ));

    // 4 speech frames (80 ms) confirm onset.
    for _ in 0..4 {
        raw_tx
            .send(AudioChunk {
                samples: vec![4000i16; 320],
                sample_rate: 16_000,
                channels: 1,
                is_end_of_utterance: false,
                ..Default::default()
            })
            .await
            .unwrap();
    }

    // Feed 26 silence frames at real-time 20 ms cadence (> 500 ms hangover) to trigger EOU.
    for _ in 0..26 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        raw_tx
            .send(AudioChunk {
                samples: vec![0i16; 320],
                sample_rate: 16_000,
                channels: 1,
                is_end_of_utterance: false,
                ..Default::default()
            })
            .await
            .unwrap();
    }
    drop(raw_tx);

    let utt = tokio::time::timeout(std::time::Duration::from_secs(2), utt_rx.recv())
        .await
        .expect("timed out waiting for utterance")
        .expect("utterance channel closed");
    let endpointing = utt
        .endpointing_latency
        .expect("endpointing_latency must be set");
    assert!(
        endpointing >= std::time::Duration::from_millis(480),
        "endpointing_latency must include the ~500 ms VAD hangover, got {endpointing:?}"
    );
    assert!(
        utt.committed_at.is_some(),
        "committed_at must be stamped on commit"
    );
}
