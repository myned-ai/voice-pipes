//! Silero voice activity detection -- local ONNX inference.
//!
//! A voice-pipes VAD plugin: the binary registers [`factory`] and
//! [`wizard_choices`]; `[vad] mode = "silero"` selects it. The voice-pipes
//! binary also names it the backend `mode = "auto"` runs when its model is
//! on disk.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

mod wizard;

pub use wizard::wizard_choices;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ort::session::Session;
use ort::value::Tensor;
use tracing::debug;

use voice_pipes_sdk::audio::i16_to_f32;
use voice_pipes_sdk::audio::resample::StreamingResampler;
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::vad::{VadBackend, VadError, VadFactory, VadResult};
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, VadConfig};

const SILERO_WINDOW: usize = 512;

const SILERO_CONTEXT: usize = 64;

const SILERO_SR: u32 = 16000;

/// Flattened Silero state tensor: shape `[2, 1, 128]` (layers × batch × dim).
const STATE_SIZE: usize = 2 * 128;

/// The end-of-speech probability when `[vad]` sets none.
const DEFAULT_NEG_THRESHOLD: f32 = 0.35;

/// Two-threshold hysteresis decision: entering speech requires
/// `prob >= threshold`; once speaking, stay speaking until `prob < neg_threshold`.
/// This suppresses the flapping a single threshold produces around borderline
/// probabilities. With `neg_threshold >= threshold` it degenerates to a
/// single threshold.
fn hysteresis_is_speech(speaking: bool, prob: f32, threshold: f32, neg_threshold: f32) -> bool {
    if speaking {
        prob >= neg_threshold
    } else {
        prob >= threshold
    }
}

/// A threshold cell's value. The cells hold `f64` bits, shared with the
/// live parameter store.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a probability in [0, 1], which f32 holds"
)]
fn load_threshold(cell: &AtomicU64) -> f32 {
    f64::from_bits(cell.load(Ordering::Relaxed)) as f32
}

/// One step of the Silero network: the input (context then window) and the
/// recurrent state in, the speech probability and the next state out.
trait SileroModel: Send {
    fn step(&mut self, input: Vec<f32>, state: Vec<f32>) -> Result<(f32, Vec<f32>), VadError>;
}

/// The Silero network as an ONNX Runtime session.
struct OrtSilero(Session);

impl SileroModel for OrtSilero {
    fn step(&mut self, input: Vec<f32>, state: Vec<f32>) -> Result<(f32, Vec<f32>), VadError> {
        let input_tensor = Tensor::from_array(([1usize, input.len()], input))
            .map_err(|e| VadError::InferenceFailed(format!("vad input tensor: {e}")))?;

        let state_tensor = Tensor::from_array(([2usize, 1usize, 128usize], state))
            .map_err(|e| VadError::InferenceFailed(format!("vad state tensor: {e}")))?;

        let sr_tensor = Tensor::from_array(([] as [usize; 0], vec![i64::from(SILERO_SR)]))
            .map_err(|e| VadError::InferenceFailed(format!("vad sr tensor: {e}")))?;

        let outputs = self
            .0
            .run(ort::inputs![input_tensor, state_tensor, sr_tensor])
            .map_err(|e| VadError::InferenceFailed(format!("vad inference: {e}")))?;
        let mut values = outputs.values();
        let (Some(prob_value), Some(state_value)) = (values.next(), values.next()) else {
            return Err(VadError::InferenceFailed(
                "vad returned fewer than two outputs".into(),
            ));
        };

        let prob_tensor = prob_value
            .try_extract_tensor::<f32>()
            .map_err(|e| VadError::InferenceFailed(format!("vad output extract: {e}")))?;
        let prob =
            prob_tensor.1.iter().copied().next().ok_or_else(|| {
                VadError::InferenceFailed("vad returned an empty probability".into())
            })?;

        let state_tensor = state_value
            .try_extract_tensor::<f32>()
            .map_err(|e| VadError::InferenceFailed(format!("vad state extract: {e}")))?;
        Ok((prob, state_tensor.1.to_vec()))
    }
}

