//! The two-channel VAP detector: the ONNX floor model, the aligned rings and the evaluation gates.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ort::session::Session;
use ort::value::Tensor;
use parking_lot::Mutex;

use voice_pipes_sdk::TranscriptionResult;
use voice_pipes_sdk::turn::{TurnDetector, TurnSignal};

/// Rate the exported graph expects.
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Where the `p_future` bins start, from the exported graph's aggregation.
///
/// A bin boundary, not an estimate: when `p_future` says the user yields but
/// `p_now` does not, all the model has said is "inside that window", and the
/// honest lead time to report is its near edge.
pub(crate) const FUTURE_BIN_START: Duration = Duration::from_millis(600);

/// Clears the in-flight flag however the evaluation ends, including a panic
/// inside the blocking closure.
struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// What the detector asks of the model: who holds the floor, given the
/// channel-major stereo window.
pub(crate) trait FloorModel: Send {
    fn predict(&mut self, stereo: &[f32]) -> anyhow::Result<Prediction>;
}

/// The exported ONNX graph behind [`FloorModel`].
struct OnnxFloorModel {
    session: Session,
    input_name: String,
    window_samples: usize,
}

impl FloorModel for OnnxFloorModel {
    fn predict(&mut self, stereo: &[f32]) -> anyhow::Result<Prediction> {
        let tensor = Tensor::from_array(([1_usize, 2_usize, self.window_samples], stereo.to_vec()))
            .map_err(|e| anyhow::anyhow!("vap input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow::anyhow!("vap run: {e}"))?;
        let p_now = last_frame(&outputs, 0)?;
        let p_future = last_frame(&outputs, 1)?;
        Ok(Prediction { p_now, p_future })
    }
}

/// Two-channel anticipatory turn detector.
///
/// The model and the rings are behind separate locks: the STT task and the
/// playback path feed audio while an inference runs, and must not wait for
/// it.
pub struct VapTurnDetector {
    model: Arc<Mutex<Box<dyn FloorModel>>>,
    rings: Arc<Mutex<Rings>>,
    threshold: f32,
    pub(crate) window_samples: usize,
    min_interval: Duration,
    fallback_timeout: Duration,
    inference_in_flight: Arc<AtomicBool>,
    last_run: Arc<Mutex<Option<Instant>>>,
}

/// The two aligned channels.
///
/// Separate from the ONNX session so the alignment rules can be tested
/// without an exported model on disk -- they are the part most likely to be
/// wrong, and the part a missing model file would stop anyone checking.
#[derive(Debug)]
pub(crate) struct Rings {
    /// Both are always the same length; see the alignment note in the crate docs.
    pub(crate) user: VecDeque<f32>,
    pub(crate) agent: VecDeque<f32>,
    capacity: usize,
    /// True while user audio is actively streaming in the current turn;
    /// cleared by [`Self::end_user_turn`] when the turn finishes so that
    /// agent TTS audio advances the clock with user silence when upstream VAD
    /// (`silero` / `energy`) or echo-gating withholds silent mic frames.
    user_turn_active: bool,
}

impl Rings {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            user: VecDeque::with_capacity(capacity),
            agent: VecDeque::with_capacity(capacity),
            capacity,
            user_turn_active: false,
        }
    }

    /// Append the user's audio; the agent's channel follows with silence.
    pub(crate) fn push_user(&mut self, samples: &[f32]) {
        self.user_turn_active = true;
        for &s in samples {
            if self.user.len() == self.capacity {
                self.user.pop_front();
                self.agent.pop_front();
            }
            self.user.push_back(s);
            self.agent.push_back(0.0);
        }
    }

    /// Mark the end of the user's speaking turn.
    pub(crate) fn end_user_turn(&mut self) {
        self.user_turn_active = false;
    }

    /// Write the agent's audio into its channel.
    ///
    /// While the user channel is actively streaming (`user_turn_active`), this
    /// overwrites the tail of `agent` without extending the timeline so the
    /// two channels stay in step. Once the user's turn has ended and upstream
    /// VAD withholds silent mic frames (`!user_turn_active`), arriving agent
    /// audio advances both channels together (with `0.0` on the user channel).
    pub(crate) fn push_agent(&mut self, samples: &[f32]) {
        if self.user.is_empty() {
            return;
        }
        if self.user_turn_active {
            for (dst, &s) in self.agent.iter_mut().rev().zip(samples.iter().rev()) {
                *dst = s;
            }
        } else {
            for &s in samples {
                if self.user.len() == self.capacity {
                    self.user.pop_front();
                    self.agent.pop_front();
                }
                self.user.push_back(0.0);
                self.agent.push_back(s);
            }
        }
    }

