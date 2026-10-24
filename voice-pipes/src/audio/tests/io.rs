use super::*;
use crate::vad::segmenter::EnergyVad;

#[test]
fn test_mix_to_mono_stereo() {
    let stereo = vec![100i16, 200, 300, 400];
    let mono = mix_to_mono(&stereo, 2);
    assert_eq!(mono, vec![150, 350]);
}

#[test]
fn test_mix_to_mono_single_channel() {
    let mono_in = vec![100i16, 200, 300];
    let mono = mix_to_mono(&mono_in, 1);
    assert_eq!(mono, vec![100, 200, 300]);
}

#[test]
fn test_compute_rms_silence() {
    assert_eq!(compute_rms(&[]), 0.0);
    assert_eq!(compute_rms(&[0, 0, 0, 0]), 0.0);
}

#[test]
fn test_compute_rms_signal() {
    let rms = compute_rms(&[1000, -1000, 1000, -1000]);
    assert!((rms - 1000.0).abs() < 0.1);
}

// ── mic_stream_loop tests ───────────────────────────────

/// Helper: generate a sine-tone chunk with RMS well above `SPEECH_THRESHOLD`.
fn speech_chunk(len: usize) -> Vec<i16> {
    // Amplitude 5000 >> threshold of 200
    vec![5000i16; len]
}

/// Helper: generate a silence chunk with RMS ~= 0.
fn silence_chunk(len: usize) -> Vec<i16> {
    vec![0i16; len]
}

#[test]
fn mic_stream_emits_speech_chunks() {
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<AudioChunk>(16);
    let stop = Arc::new(AtomicBool::new(false));

    let chunk_samples = 320; // 20ms @ 16 kHz
    let silence_limit = 8000; // 500ms @ 16 kHz

    // Send 4 speech chunks (>= MIN_ONSET_CHUNKS=3) then stop
    for _ in 0..4 {
        raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    }
    drop(raw_tx); // will cause try_recv to fail -> sleep -> next iteration -> stop check

    // Set stop after a short delay so the loop exits
    let stop2 = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        stop2.store(true, Ordering::Relaxed);
    });

    let mut vad = EnergyVad::default();
    run_mic_stream_loop(
        &raw_rx,
        &async_tx,
        &stop,
        16000,
        1,
        chunk_samples,
        silence_limit,
        &mut vad,
    );

    // Should have received 4 audio chunks (all speech, after onset confirmation)
    let c1 = async_rx.try_recv().unwrap();
    assert_eq!(c1.samples.len(), chunk_samples);
    assert_eq!(c1.sample_rate, 16000);
    assert!(!c1.is_end_of_utterance);

    let c2 = async_rx.try_recv().unwrap();
    assert_eq!(c2.samples.len(), chunk_samples);
    assert!(!c2.is_end_of_utterance);
}

#[test]
fn mic_stream_suppresses_silence() {
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<AudioChunk>(16);
    let stop = Arc::new(AtomicBool::new(false));

    let chunk_samples = 320;
    let silence_limit = 8000;

    // Send only silence
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    drop(raw_tx);

    let stop2 = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        stop2.store(true, Ordering::Relaxed);
    });

    let mut vad = EnergyVad::default();
    run_mic_stream_loop(
        &raw_rx,
        &async_tx,
        &stop,
        16000,
        1,
        chunk_samples,
        silence_limit,
        &mut vad,
    );

    // No chunks should have been emitted (silence suppressed)
    assert!(async_rx.try_recv().is_err());
}

