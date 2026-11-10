use std::time::Duration;
use voice_pipes_filter_deepfilter::factory;
use voice_pipes_sdk::filter::AudioFilter;
use voice_pipes_sdk::testing::assert_filter_metrics_conformance;
use voice_pipes_sdk::{BuildContext, FilterStageConfig};

fn test_filter() -> Box<dyn AudioFilter> {
    factory()
        .build(
            &FilterStageConfig::new("deepfilter"),
            &BuildContext::default(),
        )
        .expect("filter creation")
}

#[test]
fn provider_metrics_conformance() {
    let mut filter = test_filter();
    let mut chunk = vec![500i16; 320]; // 20 ms at 16 kHz
    let metrics = filter.process_chunk(&mut chunk, 16_000).expect("chunk process");
    assert_filter_metrics_conformance(metrics.compute_duration, Duration::ZERO)
        .expect("conformance passes");
}

#[test]
fn filter_attenuates_stationary_noise() {
    let mut filter = test_filter();

    // Feed initial noise chunks to let the noise floor tracker adapt
    for _ in 0..10 {
        let mut noise = vec![2000i16; 320];
        let _ = filter.process_chunk(&mut noise, 16_000);
    }

    // Now measure attenuation on subsequent noise chunk
    let mut noise_chunk = vec![2000i16; 320];
    let metrics = filter.process_chunk(&mut noise_chunk, 16_000).expect("filter process");

    // Energy should be significantly reduced compared to input 2000
    let rms: f64 = (noise_chunk.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>()
        / (noise_chunk.len() as f64))
        .sqrt();
    assert!(rms < 1500.0, "noise must be attenuated; rms was {rms}");
    assert!(metrics.snr_improvement_db.unwrap_or(0.0) > 0.0);
}

#[test]
fn filter_preserves_clean_tone() {
    let mut filter = test_filter();

    // Generate a 500 Hz tone at 16 kHz (32 samples per period)
    let mut tone_chunk: Vec<i16> = (0..320)
        .map(|i| {
            let angle = (i as f32) * 2.0 * std::f32::consts::PI * 500.0 / 16_000.0;
            (angle.sin() * 15_000.0) as i16
        })
        .collect();

    let metrics = filter.process_chunk(&mut tone_chunk, 16_000).expect("filter process");
    let peak = tone_chunk.iter().map(|s| s.abs()).max().unwrap_or(0);
    assert!(peak > 5000, "tone must be preserved; peak was {peak}");
    assert!(metrics.compute_duration > Duration::ZERO);
}

#[test]
fn filter_rejects_unsupported_sample_rate() {
    let mut filter = test_filter();
    let mut chunk = vec![100i16; 320];
    let err = filter.process_chunk(&mut chunk, 8_000).expect_err("should reject 8k");
    assert!(err.to_string().contains("expects 16000 Hz, received 8000 Hz"));
}

#[test]
fn filter_factory_validation() {
    let f = factory();
    assert_eq!(f.name(), "deepfilter");
    assert!(f.matches("deepfilternet3"));

    let valid_cfg = FilterStageConfig::new("deepfilter");
    assert!(f.validate(&valid_cfg).is_empty());

    let mut invalid_cfg = FilterStageConfig::new("deepfilter");
    invalid_cfg
        .options
        .insert("attenuation_limit_db".into(), serde_json::json!(150.0));
    let errors = f.validate(&invalid_cfg);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].field.contains("attenuation_limit_db"));
}
