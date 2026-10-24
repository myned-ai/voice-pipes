//! Barge-in / interruption VAD engines.
//!
//! Engines detect that the user has started speaking while the assistant is
//! talking (or thinking) and emit [`VadEvent`]s that the orchestrator turns
//! into interruptions.
//!
//! ## What lives here
//! - [`VadEngine`]: async trait for barge-in detection.
//! - Built-in engines: [`RmsVadEngine`] (local volume threshold),
//!   [`ProviderVadEngine`] (driven by an external speech signal), and
//!   [`CompositeVadEngine`] (races several engines, first to fire wins).
//! - [`SpeechSignalSource`] + [`SpeechSignal`]: provider-agnostic input for
//!   [`ProviderVadEngine`], with [`SttResultSignalSource`] as the adapter for
//!   STT result channels.
//! - [`should_evaluate_barge_in`]: the shared gating rule that decides, per
//!   [`TurnState`], whether barge-in evaluation runs at all.
//!
//! ## Observability
//! Each engine seeds a `ComponentCore` and exposes itself as a
//! [`crate::component::Component`] via [`VadEngine::as_component`], so it is
//! visible in the component registry and at `/debug/components`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::audio::compute_rms;
use crate::component::{Component, ComponentCore, ComponentKind, ComponentTelemetry, HealthStatus};
use crate::pipeline::{AudioChunk, BargeInSettings, SharedTurnState, TurnState, get_turn_state};

// ── Event ────────────────────────────────────────────────────

/// The types of generic events a VAD engine can emit.
#[derive(Debug, Clone, PartialEq)]
pub enum VadEvent {
    /// User started speaking (trips the interruption token).
    SpeechStarted { source: String, timestamp: Duration },
    /// User finished speaking (triggers End of Turn).
    SpeechStopped { source: String, timestamp: Duration },
}

// ── Trait ────────────────────────────────────────────────────

/// A generic interface for any component that can detect voice activity boundaries.
#[async_trait]
pub trait VadEngine: Send + Sync {
    /// Returns the name of the engine (e.g., `"rms"`, `"provider"`, `"composite"`).
    fn name(&self) -> &'static str;

    /// Starts the engine, feeding its detected events into the provided sender channel.
    async fn start(&mut self, tx: mpsc::Sender<VadEvent>);

    /// Optional: expose this engine as a generic [`Component`] for the
    /// component registry.
    fn as_component(&self) -> Option<Arc<dyn Component>> {
        None
    }
}

// ── Shared barge-in gating ───────────────────────────────────

/// Decides whether barge-in evaluation should run given the current pipeline state.
///
/// Exhaustive `match` so adding a new [`TurnState`] forces every engine to
/// reconsider its behaviour at compile time.
pub fn should_evaluate_barge_in(state: TurnState, settings: &BargeInSettings) -> bool {
    match state {
        TurnState::Speaking | TurnState::Draining => true,
        TurnState::Processing => settings.barge_in_during_processing(),
        TurnState::Listening => false,
    }
}

// ── Component adapter ────────────────────────────────────────

struct EngineComponent {
    core: Arc<ComponentCore>,
}

impl Component for EngineComponent {
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

// ── RmsVadEngine ─────────────────────────────────────────────

/// Local Root-Mean-Square (RMS) Voice Activity Detector.
///
/// Performs a quick volume calculation on incoming microphone PCM. If the RMS
/// exceeds the configured threshold for `min_chunks` consecutive chunks the
/// engine emits [`VadEvent::SpeechStarted`].  Sub-20 ms reaction time but
/// susceptible to false positives from transient loud noises.
pub struct RmsVadEngine {
    mic_rx: Option<mpsc::Receiver<AudioChunk>>,
    turn_state: SharedTurnState,
    settings: BargeInSettings,
    core: Arc<ComponentCore>,
}

impl fmt::Debug for RmsVadEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RmsVadEngine")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl RmsVadEngine {
    /// Create an RMS engine reading microphone chunks from `mic_rx`.
    pub fn new(
        mic_rx: mpsc::Receiver<AudioChunk>,
        turn_state: SharedTurnState,
        settings: BargeInSettings,
    ) -> Self {
        Self {
            mic_rx: Some(mic_rx),
            turn_state,
            settings,
            core: Arc::new(ComponentCore::new("vad:rms", ComponentKind::Vad)),
        }
    }
}

