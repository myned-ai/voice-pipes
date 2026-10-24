//! Google Gemini Live synthetic-caller backend.
//!
//! Thin adapter around the shared
//! [`RealtimeClient`](voice_pipes::realtime::RealtimeClient) and the
//! `gemini-live` provider the binary registers. The Bidi wire shape,
//! audio resampling and event parsing live there. This file only:
//!
//! 1. Owns the persona configuration handed to the client.
//! 2. Translates [`voice_pipes::realtime::RealtimeEvent`] -> [`CallerEvent`].
//! 3. Exposes [`GeminiLiveCallerBuilder`], the surface the benchmark
//!    runner builds this caller through.

use anyhow::Result;
use async_trait::async_trait;

use voice_pipes::realtime::{
    Modalities, RealtimeClient, RealtimeConfig, RealtimeEvent, RealtimeProtocol, SessionConfig,
    lookup_realtime_factory,
};

use super::{CallerEvent, SyntheticCaller};

const PIPELINE_SR: u32 = 16_000;

/// The provider the caller runs on.
const PROVIDER: &str = "gemini-live";

/// A `SyntheticCaller` backed by the Gemini Live Bidi API.
pub struct GeminiLiveCaller {
    client: RealtimeClient<Box<dyn RealtimeProtocol>>,
}

impl std::fmt::Debug for GeminiLiveCaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiLiveCaller").finish()
    }
}

/// Builder for [`GeminiLiveCaller`].
#[derive(Debug, Clone)]
pub struct GeminiLiveCallerBuilder {
    /// Model id, with or without the `models/` prefix; `None` is the
    /// provider's default.
    pub(super) model: Option<String>,
    voice: Option<String>,
    api_key_env: String,
}

impl Default for GeminiLiveCallerBuilder {
    fn default() -> Self {
        Self {
            model: None,
            voice: None,
            api_key_env: "GEMINI_API_KEY".to_string(),
        }
    }
}

impl GeminiLiveCallerBuilder {
    #[must_use]
    pub fn model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }
    #[must_use]
    pub fn voice(mut self, voice: Option<String>) -> Self {
        self.voice = voice;
        self
    }
    #[must_use]
    pub fn api_key_env(mut self, var: impl Into<String>) -> Self {
        self.api_key_env = var.into();
        self
    }

    pub async fn connect(self) -> Result<GeminiLiveCaller> {
        let factory = lookup_realtime_factory(PROVIDER).ok_or_else(|| {
            anyhow::anyhow!("this build has no Gemini Live provider (\"{PROVIDER}\")")
        })?;
        let cfg = RealtimeConfig::new(PROVIDER)
            .with_model(self.model.clone())
            .with_api_key(voice_pipes::sdk::secrets::resolve_key(
                None,
                Some(&self.api_key_env),
                &voice_pipes::config::credentials::process_env,
            ))
            .with_api_key_var(self.api_key_env.clone());
        // Voice is currently ignored by the Gemini protocol mapper.
        // Keep the builder field so existing runner code stays valid
        // and we can pipe it through `SessionConfig.voice` here.
        let _ = self.voice;
        let proto = factory.build(&cfg, &voice_pipes::sdk::BuildContext::new())?;
        let client = RealtimeClient::connect(proto, PIPELINE_SR).await?;
        Ok(GeminiLiveCaller { client })
    }
}

fn map_event(ev: RealtimeEvent) -> Option<CallerEvent> {
    match ev {
        RealtimeEvent::ResponseAudioDelta { samples, .. } => Some(CallerEvent::AudioDelta(samples)),
        RealtimeEvent::ResponseTextDelta { text } => Some(CallerEvent::TranscriptDelta(text)),
        RealtimeEvent::ResponseCompleted { .. } => Some(CallerEvent::ResponseDone),
        RealtimeEvent::Error { message, .. } => Some(CallerEvent::Error(message)),
        RealtimeEvent::Closed { reason, .. } => {
            Some(CallerEvent::Error(format!("gemini ws closed: {reason}")))
        }
        // Input, lifecycle, tool and usage events, and any the SDK adds.
        _ => {
            tracing::trace!(
                target = "synth_caller.gemini",
                "realtime lifecycle event dropped"
            );
            None
        }
    }
}

#[async_trait]
impl SyntheticCaller for GeminiLiveCaller {
    async fn configure(&mut self, prompt: &str) -> Result<()> {
        // A caller places the call: it speaks first, and nothing reads a
        // transcript of what it hears.
        let cfg = SessionConfig::new(prompt)
            .with_modalities(Modalities::Audio)
            .with_speaks_first(true)
            .with_transcribe_input(false);
        self.client.apply_session_config(cfg).await
    }

    async fn push_audio(&mut self, samples: &[i16]) -> Result<()> {
        self.client.send_audio(samples, PIPELINE_SR).await
    }

    async fn push_text(&mut self, text: &str) -> Result<()> {
        self.client.send_text(text).await
    }

    async fn next_event(&mut self) -> Option<CallerEvent> {
        loop {
            let ev = self.client.next_event().await?;
            if let Some(c) = map_event(ev) {
                return Some(c);
            }
        }
    }

    async fn cancel(&mut self) -> Result<()> {
        self.client.cancel().await
    }

    async fn close(self: Box<Self>) -> Result<()> {
        self.client.close().await
    }
}

#[cfg(test)]
#[path = "tests/gemini_live.rs"]
mod tests;
