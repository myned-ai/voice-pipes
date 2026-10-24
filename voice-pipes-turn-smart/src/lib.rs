//! Pipecat Smart Turn v3 -- audio-only semantic end-of-turn classifier, as
//! a voice-pipes turn detector plugin. The binary registers [`factory`]
//! and [`wizard_choices`]; `[turn_detection] mode = "smart"` selects it.
//!
//! Wraps a Whisper-style ONNX model fed [1, 80, 800] log-mel features
//! (see `mel`) computed from
//! the most recent 8 s of 16 kHz speech; it outputs the probability
//! that the user has finished their turn. This is a generic semantic endpointer that works with
//! any recogniser -- Nemotron, Parakeet,
//! `AssemblyAI`, etc. -- and plugs in beside the existing silence-timeout
//! and provider-signal detectors.
//!
//! Model: <https://github.com/pipecat-ai/smart-turn> (Apache-2.0).
//!
//! Integration points:
//! * Implements [`TurnDetector`], so the engine's STT stage treats it
//!   like any other detector.
//! * Implements [`TurnDetectorFactory`] so it is selected via
//!   `[turn_detection] mode = "smart"` in TOML.
//! * Audio is fed via the trait's `feed_audio` hook from the STT task,
//!   so we never reach back into the audio pipeline directly.

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

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use ort::session::Session;
use ort::value::Tensor;
use parking_lot::Mutex;

use voice_pipes_sdk::turn::{TurnConfig, TurnDetector, TurnDetectorFactory, TurnSignal};
use voice_pipes_sdk::{ConfigError, TranscriptionResult};

mod mel;
mod wizard;

pub use wizard::wizard_choices;

/// The factory the binary registers.
pub fn factory() -> Box<dyn TurnDetectorFactory> {
    Box::new(SmartTurnFactory)
}

/// Numerically-stable logistic sigmoid for the single completion logit.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

const TARGET_SAMPLE_RATE: u32 = 16_000;
const DEFAULT_THRESHOLD: f32 = 0.5;
const DEFAULT_WINDOW_SECS: u32 = 8;
const DEFAULT_MIN_AUDIO_MS: u64 = 500;
/// Accepted `smart_window_secs`. The model reads the last 8 s, so a longer
/// window would hold audio it never sees.
const WINDOW_SECS: std::ops::RangeInclusive<u64> = 1..=8;
/// Accepted `smart_min_audio_ms`.
const MIN_AUDIO_MS: std::ops::RangeInclusive<u64> = 0..=10_000;

/// Shortest pause worth asking the model about.
///
/// `evaluate` is called on every interim transcript and on every tick of the
/// silence timer, but a pause this brief is never an end of turn: the user is
/// still mid-word. Running the model anyway costs a full Whisper-style
/// inference per interim result, which saturates the blocking pool and
/// starves the STT and VAD tasks sharing it -- the transcript then lags, the
/// final chunk is never sent, and the session hangs in "speaking".
const MIN_SILENCE_FOR_INFERENCE: Duration = Duration::from_millis(250);

/// Clears the in-flight flag however the evaluation ends, including a panic
/// inside the blocking closure.
struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Decide whether this evaluation should reach the model, and claim the
/// single inference slot if so.
///
/// `None` means skip: there is nothing to classify, the pause is too short to
/// be an end of turn, or another evaluation is already running. Each skip is
/// one ONNX inference that does not land on the blocking pool.
fn claim_inference(
    text: &str,
    silence: Duration,
    min_silence: Duration,
    in_flight: &Arc<AtomicBool>,
) -> Option<InFlightGuard> {
    // Nothing said yet -- running on pure silence wastes compute and biases
    // the probability.
    if text.trim().is_empty() {
        return None;
    }
    if silence < min_silence {
        return None;
    }
    // One inference at a time. Interim results arrive faster than the model
    // runs, so without this the blocking pool fills with redundant
    // evaluations of nearly identical audio.
    in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    Some(InFlightGuard(Arc::clone(in_flight)))
}

// ── Detector ────────────────────────────────────────────────

/// What the detector asks of the model: the probability that the turn is
/// complete, given Smart Turn v3's `[1, 80, 800]` log-mel features.
pub(crate) trait CompletionModel: Send {
    fn probability(&mut self, features: Vec<f32>) -> anyhow::Result<f32>;
}

/// The ONNX export behind [`CompletionModel`].
struct OnnxModel {
    session: Session,
    input_name: String,
}

