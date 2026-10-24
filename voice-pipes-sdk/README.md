# voice-pipes-sdk

The stable plugin boundary for [Voice Pipes](../voice-pipes/README.md).

Add a new STT, TTS, or LLM model without touching the core repository:
implement two traits, call one registration function, ship your own crate.

📖 **Full developer guide**: [Plugin SDK](../docs/extending/plugin-sdk.md)

---

## Quick start

```toml
# Cargo.toml for your plugin crate
[dependencies]
voice-pipes-sdk = "1"
async-trait     = "0.1"
tokio           = { version = "1", features = ["sync", "macros"] }
tokio-util      = "0.7"
# Add your inference runtime — ort (ONNX), candle (pure-Rust), reqwest (remote API), etc.
# The SDK itself has no opinion on how you run inference.
```

```rust
// src/lib.rs
use std::sync::Arc;
use std::time::Instant;
use async_trait::async_trait;
// Bring mpsc channel halves into scope so signatures stay concise.
// If you also use std::sync::mpsc in this file, alias one to avoid a name clash:
//   use tokio::sync::mpsc::{Receiver as TokioReceiver, Sender as TokioSender};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use voice_pipes_sdk::{
    SttConfig, SttFactory, SttProvider, SttCapability,
    RecognitionRequest, TranscriptionResult, SttError, AudioChunk, AudioFrame,
    Component,
    component::{ComponentCore, ComponentKind, CoreComponent},
};

// Your provider holds whatever runtime state your model needs (weights, session handles, etc.).
// Wrap shared state in Arc so the provider can be cheaply cloned across threads if needed.
pub struct MyProvider { core: Arc<ComponentCore> }

#[async_trait]
impl SttProvider for MyProvider {
    /// Called for single-shot (non-streaming) transcription of a complete audio frame.
    async fn transcribe(&self, audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        // Capture the wall-clock time before inference so we can report latency.
        let t0 = Instant::now();

        // Hand off the PCM samples to your model. Replace this with your real inference call.
        let text = run_inference(audio)?;

        // Report a successful call — the framework updates p50/p90/p99 latency and
        // resets the consecutive-error counter used by the auto-degrade loop.
        self.core.record_ok(t0.elapsed());

        // `new` leaves every optional field (confidence, words, ...) at its
        // default; `with_*` sets the ones you have. It keeps compiling when
        // a field is added in a minor SDK release.
        Ok(TranscriptionResult::new(text, true))
    }

    /// Called once per session for streaming (chunk-by-chunk) transcription.
    /// `rx` delivers audio chunks as they arrive; `tx` is where you send partial/final results.
    /// `cancel` is signalled when the caller wants to abort (hangup, session teardown).
    ///
    /// A chunk with `is_last = true` closes one utterance; the stream keeps
    /// serving the next one. Return only on cancel, when `rx` closes, when
    /// `tx` has no receiver, or on an unrecoverable error.
    async fn transcribe_stream(
        &self,
        req: RecognitionRequest,
        mut rx: Receiver<AudioChunk>,       // mpsc receiver — audio chunks arrive here
        tx: Sender<TranscriptionResult>,    // mpsc sender — push transcription results here
        cancel: CancellationToken,          // signalled on hangup / teardown
    ) -> Result<(), SttError> {
        let mut buf = Vec::new();
        let mut segment_index = 0u32;       // one per utterance

        loop {
            let chunk = tokio::select! {
                // Give cancellation priority so we don't process stale audio after a hangup.
                _ = cancel.cancelled() => return Ok(()),
                chunk = rx.recv() => match chunk {
                    Some(c) => c,
                    None => return Ok(()),      // the session ended
                }
            };
            buf.extend(chunk.samples);
            if !chunk.is_last { continue }

            // Utterance boundary: run inference on what was collected, emit the
            // final for this utterance, and start collecting the next one.
            let frame = AudioFrame {
                samples: std::mem::take(&mut buf),
                sample_rate: req.sample_rate,
                channels: req.channels,
            };
            let mut result = self.transcribe(&frame).await?;
            result.is_final = true;
            result.segment_index = segment_index;
            if tx.send(result).await.is_err() { return Ok(()) } // receiver gone
            segment_index += 1;
        }
    }

    /// The sample rate (Hz) your model was trained on. The pipeline will resample incoming
    /// audio to this rate before calling `transcribe` or `transcribe_stream`.
    fn expected_sample_rate(&self) -> u32 { 16_000 }

    /// Unique identifier used in TOML config: `[stt] provider = "my-model"`.
    /// Must match the string returned by `SttFactory::name()`.
    fn name(&self) -> &str { "my-model" }

    /// Advertise which optional capabilities your provider supports.
    /// The pipeline uses this to decide whether to call `transcribe_stream`
    /// or fall back to buffering the whole utterance first.
    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        [SttCapability::Streaming].into()
    }

    // Optional — opt into the health dashboard, auto-degrade loop, and retry system.
    // Without this, your provider is still fully functional; it just won't appear in
    // `/health` responses or be subject to automatic error-rate circuit-breaking.
    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(self.core.clone())))
    }
}

// The factory is a lightweight, stateless object whose sole job is to validate config
// and construct a fully-initialised provider. It is called once at startup (or on reload).
pub struct MyFactory;

impl SttFactory for MyFactory {
    /// Must return the same string as `SttProvider::name()` so the routing table can
    /// pair incoming config with the right factory.
    fn name(&self) -> &str { "my-model" }

    /// Validate the user's TOML config section before anything is built.
    /// Return a `ConfigError` for every problem found — the host will surface all of them
    /// at once rather than failing on the first one.
    fn validate(&self, _cfg: &SttConfig) -> Vec<voice_pipes_sdk::ConfigError> { vec![] }

    /// Construct and return a ready-to-use provider.
    /// Heavy initialisation (loading weights, warming up a runtime) should happen here
    /// so that the provider itself is always in a valid state.
    fn build(
        &self, _cfg: &SttConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, voice_pipes_sdk::BuildError> {
        Ok(Box::new(MyProvider {
            // Give the component a stable ID ("<kind>:<name>") and declare its kind
            // so the health dashboard can group and label it correctly.
            core: Arc::new(ComponentCore::new("stt:my-model", ComponentKind::Stt)),
        }))
    }
}

// Stand-in for your model: replace with the real inference call.
fn run_inference(_audio: &AudioFrame) -> Result<String, SttError> {
    Ok(String::new())
}
```