    /// Channel-major `[user…, agent…]`, front-padded with silence until the
    /// window has filled.
    pub(crate) fn stereo_window(&self) -> Vec<f32> {
        let n = self.capacity;
        let pad = n.saturating_sub(self.user.len());
        let mut out = vec![0.0f32; n * 2];
        for (dst, s) in out.iter_mut().skip(pad).zip(&self.user) {
            *dst = *s;
        }
        for (dst, s) in out.iter_mut().skip(n + pad).zip(&self.agent) {
            *dst = *s;
        }
        out
    }
}

impl std::fmt::Debug for VapTurnDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VapTurnDetector")
            .field("threshold", &self.threshold)
            .field("window_samples", &self.window_samples)
            .field("min_interval", &self.min_interval)
            .field("fallback_timeout", &self.fallback_timeout)
            .finish_non_exhaustive()
    }
}

impl VapTurnDetector {
    /// Load an exported VAP graph and size the rings to its fixed window.
    pub fn new(
        model_path: &Path,
        threshold: f32,
        min_interval_ms: u64,
        fallback_timeout_ms: u64,
    ) -> anyhow::Result<Self> {
        // CPU only, whatever VP_ORT_EP says: a small model run a few times a
        // second, where a GPU round trip costs more than the inference.
        let session =
            voice_pipes_onnx::open_session(model_path, &voice_pipes_onnx::SessionOptions::new(2))
                .map_err(|e| anyhow::anyhow!("vap: {e}"))?;

        let input = session
            .inputs()
            .first()
            .ok_or_else(|| anyhow::anyhow!("vap model {} has no inputs", model_path.display()))?;
        let input_name = input.name().to_string();
        let shape = match input.dtype() {
            ort::value::ValueType::Tensor { shape, .. } => Some(shape.clone()),
            _ => None,
        };
        let window_samples = shape
            .as_deref()
            .and_then(window_from_input)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "vap model at {} does not declare a fixed stereo input [1, 2, N]; \
                     the window must be baked in at export time (see \
                     scripts/export-vap-onnx.py)",
                    model_path.display()
                )
            })?;

        let mut model = OnnxFloorModel {
            session,
            input_name,
            window_samples,
        };

        // Fail loud at startup. A graph with the wrong input rank or output
        // set would otherwise fail every inference for a whole session and
        // degrade silently to the silence-timeout fallback.
        model
            .predict(&vec![0.0f32; window_samples * 2])
            .map_err(|e| {
                anyhow::anyhow!(
                    "vap model at {} rejected a [1, 2, {}] probe -- incompatible export. {e}",
                    model_path.display(),
                    window_samples
                )
            })?;
        Ok(Self::with_model(
            Box::new(model),
            window_samples,
            threshold,
            min_interval_ms,
            fallback_timeout_ms,
        ))
    }

    /// A detector over `model` with a `window_samples` window, which the
    /// tests stub.
    pub(crate) fn with_model(
        model: Box<dyn FloorModel>,
        window_samples: usize,
        threshold: f32,
        min_interval_ms: u64,
        fallback_timeout_ms: u64,
    ) -> Self {
        Self {
            model: Arc::new(Mutex::new(model)),
            rings: Arc::new(Mutex::new(Rings::new(window_samples))),
            threshold,
            window_samples,
            min_interval: Duration::from_millis(min_interval_ms),
            fallback_timeout: Duration::from_millis(fallback_timeout_ms),
            inference_in_flight: Arc::new(AtomicBool::new(false)),
            last_run: Arc::new(Mutex::new(None)),
        }
    }

    /// i16 PCM to normalised f32 at the target rate.
    /// `None` for audio at a rate nothing can convert, which carries no
    /// turn cue and is dropped.
    fn prepare(samples: &[i16], sample_rate: u32) -> Option<Vec<f32>> {
        let f32_samples: Vec<f32> = samples
            .iter()
            .map(|&s| f32::from(s) / f32::from(i16::MAX))
            .collect();
        if sample_rate == TARGET_SAMPLE_RATE {
            return Some(f32_samples);
        }
        voice_pipes_sdk::audio::resample::resample_f32(
            &f32_samples,
            sample_rate,
            TARGET_SAMPLE_RATE,
        )
        .inspect_err(|e| tracing::debug!(error = %e, "vap: chunk dropped"))
        .ok()
    }

    fn push_user(&self, samples: &[i16], sample_rate: u32) {
        if samples.is_empty() {
            return;
        }
        if let Some(prepared) = Self::prepare(samples, sample_rate) {
            self.rings.lock().push_user(&prepared);
        }
    }

    fn push_agent(&self, samples: &[i16], sample_rate: u32) {
        if samples.is_empty() {
            return;
        }
        if let Some(prepared) = Self::prepare(samples, sample_rate) {
            self.rings.lock().push_agent(&prepared);
        }
    }

    fn stereo_window(&self) -> Vec<f32> {
        self.rings.lock().stereo_window()
    }

    /// Stamp a run unless the last one was within the minimum interval;
    /// `false` means skip.
    fn claim_run(&self) -> bool {
        let mut last = self.last_run.lock();
        if last.is_some_and(|at| at.elapsed() < self.min_interval) {
            return false;
        }
        *last = Some(Instant::now());
        true
    }
}

