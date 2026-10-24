//! Synthetic-caller backends for S2S-driven multi-turn benchmark
//! scenarios.
//!
//! A "caller" is a separate model that consumes the pipeline's audio
//! output and produces audio (and optionally a transcript) that is fed
//! back into the pipeline as user input. The runner bridges the two
//! sides until a stop condition fires.
//!
//! The [`SyntheticCaller`] trait is the abstraction; the concrete
//! backends (`OpenAI` Realtime, Gemini Live) are gated behind the
//! `synthetic-caller` cargo feature. The [`MockCaller`] backend has no
//! external dependencies and is used by unit tests.

use async_trait::async_trait;

pub mod mock;
pub mod prompts;

#[cfg(feature = "synthetic-caller")]
pub mod gemini_live;
#[cfg(feature = "synthetic-caller")]
pub mod openai_realtime;

/// Events produced by a [`SyntheticCaller`].
#[derive(Debug, Clone)]
pub enum CallerEvent {
    /// A chunk of PCM i16 little-endian samples spoken by the caller.
    AudioDelta(Vec<i16>),
    /// A partial or final transcript fragment from the caller.
    TranscriptDelta(String),
    /// The caller finished its current response (utterance boundary).
    ResponseDone,
    /// A non-fatal error from the caller backend.
    Error(String),
}

/// A bidirectional S2S caller that talks to a pipeline through the
/// benchmark runner.
#[async_trait]
pub trait SyntheticCaller: Send {
    /// Configure the caller with a persona / system prompt before any
    /// audio flows.
    async fn configure(&mut self, prompt: &str) -> anyhow::Result<()>;
    /// Push a chunk of pipeline audio (PCM i16 LE) into the caller.
    async fn push_audio(&mut self, samples: &[i16]) -> anyhow::Result<()>;
    /// Push a textual pipeline turn into the caller (text-bridged
    /// fallback for backends that cannot decode pipeline audio or when
    /// the runner is running in text-bridged mode). Default no-op so
    /// existing transcript-only callers (e.g. [`mock::MockCaller`])
    /// stay valid without overriding.
    async fn push_text(&mut self, _text: &str) -> anyhow::Result<()> {
        Ok(())
    }
    /// Pull the next event from the caller. Returns `None` when the
    /// caller stream is exhausted.
    async fn next_event(&mut self) -> Option<CallerEvent>;
    /// Cancel the caller's in-flight response (used to enforce a
    /// `BargeInPolicy`).
    async fn cancel(&mut self) -> anyhow::Result<()>;
    /// Close the caller and release resources.
    async fn close(self: Box<Self>) -> anyhow::Result<()>;
}
