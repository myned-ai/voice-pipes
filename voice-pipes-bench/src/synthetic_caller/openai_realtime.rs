//! `OpenAI` Realtime synthetic-caller backend.
//!
//! Thin adapter around the shared
//! [`RealtimeClient`](voice_pipes::realtime::RealtimeClient) and the
//! `openai-realtime` provider the binary registers. The wire-level
//! payloads, audio resampling and event parsing live there. This file
//! only:
//!
//! 1. Owns the persona/voice configuration handed to the client.
//! 2. Translates [`voice_pipes::realtime::RealtimeEvent`] -> [`CallerEvent`].
//! 3. Exposes [`OpenAiRealtimeCallerBuilder`], the surface the
//!    benchmark runner builds this caller through.

use anyhow::Result;
use async_trait::async_trait;

use voice_pipes::realtime::{
    Modalities, RealtimeClient, RealtimeConfig, RealtimeEvent, RealtimeProtocol, SessionConfig,
    lookup_realtime_factory,
};

use super::{CallerEvent, SyntheticCaller};

/// Pipeline-side sample rate (matches the worker WS protocol).
const PIPELINE_SR: u32 = 16_000;

/// The provider the caller runs on.
const PROVIDER: &str = "openai-realtime";

/// A `SyntheticCaller` backed by the `OpenAI` Realtime API.
pub struct OpenAiRealtimeCaller {
    client: RealtimeClient<Box<dyn RealtimeProtocol>>,
    voice: Option<String>,
}

impl std::fmt::Debug for OpenAiRealtimeCaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiRealtimeCaller")
            .field("voice", &self.voice)
            .finish_non_exhaustive()
    }
}

/// Builder for [`OpenAiRealtimeCaller`].
#[derive(Debug, Clone)]
pub struct OpenAiRealtimeCallerBuilder {
    /// Model id; `None` is the provider's default.
    model: Option<String>,
    voice: Option<String>,
    api_key_env: String,
}

impl Default for OpenAiRealtimeCallerBuilder {
    fn default() -> Self {
        Self {
            model: None,
            voice: None,
            api_key_env: "OPENAI_API_KEY".to_string(),
        }
    }
}

impl OpenAiRealtimeCallerBuilder {
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

    /// Open the WS, spawn the read/write tasks, return a ready caller.
    pub async fn connect(self) -> Result<OpenAiRealtimeCaller> {
        let factory = lookup_realtime_factory(PROVIDER).ok_or_else(|| {
            anyhow::anyhow!("this build has no OpenAI Realtime provider (\"{PROVIDER}\")")
        })?;
        let cfg = RealtimeConfig::new(PROVIDER)
            .with_model(self.model.clone())
            .with_api_key(voice_pipes::sdk::secrets::resolve_key(
                None,
                Some(&self.api_key_env),
                &voice_pipes::config::credentials::process_env,
            ))
            .with_api_key_var(self.api_key_env.clone());
        let proto = factory.build(&cfg, &voice_pipes::sdk::BuildContext::new())?;
        let client = RealtimeClient::connect(proto, PIPELINE_SR).await?;
        Ok(OpenAiRealtimeCaller {
            client,
            voice: self.voice,
        })
    }
}

/// Map a [`RealtimeEvent`] onto zero or more [`CallerEvent`]s. The
/// caller-facing event vocabulary is intentionally narrower (no
/// lifecycle / function-call events today) so unhandled variants are
/// folded into trace logs.
fn map_event(ev: RealtimeEvent) -> Option<CallerEvent> {
    match ev {
        RealtimeEvent::ResponseAudioDelta { samples, .. } => Some(CallerEvent::AudioDelta(samples)),
        RealtimeEvent::ResponseTextDelta { text } => Some(CallerEvent::TranscriptDelta(text)),
        RealtimeEvent::ResponseCompleted { .. } => Some(CallerEvent::ResponseDone),
        RealtimeEvent::Error { message, .. } => Some(CallerEvent::Error(message)),
        RealtimeEvent::Closed { reason, .. } => {
            Some(CallerEvent::Error(format!("realtime ws closed: {reason}")))
        }
        // Input, lifecycle, tool and usage events, and any the SDK adds.
        _ => {
            tracing::trace!(
                target = "synth_caller.openai",
                "realtime lifecycle event dropped"
            );
            None
        }
    }
}

#[async_trait]
impl SyntheticCaller for OpenAiRealtimeCaller {
    async fn configure(&mut self, prompt: &str) -> Result<()> {
        // A caller places the call: it speaks first, and nothing reads a
        // transcript of what it hears.
        let cfg = SessionConfig::new(prompt)
            .with_voice(self.voice.clone())
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
#[path = "tests/openai_realtime.rs"]
mod tests;