/// Silero VAD ONNX wrapper.
struct SileroVad {
    model: Box<dyn SileroModel>,
    state: Vec<f32>,
    buffer_16k: Vec<f32>,
    /// Stateful resampler for non-16 kHz input. A stateless per-chunk
    /// resample gave every 20 ms piece a truncated kernel and no history,
    /// a small discontinuity at every chunk boundary that the model saw as
    /// jitter near its thresholds. Rebuilt when the input rate changes.
    resampler: Option<StreamingResampler>,
    context: Vec<f32>,
    last_prob: f32,
    /// Hysteresis state: whether we are currently inside a speech region.
    /// Carried across chunks so [`hysteresis_is_speech`] can apply the low
    /// exit threshold (`neg_threshold`) only while speaking.
    speaking: bool,
    speech_prob: Arc<AtomicU64>,
    neg_threshold: Arc<AtomicU64>,
    core: Arc<ComponentCore>,
}

impl std::fmt::Debug for SileroVad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SileroVad")
            .field("last_prob", &self.last_prob)
            .finish_non_exhaustive()
    }
}

impl SileroVad {
    /// Load the Silero VAD ONNX model: one thread per operator, on the CPU
    /// (a window is 32 ms of audio; a GPU round trip costs more than it).
    fn new(model_path: &Path, threshold: f32, neg_threshold: f32) -> Result<Self, VadError> {
        let core = Arc::new(ComponentCore::new("vad:silero", ComponentKind::Vad));
        let options = voice_pipes_onnx::SessionOptions::new(1);
        let session = match voice_pipes_onnx::open_session(model_path, &options) {
            Ok(s) => s,
            Err(e) => {
                let e = VadError::ModelLoadFailed(format!("silero vad: {e}"));
                core.mark_unhealthy(format!("model load failed: {e}"));
                return Err(e);
            }
        };

        debug!("silero vad loaded from {}", model_path.display());

        Ok(Self::with_model(
            Box::new(OrtSilero(session)),
            core,
            threshold,
            neg_threshold,
        ))
    }

    /// A detector over `model`, reporting on `core`.
    fn with_model(
        model: Box<dyn SileroModel>,
        core: Arc<ComponentCore>,
        threshold: f32,
        neg_threshold: f32,
    ) -> Self {
        Self {
            model,
            state: vec![0.0; STATE_SIZE],
            buffer_16k: Vec::with_capacity(SILERO_WINDOW * 2),
            resampler: None,
            context: vec![0.0; SILERO_CONTEXT],
            last_prob: 0.0,
            speaking: false,
            speech_prob: Arc::new(AtomicU64::new(f64::from(threshold).to_bits())),
            neg_threshold: Arc::new(AtomicU64::new(f64::from(neg_threshold).to_bits())),
            core,
        }
    }

    /// Replace the speech-probability threshold cell with one shared from a
    /// parameter store, enabling live tuning.
    #[must_use]
    fn with_speech_prob_cell(mut self, cell: Arc<AtomicU64>) -> Self {
        self.speech_prob = cell;
        self
    }

    /// Replace the hysteresis exit-threshold cell with one shared from a
    /// parameter store, enabling live tuning.
    #[must_use]
    fn with_neg_threshold_cell(mut self, cell: Arc<AtomicU64>) -> Self {
        self.neg_threshold = cell;
        self
    }

    /// Run one inference window (exactly `SILERO_WINDOW` f32 samples at 16 kHz).
    fn infer_window(&mut self, window: &[f32]) -> Result<f32, VadError> {
        debug_assert_eq!(window.len(), SILERO_WINDOW);
        let tail = window
            .get(SILERO_WINDOW - SILERO_CONTEXT..)
            .ok_or_else(|| VadError::InferenceFailed("vad window too short".into()))?;

        let input_len = SILERO_CONTEXT + SILERO_WINDOW;
        let mut input_data = Vec::with_capacity(input_len);
        input_data.extend_from_slice(&self.context);
        input_data.extend_from_slice(window);

        let (prob, state) = self.model.step(input_data, self.state.clone())?;
        if state.len() != STATE_SIZE {
            return Err(VadError::InferenceFailed(format!(
                "vad state output has {} values, expected {STATE_SIZE}",
                state.len()
            )));
        }
        self.state = state;

        self.context.clear();
        self.context.extend_from_slice(tail);

        Ok(prob)
    }

