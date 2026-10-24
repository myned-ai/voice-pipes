//! Voice Activity Detection (VAD) segmenter backends.
//!
//! This module provides the low-level VAD layer used to classify incoming
//! microphone PCM chunks as speech or silence before STT processing.
//! The result is stable utterance segmentation (speech onset, speech end,
//! and end-of-utterance sentinels) so downstream transcription does not
//! receive unbounded noise/silence streams.
//!
//! ## What lives here
//! - [`VadBackend`]: trait for per-chunk speech classification.
//! - Built-in backends: [`EnergyVad`] and [`PassthroughVad`]. Model-based
//!   backends (Silero among them) are plugins on the SDK.
//! - [`VadFactory`] + [`vad_factories()`]: registry used by config/runtime code
//!   to instantiate a backend from pipeline settings.
//!
//! ## How it is used in the pipeline
//! - `config::build_vad(...)` resolves settings and creates a backend from this
//!   module's factories.
//! - `pipeline::vad_filter_task(...)` feeds audio chunks into
//!   [`VadBackend::process_chunk`] and emits utterance-bounded audio for STT.
//! - `audio::io::mic_stream(...)` can also use the same backend trait in local
//!   capture flows.
//!
//! ## Error and observability model
//! - Backend operations return [`VadError`] for VAD-specific failures.
//! - Backends can expose [`crate::component::Component`] via `as_component()` so
//!   `/debug/components` and the component registry can report health/telemetry.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::audio::compute_rms;
use crate::component::{Component, ComponentCore, ComponentKind, ComponentTelemetry, HealthStatus};
use crate::vad::error::VadError;

// ── Types ────────────────────────────────────────────────────

/// Result from a single VAD analysis. Shared with the SDK, so a plugin
/// backend returns the same type.
pub use voice_pipes_sdk::vad::VadResult;

// ── Traits ────────────────────────────────────────────────────

/// Built-in backends and plugin crates implement the same SDK traits, so a
/// registered factory joins the built-ins as it is.
pub use voice_pipes_sdk::vad::{VadBackend, VadFactory};
pub use voice_pipes_sdk::{BuildContext, VadConfig};

// ── EnergyVad ────────────────────────────────────────────────

/// Simple RMS energy-based VAD.
///
/// `energy_threshold` is held in an [`std::sync::atomic::AtomicU64`] (f64 bits)
/// so live updates from the runtime parameter registry take effect on the
/// next chunk without locks.
pub struct EnergyVad {
    threshold: Arc<AtomicU64>,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for EnergyVad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnergyVad").finish_non_exhaustive()
    }
}

impl EnergyVad {
    /// Create an energy-based VAD with the given RMS threshold.
    pub fn new(threshold: f64) -> Self {
        Self {
            threshold: Arc::new(AtomicU64::new(threshold.to_bits())),
            core: Arc::new(ComponentCore::new("vad:energy", ComponentKind::Vad)),
        }
    }

    /// Replace the internal threshold cell with one shared from a
    /// parameter store. Subsequent stores to that cell take effect on
    /// the next chunk.
    #[must_use]
    pub fn with_threshold_cell(mut self, cell: Arc<AtomicU64>) -> Self {
        self.threshold = cell;
        self
    }
}

impl Default for EnergyVad {
    fn default() -> Self {
        Self::new(200.0)
    }
}

impl VadBackend for EnergyVad {
    fn process_chunk(&mut self, samples: &[i16], _sample_rate: u32) -> Result<VadResult, VadError> {
        let started = Instant::now();
        let rms = compute_rms(samples);
        let threshold = f64::from_bits(self.threshold.load(Ordering::Relaxed));
        let is_speech = rms > threshold;
        tracing::trace!(
            rms = format!("{:.1}", rms),
            threshold = format!("{:.1}", threshold),
            is_speech,
            "energy VAD"
        );
        self.core.record_ok(started.elapsed());
        Ok(VadResult {
            speech_prob: if is_speech { 1.0 } else { 0.0 },
            is_speech,
        })
    }

    fn reset(&mut self) {}

    fn name(&self) -> &'static str {
        "energy"
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(VadBackendComponent {
            core: Arc::clone(&self.core),
        }))
    }
}

// ── PassthroughVad ───────────────────────────────────────────

/// VAD backend that passes all audio as speech.
///
/// Useful for STT models that have their own end-of-turn detection
/// and require continuous audio context.
pub struct PassthroughVad {
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for PassthroughVad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassthroughVad").finish()
    }
}

impl PassthroughVad {
    /// Create a no-op VAD that labels all audio as speech.
    pub fn new() -> Self {
        Self {
            core: Arc::new(ComponentCore::new("vad:passthrough", ComponentKind::Vad)),
        }
    }
}

impl Default for PassthroughVad {
    fn default() -> Self {
        Self::new()
    }
}

impl VadBackend for PassthroughVad {
    fn process_chunk(
        &mut self,
        _samples: &[i16],
        _sample_rate: u32,
    ) -> Result<VadResult, VadError> {
        let started = Instant::now();
        self.core.record_ok(started.elapsed());
        Ok(VadResult {
            speech_prob: 1.0,
            is_speech: true,
        })
    }

    fn reset(&mut self) {}

    fn name(&self) -> &'static str {
        "passthrough"
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(VadBackendComponent {
            core: Arc::clone(&self.core),
        }))
    }
}

