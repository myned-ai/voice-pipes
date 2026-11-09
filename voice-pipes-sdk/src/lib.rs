//! `voice-pipes-sdk` — stable plugin boundary for Voice Pipes.
//!
//! Implement provider traits here; the binary that links the engine
//! registers your factory before it serves. Your crate only needs to depend on this crate plus
//! whatever inference runtime your model requires (`ort`, `candle`, `reqwest`, …).
//!
//! 📖 **Full guide**: see `docs/extending/plugin-sdk.md` in the voice-pipes
//! repository.
//!
//! # Extension points
//!
//! | Stage | Provider trait | Factory trait | Registration |
//! |---|---|---|---|
//! | Speech-to-text | [`SttProvider`] | [`SttFactory`] | `voice_pipes::stt::register_stt_factory()` |
//! | Text-to-speech | [`TtsProvider`] | [`TtsFactory`] | `voice_pipes::tts::register_tts_factory()` |
//! | LLM / text | [`TextProcessor`] | [`TextProcessorFactory`] | `voice_pipes::text::register_text_processor_factory()` |
//! | Voice activity | [`VadBackend`] | [`VadFactory`] | `voice_pipes::vad::register_vad_factory()` |
//! | Speech-to-speech | [`RealtimeProtocol`](s2s::RealtimeProtocol) | [`RealtimeFactory`](s2s::RealtimeFactory) | `voice_pipes::realtime::register_realtime_factory()` |
//! | Turn detection | [`TurnDetector`](turn::TurnDetector) | [`TurnDetectorFactory`](turn::TurnDetectorFactory) | `voice_pipes::stt::turn::register_turn_factory()` |
//!
//! All six follow the same **provider + factory + register** pattern. A
//! factory can also describe its own setup-wizard entry ([`wizard`]), and
//! hosted streaming clients can share [`reconnect`]'s bounded retry budget.
//!
//! # A complete minimal provider
//!
//! A TTS provider that opts into the health dashboard, the auto-degrade
//! loop, and the retry system by embedding an
//! [`Arc<ComponentCore>`](component::ComponentCore) (compile-checked):
//!
//! ```no_run
//! use std::sync::Arc;
//! use voice_pipes_sdk::component::{ComponentCore, ComponentKind, CoreComponent, ErrorPolicy};
//! use voice_pipes_sdk::{AudioFrame, TtsError, TtsProvider};
//!
//! pub struct MyTtsProvider {
//!     core: Arc<ComponentCore>,
//! }
//!
//! #[async_trait::async_trait]
//! impl TtsProvider for MyTtsProvider {
//!     async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
//!         let t0 = std::time::Instant::now();
//!         let frame = run_my_model(text)?;
//!         self.core.record_ok(t0.elapsed()); // feeds telemetry + health
//!         Ok(frame)
//!     }
//!
//!     fn sample_rate(&self) -> u32 {
//!         16_000
//!     }
//!
//!     fn name(&self) -> &str {
//!         "my-provider"
//!     }
//!
//!     fn as_component(&self) -> Option<Arc<dyn voice_pipes_sdk::Component>> {
//!         Some(Arc::new(
//!             CoreComponent::new(self.core.clone())
//!                 .with_error_policy(ErrorPolicy::RetryWithBackoff { max: 3 }),
//!         ))
//!     }
//! }
//!
//! fn run_my_model(_text: &str) -> Result<AudioFrame, TtsError> {
//!     Ok(AudioFrame { samples: vec![0; 16_000], sample_rate: 16_000, channels: 1 })
//! }
//! ```
//!
//! # Versions
//! The SDK is semver-stable from 1.0: a 1.x minor release only adds, so a
//! plugin built against 1.y keeps building against every later 1.z. Within
//! 1.x, expect
//! * new trait methods with default implementations,
//! * new [`#[non_exhaustive]`](https://doc.rust-lang.org/reference/attributes/type_system.html#the-non_exhaustive-attribute)
//!   enum variants (keep a `_ => {}` arm), and new fields on
//!   `#[non_exhaustive]` structs (build them with their constructors and
//!   `with_*` setters),
//! * new types and helpers.
//!
//! The full policy, including the structs whose fields are complete and the
//! dependency versions the SDK exposes, is `docs/extending/sdk-stability.md`
//! in the repository. CI checks every change with `cargo semver-checks`.
//!
//! The crates every implementation needs, [`async_trait`](macro@async_trait) and
//! [`CancellationToken`], are re-exported, so a plugin names them through
//! this crate and cannot end up on a different major version of either.