    /// `samples` as f32 at 16 kHz, through the resampler kept for
    /// `sample_rate` when it is another rate.
    fn samples_at_16k(&mut self, samples: &[i16], sample_rate: u32) -> Result<Vec<f32>, VadError> {
        if sample_rate == SILERO_SR {
            return Ok(i16_to_f32(samples));
        }
        let kept = self
            .resampler
            .take()
            .filter(|r| r.from_rate() == sample_rate);
        let resampler = match kept {
            Some(r) => self.resampler.insert(r),
            None => self.resampler.insert(
                StreamingResampler::new(sample_rate, SILERO_SR)
                    .map_err(|e| VadError::InferenceFailed(format!("vad input: {e}")))?,
            ),
        };
        Ok(i16_to_f32(&resampler.process_chunk(samples)))
    }
}

impl VadBackend for SileroVad {
    fn process_chunk(&mut self, samples: &[i16], sample_rate: u32) -> Result<VadResult, VadError> {
        let started = Instant::now();

        let samples_f32 = self.samples_at_16k(samples, sample_rate)?;
        self.buffer_16k.extend_from_slice(&samples_f32);

        let mut prob = self.last_prob;
        while self.buffer_16k.len() >= SILERO_WINDOW {
            let window: Vec<f32> = self.buffer_16k.drain(..SILERO_WINDOW).collect();
            // A failed window is the chunk's error: reporting it as
            // silence would let a broken model pass for a quiet room.
            match self.infer_window(&window) {
                Ok(p) => prob = p,
                Err(e) => {
                    self.core.record_error(&e);
                    self.core.mark_degraded("inference error");
                    return Err(e);
                }
            }
        }
        self.last_prob = prob;

        let threshold = load_threshold(&self.speech_prob);
        let neg_threshold = load_threshold(&self.neg_threshold);
        let is_speech = hysteresis_is_speech(self.speaking, prob, threshold, neg_threshold);
        self.speaking = is_speech;

        self.core.record_ok(started.elapsed());
        Ok(VadResult {
            speech_prob: prob,
            is_speech,
        })
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.buffer_16k.clear();
        // `flush` also resets the resampler's history and offsets.
        if let Some(r) = self.resampler.as_mut() {
            let _ = r.flush();
        }
        self.context.fill(0.0);
        self.last_prob = 0.0;
        self.speaking = false;
    }

    fn name(&self) -> &'static str {
        "silero"
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}

// ── Factory ──────────────────────────────────────────────────

/// The factory the binary registers for `[vad] mode = "silero"`.
pub fn factory() -> Box<dyn VadFactory> {
    Box::new(SileroVadFactory)
}

/// Factory for the Silero ONNX VAD.
#[derive(Debug)]
struct SileroVadFactory;

impl VadFactory for SileroVadFactory {
    fn name(&self) -> &'static str {
        "silero"
    }

    fn validate(&self, cfg: &VadConfig) -> Vec<ConfigError> {
        let mut errs = vec![];
        if cfg.model.is_none() {
            errs.push(ConfigError::new(
                "vad.model",
                "silero mode requires a model path",
            ));
        }
        errs.extend(cfg.options().into_errors());
        errs
    }

    /// Only with its model on disk.
    fn can_run(&self, cfg: &VadConfig) -> bool {
        cfg.model.as_deref().is_some_and(Path::exists) && self.validate(cfg).is_empty()
    }

    fn build(
        &self,
        cfg: &VadConfig,
        ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn VadBackend>, BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let model_path = cfg
            .model
            .as_ref()
            .ok_or_else(|| BuildError::failed("vad.model required for silero"))?;
        let mut vad = SileroVad::new(
            model_path,
            cfg.speech_prob,
            cfg.neg_threshold.unwrap_or(DEFAULT_NEG_THRESHOLD),
        )?;
        if let Some(store) = ctx.params {
            if let Some(cell) = store.float_cell("speech_prob") {
                vad = vad.with_speech_prob_cell(cell);
            }
            if let Some(cell) = store.float_cell("neg_threshold") {
                vad = vad.with_neg_threshold_cell(cell);
            }
        }
        Ok(Box::new(vad))
    }
}

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;