/// The model's view at the most recent frame.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Prediction {
    /// P(the user holds the floor) now.
    pub(crate) p_now: f32,
    /// P(the user holds the floor) over the next bin.
    pub(crate) p_future: f32,
}

impl Prediction {
    /// Turn the two probabilities into a signal.
    ///
    /// `p_now` is P(channel 0 holds the floor), so the probability that the
    /// user's turn is ending is its complement.
    pub(crate) fn to_signal(self, threshold: f32) -> TurnSignal {
        let confidence = 1.0 - self.p_now;
        let future_confidence = 1.0 - self.p_future;

        if confidence >= threshold {
            return TurnSignal::likely_complete()
                .with_confidence(confidence)
                .ending_in(Duration::ZERO);
        }
        let signal = TurnSignal::incomplete().with_confidence(confidence);
        if future_confidence >= threshold {
            // Still holding the floor now, expected to yield inside the
            // future bin. Report that bin's near edge, not a made-up point.
            signal.ending_in(FUTURE_BIN_START)
        } else {
            signal
        }
    }
}

/// Read the last frame of output `index`, which is `[1, frames]`. An export
/// with fewer outputs is an error, not a panic: the probe inference runs
/// while the detector is built, and a worker must refuse an incompatible
/// export rather than crash on it.
fn last_frame(outputs: &ort::session::SessionOutputs<'_>, index: usize) -> anyhow::Result<f32> {
    let output = outputs.values().nth(index).ok_or_else(|| {
        anyhow::anyhow!(
            "vap export has {} outputs, expected p_now and p_future",
            outputs.len()
        )
    })?;
    let (_shape, data) = output
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("vap extract output {index}: {e}"))?;
    data.last()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("vap output {index} was empty"))
}

/// Pull `N` out of a declared `[1, 2, N]` input shape.
pub(crate) fn window_from_input(dims: &[i64]) -> Option<usize> {
    let [_, 2, window] = *dims else {
        return None;
    };
    // A dynamic axis is reported as -1; the window has to be concrete.
    usize::try_from(window).ok().filter(|n| *n > 0)
}

#[async_trait]
impl TurnDetector for VapTurnDetector {
    async fn evaluate(
        &self,
        _result: &TranscriptionResult,
        silence_duration: Duration,
    ) -> TurnSignal {
        // Hard fallback -- never let the model stall the pipeline forever.
        if silence_duration >= self.fallback_timeout {
            return TurnSignal::forced_complete();
        }

        // The single-inference gate, then the cadence gate. Both skip rather
        // than queue: a prediction about audio that has since moved on is
        // worth less than the CPU it costs. The cadence is stamped only when
        // an inference runs, so a call skipped because one is in flight does
        // not push the next run back a whole interval.
        if self
            .inference_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return TurnSignal::incomplete();
        }
        let _guard = InFlightGuard(Arc::clone(&self.inference_in_flight));
        if !self.claim_run() {
            return TurnSignal::incomplete();
        }

        let stereo = self.stereo_window();
        let threshold = self.threshold;
        let model = Arc::clone(&self.model);
        // Only the model is locked while it runs; audio keeps arriving.
        let predicted = tokio::task::spawn_blocking(move || model.lock().predict(&stereo))
            .await
            .map_err(|e| anyhow::anyhow!("vap join: {e}"))
            .and_then(|r| r);

        match predicted {
            Ok(p) => {
                let signal = p.to_signal(threshold);
                tracing::trace!(
                    p_now = format!("{:.3}", p.p_now),
                    p_future = format!("{:.3}", p.p_future),
                    confidence = ?signal.confidence,
                    "vap: prediction"
                );
                signal
            }
            Err(e) => {
                tracing::warn!(error = %e, "vap inference failed; treating as incomplete");
                TurnSignal::incomplete()
            }
        }
    }

    fn feed_audio(&self, samples: &[i16], sample_rate: u32) {
        self.push_user(samples, sample_rate);
    }

    fn feed_agent_audio(&self, samples: &[i16], sample_rate: u32) {
        self.push_agent(samples, sample_rate);
    }

    fn turn_ended(&self) {
        self.rings.lock().end_user_turn();
    }

    fn name(&self) -> &'static str {
        "vap"
    }
}