impl CompletionModel for OnnxModel {
    fn probability(&mut self, features: Vec<f32>) -> anyhow::Result<f32> {
        let tensor = Tensor::from_array(([1_usize, mel::N_MELS, mel::N_FRAMES], features))
            .map_err(|e| anyhow::anyhow!("smart-turn input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|e| anyhow::anyhow!("smart-turn run: {e}"))?;

        // An export without an output is an error, not a panic.
        let output = outputs
            .values()
            .next()
            .ok_or_else(|| anyhow::anyhow!("smart-turn export has no output"))?;
        let (_shape, data) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("smart-turn extract output: {e}"))?;

        // Smart Turn v3 emits a single completion LOGIT -> sigmoid. (Older
        // two-class exports emit [incomplete, complete] logits -> softmax over
        // the last two.)
        Ok(match *data {
            [] => 0.0,
            [logit] => sigmoid(logit),
            [.., a, b] => {
                let m = a.max(b);
                let ea = (a - m).exp();
                let eb = (b - m).exp();
                eb / (ea + eb)
            }
        })
    }
}

/// Smart Turn v3 detector.
///
/// Maintains a rolling buffer of the most recent `window_samples` of
/// 16 kHz mono f32 audio (normalised to `[-1.0, 1.0]`) for the current
/// turn. Each call to [`evaluate`](TurnDetector::evaluate) runs the model
/// when there is enough buffered speech and a non-empty partial transcript;
/// the silence timeout is the hard fallback.
///
/// The model and the audio are behind separate locks: the STT task feeds
/// audio while an inference runs, and must not wait for it.
pub(crate) struct SmartTurnDetector {
    model: Arc<Mutex<Box<dyn CompletionModel>>>,
    ring: Arc<Mutex<VecDeque<f32>>>,
    threshold: f32,
    window_samples: usize,
    min_audio_samples: usize,
    fallback_timeout: Duration,
    /// Set while one inference is running, so overlapping `evaluate` calls
    /// skip instead of queueing another model run behind it.
    inference_in_flight: Arc<AtomicBool>,
}

impl std::fmt::Debug for SmartTurnDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmartTurnDetector")
            .field("threshold", &self.threshold)
            .field("window_samples", &self.window_samples)
            .field("min_audio_samples", &self.min_audio_samples)
            .field("fallback_timeout", &self.fallback_timeout)
            .finish_non_exhaustive()
    }
}

impl SmartTurnDetector {
    /// Load a Smart Turn v3 ONNX export.
    pub fn new(
        model_path: &Path,
        threshold: f32,
        window_secs: u32,
        min_audio_ms: u64,
        fallback_timeout_ms: u64,
    ) -> anyhow::Result<Self> {
        // CPU only, whatever VP_ORT_EP says: one small inference per pause,
        // where a GPU round trip costs more than the inference.
        let session =
            voice_pipes_onnx::open_session(model_path, &voice_pipes_onnx::SessionOptions::new(2))
                .map_err(|e| anyhow::anyhow!("smart-turn: {e}"))?;

        // Smart Turn v3's input is the Whisper feature-extractor tensor
        // `input_features`; fall back to that name when introspection fails.
        let input_name = session
            .inputs()
            .first()
            .map_or_else(|| "input_features".to_string(), |i| i.name().to_string());
        let mut model = OnnxModel {
            session,
            input_name,
        };

        // Fail-loud guard: probe the model with the exact [1, 80, 800] log-mel
        // tensor we feed at runtime. An incompatible model (Smart Turn v2, or any
        // non-Whisper export) errors HERE at startup instead of silently failing
        // every inference and degrading to the silence-timeout fallback for a
        // whole session (the regression this replaces).
        model
            .probability(mel::log_mel_features(&vec![0.0f32; mel::SAMPLE_RATE]))
            .map_err(|e| {
                anyhow::anyhow!(
                    "smart-turn model at {} rejected the Whisper log-mel input \
                     [1, {}, {}] — incompatible model (expected Smart Turn v3, \
                     e.g. smart-turn-v3.2-cpu.onnx). Inference error: {e}",
                    model_path.display(),
                    mel::N_MELS,
                    mel::N_FRAMES,
                )
            })?;

        Ok(Self::with_model(
            Box::new(model),
            threshold,
            window_secs,
            min_audio_ms,
            fallback_timeout_ms,
        ))
    }