// ── Component adapter ────────────────────────────────────────

/// Wraps a `ComponentCore` so a VAD backend can be exposed as a generic
/// [`Component`] via [`VadBackend::as_component`].
struct VadBackendComponent {
    core: Arc<ComponentCore>,
}

impl Component for VadBackendComponent {
    fn id(&self) -> &str {
        self.core.id()
    }
    fn kind(&self) -> ComponentKind {
        self.core.kind()
    }
    fn health(&self) -> HealthStatus {
        self.core.health()
    }
    fn telemetry(&self) -> ComponentTelemetry {
        self.core.telemetry()
    }
}

// ── Factory ──────────────────────────────────────────────────

/// Factory for the simple energy-threshold VAD.
#[derive(Debug)]
pub struct EnergyVadFactory;

impl VadFactory for EnergyVadFactory {
    fn name(&self) -> &'static str {
        "energy"
    }

    fn validate(&self, cfg: &VadConfig) -> Vec<crate::config::ConfigError> {
        // The backend's settings are [vad]'s own fields; `[vad.options]`
        // is for plugin backends.
        cfg.options().into_errors()
    }

    fn build(
        &self,
        cfg: &VadConfig,
        ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn VadBackend>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let mut vad = EnergyVad::new(cfg.energy_threshold);
        if let Some(store) = ctx.params
            && let Some(cell) = store.float_cell("energy_threshold")
        {
            vad = vad.with_threshold_cell(cell);
        }
        Ok(Box::new(vad))
    }
}

/// Factory for the passthrough (no-op) VAD.
#[derive(Debug)]
pub struct PassthroughVadFactory;

impl VadFactory for PassthroughVadFactory {
    fn name(&self) -> &'static str {
        "passthrough"
    }

    fn validate(&self, cfg: &VadConfig) -> Vec<crate::config::ConfigError> {
        // The backend's settings are [vad]'s own fields; `[vad.options]`
        // is for plugin backends.
        cfg.options().into_errors()
    }

    fn build(
        &self,
        cfg: &VadConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn VadBackend>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        Ok(Box::new(PassthroughVad::new()))
    }
}

// ── Plugin registry ──────────────────────────────────────────

/// The factories plugin crates registered, each constructed once.
pub(crate) static VAD_FACTORIES: crate::plugins::Registry<dyn VadFactory> =
    crate::plugins::Registry::new("VAD", builtin_vad_factories);

/// Register a plugin VAD factory. Call before building any pipeline; the
/// backend is then selected by `[vad] mode = "<its name>"`.
///
/// ```ignore
/// voice_pipes::vad::register_vad_factory(my_vad_plugin::factory)?;
/// ```
///
/// # Errors
/// When the backend's name or one of its aliases already names a built-in
/// or an earlier plugin: one backend would shadow the other.
pub fn register_vad_factory(
    ctor: fn() -> Box<dyn VadFactory>,
) -> Result<(), crate::plugins::RegistrationError> {
    VAD_FACTORIES.register(ctor)
}

/// The VAD factory `[vad] mode` names, by name or alias.
pub fn lookup_vad_factory(mode: &str) -> Option<std::sync::Arc<dyn VadFactory>> {
    VAD_FACTORIES.lookup(mode)
}

/// The VAD factories compiled into this build.
fn builtin_vad_factories() -> Vec<Box<dyn VadFactory>> {
    vec![Box::new(EnergyVadFactory), Box::new(PassthroughVadFactory)]
}

/// Collect all VAD factories: the compiled-in ones, then any registered by
/// plugins.
pub fn vad_factories() -> Vec<std::sync::Arc<dyn VadFactory>> {
    VAD_FACTORIES.all()
}

static AUTO_BACKEND: std::sync::OnceLock<parking_lot::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

/// Name the backend `[vad] mode = "auto"` runs when it can
/// ([`VadFactory::can_run`]); otherwise auto runs energy. The binary that
/// links the backend decides this, as it decides what to link.
///
/// # Errors
/// When no VAD factory answers to `name`
/// ([`RegistrationError::NoSuchProvider`](crate::plugins::RegistrationError::NoSuchProvider)),
/// or another backend is already auto's
/// ([`RegistrationError::FallbackTaken`](crate::plugins::RegistrationError::FallbackTaken)).
/// Naming the same one again is not an error.
pub fn register_auto_backend(name: &str) -> Result<(), crate::plugins::RegistrationError> {
    use crate::plugins::RegistrationError;
    if lookup_vad_factory(name).is_none() {
        return Err(RegistrationError::NoSuchProvider {
            stage: "VAD",
            provider: name.to_string(),
        });
    }
    let mut slot = AUTO_BACKEND.get_or_init(Default::default).lock();
    match slot.as_deref() {
        Some(existing) if existing == name => Ok(()),
        Some(existing) => Err(RegistrationError::FallbackTaken {
            stage: "VAD",
            provider: name.to_string(),
            existing: existing.to_string(),
        }),
        None => {
            *slot = Some(name.to_string());
            Ok(())
        }
    }
}

/// The backend `auto` tries first: the one [`register_auto_backend`] named.
pub fn auto_backend() -> Option<String> {
    AUTO_BACKEND.get().and_then(|slot| slot.lock().clone())
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/segmenter.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/stand_in.rs"]
pub(crate) mod stand_in;
