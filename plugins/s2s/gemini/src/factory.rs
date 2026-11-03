//! Registration and construction of the Gemini Live protocol.

use anyhow::Result;
use voice_pipes_sdk::s2s::{RealtimeConfig, RealtimeFactory, RealtimeProtocol};
use voice_pipes_sdk::{BuildContext, ConfigError};

use crate::GeminiProtocol;

/// Factory for [`GeminiProtocol`].
#[derive(Debug)]
pub struct GeminiLiveFactory;

impl RealtimeFactory for GeminiLiveFactory {
    fn name(&self) -> &'static str {
        "gemini-live"
    }

    fn aliases(&self) -> &[&str] {
        &["gemini", "google-live"]
    }

    fn default_api_key_env(&self) -> &'static str {
        "GEMINI_API_KEY"
    }

    fn validate(&self, cfg: &RealtimeConfig) -> Vec<ConfigError> {
        let mut errors: Vec<ConfigError> =
            cfg.missing_key_error("Gemini Live").into_iter().collect();
        errors.extend(option_errors(cfg));
        errors
    }

    fn build(
        &self,
        cfg: &RealtimeConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let mut p = GeminiProtocol::new();
        if let Some(model) = cfg.model.as_deref() {
            p = p.with_model(model);
        }
        p = p.with_api_key(cfg.api_key.clone());
        // Optional override for the async function-calling vocabulary
        // (NON_BLOCKING declarations + FunctionResponse scheduling).
        // Absent = decide from the model name (`native-audio`).
        p = p.with_async_function_calling(cfg.options().optional_bool("async_function_calling"));
        // Whether the opener is sent, and whether input is transcribed, are
        // the session's to say (`SessionConfig::speaks_first`,
        // `transcribe_input`).
        Ok(Box::new(p))
    }
}

/// The options `build` and the session setup read, typed and bounded;
/// any other key is refused (the ones the engine's session runner reads
/// are its own, including the `vad_*` timings this provider also maps).
fn option_errors(cfg: &RealtimeConfig) -> Vec<ConfigError> {
    let mut o = cfg.options();
    let _ = o.optional_bool("async_function_calling");
    let _ = o.optional_bool("context_window_compression");
    let _ = o.optional_bool("google_search_grounding");
    if let Some(budget) = o.value("thinking_budget")
        && !budget.as_i64().is_some_and(|b| (-1..=32_768).contains(&b))
    {
        o.error(
            "thinking_budget",
            "must be a whole number from -1 (dynamic) to 32768",
        );
    }
    if let Some(level) = o.string("thinking_level")
        && !matches!(level.as_str(), "minimal" | "low" | "medium" | "high")
    {
        o.error(
            "thinking_level",
            format!("must be \"minimal\", \"low\", \"medium\" or \"high\", got \"{level}\""),
        );
    }
    match o.value("input_transcription_language") {
        None | Some(serde_json::Value::String(_)) => {}
        Some(serde_json::Value::Array(codes)) if codes.iter().all(serde_json::Value::is_string) => {
        }
        Some(_) => o.error(
            "input_transcription_language",
            "must be a language code, \"auto\", or a list of language codes",
        ),
    }
    for key in ["start_sensitivity", "end_sensitivity"] {
        if let Some(value) = o.string(key)
            && !matches!(value.to_ascii_lowercase().as_str(), "high" | "low")
        {
            o.error(key, format!("must be \"high\" or \"low\", got \"{value}\""));
        }
    }
    o.into_errors()
}