Registration lives in the binary that links the engine (`voice-pipes`), not in
the plugin, so the plugin depends on the SDK alone:

```rust,ignore
// The binary's main.rs. Register every external provider before serving.
// Order does not matter; only providers named in the active TOML config are
// built. A provider name that is already taken is an error, not an override.
voice_pipes::stt::register_stt_factory(|| Box::new(my_model::MyFactory))?;

let cfg = voice_pipes::config::merge(
    voice_pipes::config::load(Some("pipeline.toml".as_ref()))?,
    voice_pipes::config::CliOverrides::default(),
);
voice_pipes::commands::run_serve(cfg, Some("worker.toml".into())).await?;
```

`[stt] provider = "my-model"` in any TOML config now routes to your factory.

---

## What the SDK covers

| Extension point | Traits to implement | Registration function |
|---|---|---|
| Speech-to-text | `SttProvider` + `SttFactory` | `register_stt_factory()` |
| Text-to-speech | `TtsProvider` + `TtsFactory` | `register_tts_factory()` |
| LLM / text processor | `TextProcessor` + `TextProcessorFactory` | `register_text_processor_factory()` |

All three follow the same **provider + factory + register** pattern.

---

## Component health (optional but recommended)

Override `as_component()` on your provider to appear in the health dashboard,
the auto-degrade loop, and the error-retry system:

```rust,ignore
use voice_pipes_sdk::component::{ComponentCore, ComponentKind, CoreComponent, ErrorPolicy};

fn as_component(&self) -> Option<Arc<dyn voice_pipes_sdk::Component>> {
    Some(Arc::new(
        CoreComponent::new(self.core.clone())
            // Automatically retry up to 3 times with exponential back-off before
            // marking the component as degraded. Other policies: `Propagate` (surface
            // the error immediately — the default) and `LogAndContinue` (log and swallow).
            .with_error_policy(ErrorPolicy::RetryWithBackoff { max: 3 }),
    ))
}
```

Call `self.core.record_ok(elapsed)` / `self.core.record_error(&err)` in your hot path
and the framework tracks p50/p90/p99 latency, error rate, and health state automatically.

---

## Dependency footprint

| Crate | Why |
|---|---|
| `serde` + `serde_json` | Config serialisation and telemetry extras |
| `tokio` | Async traits |
| `tokio-util` | `CancellationToken` |
| `async-trait` | Object-safe async traits |
| `parking_lot` | Lock-free-adjacent counters in `ComponentCore` |
| `anyhow` / `thiserror` | Error plumbing |

No `ort`, no `candle`, no `tonic`, no `axum`. Add your inference runtime yourself.

---

## Stability guarantees

| Change | Minor version? |
|---|---|
| New field on a `#[non_exhaustive]` struct | ✅ Build it with `new` / `with_*` |
| New `Option` field on a plain data struct | ✅ Use `..Default::default()` |
| New `#[non_exhaustive]` enum variant | ✅ Add `_ => {}` arm |
| New trait method with a default impl | ✅ Existing impls unaffected |
| Signature change / removal | ❌ Requires major version bump |

Until 1.0, a minor bump may also add a required field to a public struct:
0.2.0 added `ParamConstraints::max_len` (string parameters are bounded by
`ParamDescriptor::validate`; `mutable_str` defaults to 4096 bytes) and
`ParamError::TooLong`. Literals need `max_len: None` to keep the old,
unbounded behaviour.

---

## Exported types (root re-exports)

| Types |
|---|
| `AudioFrame`, `TraceEvent` |
| `Component`, `ComponentCore`, `ComponentKind`, `CoreComponent`, `ErrorPolicy`, `HealthState`, `HealthStatus`, `LatencyStats` |
| `ConfigError`, `SttConfig`, `TtsConfig`, `TextProcessorConfig`, `TextChunkingConfig` |
| `ProviderSchema`, `ParamDescriptor`, `ParamConstraints`, `ParamValueType`, `ParamValue`, `OnlyWhen`, `ParamError` |
| `SttProvider`, `SttFactory`, `SttError`, `SttCapability`, `RecognitionRequest`, `AudioChunk`, `AudioEncoding`, `TranscriptionResult`, `WordInfo`, `InferenceMetrics` |
| `TtsProvider`, `TtsFactory`, `TtsError`, `SynthesisHints`, `WordBoundary`, `TtsSession`, `TtsSessionConfig` |
| `TextProcessor`, `TextProcessorFactory`, `TextProcessorError`, `TextFragment` |

All types are re-exported at the crate root — `use voice_pipes_sdk::SttFactory` without specifying the sub-module.
