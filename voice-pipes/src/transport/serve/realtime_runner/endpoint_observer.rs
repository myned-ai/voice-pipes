//! vp's own end-of-speech observer on the realtime uplink.
//!
//! Speech-to-speech providers tell us when the user stopped talking in
//! their own time: `OpenAI`'s `input_audio_buffer.speech_stopped` after its
//! server VAD window, Gemini Live only implicitly through the input
//! transcription that lands 0.45–1 s after the fact. Anchoring TTFB on
//! that hides the provider's endpointing cost and makes `endpointing_ms`
//! structurally null for S2S turns.
//!
//! The observer runs vp's existing VAD backends over the uplink *as a
//! passive listener*: it never gates, delays or interrupts audio and is
//! never consulted by barge-in logic. It only stamps the instant the user's
//! speech actually ended, so the turn can report
//! `provider_endpointing_ms = provider anchor − vp stamp` and, when the
//! provider gives no anchor at all, fall back to an honest one
//! (`ttfb_anchor_source = "vp_vad"`).
//!
//! Timing is accounted in *audio time* (sample counts), not wall time: the
//! uplink is realtime-paced so the two agree in production, and audio time
//! is deterministic in tests. The stamp itself is the wall-clock instant
//! of the last frame classified as speech — the true end — captured when
//! that frame was processed, not when the hangover expired.
//!
//! The same pass also meters the input: the
//! chunk and the VAD's verdict go to an [`InputAudioMeter`], and each
//! declared end carries its segment's speech level, noise floor and
//! clipping on the stamp. Rejected clicks discard their segment.

use std::time::Duration;

use tracing::{debug, warn};

use super::turn::SpeechEndStamp;
use crate::audio::InputAudioMeter;
use crate::config::EndpointObserverConfig;
use crate::vad::segmenter::{EnergyVad, VadBackend};