// Every public item of the plugin contract is documented; the workspace
// policy does not require it of the other crates.
#![warn(missing_docs)]

/// `with_<field>` setters for a `#[non_exhaustive]` struct, which a plugin
/// cannot build with a literal: each takes the field's type and returns the
/// value with that field set.
macro_rules! with_setters {
    ($($setter:ident => $field:ident: $ty:ty),* $(,)?) => {
        $(
            #[doc = concat!("With [`", stringify!($field), "`](Self::", stringify!($field), ") set to `value`.")]
            #[must_use]
            pub fn $setter(mut self, value: $ty) -> Self {
                self.$field = value;
                self
            }
        )*
    };
}

pub mod agent;
pub mod audio;
pub mod audit;
pub mod build;
pub mod component;
pub mod config;
pub mod options;
pub mod params;
pub mod phonemize;
pub mod reconnect;
pub mod s2s;
pub mod sanitize;
pub mod schema;
pub mod secrets;
pub mod sentence;
pub mod stt;
pub mod testing;
pub mod text;
pub mod tts;
pub mod turn;
pub mod vad;
pub mod wizard;

// ── Convenience re-exports ────────────────────────────────────

pub use agent::{ClientToolCall, ClientToolSpec, SessionEnded, SessionHandle, SessionSink};
pub use async_trait::async_trait;
pub use audio::AudioFrame;
pub use audit::TraceEvent;
pub use build::{BuildContext, BuildError};
pub use component::{
    Component, ComponentCore, ComponentKind, CoreComponent, DeviceInfo, ErrorPolicy, HealthState,
    HealthStatus, LatencyStats, ModelIdentity,
};
pub use config::{
    ConfigError, ConfigErrorKind, SttConfig, TextChunkingConfig, TextProcessorConfig, TtsConfig,
    VadConfig,
};
pub use params::{AtomicParamStore, AtomicParamStoreBuilder, MutableParamStore};
pub use sanitize::{
    DualTrackText, SanitizeConfig, StrippedDirectiveRecord, ThinkTagFilter, TtsDirectiveRecord,
    is_unspeakable_symbol, sanitize_dual_track, strip_unspeakable,
};
pub use schema::{
    OnlyWhen, ParamConstraints, ParamDescriptor, ParamError, ParamValue, ParamValueType,
    ProviderSchema,
};
pub use stt::{
    AudioChunk, AudioEncoding, InferenceMetrics, RecognitionRequest, SttCapability, SttError,
    SttFactory, SttProvider, TranscriptionResult, WarmUpScope, WordInfo,
};
pub use text::{TextFragment, TextProcessor, TextProcessorError, TextProcessorFactory};
pub use tokio_util::sync::CancellationToken;
pub use tts::{
    SynthesisHints, SynthesisRequest, TtsError, TtsFactory, TtsProvider, TtsSession,
    TtsSessionConfig, WordBoundary,
};
pub use vad::{VadBackend, VadError, VadFactory, VadResult};
pub use wizard::{
    WizardChoice, WizardDeploy, WizardDownload, WizardField, WizardFieldKind, WizardLayout,
    WizardModel,
};

/// The README's examples, compiled as doctests so they cannot drift from the
/// traits they show.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
