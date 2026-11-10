//! Ingress audio filtering chain -- runs on microphone PCM before VAD and STT.

use std::sync::Arc;
use std::time::Instant;
use voice_pipes_sdk::filter::{AudioFilter, AudioFilterFactory, FilterMetrics};
use voice_pipes_sdk::FilterStageConfig;
use crate::config::PipelineConfig;

/// A sequence of audio filters executed in order on each audio chunk.
pub struct FilterChain {
    filters: Vec<Box<dyn AudioFilter>>,
}

impl std::fmt::Debug for FilterChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.filters.iter().map(|f| f.name()).collect();
        f.debug_struct("FilterChain").field("filters", &names).finish()
    }
}

impl FilterChain {
    /// Create a new filter chain.
    pub fn new(filters: Vec<Box<dyn AudioFilter>>) -> Self {
        Self { filters }
    }

    /// Whether the chain contains no filters.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Number of filters in the chain.
    pub fn len(&self) -> usize {
        self.filters.len()
    }

    /// Process audio chunk samples in place through all filters in the chain.
    pub fn process_chunk(&mut self, samples: &mut [i16], sample_rate: u32) -> FilterMetrics {
        let mut total_duration = std::time::Duration::ZERO;
        let mut max_snr: Option<f32> = None;
        let mut any_suppressed = false;

        for filter in &mut self.filters {
            let t0 = Instant::now();
            match filter.process_chunk(samples, sample_rate) {
                Ok(m) => {
                    total_duration += m.compute_duration;
                    if let Some(snr) = m.snr_improvement_db {
                        max_snr = Some(max_snr.map_or(snr, |prev| prev.max(snr)));
                    }
                    if m.is_suppressed {
                        any_suppressed = true;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        filter = filter.name(),
                        error = %e,
                        "audio filter failed; chunk continues unmodified"
                    );
                    total_duration += t0.elapsed();
                }
            }
        }

        let mut metrics = FilterMetrics::new(total_duration).with_suppressed(any_suppressed);
        if let Some(snr) = max_snr {
            metrics = metrics.with_snr_improvement(snr);
        }
        metrics
    }

    /// Reset internal state of all filters in the chain.
    pub fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
    }
}

// ── Plugin registry ──────────────────────────────────────────

fn builtin_filter_factories() -> Vec<Box<dyn AudioFilterFactory>> {
    Vec::new()
}

pub(crate) static FILTER_FACTORIES: crate::plugins::Registry<dyn AudioFilterFactory> =
    crate::plugins::Registry::new("Filter", builtin_filter_factories);

/// Register a plugin audio filter factory.
pub fn register_filter_factory(
    ctor: fn() -> Box<dyn AudioFilterFactory>,
) -> Result<(), crate::plugins::RegistrationError> {
    FILTER_FACTORIES.register(ctor)
}

/// The filter factory answering to `name`.
pub fn lookup_filter_factory(name: &str) -> Option<Arc<dyn AudioFilterFactory>> {
    FILTER_FACTORIES.lookup(name)
}

/// Collect all filter factories.
pub fn filter_factories() -> Vec<Arc<dyn AudioFilterFactory>> {
    FILTER_FACTORIES.all()
}

/// Build an audio filter chain from pipeline configuration.
pub fn build_filter_chain(
    cfg: &PipelineConfig,
    param_store: Option<&crate::api::param_store::AtomicParamStore>,
) -> Result<Option<FilterChain>, anyhow::Error> {
    let mut built_filters = Vec::new();

    for stage_cfg in &cfg.audio_filter {
        let factory = lookup_filter_factory(&stage_cfg.provider)
            .ok_or_else(|| anyhow::anyhow!("unknown audio filter provider: \"{}\"", stage_cfg.provider))?;
        let backend_cfg = FilterStageConfig::new(&stage_cfg.provider)
            .with_model(stage_cfg.model.clone())
            .with_options(stage_cfg.options.clone());
        let filter = factory.build(
            &backend_cfg,
            &voice_pipes_sdk::BuildContext::new().with_params(param_store),
        )?;
        built_filters.push(filter);
    }

    if built_filters.is_empty() {
        Ok(None)
    } else {
        Ok(Some(FilterChain::new(built_filters)))
    }
}