#[async_trait]
impl VadEngine for RmsVadEngine {
    fn name(&self) -> &'static str {
        "rms"
    }

    async fn start(&mut self, tx: mpsc::Sender<VadEvent>) {
        let Some(mut mic_rx) = self.mic_rx.take() else {
            return;
        };

        let mut speech_chunks: u32 = 0;
        let mut silence_chunks: u32 = 0;
        let grace_chunks: u32 = 5;

        while let Some(c) = mic_rx.recv().await {
            let state = get_turn_state(&self.turn_state);

            if !should_evaluate_barge_in(state, &self.settings) {
                speech_chunks = 0;
                silence_chunks = 0;
                continue;
            }

            let rms = compute_rms(&c.samples);
            let threshold = self.settings.speech_threshold();
            let min_chunks = self.settings.min_chunks();

            if rms > threshold {
                speech_chunks += 1;
                silence_chunks = 0;

                if speech_chunks >= min_chunks {
                    tracing::info!(
                        rms = format!("{rms:.0}"),
                        chunks = speech_chunks,
                        threshold = format!("{threshold:.0}"),
                        "RMS threshold crossed: emitting VadEvent::SpeechStarted"
                    );

                    let _ = tx
                        .send(VadEvent::SpeechStarted {
                            source: self.name().to_string(),
                            timestamp: Duration::ZERO,
                        })
                        .await;

                    self.core.record_ok(Duration::ZERO);
                    speech_chunks = 0;
                    silence_chunks = 0;
                }
            } else if speech_chunks > 0 {
                silence_chunks += 1;
                if silence_chunks >= grace_chunks {
                    speech_chunks = 0;
                    silence_chunks = 0;
                }
            }
        }
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(EngineComponent {
            core: Arc::clone(&self.core),
        }))
    }
}

// ── SpeechSignalSource ───────────────────────────────────────

/// A normalised speech-activity signal coming from any out-of-band source
/// (STT provider, sidecar VAD service, custom plugin).
///
/// This is the only shape engines consume, keeping them independent of any
/// provider-specific event types.
#[derive(Debug, Clone, Default)]
pub struct SpeechSignal {
    /// True iff the upstream source explicitly declared speech-started
    /// (e.g. `OpenAI` Realtime `input_audio_buffer.speech_started`).
    pub speech_started: bool,
    /// True iff the upstream source has produced any non-empty transcript.
    /// Used as an inferred speech-started heuristic for providers that don't
    /// emit an explicit signal (e.g. `AssemblyAI` partial text).
    pub has_text: bool,
}

/// Source trait that yields [`SpeechSignal`] events to a [`VadEngine`].
///
/// Implementations adapt provider-specific event streams (STT result
/// channels, WebSocket frames, local file replays) into a uniform shape.
#[async_trait]
pub trait SpeechSignalSource: Send + Sync {
    /// Pull the next signal. Returns `None` when the upstream stream closes.
    async fn next(&mut self) -> Option<SpeechSignal>;
}

/// Adapter that turns an STT result receiver into a [`SpeechSignalSource`].
///
/// Lives here (rather than in `crate::stt`) so this module owns the only
/// place where the conversion happens; the STT module stays unaware of VAD
/// internals.
#[derive(Debug)]
pub struct SttResultSignalSource {
    rx: mpsc::Receiver<crate::stt::TranscriptionResult>,
}

impl SttResultSignalSource {
    /// Wrap an STT result receiver as a speech signal source.
    pub fn new(rx: mpsc::Receiver<crate::stt::TranscriptionResult>) -> Self {
        Self { rx }
    }
}

#[async_trait]
impl SpeechSignalSource for SttResultSignalSource {
    async fn next(&mut self) -> Option<SpeechSignal> {
        let r = self.rx.recv().await?;
        Some(SpeechSignal {
            speech_started: r.speech_started,
            has_text: !r.text.trim().is_empty(),
        })
    }
}

// ── ProviderVadEngine ────────────────────────────────────────

