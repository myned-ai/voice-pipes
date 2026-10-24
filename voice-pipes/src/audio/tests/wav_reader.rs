use super::super::wav_writer::write_wav;
use super::*;

#[test]
fn round_trip_read_write() {
    let original = AudioFrame {
        samples: vec![0, 1000, -1000, 5000, -5000],
        sample_rate: 16000,
        channels: 1,
    };

    let dir = std::env::temp_dir().join("vp_wav_reader_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("roundtrip.wav");

    write_wav(&original, &path).unwrap();
    let loaded = read_wav(&path).unwrap();

    assert_eq!(loaded.samples, original.samples);
    assert_eq!(loaded.sample_rate, 16000);
    assert_eq!(loaded.channels, 1);

    let _ = std::fs::remove_dir_all(&dir);
}
