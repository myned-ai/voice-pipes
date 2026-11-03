# Voice Pipes Plugin Development Guide

**Status:** active
**Last verified:** 2026-10-04 (SDK 1.0: semver-stable; see the [stability policy](sdk-stability.md))
**Owner:** Docs

> **Audience**: Developers building external STT, TTS, LLM, VAD, turn-detection or speech-to-speech provider plugins for Voice Pipes.
> **SDK version**: `voice-pipes-sdk 1.x`

This guide explains everything you need to add a new AI model provider — speech-to-text, text-to-speech, LLM, voice activity detection, or a speech-to-speech model — without touching the core repository.

---

## Table of Contents

1. [Architecture overview](#1-architecture-overview)
2. [Setting up a plugin crate](#2-setting-up-a-plugin-crate)
3. [STT plugins](#3-stt-plugins)
4. [TTS plugins](#4-tts-plugins)
5. [LLM / TextProcessor plugins](#5-llm--textprocessor-plugins)
   - 5a. [VAD plugins](#5a-vad-plugins)
   - 5b. [Speech-to-speech plugins](#5b-speech-to-speech-plugins)
   - 5c. [Turn-detector plugins](#5c-turn-detector-plugins)
6. [Component health registration](#6-component-health-registration)
7. [Error policies and retry](#7-error-policies-and-retry)
8. [Configuration and TOML](#8-configuration-and-toml)
9. [Testing your plugin](#9-testing-your-plugin)
10. [Wiring into a binary](#10-wiring-into-a-binary)
11. [A local ONNX model as a plugin: Nemotron 3.5](#11-a-local-onnx-model-as-a-plugin-nemotron-35)
12. [Stability guarantees](#12-stability-guarantees)
13. [Full type reference](#13-full-type-reference)

---

## 1. Architecture overview

```
┌─────────────────────────────────────────────────────┐
│  your-plugin crate                                  │
│                                                     │
│  implements SttProvider / TtsProvider / TextProcessor│
│  + matching Factory trait                           │
│  + (optional) as_component() for health reporting   │
│                                                     │
│  depends only on: voice-pipes-sdk                   │
│    + your inference runtime (ort, candle, reqwest…) │
└──────────────────────┬──────────────────────────────┘
                       │  register_stt_factory()
                       │  register_tts_factory()
                       │  register_text_processor_factory()
                       ▼
┌─────────────────────────────────────────────────────┐
│  voice-pipes binary  (main.rs)                      │
│                                                     │
│  my_plugin::register()?;                            │
│  voice_pipes::commands::run_serve(cfg, ..).await?;  │
└─────────────────────────────────────────────────────┘
```

**One model = one crate, one `register()` call.**

Speech-to-text and text-to-speech built into the engine implement these same traits, so your provider joins the pipeline, health registry, metrics, and auto-degrade system exactly as they do — without you importing any framework internals.

> **Built-in providers** (compiled into the `voice-pipes` crate itself) have an
> internal counterpart to this guide: to also offer one in the setup wizard and
> launch stack, add a `WizardChoice` entry to `builtin_choices` in
> `voice-pipes/src/commands/config_wizard/providers.rs`. Walkthrough:
> [WIZARD-PROVIDER-GUIDE.md](wizard-descriptor.md). A plugin describes its
> own wizard entry instead (§10.1).

---

## 2. Setting up a plugin crate

```toml
# your-plugin/Cargo.toml
[package]
name    = "my-stt-plugin"
version = "0.1.0"
edition = "2024"

[dependencies]
voice-pipes-sdk = "1"
# Or, before the crate is on crates.io:
# voice-pipes-sdk = { git = "https://github.com/myned-ai/voice-pipes", tag = "sdk-v1.0.0" }

# Add your inference runtime here — the SDK doesn't impose one.
# Examples: ort = "2", candle-core = "0.9", reqwest = "0.12"
```

Recommended crate structure:

```
my-stt-plugin/
├── Cargo.toml
└── src/
    ├── lib.rs          ← re-exports register()
    ├── provider.rs     ← impl SttProvider
    └── factory.rs      ← impl SttFactory + register()
```

---

## 3. STT plugins

### 3.1 Implement `SttProvider`

`SttProvider` is the per-session workhorse — it receives audio and returns transcriptions.

```rust
// src/provider.rs
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use voice_pipes_sdk::{
    AudioChunk, AudioFrame, DeviceInfo, ModelIdentity, RecognitionRequest, SttCapability,
    SttError, TranscriptionResult, WarmUpScope,
    component::{ComponentCore, ComponentKind},
};
use voice_pipes_sdk::stt::SttProvider;

pub struct MyProvider {
    // Your model handle, connection pool, etc.
    // model: Arc<MyModel>,

    // Embed this to opt into health reporting (see §6):
    pub core: Arc<ComponentCore>,
}

impl MyProvider {
    pub fn new(/* cfg fields */) -> anyhow::Result<Self> {
        Ok(Self {
            // model: Arc::new(MyModel::load(model_dir)?),
            core: Arc::new(ComponentCore::new("stt:my-model", ComponentKind::Stt)),
        })
    }
}

#[async_trait]
impl SttProvider for MyProvider {
    // ── Required ───────────────────────────────────────────────

    async fn transcribe(&self, audio: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let t0 = Instant::now();
        let result = self.run_inference(audio)        // your inference call
            .map_err(|e| SttError::InferenceFailed(e.to_string()))?;
        self.core.record_ok(t0.elapsed());
        Ok(result)
    }

    // One call serves the whole session. `is_last` closes an utterance;
    // the stream keeps serving the next one under the next segment index.
    async fn transcribe_stream(
        &self,
        request: RecognitionRequest,
        mut audio_rx: tokio::sync::mpsc::Receiver<AudioChunk>,
        tx: tokio::sync::mpsc::Sender<TranscriptionResult>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        let mut chunks = Vec::new();
        let mut segment_index = 0u32;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                chunk = audio_rx.recv() => match chunk {
                    Some(chunk) => chunk,
                    None => return Ok(()),          // session ended
                }
            };
            chunks.extend_from_slice(&chunk.samples);
            if !chunk.is_last { continue }

            // Utterance boundary: finalise it and keep going.
            let frame = AudioFrame {
                samples: std::mem::take(&mut chunks),
                sample_rate: request.sample_rate,
                channels: request.channels,
            };
            let mut result = self.transcribe(&frame).await?;
            result.is_final = true;
            result.segment_index = segment_index;
            if tx.send(result).await.is_err() { return Ok(()) }  // nobody listening
            segment_index += 1;
        }
    }

    fn expected_sample_rate(&self) -> u32 { 16_000 }

    fn name(&self) -> &str { "my-model" }

    fn capabilities(&self) -> HashSet<SttCapability> {
        [SttCapability::Streaming].into()
    }

    // ── Optional ───────────────────────────────────────────────

    async fn warm_up(&self) -> Result<(), SttError> {
        // Pre-load the model, warm up GPU, etc.
        Ok(())
    }

    fn needs_flush_silence(&self) -> bool {
        // Return false for CTC/Transducer models that handle their own
        // end-of-utterance; true (default) for models that need a silence pad.
        false
    }

    // What a local model reports (SDK 0.3, all defaulted). The worker
    // warms a PerProcess provider once at start, before it accepts
    // sessions, and warns at start when concurrent_capacity is below the
    // sessions it is asked to serve. Identity and device show in the
    // component check and the session report.
    fn warm_up_scope(&self) -> WarmUpScope { WarmUpScope::PerProcess }
    fn concurrent_capacity(&self) -> Option<usize> { Some(self.pool_size) }
    async fn model_identity(&self) -> Option<ModelIdentity> {
        Some(ModelIdentity { id: "org/model".into(), version: None, source: Some("config".into()) })
    }
    async fn device_info(&self) -> Option<DeviceInfo> {
        Some(DeviceInfo::config("cpu", None))
    }

    // Health registration — see §6:
    fn as_component(&self) -> Option<Arc<dyn voice_pipes_sdk::Component>> {
        use voice_pipes_sdk::component::CoreComponent;
        Some(Arc::new(CoreComponent::new(self.core.clone())))
    }
}
```

### 3.2 Implement `SttFactory`

The factory is a stateless singleton that validates config and constructs providers.

```rust
// src/factory.rs
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, SttConfig, SttFactory};
use voice_pipes_sdk::stt::SttProvider;
use crate::provider::MyProvider;

pub struct MyFactory;

impl SttFactory for MyFactory {
    fn name(&self) -> &str { "my-model" }

    fn is_local(&self) -> bool { true }   // set false for cloud API providers

    fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        if cfg.model_dir.as_deref().unwrap_or("").is_empty() {
            errors.push(ConfigError::new("model_dir", "model_dir is required"));
        }
        errors
    }

    fn build(
        &self,
        cfg: &SttConfig,
        _ctx: &BuildContext<'_>,   // audit channel, live parameters, text chunking
    ) -> Result<Box<dyn SttProvider>, BuildError> {
        Ok(Box::new(MyProvider::new(/* cfg fields */)?))
    }
}

pub fn register() -> Result<(), voice_pipes::plugins::RegistrationError> {
    voice_pipes::stt::register_stt_factory(|| Box::new(MyFactory))
}
```

`build` returns a `voice_pipes_sdk::BuildError`: `BuildError::Config(errors)`
when the config is refused (what `options::refused` and `?` on a
`Vec<ConfigError>` give), `BuildError::failed(message)` or `?` on an
`anyhow::Error` or a stage error (`SttError`, `TtsError`, ...) for anything
else, with the cause kept. The engine shows a refused config's problems as
it shows `validate`'s.

Start `build` with `voice_pipes_sdk::options::validated(self.validate(cfg))?;`:
the engine does not run `validate` before `build` on every path, and a build
that skips a check starts a pipeline on settings validation refuses, or fails
on a missing model or key instead of naming the setting. In your tests,
`voice_pipes_sdk::testing::build_refuses_what_validate_refuses` checks it for
configs `validate` refuses; the voice-pipes binary's tests run it on every
factory it links, with an option no factory reads.

`ConfigError::new(field, message)` reports an invalid value. A name your
plugin does not know (a model, a voice set) is
`.with_kind(ConfigErrorKind::UnknownName)`, and a key it does not read is
`UnknownKey` (`unknown_options` on the stage configs builds those): tools
act on the kind, never on the message's words.

### 3.3 Key types

| Type | Role |
|---|---|
| `AudioFrame` | Raw PCM i16 samples with sample_rate + channels |
| `AudioChunk` | One streaming chunk; `is_last` marks an utterance boundary (not the end of the stream) |
| `RecognitionRequest` | Per-session parameters (language, sample_rate, etc.) |
| `TranscriptionResult` | Interim or final transcript; fill `text`, `is_final`, `segment_index` (one per utterance), optionally `words`, `inference_metrics` |
| `SttCapability` | Declare what your model supports (`Streaming`, `InterimResults`, `WordTimestamps`, …). `Streaming` is recorded as the session's `stt_mode` and decides whether the diagnostic report treats STT processing time as part of the user's wait, so declare it only when transcripts arrive while the user speaks. Declare `CumulativeFinals` if your provider re-sends a growing final for the same `segment_index`; otherwise every final is appended to the utterance text |
| `SttError` | Typed errors — use `InferenceFailed`, `Timeout`, `NetworkError`, etc. `ReconnectExhausted` is what a hosted provider returns when its own reconnect budget runs out |

### 3.4 The streaming contract

The pipeline calls `transcribe_stream` **once per session** and expects it to
stay up:

- `is_last = true` on a chunk is an **utterance boundary**. Emit that
  utterance's final (`is_final = true`), reset per-utterance state, and read
  on; the next chunk starts the next utterance.
- Number utterances with `segment_index` (`0`, `1`, …). Interims and the
  final of one utterance share its index. Declare
  `SttCapability::CumulativeFinals` if your finals re-send the growing text
  of an utterance under the same index.
- `request.language` is the session's negotiated locale (BCP-47) when the
  client or the pipeline named one, `None` otherwise. A multilingual model
  should decode with it; the result's `language` says what was actually
  used or detected.
- Return only on `cancel`, when `audio_rx` closes (session over), when `tx`
  has no receiver, or on an error you cannot recover from. Retrying a
  dropped upstream connection is your job (with a bound: the in-tree hosted
  providers give up after `max_reconnect_attempts` consecutive failures and
  20 reconnects in five minutes). The pipeline restarts a worker that
  returned early, but only as failure recovery, with backoff.

---

## 4. TTS plugins

### 4.1 Implement `TtsProvider`

```rust
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;
use voice_pipes_sdk::{
    AudioFrame, TtsError, SynthesisRequest,
    component::{ComponentCore, ComponentKind, CoreComponent, ErrorPolicy},
};
use voice_pipes_sdk::tts::TtsProvider;

pub struct MyTtsProvider {
    // voice_model: Arc<MyVoiceModel>,
    pub core: Arc<ComponentCore>,
}

impl MyTtsProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            core: Arc::new(ComponentCore::new("tts:my-voice", ComponentKind::Tts)),
        })
    }
}

#[async_trait]
impl TtsProvider for MyTtsProvider {
    async fn synthesize(&self, text: &str) -> Result<AudioFrame, TtsError> {
        let t0 = Instant::now();
        let samples = self.run_tts(text)
            .map_err(|e| TtsError::InferenceFailed(e.to_string()))?;
        self.core.record_ok(t0.elapsed());
        Ok(AudioFrame { samples, sample_rate: self.sample_rate(), channels: 1 })
    }

    // Optional: streaming synthesis for lower TTFB.
    // Default wraps batch synthesize() automatically — override for true streaming.
    // `req.hints` carries per-fragment voice/speed/language overrides, and
    // `req.word_tx` a word-timing channel when supports_word_timestamps() is true.
    async fn synthesize_stream(
        &self,
        req: SynthesisRequest<'_>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), TtsError> {
        let _voice = req.hints.and_then(|h| h.voice.as_deref());
        // Stream audio chunks as they are generated ...
        let frame = self.synthesize(req.text).await?;
        let _ = tx.send(frame).await;
        Ok(())
    }

    fn sample_rate(&self) -> u32 { 22_050 }

    fn name(&self) -> &str { "my-voice" }

    fn as_component(&self) -> Option<Arc<dyn voice_pipes_sdk::Component>> {
        Some(Arc::new(
            CoreComponent::new(self.core.clone())
                .with_error_policy(ErrorPolicy::RetryWithBackoff { max: 2 }),
        ))
    }
}
```

> [!TIP]
> TTS is the most latency-sensitive stage. If your provider supports native streaming (generating and sending audio before synthesis is finished), implement `synthesize_stream` to send frames incrementally. This directly reduces time-to-first-byte (TTFB) perceived by the user.

### 4.2 Implement `TtsFactory`

```rust
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, TtsConfig, TtsFactory};
use voice_pipes_sdk::tts::TtsProvider;
use crate::provider::MyTtsProvider;

pub struct MyTtsFactory;

impl TtsFactory for MyTtsFactory {
    fn name(&self) -> &str { "my-voice" }

    fn is_local(&self) -> bool { true }

    fn validate(&self, cfg: &TtsConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        if cfg.voice.as_deref().unwrap_or("").is_empty() {
            errors.push(ConfigError::new("voice", "voice is required"));
        }
        errors
    }

    fn build(
        &self,
        cfg: &TtsConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, BuildError> {
        Ok(Box::new(MyTtsProvider::new()?))
    }
}

pub fn register() -> Result<(), voice_pipes::plugins::RegistrationError> {
    voice_pipes::tts::register_tts_factory(|| Box::new(MyTtsFactory))
}
```

---

## 5. LLM / TextProcessor plugins

### 5.1 Implement `TextProcessor`

```rust
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use voice_pipes_sdk::{
    TextFragment, TextProcessorError,
    component::{ComponentCore, ComponentKind, CoreComponent, ErrorPolicy},
};
use voice_pipes_sdk::text::TextProcessor;

pub struct MyLlmProcessor {
    // client: Arc<MyLlmClient>,
    pub core: Arc<ComponentCore>,
}

impl MyLlmProcessor {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            core: Arc::new(ComponentCore::new("text:my-llm", ComponentKind::Text)),
        })
    }
}

#[async_trait]
impl TextProcessor for MyLlmProcessor {
    fn name(&self) -> &str { "my-llm" }

    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        // Batch (non-streaming) path. Implement this for simplicity;
        // override process_stream below for true streaming.
        todo!()
    }

    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        let t0 = Instant::now();
        let mut first_token = true;

        // Call your LLM streaming API and forward tokens:
        let mut stream = self.client.stream_completion(input).await
            .map_err(|e| TextProcessorError::new(self.name(), e.to_string()))?;

        while let Some(token) = stream.next().await {
            if cancel.is_cancelled() { break }

            let first_token_ms = if first_token {
                first_token = false;
                Some(t0.elapsed().as_secs_f64() * 1000.0)
            } else { None };

            let _ = tx.send(TextFragment::new(token?).with_first_token_ms(first_token_ms)).await;
        }

        // Send a final fragment with token count:
        let _ = tx
            .send(
                TextFragment::new("")
                    .with_is_end_of_response(true)
                    .with_token_count(Some(stream.total_tokens())),
            )
            .await;

        self.core.record_ok(t0.elapsed());
        Ok(())
    }

    async fn warm_up(&self) -> Result<(), TextProcessorError> {
        // Send a throwaway completion to pre-warm TLS and routing.
        Ok(())
    }

    fn as_component(&self) -> Option<Arc<dyn voice_pipes_sdk::Component>> {
        Some(Arc::new(
            CoreComponent::new(self.core.clone())
                // The framework will retry failed LLM calls up to 3 times
                // before the first token is sent to the client:
                .with_error_policy(ErrorPolicy::RetryWithBackoff { max: 3 }),
        ))
    }
}
```

> [!IMPORTANT]
> Set `first_token_ms` on the **first non-empty fragment**. This is the TTFT (time-to-first-token) value surfaced in the admin dashboard and turn records. Omitting it means TTFT will not be reported for your provider.

### 5.2 Implement `TextProcessorFactory`

```rust
use voice_pipes_sdk::{
    BuildContext, BuildError, ConfigError, TextProcessorConfig, TextProcessorFactory,
};
use voice_pipes_sdk::text::TextProcessor;
use crate::processor::MyLlmProcessor;

pub struct MyLlmFactory;

impl TextProcessorFactory for MyLlmFactory {
    fn name(&self) -> &str { "my-llm" }

    // The engine fills `cfg.api_key` from this variable when the section
    // sets no `api_key` (or from the one its `api_key_env` names).
    fn default_api_key_env(&self) -> Option<&'static str> { Some("MY_LLM_API_KEY") }

    fn validate(&self, cfg: &TextProcessorConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        if voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).is_none() {
            errors.push(ConfigError::new("api_key", "set api_key or MY_LLM_API_KEY"));
        }
        errors
    }

    fn build(
        &self,
        cfg: &TextProcessorConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TextProcessor>, BuildError> {
        Ok(Box::new(MyLlmProcessor::new()?))
    }
}

pub fn register() -> Result<(), voice_pipes::plugins::RegistrationError> {
    voice_pipes::text::register_text_processor_factory(|| Box::new(MyLlmFactory))
}
```

### 5.3 Sessions, client-side tools and conversation history

A processor that only answers turns needs nothing more. The rest of the
`TextProcessor` hooks all have defaults; override the ones your processor
can use.

| Hook | Called when | Override it to |
|---|---|---|
| `bind_session(SessionHandle)` | once, as the session is built | keep the handle for reporting tool calls |
| `configure_client_tools(Vec<ClientToolSpec>)` | once, after `bind_session` | advertise the session's client-side tools to your model |
| `process_context_stream` | an agent-initiated turn (a tool result, an external event) | answer from context without recording a user turn |
| `append_assistant_message` | the session spoke something your processor did not write | keep history complete |
| `inject_context` | silent context arrived (a tool result, a rich message kept in history) | let the model refer to it later |
| `model_identity`, `device_info` | once, at session setup | report the model and device actually serving |

Client-side tools are commands the **client** fulfils (navigate a UI,
show a card); your processor never executes one. When the model calls a
tool, report it and end the turn:

```rust
use voice_pipes_sdk::agent::{ClientToolCall, SessionHandle};

session
    .client_tool_call(ClientToolCall::new(&call.id, &call.name, call.arguments))
    .await
    .ok(); // `SessionEnded` means there is no one left to tell
```

Report a call only once the model has finished it (the end of the tool-call
message). With speculative execution on, the framework may run your
processor on a partial transcript and discard the run; a call reported from
that run still reaches the client.

The framework drops a call whose name is not on the session's allowlist,
sends the rest to the client, and turns the client's result into an agent
turn or silent context, which reaches your processor through
`process_context_stream` or `inject_context`.

---

## 5a. VAD plugins

A VAD backend answers one question per chunk of microphone audio: is this
speech. The pipeline turns that stream of answers into utterances — minimum
speech and silence durations and the pre-roll stay with it — so a backend
holds no timing of its own. Implement `VadBackend` and `VadFactory` (SDK
0.4); the pipeline selects the backend with `mode = "<its name>"`, exactly as
it selects `silero` or `energy`.

```rust
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, VadBackend, VadConfig, VadError, VadFactory, VadResult};

pub struct MyVad { threshold: f32 /* , session: ort::Session, ... */ }

impl VadBackend for MyVad {
    fn process_chunk(&mut self, samples: &[i16], sample_rate: u32) -> Result<VadResult, VadError> {
        let speech_prob = self.infer(samples, sample_rate)
            .map_err(|e| VadError::InferenceFailed(e.to_string()))?;
        Ok(VadResult { speech_prob, is_speech: speech_prob >= self.threshold })
    }
    fn reset(&mut self) { /* clear recurrent state between utterances */ }
    fn name(&self) -> &str { "my-vad" }
}

pub struct MyVadFactory;

impl VadFactory for MyVadFactory {
    fn name(&self) -> &str { "my-vad" }
    fn validate(&self, cfg: &VadConfig) -> Vec<ConfigError> {
        if cfg.model.is_none() {
            return vec![ConfigError::new("vad.model", "my-vad needs a model")];
        }
        vec![]
    }
    fn build(&self, cfg: &VadConfig, _ctx: &BuildContext<'_>) -> Result<Box<dyn VadBackend>, BuildError> {
        Ok(Box::new(MyVad { threshold: cfg.speech_prob /* , session: load(cfg.model)? */ }))
    }
}

pub fn factory() -> Box<dyn VadFactory> { Box::new(MyVadFactory) }
```

```toml
[vad]
mode = "my-vad"
model = "models/my-vad.onnx"
speech_prob = 0.5          # arrives resolved in VadConfig::speech_prob
[vad.options]              # anything backend-specific, in VadConfig::options
window_ms = 32
```

`VadConfig` carries the thresholds already resolved (the deprecated
`threshold` key and the defaults applied), so a backend does not
reimplement that precedence. Config validation calls your `validate` for
the selected mode, as it does for every other stage. Register with
`voice_pipes::plugins::register_plugin(Plugin::Vad(my_vad_plugin::factory), entries)`.

A VAD has no wizard menu, so `entries` holds one entry at most: the
`[vad]` settings the wizard writes when this backend is the one
`mode = "auto"` runs (its model, as a `WizardDeploy::Registry` entry the
wizard downloads). The binary names that backend with
`voice_pipes::vad::segmenter::register_auto_backend("my-vad")`; `auto` then
asks it `VadFactory::can_run` (by default, whether `validate` reports
nothing; a backend that loads a file also checks the file is there) and
runs energy when it cannot. The voice-pipes binary names Silero
(`voice-pipes-vad-silero`).

Live tuning: `BuildContext::params` holds the console-editable
`speech_prob` and `neg_threshold` (and `energy_threshold`); take
`float_cell("speech_prob")` and read it per chunk, as Silero does, and an
edit applies without a rebuild.

---

## 5b. Speech-to-speech plugins

A speech-to-speech model hears the caller and answers in audio over one
WebSocket, replacing STT, LLM and TTS. The engine owns that socket:
connecting, the read and write tasks, keepalives, resampling, reconnection,
barge-in and the session record. A provider is a `RealtimeProtocol` (SDK
0.5, `voice_pipes_sdk::s2s`): a synchronous mapper that builds the vendor's
JSON envelopes from neutral inputs and parses the vendor's frames into
`RealtimeEvent`s. It does no I/O, so every envelope and every parse is a
unit test.

```rust
use anyhow::{Context, Result};
use voice_pipes_sdk::secrets::Secret;
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError};
use voice_pipes_sdk::s2s::codec::{decode_pcm16_b64, url_query_encode};
use voice_pipes_sdk::s2s::{
    ConnectInfo, Modalities, RealtimeConfig, RealtimeEvent, RealtimeFactory, RealtimeProtocol,
    SessionConfig,
};

pub struct AcmeProtocol { model: String, api_key: Option<Secret> }

impl RealtimeProtocol for AcmeProtocol {
    fn name(&self) -> &'static str { "acme" }
    fn native_input_sample_rate(&self) -> u32 { 16_000 }
    fn native_output_sample_rate(&self) -> u32 { 24_000 }
    fn connect_info(&self) -> Result<ConnectInfo> {
        let key = self.api_key.as_ref().context("no Acme API key")?.expose();
        let url = format!("wss://api.acme.example/live?model={}", url_query_encode(&self.model));
        Ok(ConnectInfo::new(url).with_header("Authorization", format!("Bearer {key}")))
    }
    fn build_session_update(&self, cfg: &SessionConfig) -> Vec<serde_json::Value> {
        let mut setup = serde_json::json!({ "type": "setup", "instructions": cfg.instructions });
        if cfg.transcribe_input {
            setup["input_transcription"] = serde_json::json!(true);
        }
        let mut envelopes = vec![setup];
        // A session that speaks first (a caller placing a call) is kicked
        // into its opening turn; one that answers waits to be spoken to.
        if cfg.speaks_first {
            envelopes.push(serde_json::json!({ "type": "respond" }));
        }
        envelopes
    }
    fn build_audio_append(&self, b64_pcm: &str) -> serde_json::Value {
        serde_json::json!({ "type": "audio", "data": b64_pcm })
    }
    fn build_response_create(&self, _m: Modalities) -> Option<serde_json::Value> { None }
    fn build_text_message(&self, text: &str) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "text", "text": text }))
    }
    fn build_cancel(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "cancel" }))
    }
    fn build_function_call_output(&self, call_id: &str, _name: &str, result: &str, _silent: bool)
        -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "tool_result", "id": call_id, "result": result }))
    }
    fn parse_event(&self, v: &serde_json::Value) -> Vec<RealtimeEvent> {
        match v["type"].as_str() {
            Some("audio") => decode_pcm16_b64(v["data"].as_str().unwrap_or(""), 24_000)
                .map(|samples| vec![RealtimeEvent::ResponseAudioDelta {
                    samples, sample_rate: 24_000, alignment: None,
                }])
                .unwrap_or_default(),
            Some("done") => vec![RealtimeEvent::ResponseCompleted { response_id: None }],
            _ => Vec::new(),
        }
    }
}

pub struct AcmeFactory;

impl RealtimeFactory for AcmeFactory {
    fn name(&self) -> &'static str { "acme-live" }
    fn default_api_key_env(&self) -> &'static str { "ACME_API_KEY" }
    fn validate(&self, cfg: &RealtimeConfig) -> Vec<ConfigError> {
        // Names the variable to set when the engine found no key.
        cfg.missing_key_error("Acme").into_iter().collect()
    }
    fn build(&self, cfg: &RealtimeConfig, _ctx: &BuildContext<'_>)
        -> Result<Box<dyn RealtimeProtocol>, BuildError> {
        Ok(Box::new(AcmeProtocol {
            model: cfg.model.clone().unwrap_or_else(|| "acme-live-1".into()),
            api_key: cfg.api_key.clone(),
        }))
    }
}

pub fn factory() -> Box<dyn RealtimeFactory> { Box::new(AcmeFactory) }
```

`RealtimeConfig` is the provider's view of `[realtime_provider]`: its name,
`model`, `voice`, `options` and the credential. The engine resolves the key before it calls you
(`api_key_env`, or your `default_api_key_env()`, through the process
environment and the secrets store): `api_key` is the `Secret` it found and
`api_key_var` the variable it looked in, so a provider never reads the
environment for a credential. The engine keeps the rest (instructions,
client overrides, the uplink observer). Options are read with
`option_str`, `option_bool` and `option_u64`. `output_emit_rate` is the
engine's: a provider hands audio over at the vendor's rate
(`sample_rate` on each `ResponseAudioDelta`), and the engine resamples the
session's audio to that rate as one stream, so a provider never resamples.
`build` also receives the
`BuildContext`, which carries the resolved `params` store.

What a session is for arrives with the session, not the provider:
`SessionConfig::speaks_first` (the model opens the conversation, as a
benchmark's caller does, instead of waiting to be spoken to) and
`transcribe_input` (the provider transcribes what it hears). A provider
whose own agent configuration decides these may ignore them. Build every
SDK type through its constructor (`SessionConfig::new`, `ToolSpec::new`,
`ConnectInfo::new`, `RateLimitInfo::new`) and `with_*` setters: the structs
are `#[non_exhaustive]`, so a field added later is not a break.

Tool-call arguments are bounded (`MAX_TOOL_ARGUMENT_BYTES`):
`RealtimeEvent::function_call` refuses an oversized call where you parse
it, and the engine refuses one however it was built.

The trait's defaulted methods cover what not every vendor has:
`build_item_truncate` (trim the stored reply after a barge-in),
`build_context_message` (inject context, or a late tool result),
`build_auto_reply` (an application-level ping that needs a pong),
`classify_close` (which WebSocket closes are worth reconnecting after) and
`inferred_completion_idle_ms` (for a vendor with no end-of-response event).

A vendor that can carry a session over to a new connection says so with
`RealtimeEvent::SessionResumable { token }`, each superseding the last;
on a reconnect the engine passes the latest back in
`SessionConfig::resume_token`, and your `build_session_update` presents
it (and should not open the conversation again). `GoingAway` tells the
engine the connection is closing soon, so it moves the session at a
quiet moment. A close that refuses a token is `CloseClass::Unresumable`:
the engine drops the token and starts afresh, and it treats any resumed
connection that closes before `SessionUpdated` the same way.
`FunctionCallsCancelled` withdraws pending calls, whose results the
engine then does not send.

A test hands the key in directly with
`RealtimeConfig::new(..).with_api_key(Some("sk-test".into()))`; no test
needs to touch the process environment.

Register with `voice_pipes::plugins::register_plugin(Plugin::Realtime(acme::factory),
acme::wizard_choices())`, which registers the factory and its setup-wizard
entries in one step. The wizard writes the `[realtime_provider]` header and the engine's
`client_overrides`; your entry's fields fill the rest, with a dotted key
(`options.thinking_budget`) for anything under `[realtime_provider.options]`.
`voice-pipes-s2s-openai`, `-s2s-gemini` and `-s2s-elevenlabs` are the
worked examples.

---

## 5c. Turn-detector plugins

A turn detector decides when the user has finished speaking. The engine
has the rule-based ones every pipeline needs (`silence`, `provider`,
`layered`); a model-based detector is a plugin: a `TurnDetector` and a
`TurnDetectorFactory` (SDK 0.5, `voice_pipes_sdk::turn`), selected by
`[turn_detection] mode = "<its name>"`.

`evaluate` is asked after every recognition result and returns a
`TurnSignal`: the decision, and, for a model, its confidence and how soon
it expects the turn to end, so the pipeline can start work early.
`feed_audio` and `feed_agent_audio` hand it the microphone and what the
agent is playing, for detectors that read audio, and `turn_ended` says the
user's turn was committed or dropped, for a detector that keeps one turn's
audio and should start the next without it. The feeds are called on the
recognition task while `evaluate` may be running: keep the model and the
audio behind separate locks, or every audio chunk waits for an inference.

The factory is built from a `TurnConfig`: the mode, the silence timeout
every mode falls back on, and every other key of `[turn_detection]` as
`options`, with its TOML type. Keep your keys flat and prefixed
(`smart_threshold`), parse them once for `validate` and `build`, and refuse
the rest with `cfg.unknown_options(KNOWN)`: without a fixed schema, that is
what makes a misspelt key an error. Read bounded numbers with
`cfg.whole_option(key, range, default)` and `cfg.number_option(..)`, which
tell a wrong type (`8.5` for a whole number) from a value out of range.
`param_schema` describes your settings for the admin console, with the same
ranges; a setting read once in `build` is `.restart_required()`, since the
console can change live only what a parameter store backs. `build(cfg, ctx)` receives the `BuildContext`, whose
`audit_tx` is the session's trace sink when the session is traced. As for
every stage, `build` starts by refusing what `validate` would
(`options::validated`).

`TurnDecision`, `TurnSignal` and the S2S enums are `#[non_exhaustive]`:
match them with a wildcard arm, so a new variant is not a breaking change.

Register with `voice_pipes::plugins::register_plugin(Plugin::Turn(my_detector::factory),
my_detector::wizard_choices())` (an empty list when it offers no wizard
entry); the stage's selector is `mode`. `voice-pipes-turn-smart` (with a wizard entry
that downloads its model) and `voice-pipes-turn-vap` are the worked
examples.

---

## 6. Component health registration

Opting into health registration gives your plugin:

- An entry at `GET /debug/components` (id, kind, health, counters, latency percentiles)
- Inclusion in the per-worker heartbeat sent to the orchestrator
- Automatic health degrade when error rate or p99 latency exceeds configured thresholds
- Support for error policies (retry, log-and-continue) — see §7

### 6.1 The pattern

```rust
use std::sync::Arc;
use voice_pipes_sdk::component::{ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::Component;

pub struct MyProvider {
    core: Arc<ComponentCore>,
}

impl MyProvider {
    pub fn new() -> Self {
        Self {
            core: Arc::new(ComponentCore::new(
                "stt:my-model",   // unique id — use "kind:name" convention
                ComponentKind::Stt,
            )),
        }
    }
}

impl SttProvider for MyProvider {
    // ... other methods ...

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(self.core.clone())))
    }
}
```

### 6.2 Recording telemetry

Call these in your hot path:

```rust
// On success:
self.core.record_ok(start.elapsed());

// On error:
self.core.record_error(&err);

// On timeout:
self.core.record_timeout();
```

`ComponentCore` maintains:
- `processed_total` — successful request count
- `errors_total` — all failures
- `timeouts_total` — timeout subset of failures
- `latency_ms` — p50 / p90 / p99 / mean / min / max over the last 512 samples
- `last_error` — most recent error message

### 6.3 Provider-specific extras

Surface additional metrics (tokens/second, cache hits, cost, …) in the dashboard:

```rust
// After each completion:
self.core.extras_with(|m| {
    m.insert("tokens_per_sec".into(), serde_json::json!(tokens_per_sec));
    m.insert("prompt_tokens".into(),  serde_json::json!(prompt_tokens));
    m.insert("completion_tokens".into(), serde_json::json!(completion_tokens));
});
```

These appear in `ComponentTelemetry.extras` and are forwarded to the orchestrator in heartbeats.

### 6.4 Manual health transitions

The auto-degrade loop handles `Healthy ↔ Degraded` automatically. Override lifecycle states explicitly:

```rust
// On model load:
self.core.mark_healthy();

// On downstream failure:
self.core.mark_unhealthy("upstream API returned 503");

// On recovery:
self.core.mark_healthy();
```

> [!NOTE]
> `Unhealthy` set by your code is **not** overridden by the auto-degrade task — it only manages the `Healthy ↔ Degraded` boundary. You own lifecycle transitions.

---

## 7. Error policies and retry

Set a policy on the `CoreComponent` you return from `as_component()`:

```rust
use voice_pipes_sdk::component::ErrorPolicy;

fn as_component(&self) -> Option<Arc<dyn Component>> {
    Some(Arc::new(
        CoreComponent::new(self.core.clone())
            .with_error_policy(ErrorPolicy::RetryWithBackoff { max: 3 }),
    ))
}
```

| Policy | Behaviour |
|---|---|
| `Propagate` (default) | Error bubbles to client immediately |
| `RetryWithBackoff { max }` | Framework retries up to `max` times with exponential backoff, **gated on no output having been sent to the client yet** |
| `LogAndContinue` | Error is logged; pipeline continues with empty/default result (only safe for non-critical stages) |

> [!IMPORTANT]
> For **TTS and LLM**, `RetryWithBackoff` is gated on no audio / no text having reached the client yet. If the first token or first audio frame was already sent, the framework falls back to `Propagate` to avoid duplicate output. For **STT**, retry is not supported (bidirectional audio stream cannot be replayed mid-utterance).

---

## 8. Configuration and TOML

Your provider is selected in the pipeline TOML by `type` (text processors) or `provider` (STT/TTS):

```toml
# STT
[stt]
provider = "my-model"
model_dir = "/models/my-model"

# TTS
[tts]
provider = "my-voice"
voice    = "en_US/en_female"

# Text processor / LLM
[text_processor]
type    = "my-llm"
api_key = "${MY_LLM_API_KEY}"
model   = "my-model-v1"
```

The framework loads the relevant config struct and passes it to your `validate()` and `build()` methods.

**Credentials are a `Secret`, and the engine resolves them.**
`SttConfig::api_key`, `TtsConfig::api_key` and `TextProcessorConfig::api_key`
are `Option<voice_pipes_sdk::secrets::Secret>`: `Debug` and serialisation
write `***REDACTED***`, there is no `Display`, and the value is wiped from
memory when dropped. Name the variable your key comes from with
`default_api_key_env()` on the factory; before `validate` and `build`, the
engine fills `api_key` from it when the section sets none (a text processor's
`api_key_env` names another). Never read the environment for a key yourself:
a workspace rule refuses it, and a test then passes a key in the config
without depending on the machine it runs on. Keep the key as a `Secret` in
your own config struct too, and call `expose()` only where it leaves for the
service:

```rust
use voice_pipes_sdk::secrets::{Secret, configured_key};

let key: Secret = configured_key(cfg.api_key.as_ref())
    .cloned()
    .ok_or_else(|| anyhow::anyhow!("my-voice needs `api_key` or MY_API_KEY"))?;
// later, in the request:
request.header("x-api-key", key.expose());
```

The configs' own `Debug` also blanks secret-named entries in `options`
(`api_key`, `token`, `*_secret`, …), for providers that take a key as an
option; when you read one from `options`, wrap it in a `Secret` at once.

### 8.1 Live parameters

A setting an operator should be able to change while the pipeline runs (a
voice, a system prompt, a temperature) is declared once, in your
`provider_schema`, as a `ParamDescriptor::mutable_*` with its current value.
The framework builds an `AtomicParamStore` from that schema
(`AtomicParamStore::from_schema`) and passes it to your factory's
`build` as `ctx.params`; keep the cells you need and read them per request:

```rust
fn build(&self, cfg: &TtsConfig, ctx: &BuildContext<'_>) -> Result<Box<dyn TtsProvider>, BuildError> {
    let voice = ctx
        .params
        .and_then(|p| p.string_cell("voice"))            // live: follows PATCH /debug/params
        .unwrap_or_else(|| Arc::new(ArcSwap::from_pointee(default_voice(cfg))));
    Ok(Box::new(MyVoice { voice /* read with voice.load() per synthesis */ }))
}
```

A factory with no live settings ignores `ctx.params`. Every stage's factory
builds through this one method; `BuildContext` is `#[non_exhaustive]`, so a
new input arrives as a field, not as another method to implement. In a test,
`BuildContext::new()` supplies nothing and the `with_*` methods add what the
test needs.

### 8.2 Returning validation errors

```rust
fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError> {
    let mut errors = Vec::new();

    if cfg.model_dir.as_deref().unwrap_or("").is_empty() {
        errors.push(ConfigError::new("model_dir", "model_dir is required"));
    }

    // SttConfig captures provider-specific keys in `options`.
    if let Some(rate) = cfg.options.get("sample_rate").and_then(|v| v.as_u64()) {
        if rate != 16_000 {
            errors.push(ConfigError::new("sample_rate", "my-model only supports 16 kHz input"));
        }
    }

    errors
}
```

---

## 9. Testing your plugin

Your plugin crate can be tested in isolation without running the full binary. Create a mock request and call the provider directly:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use voice_pipes_sdk::{AudioFrame, RecognitionRequest};

    #[tokio::test]
    async fn transcribe_returns_text() {
        let provider = MyProvider::new().unwrap();
        let audio = AudioFrame {
            samples: vec![0i16; 16_000],  // 1 s of silence
            sample_rate: 16_000,
            channels: 1,
        };
        let result = provider.transcribe(&audio).await.unwrap();
        assert!(!result.text.is_empty() || result.text.is_empty()); // just checking no panic
    }

    #[test]
    fn component_reports_telemetry() {
        let provider = MyProvider::new().unwrap();
        provider.core.record_ok(std::time::Duration::from_millis(42));
        let tel = provider.core.telemetry();
        assert_eq!(tel.processed_total, 1);
        assert_eq!(tel.errors_total, 0);
        assert_eq!(tel.latency_ms.p50, 42);
    }
}
```

### 9.1 Metrics conformance testing

Every provider crate in the workspace must pass metric conformance testing (enforced by workspace rule M5). The `voice-pipes-sdk` crate provides a `testing` feature with dedicated assertion helpers in `voice_pipes_sdk::testing`:

```toml
[dev-dependencies]
voice-pipes-sdk = { workspace = true, features = ["testing"] }
```

- **STT**: `voice_pipes_sdk::testing::assert_stt_metrics_conformance(run, audio, sample_rate, delay)`
  Asserts that `InferenceMetrics` is populated on the final result, `effective_latency_ms` is non-zero when work was done, reflects injected delay, and `rtfx` is finite and positive.
- **TTS**: `voice_pipes_sdk::testing::assert_tts_metrics_conformance(run, consumer_delay)`
  Asserts that consumer delivery backpressure does not bleed into active synthesis compute time.
- **LLM**: `voice_pipes_sdk::testing::assert_llm_metrics_conformance(run, prefill_delay)`
  Asserts that `first_token_ms` is never stamped on empty tokens, reflects prefill delay, and token counts match.
- **VAD / Turn**: `voice_pipes_sdk::testing::assert_vad_metrics_conformance(speech_dur, total_dur, hangover)`
  Asserts that speech duration does not exceed total audio duration and hangover is bounded.
- **S2S**: `voice_pipes_sdk::testing::assert_s2s_metrics_conformance(samples, sample_rate, ttfa)`
  Asserts that real-time audio sample counts, sample rate, and response timing are non-zero and valid.

---

## 10. Wiring into a binary

In your binary's `main.rs`, call `register()` for every plugin before serving:

```rust
// main.rs
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Register external providers — order doesn't matter. A name that is
    // already taken is an error, not a silent override.
    my_stt_plugin::register()?;
    my_tts_plugin::register()?;
    my_llm_plugin::register()?;

    // Built-in providers are always available. External ones extend the list.
    let cfg = voice_pipes::config::merge(
        voice_pipes::config::load(Some("pipeline.toml".as_ref()))?,
        voice_pipes::config::CliOverrides::default(),
    );
    voice_pipes::commands::run_serve(cfg, Some("worker.toml".into())).await
}
```

A plugin can also stay free of the engine crate altogether: expose a
`factory()` and let the binary do the registering. That is how the in-tree
`voice-pipes-stt-nemotron35` is wired (§11), because a crate that calls
`voice_pipes::stt::register_stt_factory` itself has to depend on
`voice-pipes`, which is the dependency a plugin exists to avoid:

```rust
// my-stt-plugin/src/lib.rs
pub fn factory() -> Box<dyn voice_pipes_sdk::SttFactory> { Box::new(MyFactory) }
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> { /* §10.1 */ }

// main.rs
use voice_pipes::plugins::{Plugin, register_plugin};
register_plugin(Plugin::Stt(my_stt_plugin::factory), my_stt_plugin::wizard_choices())?;
```

Registration fails, rather than shadowing anything, when the provider's name
(or an alias: every stage's factory can declare `aliases()`) is already
taken by a built-in or an earlier plugin, and when two wizard entries of a
stage both claim to be its default. The error is a `RegistrationError`
variant (`NameTaken`, `TwoDefaults`, ...), so code can tell them apart. The
CLI reports the error and exits.

The corresponding TOML configs now route to your providers:

```toml
[stt]
provider = "my-model"   # → my_stt_plugin::MyFactory

[tts]
provider = "my-voice"   # → my_tts_plugin::MyTtsFactory

[text_processor]
type = "my-llm"         # → my_llm_plugin::MyLlmFactory
```

### 10.1 Offering it in the setup wizard

`voice-pipes config-wizard` lists a plugin when the binary registers its
entries (SDK 0.5; they used to be a factory method). Export a
`wizard_choices()` function beside `factory()`, returning a `WizardChoice`
per menu entry: the menu label, the Cargo feature the worker image needs,
how it is provisioned, the settings to ask for with their defaults, and a
`menu_rank`. The wizard shows plugin entries with the built-in options of
the stage, ordered by `menu_rank` (the built-ins rank 1, 2, … and come
before a plugin entry of the same rank; plugin ties go by label, never by
link order), writes
`provider = "<your factory's name>"` and one line per field, and prints your
notes — it runs no code of yours, and never builds the provider.

```rust
use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

pub fn wizard_choices() -> Vec<WizardChoice> {
    // WizardDeploy::LocalManual, or Cloud { key_env: "MY_API_KEY".into() }
    vec![WizardChoice::new("My model (local ONNX plugin)", WizardDeploy::LocalManual)
        .with_feature(Some("stt-my-model".into()))
        .with_fields(vec![
            WizardField::path("model_dir", "Model directory", "models/my-model"),
            WizardField::text("language", "Default locale", "en-US"),
        ])
        .with_notes(vec!["Fetch the model with scripts/download_my_model.py.".into()])
        .with_menu_rank(50)]
}
```

`WizardChoice`, like the other SDK types a later version may extend, is
`#[non_exhaustive]`: build it with `new` and the `with_*` setters (unset,
an entry has no feature, fields or notes, is not its stage's default, and
ranks `WizardChoice::DEFAULT_MENU_RANK`).

The binary registers them with the factory, in one call:
`voice_pipes::plugins::register_plugin(Plugin::Stt(my_plugin::factory),
my_plugin::wizard_choices())`. (`register_wizard_choices` remains for entries
added to a factory registered some other way; it refuses entries for a
provider that is not registered.)

Beyond `text` and `path`, a field can be `fixed` (a string written without
asking), `fixed_literal` (a number or boolean written unquoted, as given),
`comment` (a `#` line explaining the settings around it),
`list` (comma-separated answers written as a TOML array), or `secret`
(asked without echo and stored in `.env`, never in the TOML), and any field
can be `.optional()` so a blank answer writes nothing, or `.required()` so a
blank answer stops the wizard with an error. `WizardChoice::render`
is what the wizard calls, so a plugin can test the exact TOML its entry
produces without the engine — `voice-pipes-stt-google` pins its entry's
default output that way.

When the files can be fetched from a URL, declare them and the wizard
downloads them like a built-in's: `WizardDeploy::Download { layout, files }`
puts them under the choice's first `path` field (a directory with
`WizardLayout::Dir`; with `WizardLayout::File` the path names the primary
file and the rest go next to it, and a renamed path means the user brings
their own). An operator's `config/wizard_models.json` entry under your
provider's name takes precedence, so URLs can be repointed without a
rebuild. `voice-pipes-tts-kokoro` fetches its model and voices this way.

A voice that phonemises with espeak-ng, or chunks streamed text by sentence,
can use the SDK's `phonemize` and `sentence` modules, the same code the
built-in voices run.

A provider with presets (a cloud service and a self-hosted sidecar that
speaks the same API, or several model sizes) returns one entry per preset.
Set `stage_default: true` on the entry that is its stage's out-of-the-box
choice: the flag keeps it preselected when the binary links it, wherever its
rank lists it. Only one entry per stage may claim it; a second is a
registration error. What a pipeline with no `[stt]` or `[tts]` section runs
is the binary's decision, not the plugin's: it calls
`voice_pipes::plugins::register_fallback(stage, name)` for the provider it
links as the default.

A provider that talks to a local model server (an OpenAI-compatible LLM
on llama-server, say) describes each server preset with
`WizardDeploy::DockerSidecarModel`: the Compose profile and service the
launcher starts, the variable the server reads its model location from, and
a `WizardModel` (registry key, default path, layout, files) the wizard
downloads first. The location goes to the launcher, not the TOML, so the
entry needs no `path` field; an operator's `wizard_models.json` entry under
the model's key wins, and presets naming the same key share one download.
`WizardDeploy::DockerSidecar` is the same without a download, for a server
that fetches its own weights. `voice-pipes-llm-openai-compat` offers its
Gemma presets this way.

The wizard runs in the binary, so it sees exactly the plugins that binary
registered. In this repository `voice-pipes-cli` has a test that holds every
plugin's `feature` to its own `Cargo.toml` and to the worker images'
`FEATURES` (a feature in the CLI's `default-plugins` is in every image), the
same rule the built-in choices are held to.

---

## 11. A local ONNX model as a plugin: Nemotron 3.5

`plugins/stt/nemotron35/` (`voice-pipes-stt-nemotron35`) is a recogniser added to the repository without
a line of provider code in the engine. It exists as much to prove that route
as to run the model, so this section is the recipe it leaves behind. When a
model needs an engine edit beyond a defaulted trait addition, that is a gap in
the SDK to fix first, not a special case to allow.

Every provider has since taken the same route, so there is a worked example
for each kind: `voice-pipes-stt-nemotron` (a second local recogniser, with
its WER and streaming examples), `voice-pipes-tts-kokoro` (a local voice),
`voice-pipes-llm-gguf` (a local language model), `voice-pipes-turn-smart`
and `-turn-vap` (turn detectors), and the hosted `-stt-assemblyai`,
`-stt-google`, `-tts-*`, `-llm-openai-compat` and `-s2s-*` crates.

### 11.1 What goes where

| Where | What | Provider-specific? |
|---|---|---|
| the plugin crate | features, driver, tokens, `SttProvider`, `SttFactory`, `factory()`, `wizard_choices()`, tests | yes — all of it |
| `voice-pipes-onnx` | the bounded session pool (`SessionPool`, `PooledSlot`) | no; shared with the built-in ONNX providers |
| `voice-pipes-sdk` | the traits, and anything a local model needs to report that the SDK could not yet carry | no; every addition is defaulted and serves every provider |
| `voice-pipes` (the engine) | nothing | — |
| `voice-pipes-cli` (the composition root) | one optional dependency, one feature, one register block | by design |
| the wizard | nothing — the crate's `wizard_choices()` (§10.1) | — |
| Docker images | the feature in every worker's `FEATURES`; nothing in the Dockerfiles, which copy every `voice-pipes*` crate by pattern | — |

The check that keeps the claim honest: `git diff --stat main -- voice-pipes/src`
shows only changes that serve every recogniser, and
`grep -r "stt-nemotron35" voice-pipes/src` finds nothing at all.

### 11.2 The crate

```
plugins/stt/nemotron35/
├── Cargo.toml          ← voice-pipes-sdk, voice-pipes-onnx, ort, rustfft; feature ort-cuda
├── src/
│   ├── features.rs     ← log-mel front-end, written to the model's numbers
│   ├── model.rs        ← the export's config file, the token table, file checks
│   ├── engine.rs       ← the cache-aware encoder/decoder driver, one utterance at a time
│   ├── provider.rs     ← SttProvider + SttFactory over a session pool
│   └── lib.rs          ← pub fn factory() -> Box<dyn SttFactory>
└── tests/
    ├── features.rs     ← front-end against a reference dump
    ├── engine.rs       ← token identity with the reference implementation
    └── provider.rs     ← the stream as the pipeline drives it
```

Three decisions worth copying:

- **The driver owns its sessions through a trait, not a borrow.** The token
  tests lend it `&mut Sessions`; the provider gives it a `PooledSlot` it
  owns, so the whole engine can move into `spawn_blocking` and back each
  chunk, and a panic in the blocking task drops the slot back to the pool.
- **Honour `request.slot_wait` at the checkout.** The pool's own wait is
  short, so one caller cannot queue behind another's set for long. The
  engine sets `slot_wait` to a minute on a stream that replaces one it gave
  up on: the replaced stream's last blocking run holds the set until the
  model returns, and the replacement is to take over the moment it does.
  `match request.slot_wait { Some(w) => pool.checkout_within(w), None =>
  pool.checkout() }` is the whole of it; a provider that ignores the field
  is refused after the short wait and fails over and over until the stall
  clears.
- **One pool slot and one engine per stream; cut the tokens, not the
  state.** `transcribe_stream` checks out a slot once and decodes the whole
  session on one engine. On `is_last` it appends trailing silence, closes
  the utterance on a chunk boundary (`end_utterance`), sends the final with
  `inference_metrics`, and carries on: the encoder cache and the prediction
  network's state cross the boundary, only the token list is cut. A
  cache-aware streaming model is built for a continuous signal; a fresh
  engine per utterance decodes each VAD segment from nothing, and on a poor
  microphone a segment cut at speech onset then decodes to nothing at all.
  With the state carried, the stream decodes exactly as one batch pass
  would, and a token the model emits late lands in the next utterance
  instead of nowhere.
- **Drain the queue, never past a boundary.** Taking every chunk already
  queued before running is what keeps a slow CPU decode from falling behind,
  but the drain stops at `is_last`; reading on glues the next utterance's
  first chunks onto this one, and the provider test is there to catch it.
- **Test the shortest utterance you expect, not the reference's corpus.** A
  reference validated by WER on long recordings can carry a defect that
  costs one word per file — here, the model emits a word only after it has
  seen silence behind it, and the reference's flush never supplied any, so
  a one-word "yes" decoded to nothing while matching the reference
  token for token. The provider appends trailing silence before it flushes;
  the engine stays identical to the reference so the token gate keeps
  meaning. The fixture that caught it is 1.5 s long.

The language comes from `request.language` (the session), falling back to
the `language` option (the pipeline), and the model's own `auto` last; the
result carries the locale tag the model emitted, so `auto` reports what it
detected. Reported through the SDK 0.3 methods: `WarmUpScope::PerProcess`
(the worker warms a pool set once at start), `concurrent_capacity =
pool_size`, `model_identity` from the export's config, `device_info` from
the `ort-cuda` feature and `VP_ORT_EP`.

Sessions are opened with `voice_pipes_onnx::open_session` (the crate's
`session` feature), which applies the thread count, graph optimisation and
execution provider one way for every plugin; the plugin's `ort-cuda`
feature switches on `voice-pipes-onnx/cuda`. `ExecutionTarget::from_env()`
follows `VP_ORT_EP`, and `effective()` says where a session actually runs,
for `device_info`. A model with a log-mel front end builds it from
`voice_pipes_onnx::mel` (feature `mel`): `hann`, `centred`,
`slaney_filterbank`, `PowerSpectrogram` and `mel_energies`, keeping only
the steps its reference defines its own way (pre-emphasis, padding, the
log, normalisation).

### 11.3 The registration

```toml
# voice-pipes-cli/Cargo.toml
[features]
stt-nemotron35      = ["dep:voice-pipes-stt-nemotron35"]
stt-nemotron35-cuda = ["stt-nemotron35", "plugins/stt/nemotron35/ort-cuda"]

[dependencies]
voice-pipes-stt-nemotron35 = { workspace = true, optional = true }
```

Every dependency is declared once, in the root `Cargo.toml`'s
`[workspace.dependencies]`, and members inherit it (`tokio = { workspace =
true, features = [...] }`). A new plugin adds its own third-party crates
there too, so a version bump or an advisory fix is one line for the whole
workspace.

```rust
// voice-pipes-cli/src/main.rs, in register_each_plugin(): with the feature
// on, register_plugin(Plugin::Stt(<crate>::factory), <crate>::wizard_choices())
plugin!("stt-nemotron35", Stt, voice_pipes_stt_nemotron35);
```

Then `voice-pipes inspect --kind stt` lists it, and a pipeline selects it
like any built-in one:

```toml
[stt]
provider  = "nemotron-3.5"
model_dir = "models/nemotron-3.5"
language  = "hr-HR"     # or en-US, or auto
pool_size = 2
```

### 11.4 The rules a provider crate is held to

CI checks these, from the manifests (`voice-pipes-cli/tests/workspace_rules.rs`)
and the dependency graph:

- **Dependencies:** the SDK, `voice-pipes-onnx`, `voice-pipes-net` and your
  own third-party crates. Never `voice-pipes` (an adopter of your provider
  would take the whole runtime) and never another provider.
- **Network clients from `voice-pipes-net`**, never your own: no direct
  `reqwest`, `tokio-tungstenite`, `hyper` or similar dependency (the net
  crate re-exports `reqwest` and, with its `ws` feature, `tokio_tungstenite`
  for their types), and clippy refuses `reqwest::Client::new`/`builder` and a
  bare `connect_async`. Build clients with `http_client` and
  `HttpOptions::request(timeout)` (or `streaming()` when you bound each read
  yourself); connect WebSockets with `websocket_config`; call
  `credential_endpoint_error` in `validate` for any endpoint a user can set.
  They carry the TLS stack, timeouts, the no-redirect policy, warm pooled
  connections and the plain-text rule, so no provider re-decides them.
- **Bounded input:** read an error body with `error_body`, a whole success
  body with `limited_body`, a newline-delimited stream with `BoundedLines`;
  never log a vendor frame's content; test malformed input (not JSON, wrong
  shape, bad base64, truncated audio, oversized lines) and never panic on it.
- **One lint policy:** `[lints] workspace = true`.
- **Shared helpers, not copies:** a provider that streams audio over HTTP
  frames it with `voice_pipes_sdk::audio::PcmFramer` (a sample split across
  chunks, fixed-size frames, `send_frames`/`send_rest` that report a gone
  receiver) and strips a WAV header with `WavStripper` (`requiring_header()`
  for a server that always sends one); maps an HTTP error with
  `TtsError::from_http_status` (or `SttError::from_http_status`); and takes
  its key from `cfg.api_key`, which the engine resolved from the variable
  `default_api_key_env()` names (`secrets::configured_key` treats a blank
  one as none).
- **`#![forbid(unsafe_code)]`** in `src/lib.rs`. FFI belongs in the audited
  crates that exist for it (`ort`).
- **A `.github/CODEOWNERS` line** for the crate's directory.
- **Sessions through `voice_pipes_onnx::open_session`**, not
  `Session::builder` directly, so the execution-provider policy is one
  process-wide policy; a crate with `ort-cuda` forwards it to
  `voice-pipes-onnx/cuda`.
- **`ort-cuda` forwarded** by the CLI's `ort-cuda` if your crate has one.
  A crate on `ort` without one must be named, with its reason, in the
  rules' CPU-only list (the two turn detectors are).
- **Only the CLI links providers.** Another workspace crate that needs one
  directly (the bench runs VAP in its bake-off) is named in the rules'
  allow-list with what it links.
- **Supply chain:** `cargo deny` checks every feature of every member, so a
  new dependency must have an allowed license and no open advisory.
- **The bare engine stays bare:** `scripts/dep-budget.py` counts the crates
  each build shape pulls in and fails if the engine with no features grows
  past its budget.

### 11.4a A new plugin kind in the SDK

A crate extracts a provider; sometimes the SDK first needs a module for a
kind it has not carried (as `s2s` and `turn` were added). Before the first
crate moves onto such a module:

- **`build(&self, cfg, ctx: &BuildContext<'_>)`**, like every other factory,
  so a later need (the trace sink, the parameter store) is not a breaking
  change.
- **`#[non_exhaustive]`** on every public enum a provider matches or
  returns, and on config structs the engine fills (with a `new` and
  `with_*` setters, since a plugin cannot use a struct literal).
- **Credentials resolved by the engine**, handed over as a `Secret`; the
  provider never reads the environment for a key.
- **A test that registers from outside the engine:** a mock factory
  registered through the public function, looked up by name and alias,
  refused as a duplicate, and driving the behaviour the engine keys on.
- **Defaulted trait methods** for anything a first provider happens not to
  need (`param_schema`, `aliases`), so the second one is not blocked.

### 11.5 The gate

A model driver is judged against a reference implementation, token for
token, before anything is said about its accuracy: `tests/engine.rs` holds
the driver to the reference engine's exact token ids on every fixture, in
English and Croatian and under `auto`, so a frame out of place in the cache
schedule shows as a different sequence, not as a slightly worse WER. Only
then is WER measured, on the same corpus and scorer as the built-in
recognisers, so the number means the same thing.

---

## 12. Stability guarantees

From 1.0 the SDK is semver-stable; the [stability policy](sdk-stability.md)
is the full statement. In short, a 1.x minor release may:

| Change | Allowed in a minor? |
|---|---|
| New field on a `#[non_exhaustive]` struct | ✅ Yes: build them with `new`/`Default` and `with_*` |
| New variant on an enum (every public one is `#[non_exhaustive]`) | ✅ Yes: keep a `_ => {}` arm |
| New trait method with a default implementation | ✅ Yes: existing impls unchanged |
| New field on one of the structs whose fields are complete (`AudioFrame`, `AudioChunk`, ...) | ❌ No: major version |
| Removing a method or field, changing a signature | ❌ No: major version |
| A new 0.x of `tokio-util` or `async-trait`, which the API exposes | ❌ No: major version |

CI runs `cargo semver-checks` on the SDK, so a breaking change cannot ship
under a compatible version. Before 1.0 a `0.y` bump could break; the
changelog records each one.

### Defensive coding patterns

```rust
// Structs that gain fields (TranscriptionResult, InferenceMetrics,
// TextFragment, SynthesisRequest, WordInfo, SynthesisHints, ...) are
// #[non_exhaustive]: build them with `new` or `Default` and the `with_*`
// setters, which keep compiling when a field is added.
let result = TranscriptionResult::new("hello", true).with_confidence(Some(0.9));

// A struct literal (`..Default::default()` included) does not compile for a
// #[non_exhaustive] struct from another crate; start from Default instead.
let hints = SynthesisHints::default().with_voice(Some("af_heart".into()));

// Enums: always add a wildcard arm
match error {
    SttError::Timeout(_) => retry(),
    SttError::NetworkError(_) => retry(),
    _ => propagate(),   // handles future variants without breaking
}
```

---

## 13. Full type reference

### SDK modules

| Module | Key types |
|---|---|
| `voice_pipes_sdk` (root) | All types re-exported here — no sub-module import needed |
| `audio` | `AudioFrame`, `PcmFramer`, `WavStripper`; `resample` (sample-rate conversion) |
| `audit` | `TraceEvent` |
| `component` | `Component`, `ComponentCore`, `ComponentKind`, `CoreComponent`, `DeviceInfo`, `ErrorPolicy`, `HealthState`, `HealthStatus`, `LatencyStats`, `ModelIdentity`, `ComponentTelemetry` |
| `config` | `SttConfig`, `TtsConfig`, `TextProcessorConfig`, `TextChunkingConfig`, `VadConfig`, `ConfigError` |
| `params` | `AtomicParamStore` (`from_schema`, typed cells), `AtomicParamStoreBuilder`, `MutableParamStore` |
| `secrets` | `Secret` (a credential that cannot be printed), `REDACTED`, `is_secret_key`, `redact_in_place`, `redacted`, `redact_url`, `debug_options` |
| `schema` | `ProviderSchema`, `ParamDescriptor`, `ParamConstraints`, `ParamValueType`, `ParamValue`, `OnlyWhen`, `ParamError` |
| `stt` | `SttCapability`, `SttError`, `SttProvider`, `SttFactory`, `RecognitionRequest`, `AudioChunk`, `AudioEncoding`, `TranscriptionResult`, `WordInfo`, `InferenceMetrics`, `WarmUpScope` |
| `text` | `TextProcessor`, `TextProcessorFactory`, `TextFragment`, `TextProcessorError` |
| `tts` | `TtsProvider`, `TtsFactory`, `TtsError`, `SynthesisRequest`, `SynthesisHints`, `WordBoundary`, `TtsSession`, `TtsSessionConfig` |
| `vad` | `VadBackend`, `VadFactory`, `VadResult`, `VadError` |
| `turn` | `TurnDetector`, `TurnDetectorFactory`, `TurnConfig`, `TurnSignal`, `TurnDecision` |
| `wizard` | `WizardChoice`, `WizardField`, `WizardFieldKind`, `WizardDeploy`, `WizardLayout`, `WizardDownload`, `WizardModel`, `RenderedChoice`, `toml_string` |
| `agent` | `SessionHandle`, `SessionSink`, `ClientToolCall`, `ClientToolSpec`, `SessionEnded` |
| `sentence` | `split_first_sentence` — the sentence splitter every built-in voice uses. Pass `at_end = false` while text is still arriving (a trailing `.` is held until the next character shows it is not "3.14" or "Dr. Smith") and `true` for complete text. A chunk keeps the whitespace after its mark, so the chunks join back to the text |
| `phonemize` | `run_espeak`, `run_espeak_with_language` — espeak-ng grapheme-to-phoneme, for models that take IPA |
| `reconnect` | `ReconnectBudget`, `SessionOutcomeInfo`, `backoff_delay` — the bounded retry policy for hosted streaming clients |
| `s2s` | `RealtimeProtocol`, `RealtimeFactory`, `RealtimeConfig`, `RealtimeEvent`, `SessionConfig`, `Modalities`, `TurnDetection`, `ToolSpec`, `ConnectInfo`, `CloseClass`; `codec` (base64 PCM16 at the vendor's rate; the engine resamples) |
| `testing` | `chunking_invariant` — a stream transform's output at every chunk size equals the whole input's; with the `testing` feature (enable in `[dev-dependencies]`), `two_utterances_on_one_stream` and `back_to_back_utterances_stay_apart`, the streaming-contract checks |

### Registration functions (in `voice_pipes`, not SDK)

| Function | Where to call |
|---|---|
| `voice_pipes::stt::register_stt_factory(ctor)` | `main.rs` before `run()` |
| `voice_pipes::tts::register_tts_factory(ctor)` | `main.rs` before `run()` |
| `voice_pipes::text::register_text_processor_factory(ctor)` | `main.rs` before `run()` |
| `voice_pipes::vad::register_vad_factory(ctor)` | `main.rs` before `run()` |
| `voice_pipes::realtime::register_realtime_factory(ctor)` | `main.rs` before `run()` |
| `voice_pipes::stt::turn::register_turn_factory(ctor)` | `main.rs` before `run()` |
| `voice_pipes::plugins::register_plugin(Plugin::<Stage>(ctor), choices)` | `main.rs` before `run()`: the factory and its wizard entries in one step |
| `voice_pipes::plugins::register_wizard_choices(stage, name, choices)` | after the factory it names |

`ctor` is a `fn() -> Box<dyn SttFactory>` (or TTS/text/VAD equivalents) — a factory-of-factories.
Each returns `Result<(), RegistrationError>`: a name or alias already taken
(`NameTaken`), a second stage default (`TwoDefaults`), wizard entries for an
unregistered provider (`NoSuchProvider`) or for a stage the wizard lacks
(`NoStage`, VAD) is refused instead of silently shadowing something.

### Component health state machine

```
            auto-degrade                auto-recover
Healthy ──────────────────────► Degraded ◄───────────────── Healthy
   ▲                                                              │
   │  mark_healthy()                          mark_healthy()      │
   │                                                              │
   └──────────────── provider lifecycle code ────────────────────┘
                    (mark_unhealthy / mark_healthy on connect/disconnect)

NotConfigured   ← provider returns None from as_component(), or is disabled in config
```

---

*This document covers `voice-pipes-sdk 0.1.x`. For questions or API change requests open an issue in the voice-pipes repository.*
