use super::*;
use hound::WavReader;

#[test]
fn round_trip_wav() {
    let frame = AudioFrame {
        samples: vec![0, 1000, -1000, i16::MAX, i16::MIN],
        sample_rate: 22050,
        channels: 1,
    };

    let dir = std::env::temp_dir().join("voice_pipes_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test_round_trip.wav");

    write_wav(&frame, &path).unwrap();

    // Read back and verify
    let mut reader = WavReader::open(&path).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 22050);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, 16);

    let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
    assert_eq!(samples, frame.samples);

    // Cleanup
    let _ = std::fs::remove_file(&path);
}