    /// A detector over `model`, which the tests stub.
    pub(crate) fn with_model(
        model: Box<dyn CompletionModel>,
        threshold: f32,
        window_secs: u32,
        min_audio_ms: u64,
        fallback_timeout_ms: u64,
    ) -> Self {
        let window_samples = (TARGET_SAMPLE_RATE as usize) * window_secs as usize;
        Self {
            model: Arc::new(Mutex::new(model)),
            ring: Arc::new(Mutex::new(VecDeque::with_capacity(window_samples))),
            threshold,
            window_samples,
            min_audio_samples: ((TARGET_SAMPLE_RATE as u64) * min_audio_ms / 1000) as usize,
            fallback_timeout: Duration::from_millis(fallback_timeout_ms),
            inference_in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Convert i16 PCM to normalised f32 and append to the rolling buffer.
    /// Resamples to 16 kHz when the input rate differs.
    fn push_samples(&self, samples: &[i16], sample_rate: u32) {
        if samples.is_empty() {
            return;
        }

        let f32_samples: Vec<f32> = samples
            .iter()
            .map(|&s| (s as f32) / (i16::MAX as f32))
            .collect();

        let resampled = if sample_rate == TARGET_SAMPLE_RATE {
            f32_samples
        } else {
            match voice_pipes_sdk::audio::resample::resample_f32(
                &f32_samples,
                sample_rate,
                TARGET_SAMPLE_RATE,
            ) {
                Ok(resampled) => resampled,
                // Audio at a rate nothing can convert carries no turn cue.
                Err(e) => {
                    tracing::debug!(error = %e, "smart-turn: chunk dropped");
                    return;
                }
            }
        };

        let mut ring = self.ring.lock();
        let cap = self.window_samples;
        for s in resampled {
            if ring.len() == cap {
                ring.pop_front();
            }
            ring.push_back(s);
        }
    }

    /// Samples held for the current turn.
    #[cfg(test)]
    pub(crate) fn buffered_samples(&self) -> usize {
        self.ring.lock().len()
    }
}

/// Run the model on a snapshot of the ring. `None` when the turn has less
/// audio than the model needs: there is no probability to report.
fn run_inference(
    model: &Mutex<Box<dyn CompletionModel>>,
    ring: &Mutex<VecDeque<f32>>,
    min_audio_samples: usize,
) -> anyhow::Result<Option<f32>> {
    let samples: Vec<f32> = {
        let ring = ring.lock();
        if ring.len() < min_audio_samples {
            return Ok(None);
        }
        ring.iter().copied().collect()
    };
    // Only the model is locked while it runs; audio keeps arriving.
    let features = mel::log_mel_features(&samples);
    model.lock().probability(features).map(Some)
}

#[async_trait]
impl TurnDetector for SmartTurnDetector {
    async fn evaluate(
        &self,
        result: &TranscriptionResult,
        silence_duration: Duration,
    ) -> TurnSignal {
        // Hard fallback -- never let the model stall the pipeline forever.
        if silence_duration >= self.fallback_timeout {
            return TurnSignal::forced_complete();
        }

        let Some(_in_flight) = claim_inference(
            &result.text,
            silence_duration,
            MIN_SILENCE_FOR_INFERENCE,
            &self.inference_in_flight,
        ) else {
            // A skipped evaluation has no opinion on the probability. Saying
            // "incomplete with confidence 0" would read as the model having
            // ruled the turn out, which it has not: it never ran.
            return TurnSignal::incomplete();
        };

        // Move ONNX inference off the async runtime so we don't block other
        // tasks during the ~tens-of-ms CPU call.
        let model = Arc::clone(&self.model);
        let ring = Arc::clone(&self.ring);
        let min_audio_samples = self.min_audio_samples;
        let probability =
            tokio::task::spawn_blocking(move || run_inference(&model, &ring, min_audio_samples))
                .await
                .map_err(|e| anyhow::anyhow!("smart-turn join: {e}"))
                .and_then(|r| r);

        match probability {
            // Too little audio to ask the model: no opinion, as for a skip.
            Ok(None) => TurnSignal::incomplete(),
            Ok(Some(p)) if p >= self.threshold => {
                tracing::debug!(
                    probability = format!("{p:.3}"),
                    threshold = self.threshold,
                    "smart-turn: end-of-turn predicted"
                );
                TurnSignal::likely_complete().with_confidence(p)
            }
            Ok(Some(p)) => {
                tracing::trace!(
                    probability = format!("{p:.3}"),
                    threshold = self.threshold,
                    "smart-turn: not yet complete"
                );
                // Below the commit threshold the probability is still the
                // most informative thing in the pipeline about whether this
                // turn is ending, so it travels with the signal instead of
                // being collapsed into a bare "no".
                TurnSignal::incomplete().with_confidence(p)
            }
            Err(e) => {
                tracing::warn!(error = %e, "smart-turn inference failed; treating as incomplete");
                TurnSignal::incomplete()
            }
        }
    }

    fn feed_audio(&self, samples: &[i16], sample_rate: u32) {
        self.push_samples(samples, sample_rate);
    }

    /// The window holds one turn's audio: the next turn is judged on its
    /// own speech, not the tail of the last one.
    fn turn_ended(&self) {
        self.ring.lock().clear();
    }

    fn name(&self) -> &'static str {
        "smart"
    }
}

// ── Factory ─────────────────────────────────────────────────

/// Factory for the Smart Turn v3 detector.
#[derive(Debug)]
pub(crate) struct SmartTurnFactory;

/// The settings Smart Turn reads from `[turn_detection]`.
const SMART_KEYS: &[&str] = &[
    "smart_model_path",
    "smart_threshold",
    "smart_window_secs",
    "smart_min_audio_ms",
];

/// Smart Turn's settings, parsed from the section's options.
#[derive(Debug)]
struct SmartSettings {
    model_path: String,
    threshold: f32,
    window_secs: u32,
    min_audio_ms: u64,
}

impl SmartSettings {
    /// Every problem with the settings, or the settings.
    fn parse(cfg: &TurnConfig) -> Result<Self, Vec<ConfigError>> {
        let mut errors = cfg.unknown_options(SMART_KEYS);
        let err =
            |key: &str, message: String| ConfigError::new(format!("turn_detection.{key}"), message);
        let model_path = match cfg.options.get("smart_model_path") {
            None => {
                errors.push(err(
                    "smart_model_path",
                    "required when turn_detection.mode = \"smart\"".into(),
                ));
                String::new()
            }
            Some(v) => match v.as_str() {
                Some(p) if Path::new(p).exists() => p.to_string(),
                Some(p) => {
                    errors.push(err("smart_model_path", format!("file not found: {p}")));
                    String::new()
                }
                None => {
                    errors.push(err("smart_model_path", "must be a path".into()));
                    String::new()
                }
            },
        };
        let mut o = cfg.options();
        let threshold = o.f64_in("smart_threshold", 0.0..=1.0, f64::from(DEFAULT_THRESHOLD));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a probability in [0, 1]; f32 is the model's precision"
        )]
        let threshold = threshold as f32;
        let window_secs = o.u64_in(
            "smart_window_secs",
            WINDOW_SECS,
            u64::from(DEFAULT_WINDOW_SECS),
        );
        let window_secs = u32::try_from(window_secs).unwrap_or(DEFAULT_WINDOW_SECS);
        let min_audio_ms = o.u64_in("smart_min_audio_ms", MIN_AUDIO_MS, DEFAULT_MIN_AUDIO_MS);
        errors.extend(o.into_value_errors());
        if errors.is_empty() {
            Ok(Self {
                model_path,
                threshold,
                window_secs,
                min_audio_ms,
            })
        } else {
            Err(errors)
        }
    }
}

