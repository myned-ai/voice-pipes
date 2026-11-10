use super::*;

struct MockFilter {
    samples_seen: usize,
    should_fail: bool,
}

impl AudioFilter for MockFilter {
    fn process_chunk(
        &mut self,
        samples: &mut [i16],
        sample_rate: u32,
    ) -> Result<FilterMetrics, FilterError> {
        if self.should_fail {
            return Err(FilterError::InferenceFailed("simulated inference error".into()));
        }
        if sample_rate != 16_000 {
            return Err(FilterError::UnsupportedSampleRate {
                expected: 16_000,
                actual: sample_rate,
            });
        }
        self.samples_seen += samples.len();
        // Attenuate samples slightly as a mock filter
        for s in samples.iter_mut() {
            *s /= 2;
        }
        Ok(FilterMetrics::new(Duration::from_micros(150))
            .with_snr_improvement(12.5)
            .with_suppressed(false))
    }

    fn reset(&mut self) {
        self.samples_seen = 0;
    }

    fn name(&self) -> &'static str {
        "mock-filter"
    }
}

struct MockFilterFactory;

impl AudioFilterFactory for MockFilterFactory {
    fn name(&self) -> &'static str {
        "mock-filter"
    }

    fn validate(&self, cfg: &FilterStageConfig) -> Vec<ConfigError> {
        cfg.options().into_errors()
    }

    fn build(
        &self,
        _cfg: &FilterStageConfig,
        _ctx: &crate::build::BuildContext<'_>,
    ) -> Result<Box<dyn AudioFilter>, crate::BuildError> {
        Ok(Box::new(MockFilter {
            samples_seen: 0,
            should_fail: false,
        }))
    }
}

#[test]
fn filter_metrics_builder_and_fields() {
    let m = FilterMetrics::new(Duration::from_millis(5))
        .with_snr_improvement(14.2)
        .with_suppressed(true);
    assert_eq!(m.compute_duration, Duration::from_millis(5));
    assert_eq!(m.snr_improvement_db, Some(14.2));
    assert!(m.is_suppressed);
}

#[test]
fn filter_process_chunk_in_place_and_reset() {
    let mut filter = MockFilter {
        samples_seen: 0,
        should_fail: false,
    };
    let mut chunk = vec![100, -200, 300, -400];
    let metrics = filter.process_chunk(&mut chunk, 16_000).expect("filter succeeds");
    assert_eq!(chunk, vec![50, -100, 150, -200]);
    assert_eq!(filter.samples_seen, 4);
    assert_eq!(metrics.snr_improvement_db, Some(12.5));

    filter.reset();
    assert_eq!(filter.samples_seen, 0);
}

#[test]
fn filter_reports_expected_errors() {
    let mut filter = MockFilter {
        samples_seen: 0,
        should_fail: true,
    };
    let mut chunk = vec![100, 200];
    let err = filter.process_chunk(&mut chunk, 16_000).expect_err("should fail");
    assert!(matches!(err, FilterError::InferenceFailed(_)));
    assert!(err.to_string().contains("simulated inference error"));

    filter.should_fail = false;
    let rate_err = filter.process_chunk(&mut chunk, 48_000).expect_err("bad rate");
    assert!(matches!(rate_err, FilterError::UnsupportedSampleRate { expected: 16_000, actual: 48_000 }));
}

#[test]
fn filter_factory_matching() {
    let factory = MockFilterFactory;
    assert!(factory.matches("mock-filter"));
    assert!(!factory.matches("other-filter"));
}