/// A VAD engine driven by an external [`SpeechSignalSource`] (typically the STT
/// provider's own speech-started events).
///
/// Callers wrap their event stream with [`SttResultSignalSource`] (or a
/// custom [`SpeechSignalSource`]) before constructing the engine, so the
/// engine itself carries no provider-specific dependencies.
pub struct ProviderVadEngine {
    source: Option<Box<dyn SpeechSignalSource>>,
    turn_state: SharedTurnState,
    settings: BargeInSettings,
    /// Whether transcript text alone counts as the start of speech.
    text_implies_speech: bool,
    core: Arc<ComponentCore>,
}

impl fmt::Debug for ProviderVadEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderVadEngine")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl ProviderVadEngine {
    /// Construct from any [`SpeechSignalSource`].
    pub fn from_source(
        source: Box<dyn SpeechSignalSource>,
        turn_state: SharedTurnState,
        settings: BargeInSettings,
    ) -> Self {
        Self {
            source: Some(source),
            turn_state,
            settings,
            text_implies_speech: true,
            core: Arc::new(ComponentCore::new("vad:provider", ComponentKind::Vad)),
        }
    }

    /// Whether interim transcript text is taken as speech having started.
    ///
    /// Text is the only evidence of speech a hosted recogniser gives when it
    /// does its own detection and the local VAD is a passthrough, so it must
    /// be able to interrupt there. Where a real local VAD is running it is
    /// worse than redundant: a recogniser that invents words from a sound too
    /// quiet to cross the energy gate interrupts the agent with them, and the
    /// gate that would have refused the sound is never consulted. An explicit
    /// speech-started signal from the provider always counts.
    #[must_use]
    pub fn text_implies_speech(mut self, yes: bool) -> Self {
        self.text_implies_speech = yes;
        self
    }
}

#[async_trait]
impl VadEngine for ProviderVadEngine {
    fn name(&self) -> &'static str {
        "provider"
    }

    async fn start(&mut self, tx: mpsc::Sender<VadEvent>) {
        let Some(mut source) = self.source.take() else {
            return;
        };

        while let Some(signal) = source.next().await {
            if signal.speech_started || (signal.has_text && self.text_implies_speech) {
                let state = get_turn_state(&self.turn_state);

                if should_evaluate_barge_in(state, &self.settings) {
                    tracing::info!(
                        source = self.name(),
                        "Provider detected speech start (explicit={} text={}): emitting VadEvent::SpeechStarted",
                        signal.speech_started,
                        signal.has_text
                    );
                    let _ = tx
                        .send(VadEvent::SpeechStarted {
                            source: self.name().to_string(),
                            timestamp: Duration::ZERO,
                        })
                        .await;
                    self.core.record_ok(Duration::ZERO);
                }
            }
        }
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(EngineComponent {
            core: Arc::clone(&self.core),
        }))
    }
}

// ── CompositeVadEngine ───────────────────────────────────────

/// Aggregates multiple underlying VAD engines, racing them concurrently.
///
/// The first engine to emit a [`VadEvent::SpeechStarted`] effectively wins
/// (the orchestrator only reacts to the first event per turn). Implements
/// the "First-to-Fire" synchronisation strategy.
pub struct CompositeVadEngine {
    engines: Vec<Box<dyn VadEngine + Send + Sync + 'static>>,
    core: Arc<ComponentCore>,
}

impl fmt::Debug for CompositeVadEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompositeVadEngine")
            .field("engines_count", &self.engines.len())
            .finish_non_exhaustive()
    }
}

impl CompositeVadEngine {
    /// Create a composite over the given engines; an empty list is a no-op engine.
    pub fn new(engines: Vec<Box<dyn VadEngine + Send + Sync + 'static>>) -> Self {
        Self {
            engines,
            core: Arc::new(ComponentCore::new("vad:composite", ComponentKind::Vad)),
        }
    }
}

#[async_trait]
impl VadEngine for CompositeVadEngine {
    fn name(&self) -> &'static str {
        "composite"
    }

    async fn start(&mut self, tx: mpsc::Sender<VadEvent>) {
        if self.engines.is_empty() {
            return;
        }

        let engines = std::mem::take(&mut self.engines);
        let mut handles = Vec::new();

        for mut engine in engines {
            let tx_clone = tx.clone();
            handles.push(tokio::spawn(async move {
                engine.start(tx_clone).await;
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(EngineComponent {
            core: Arc::clone(&self.core),
        }))
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/engine.rs"]
mod tests;