impl TurnDetectorFactory for SmartTurnFactory {
    fn name(&self) -> &'static str {
        "smart"
    }

    fn validate(&self, cfg: &TurnConfig) -> Vec<ConfigError> {
        SmartSettings::parse(cfg).err().unwrap_or_default()
    }

    fn build(
        &self,
        cfg: &TurnConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let s = SmartSettings::parse(cfg).map_err(|errors| {
            let messages: Vec<String> = errors
                .iter()
                .map(|e| format!("{}: {}", e.field, e.message))
                .collect();
            anyhow::anyhow!("{}", messages.join("; "))
        })?;
        let detector = SmartTurnDetector::new(
            Path::new(&s.model_path),
            s.threshold,
            s.window_secs,
            s.min_audio_ms,
            cfg.silence_timeout_ms,
        )?;
        tracing::info!(
            model = s.model_path,
            threshold = s.threshold,
            window_secs = s.window_secs,
            min_audio_ms = s.min_audio_ms,
            fallback_timeout_ms = cfg.silence_timeout_ms,
            "smart-turn detector ready"
        );
        Ok(Box::new(detector))
    }

    fn param_schema(&self) -> Vec<voice_pipes_sdk::schema::ParamDescriptor> {
        use voice_pipes_sdk::schema::ParamDescriptor;
        vec![
            ParamDescriptor::static_path(
                "smart_model_path",
                "Smart model path",
                "Path to the Smart Turn ONNX model file. Runs on the CPU whatever VP_ORT_EP says.",
            ),
            // Read once when the detector is built: shown, changed by a restart.
            ParamDescriptor::mutable_float(
                "smart_threshold",
                "Smart threshold",
                "Probability threshold above which the turn is considered complete (range 0.0-1.0, default 0.5).",
                0.0,
                1.0,
            )
            .restart_required(),
            ParamDescriptor::mutable_int(
                "smart_window_secs",
                "Smart window (s)",
                "Rolling audio window duration in seconds (default 8).",
                bound(*WINDOW_SECS.start()),
                bound(*WINDOW_SECS.end()),
            )
            .restart_required(),
            ParamDescriptor::mutable_int(
                "smart_min_audio_ms",
                "Smart min audio (ms)",
                "Minimum audio duration buffered before running ONNX evaluation (default 500 ms).",
                bound(*MIN_AUDIO_MS.start()),
                bound(*MIN_AUDIO_MS.end()),
            )
            .restart_required(),
        ]
    }
}

/// A range bound as the schema's integer type; the bounds are small.
fn bound(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "tests/turn_smart.rs"]
mod tests;
