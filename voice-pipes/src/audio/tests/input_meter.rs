use super::*;

const RATE: u32 = 16_000;
/// 20 ms at 16 kHz.
const FRAME: usize = 320;

fn feed_n(m: &mut InputAudioMeter, value: i16, frames: usize, is_speech: bool) {
    let chunk = vec![value; FRAME];
    for _ in 0..frames {
        m.feed(&chunk, RATE, is_speech);
    }
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn constant_tone_levels_and_duration() {
    let mut m = InputAudioMeter::new("test");
    feed_n(&mut m, 3000, 20, true);
    let s = m.take().expect("speech fed");
    // 20·log10(3000 / 32768) = −20.77
    assert!(close(s.speech_dbfs(), -20.77, 0.01), "{}", s.speech_dbfs());
    assert!(close(s.peak_dbfs(), -20.77, 0.01));
    assert_eq!(s.speech_ms(), 400);
    assert!(s.noise_dbfs().is_none() && s.snr_db().is_none());
    assert!(close(s.clipped_ratio(), 0.0, 0.0));
    assert!(close(s.speech_ratio(), 1.0, 1e-9));
}

#[test]
fn full_scale_counts_as_clipped() {
    let mut m = InputAudioMeter::new("test");
    let square: Vec<i16> = (0..FRAME)
        .map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN })
        .collect();
    m.feed(&square, RATE, true);
    let s = m.take().unwrap();
    assert!(close(s.peak_dbfs(), 0.0, 1e-9));
    assert!(close(s.speech_dbfs(), 0.0, 0.01));
    assert!(close(s.clipped_ratio(), 1.0, 1e-9));
}

#[test]
fn snr_from_speech_over_noise() {
    let mut m = InputAudioMeter::new("test");
    feed_n(&mut m, 30, 10, false);
    feed_n(&mut m, 3000, 20, true);
    feed_n(&mut m, 30, 15, false);
    let s = m.take().unwrap();
    assert!(close(s.noise_dbfs().unwrap(), -60.77, 0.01));
    assert!(close(s.snr_db().unwrap(), 40.0, 0.01));
    // 20 speech frames of the 35 since the first speech frame.
    assert!(close(s.speech_ratio(), 20.0 / 35.0, 1e-9));
}

#[test]
fn noise_window_rolls_and_survives_take() {
    let mut m = InputAudioMeter::new("test");
    // 60 quiet frames then 50 louder ones (1 s): only the louder remain.
    feed_n(&mut m, 10, 60, false);
    feed_n(&mut m, 100, 50, false);
    feed_n(&mut m, 3000, 5, true);
    let s = m.take().unwrap();
    assert!(close(
        s.noise_dbfs().unwrap(),
        to_dbfs(100.0 / FULL_SCALE),
        0.01
    ));
    // Next segment: no new noise, the window is still there.
    feed_n(&mut m, 3000, 5, true);
    let s2 = m.take().unwrap();
    assert!(s2.noise_dbfs().is_some());
}

#[test]
fn digital_silence_is_the_floor_not_infinity() {
    let mut m = InputAudioMeter::new("test");
    feed_n(&mut m, 0, 10, false);
    feed_n(&mut m, 3000, 10, true);
    let s = m.take().unwrap();
    assert!(close(s.noise_dbfs().unwrap(), DBFS_FLOOR, 0.0));
    assert!(s.snr_db().unwrap().is_finite());
}

#[test]
fn take_resets_segment_and_discard_drops_it() {
    let mut m = InputAudioMeter::new("test");
    assert!(m.take().is_none(), "nothing fed");
    feed_n(&mut m, 3000, 5, true);
    assert!(m.take().is_some());
    assert!(m.take().is_none(), "segment reset by take");
    feed_n(&mut m, 3000, 5, true);
    m.discard();
    assert!(m.take().is_none(), "discarded");
    feed_n(&mut m, 0, 5, true);
    assert!(m.take().is_some(), "speech verdict decides, not loudness");
}

#[test]
fn merge_earlier_pools_exactly() {
    let mut m = InputAudioMeter::new("test");
    feed_n(&mut m, 1000, 10, true);
    let early = m.take().unwrap();
    feed_n(&mut m, 30, 10, false);
    feed_n(&mut m, 4000, 10, true);
    let mut late = m.take().unwrap();
    late.merge_earlier(&early);
    // Pooled RMS of equal-length 1000 and 4000 segments.
    let expected = to_dbfs(f64::midpoint(1000f64.powi(2), 4000f64.powi(2)).sqrt() / FULL_SCALE);
    assert!(close(late.speech_dbfs(), expected, 0.01));
    assert!(close(late.peak_dbfs(), to_dbfs(4000.0 / FULL_SCALE), 0.01));
    assert_eq!(late.speech_ms(), 400);
    assert!(late.noise_dbfs().is_some(), "newer segment's floor kept");
}

#[test]
fn empty_and_zero_rate_are_noops() {
    let mut m = InputAudioMeter::new("test");
    m.feed(&[], RATE, true);
    m.feed(&[3000; FRAME], 0, true);
    assert!(m.take().is_none());
}

#[test]
fn write_extras_emits_every_key_when_noise_known() {
    let mut m = InputAudioMeter::new("vp_energy");
    feed_n(&mut m, 30, 5, false);
    feed_n(&mut m, 3000, 5, true);
    let mut map = Map::new();
    m.take().unwrap().write_extras(&mut map);
    for key in EXTRAS_KEYS {
        assert!(map.contains_key(key), "{key}");
    }
    assert_eq!(map["input_level_source"], "vp_energy");
    assert_eq!(map["input_speech_dbfs"], -20.8);
    assert_eq!(map["input_snr_db"], 40.0);

    let mut m = InputAudioMeter::new("vp_energy");
    feed_n(&mut m, 3000, 5, true);
    let mut map = Map::new();
    m.take().unwrap().write_extras(&mut map);
    assert!(!map.contains_key("input_noise_dbfs"));
    assert!(!map.contains_key("input_snr_db"));
}

/// The meter rides the audio path's own pass; it must stay far below
/// the 20 ms a chunk represents even in an unoptimised test build.
#[test]
fn feed_is_cheap() {
    let mut m = InputAudioMeter::new("test");
    let speech = vec![3000i16; FRAME];
    let quiet = vec![30i16; FRAME];
    let started = std::time::Instant::now();
    for i in 0..1_000 {
        m.feed(if i % 2 == 0 { &speech } else { &quiet }, RATE, i % 2 == 0);
    }
    let per_chunk = started.elapsed() / 1_000;
    assert!(
        per_chunk < std::time::Duration::from_micros(200),
        "{per_chunk:?} per 20 ms chunk"
    );
}
