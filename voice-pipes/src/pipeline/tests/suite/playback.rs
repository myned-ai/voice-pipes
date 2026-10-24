use super::super::*;
use crate::pipeline::playback::process_chunk_for_device;

#[test]
fn test_playback_channel_mixing() {
    // Case 1: Stereo input to Stereo output (equal channels)
    let chunk = AudioChunk {
        samples: vec![1000, 2000, 3000, 4000], // L0, R0, L1, R1
        channels: 2,
        sample_rate: 16000,
        ..Default::default()
    };
    let output = process_chunk_for_device(&chunk, 2, 16000);
    let expected: Vec<f32> = vec![
        1000.0 / i16::MAX as f32,
        2000.0 / i16::MAX as f32,
        3000.0 / i16::MAX as f32,
        4000.0 / i16::MAX as f32,
    ];
    assert_eq!(output, expected);

    // Case 2: Mono input to Stereo output (mono expansion)
    let chunk_mono = AudioChunk {
        samples: vec![1000, 2000],
        channels: 1,
        sample_rate: 16000,
        ..Default::default()
    };
    let output_mono = process_chunk_for_device(&chunk_mono, 2, 16000);
    let expected_mono: Vec<f32> = vec![
        1000.0 / i16::MAX as f32,
        1000.0 / i16::MAX as f32,
        2000.0 / i16::MAX as f32,
        2000.0 / i16::MAX as f32,
    ];
    assert_eq!(output_mono, expected_mono);

    // Case 3: Stereo input to Mono output (mixdown)
    let output_mix = process_chunk_for_device(&chunk, 1, 16000);
    let expected_mix: Vec<f32> = vec![
        f32::midpoint(1000.0, 2000.0) / i16::MAX as f32,
        f32::midpoint(3000.0, 4000.0) / i16::MAX as f32,
    ];
    assert_eq!(output_mix, expected_mix);
}

#[test]
fn test_playback_lock_free_realtime_safety() {
    let (mut prod, mut cons) = rtrb::RingBuffer::new(10);
    assert!(prod.push(1.0).is_ok());
    assert!(prod.push(2.0).is_ok());
    assert!(prod.push(3.0).is_ok());
    assert_eq!(cons.pop().unwrap(), 1.0);
    assert_eq!(cons.pop().unwrap(), 2.0);
    assert_eq!(cons.pop().unwrap(), 3.0);
}
