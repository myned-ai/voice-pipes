//! Registration and construction of the `OpenAI` Realtime protocol.

use anyhow::Result;
use voice_pipes_sdk::s2s::{RealtimeConfig, RealtimeFactory, RealtimeProtocol};
use voice_pipes_sdk::{BuildContext, ConfigError};

use crate::OpenAiProtocol;

/// Factory for [`OpenAiProtocol`].
#[derive(Debug)]
pub struct OpenAiRealtimeFactory;

impl RealtimeFactory for OpenAiRealtimeFactory {
    fn name(&self) -> &'static str {
        "openai-realtime"
    }

    fn aliases(&self) -> &[&str] {
        &["openai", "gpt-realtime"]
    }

    fn default_api_key_env(&self) -> &'static str {
        "OPENAI_API_KEY"
    }

    fn validate(&self, cfg: &RealtimeConfig) -> Vec<ConfigError> {
        let mut errors: Vec<ConfigError> = cfg
            .missing_key_error("OpenAI Realtime")
            .into_iter()
            .collect();
        errors.extend(option_errors(cfg));
        errors
    }

    fn build(
        &self,
        cfg: &RealtimeConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let mut p = OpenAiProtocol::new();
        if let Some(model) = cfg.model.as_deref() {
            p = p.with_model(model);
        }
        p = p.with_api_key(cfg.api_key.clone());
        // Whether the session opens the conversation, and whether its
        // input is transcribed at all, are the session's to say
        // (`SessionConfig::speaks_first`, `transcribe_input`); these
        // settings shape the transcription when it is.
        let mut o = cfg.options();
        if let Some(model) = o.string("input_transcription_model") {
            p = p.with_input_transcription_model(Some(model));
        }
        // Language hint defaults to "en" so short utterances aren't
        // mis-detected as Korean / Japanese / etc. by the
        // `gpt-4o-mini-transcribe` auto-detector. Operators can
        // override via `[realtime_provider.options]
        // input_transcription_language = "es"` (or `"auto"` to
        // restore auto-detection).
        let lang = o.string_or("input_transcription_language", "en");
        if lang != "auto" {
            p = p.with_input_transcription_language(Some(lang));
        }
        if let Some(prompt) = o.string("input_transcription_prompt") {
            p = p.with_input_transcription_prompt(Some(prompt));
        }
        Ok(Box::new(p))
    }
}

/// The options `build` reads, typed; any other key is refused (the ones
/// the engine's session runner reads are its own).
fn option_errors(cfg: &RealtimeConfig) -> Vec<ConfigError> {
    let mut o = cfg.options();
    let _ = o.string("input_transcription_model");
    let _ = o.string("input_transcription_language");
    let _ = o.string("input_transcription_prompt");
    o.into_errors()
}