/// `input_level_source` for a VAD backend name: `vp_` and the name
/// (`vp_energy`, or `vp_<backend>` for a registered model backend). Each
/// distinct name is made static once; the set is the registered backends.
fn level_source(backend: &str) -> &'static str {
    if backend == "energy" {
        return "vp_energy";
    }
    static LABELS: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<String, &'static str>>,
    > = std::sync::OnceLock::new();
    let mut labels = LABELS.get_or_init(Default::default).lock();
    labels
        .entry(backend.to_string())
        .or_insert_with(|| Box::leak(format!("vp_{backend}").into_boxed_str()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No speech in progress.
    Idle,
    /// Speech frames are arriving.
    Speech,
    /// Silence after speech; the end is declared once it lasts `hangover`.
    Trailing,
}

/// Passive end-of-speech detector over the realtime uplink.
pub struct EndpointObserver {
    vad: Box<dyn VadBackend>,
    hangover: Duration,
    min_speech: Duration,
    state: State,
    /// Speech accumulated in the current segment (audio time).
    speech_audio: Duration,
    /// Silence accumulated since the last speech frame (audio time).
    silence_audio: Duration,
    /// Wall clock of the last frame classified as speech.
    last_speech: Option<SpeechEndStamp>,
    /// Input level / noise / clipping for the current segment.
    meter: InputAudioMeter,
}

impl std::fmt::Debug for EndpointObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointObserver")
            .field("backend", &self.vad.name())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl EndpointObserver {
    /// Build from config. `None` when the observer is disabled. A missing
    /// section means the defaults (enabled, energy backend).
    pub fn from_config(cfg: Option<&EndpointObserverConfig>) -> Option<Self> {
        let defaults = EndpointObserverConfig::default();
        let cfg = cfg.unwrap_or(&defaults);
        if !cfg.enabled {
            return None;
        }
        let vad = Self::build_backend(cfg);
        debug!(
            backend = vad.name(),
            hangover_ms = cfg.hangover_ms,
            min_speech_ms = cfg.min_speech_ms,
            "realtime endpoint observer enabled"
        );
        Some(Self::new(vad, cfg.hangover_ms, cfg.min_speech_ms))
    }

    /// The named VAD backend, built by its factory like the pipeline's own
    /// VAD, so a plugin backend serves here too. The observer only measures,
    /// so a backend that cannot be built falls back to the energy detector
    /// rather than failing the session.
    fn build_backend(cfg: &EndpointObserverConfig) -> Box<dyn VadBackend> {
        let built = crate::vad::segmenter::vad_factories()
            .into_iter()
            .find(|f| f.matches(&cfg.backend))
            .ok_or_else(|| {
                anyhow::anyhow!("no VAD backend named \"{}\" in this build", cfg.backend)
            })
            .and_then(|f| {
                f.build(
                    &cfg.vad_settings().backend_config(),
                    &voice_pipes_sdk::BuildContext::new(),
                )
                .map_err(anyhow::Error::from)
            });
        built.unwrap_or_else(|e| {
            warn!(
                backend = %cfg.backend,
                error = %e,
                "endpoint observer: backend unavailable; falling back to energy VAD"
            );
            Box::new(EnergyVad::new(cfg.energy_threshold))
        })
    }

    /// Build around an explicit backend (tests, custom wiring).
    pub fn new(vad: Box<dyn VadBackend>, hangover_ms: u64, min_speech_ms: u64) -> Self {
        let meter = InputAudioMeter::new(level_source(vad.name()));
        Self {
            meter,
            vad,
            hangover: Duration::from_millis(hangover_ms),
            min_speech: Duration::from_millis(min_speech_ms),
            state: State::Idle,
            speech_audio: Duration::ZERO,
            silence_audio: Duration::ZERO,
            last_speech: None,
        }
    }

    /// Feed one uplink chunk. Returns the end-of-speech stamp (wall clock of
    /// the last speech frame) when a segment of at least `min_speech` is
    /// followed by `hangover` of silence. Speech resuming inside the
    /// hangover cancels the pending end; segments shorter than
    /// `min_speech` (clicks, breaths) are dropped silently.
    pub fn observe(&mut self, samples: &[i16], sample_rate: u32) -> Option<SpeechEndStamp> {
        if samples.is_empty() || sample_rate == 0 {
            return None;
        }
        let chunk = Duration::from_secs_f64(samples.len() as f64 / f64::from(sample_rate));
        let is_speech = self
            .vad
            .process_chunk(samples, sample_rate)
            .is_ok_and(|r| r.is_speech);
        self.meter.feed(samples, sample_rate, is_speech);

        match (self.state, is_speech) {
            (State::Idle, false) => {}
            (State::Idle, true) => {
                self.state = State::Speech;
                self.speech_audio = chunk;
                self.silence_audio = Duration::ZERO;
                self.last_speech = Some(SpeechEndStamp::now());
            }
            (State::Speech | State::Trailing, true) => {
                self.state = State::Speech;
                self.speech_audio += chunk;
                self.silence_audio = Duration::ZERO;
                self.last_speech = Some(SpeechEndStamp::now());
            }
            (State::Speech, false) => {
                self.state = State::Trailing;
                self.silence_audio = chunk;
            }
            (State::Trailing, false) => {
                self.silence_audio += chunk;
                if self.silence_audio >= self.hangover {
                    let long_enough = self.speech_audio >= self.min_speech;
                    self.state = State::Idle;
                    self.speech_audio = Duration::ZERO;
                    self.silence_audio = Duration::ZERO;
                    let stamp = self.last_speech.take();
                    if long_enough {
                        let input_audio = self.meter.take();
                        return stamp.map(|s| SpeechEndStamp { input_audio, ..s });
                    }
                    self.meter.discard();
                }
            }
        }
        None
    }

    /// Forget any segment in progress.
    #[cfg(test)]
    pub fn reset(&mut self) {
        self.vad.reset();
        self.meter.reset();
        self.state = State::Idle;
        self.speech_audio = Duration::ZERO;
        self.silence_audio = Duration::ZERO;
        self.last_speech = None;
    }

    /// Backend name.
    #[cfg(test)]
    pub fn backend_name(&self) -> &str {
        self.vad.name()
    }
}

#[cfg(test)]
#[path = "tests/endpoint_observer.rs"]
mod tests;