#[test]
fn mic_stream_end_of_utterance_on_silence_after_speech() {
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<AudioChunk>(32);
    let stop = Arc::new(AtomicBool::new(false));

    let chunk_samples = 160; // 10ms @ 16 kHz
    let silence_limit = 480; // 30ms @ 16 kHz -- 3 silent chunks trigger end

    // 3 speech chunks (meets MIN_ONSET_CHUNKS) -> 3 silence chunks (>= silence_limit)
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    drop(raw_tx);

    let stop2 = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        stop2.store(true, Ordering::Relaxed);
    });

    let mut vad = EnergyVad::default();
    run_mic_stream_loop(
        &raw_rx,
        &async_tx,
        &stop,
        16000,
        1,
        chunk_samples,
        silence_limit,
        &mut vad,
    );

    // Collect all chunks
    let mut chunks = Vec::new();
    while let Ok(c) = async_rx.try_recv() {
        chunks.push(c);
    }

    // Should have 7 chunks: 3 speech + 3 silence + 1 empty end-of-utterance sentinel
    // (silence chunks are forwarded during active speech to avoid clipping consonants)
    assert_eq!(chunks.len(), 7, "expected 7 chunks, got {}", chunks.len());

    // First 6 chunks: speech/silence, not end
    for c in &chunks[..6] {
        assert!(!c.is_end_of_utterance);
        assert_eq!(c.samples.len(), chunk_samples);
    }
    // Last chunk: empty sentinel
    let last = chunks.last().unwrap();
    assert!(
        last.is_end_of_utterance,
        "last chunk should be end-of-utterance"
    );
    assert!(last.samples.is_empty(), "sentinel should have no samples");
}

#[test]
fn mic_stream_resets_after_end_of_utterance() {
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<AudioChunk>(64);
    let stop = Arc::new(AtomicBool::new(false));

    let chunk_samples = 160;
    let silence_limit = 320; // 2 silent chunks to trigger end

    // First utterance: 3 speech (onset) + 2 silence (end)
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    // Gap of pure silence (should be suppressed -- speech_active is false)
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    // Second utterance: 3 speech (onset) + 2 silence (end)
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    drop(raw_tx);

    let stop2 = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        stop2.store(true, Ordering::Relaxed);
    });

    let mut vad = EnergyVad::default();
    run_mic_stream_loop(
        &raw_rx,
        &async_tx,
        &stop,
        16000,
        1,
        chunk_samples,
        silence_limit,
        &mut vad,
    );

    let mut chunks = Vec::new();
    while let Ok(c) = async_rx.try_recv() {
        chunks.push(c);
    }

    // Each utterance includes speech chunks, silence chunks, and one empty sentinel.
    // First utterance: 3 speech + 2 silence + 1 sentinel = 6 chunks.
    // Second utterance flushes 1 pre-roll silence + 3 speech + 2 silence + 1 sentinel = 7 chunks.
    // Total = 6 + 7 = 13 chunks.
    let eou_count = chunks.iter().filter(|c| c.is_end_of_utterance).count();
    assert_eq!(
        eou_count, 2,
        "expected 2 end-of-utterance markers, got {eou_count}"
    );
    assert_eq!(
        chunks.len(),
        13,
        "expected 13 total chunks, got {}",
        chunks.len()
    );
}

#[test]
fn mic_stream_stop_flag_exits() {
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let (async_tx, _async_rx) = tokio::sync::mpsc::channel::<AudioChunk>(16);
    let stop = Arc::new(AtomicBool::new(true)); // pre-set

    // Feed data that would normally produce output
    raw_tx.send(speech_chunk(320)).unwrap();
    drop(raw_tx);

    // Should return immediately due to stop flag
    let mut vad = EnergyVad::default();
    run_mic_stream_loop(&raw_rx, &async_tx, &stop, 16000, 1, 320, 8000, &mut vad);
    // If we get here without hanging, the test passes
}

#[test]
fn mic_stream_noise_spike_rejected_by_onset_confirmation() {
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<i16>>();
    let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<AudioChunk>(32);
    let stop = Arc::new(AtomicBool::new(false));

    let chunk_samples = 160; // 10ms @ 16 kHz
    let silence_limit = 480;

    // 1-2 speech chunks followed by silence = noise spike, should be discarded.
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(speech_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    raw_tx.send(silence_chunk(chunk_samples)).unwrap();
    drop(raw_tx);

    let stop2 = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        stop2.store(true, Ordering::Relaxed);
    });

    let mut vad = EnergyVad::default();
    run_mic_stream_loop(
        &raw_rx,
        &async_tx,
        &stop,
        16000,
        1,
        chunk_samples,
        silence_limit,
        &mut vad,
    );

    // No chunks should be emitted -- onset never confirmed.
    assert!(
        async_rx.try_recv().is_err(),
        "noise spike (< MIN_ONSET_CHUNKS) should not produce any output"
    );
}
