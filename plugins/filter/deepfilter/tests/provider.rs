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
    assert!(rms < 500.0, "noise must be attenuated; rms was {rms}");
    assert!(metrics.snr_improvement_db.unwrap_or(0.0) > 10.0);
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
    assert!(peak > 10_000, "tone must be preserved; peak was {peak}");
    assert!(metrics.compute_duration > Duration::ZERO);
}

#[test]
fn filter_does_not_self_suppress_continuous_speech_harmonic() {
    let mut filter = test_filter();

    // Simulate 2 seconds (100 chunks of 20 ms) of continuous harmonic voice signal
    let mut last_peak = 0i16;
    for chunk_idx in 0..100 {
        let mut chunk: Vec<i16> = (0..320)
            .map(|i| {
                let sample_idx = chunk_idx * 320 + i;
                let t = (sample_idx as f32) / 16_000.0;
                // Fundamental 200 Hz + first two formants (600 Hz, 1200 Hz)
                let val = 0.6 * (2.0 * std::f32::consts::PI * 200.0 * t).cos()
                    + 0.3 * (2.0 * std::f32::consts::PI * 600.0 * t).cos()
                    + 0.1 * (2.0 * std::f32::consts::PI * 1200.0 * t).cos();
                (val * 14_000.0) as i16
            })
            .collect();
        filter.process_chunk(&mut chunk, 16_000).expect("filter process");
        last_peak = chunk.iter().map(|s| s.abs()).max().unwrap_or(0);
    }

    assert!(
        last_peak > 10_000,
        "continuous harmonic voice over 2s must not self-suppress; peak was {last_peak}"
    );
}

#[test]
fn filter_handles_non_multiple_chunk_sizes_without_sample_loss() {
    let mut filter = test_filter();

    // Feed 5 chunks of 200 samples (total 1000 samples = 6 hops of 160 + 40 remainder)
    for chunk_idx in 0..5 {
        let mut chunk: Vec<i16> = (0..200)
            .map(|i| {
                let idx = chunk_idx * 200 + i;
                let angle = (idx as f32) * 2.0 * std::f32::consts::PI * 500.0 / 16_000.0;
                (angle.sin() * 12_000.0) as i16
            })
            .collect();
        filter
            .process_chunk(&mut chunk, 16_000)
            .expect("arbitrary chunk size should succeed");
        if chunk_idx >= 1 {
            let peak = chunk.iter().map(|s| s.abs()).max().unwrap_or(0);
            assert!(peak > 8_000, "chunk {chunk_idx} should carry filtered signal, got {peak}");
        }
    }
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
