---
name: voice-pipes-plugin-builder
description: Scaffold and build external STT, TTS, LLM, VAD, S2S, or Turn detection provider plugins for Voice Pipes using voice-pipes-sdk 1.x.
---

# Voice Pipes Plugin Builder

Use this skill when a user asks to create, scaffold, or implement a new provider plugin for Voice Pipes (e.g. "Add a Whisper STT plugin", "Create a Cartesia TTS provider", "Add an Ollama LLM processor").

## 1. Plugin Architecture Rules

- **Zero Core Dependency**: Plugins depend ONLY on `voice-pipes-sdk 1.x`. Never add dependencies on internal engine crates (`voice-pipes`).
- **Memory Safety**: Always declare `#![forbid(unsafe_code)]` at the root of `src/lib.rs`.
- **Component Health**: Every provider must implement `as_component(&self) -> Option<Arc<dyn Component>>` returning `CoreComponent::new(Arc::clone(&self.core))`.
- **Telemetry**: Report latencies and errors to `self.core.record_ok(elapsed)` and `self.core.record_error(&err)`.
- **Registration**: Export a single `pub fn register() -> Result<(), ConfigError>` that registers the provider factory with `voice-pipes-sdk`.

## 2. Scaffolding Steps

### Step 1: Crate Setup
Create a new directory under `plugins/<stage>/<name>` (with package name `voice-pipes-<stage>-<name>`):
- STT: `voice-pipes-stt-<name>`
- TTS: `voice-pipes-tts-<name>`
- LLM: `voice-pipes-llm-<name>`
- VAD: `voice-pipes-vad-<name>`
- S2S: `voice-pipes-s2s-<name>`
- Turn: `voice-pipes-turn-<name>`
- Filter: `voice-pipes-filter-<name>`

Add the crate to workspace `Cargo.toml` under `members = [...]`.

In `plugins/<stage>/<name>/Cargo.toml`:
```toml
[package]
name = "voice-pipes-<stage>-<name>"
version.workspace = true
edition.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
voice-pipes-sdk.workspace = true
async-trait.workspace = true
tracing.workspace = true
serde = { workspace = true, features = ["derive"] }
# Add provider-specific inference or network client libraries
```

### Step 2: Implement the Provider Struct
In `src/provider.rs`:
```rust
use std::sync::Arc;
use async_trait::async_trait;
use voice_pipes_sdk::component::{Component, ComponentCore, ComponentKind, CoreComponent};
use voice_pipes_sdk::stt::{SttProvider, SttError, TranscriptionResult, RecognitionRequest, AudioChunk};
use voice_pipes_sdk::AudioFrame;

pub struct CustomProvider {
    core: Arc<ComponentCore>,
    // provider configuration & state
}

impl CustomProvider {
    pub fn new(id: &str) -> Self {
        Self {
            core: Arc::new(ComponentCore::new(format!("stt:{id}"), ComponentKind::Stt)),
        }
    }
}

#[async_trait]
impl SttProvider for CustomProvider {
    async fn transcribe(&self, audio_frame: &AudioFrame) -> Result<TranscriptionResult, SttError> {
        let start = std::time::Instant::now();
        // Run inference
        self.core.record_ok(start.elapsed());
        Ok(TranscriptionResult::new("transcription text", true))
    }

    fn as_component(&self) -> Option<Arc<dyn Component>> {
        Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
    }
}
```

### Step 3: Implement the Factory & Registration
In `src/factory.rs`:
```rust
use std::sync::Arc;
use voice_pipes_sdk::stt::{SttFactory, SttConfig, SttError, SttProvider};

pub struct CustomFactory;

impl SttFactory for CustomFactory {
    fn validate(&self, config: &SttConfig) -> Result<(), SttError> {
        // Validate options from config.options
        Ok(())
    }

    fn build(&self, config: &SttConfig) -> Result<Arc<dyn SttProvider>, SttError> {
        Ok(Arc::new(crate::provider::CustomProvider::new("custom")))
    }
}
```

In `src/lib.rs`:
```rust
#![forbid(unsafe_code)]

mod factory;
mod provider;

pub use factory::CustomFactory;
pub use provider::CustomProvider;

use voice_pipes_sdk::ConfigError;
use std::sync::Arc;

pub fn register() -> Result<(), ConfigError> {
    voice_pipes_sdk::stt::register_stt_factory("custom-name", Arc::new(CustomFactory))
}
```

### Step 4: Verification
Run the standard validation commands:
```sh
cargo check -p voice-pipes-<stage>-<name>
cargo clippy -p voice-pipes-<stage>-<name> -- -D warnings
cargo test -p voice-pipes-<stage>-<name>
```
