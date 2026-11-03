//! Config-driven construction of the OpenAI-compatible text processor.

use super::config::{self, OpenAiCompatConfig};
use super::{OpenAiCompatProcessor, schema};

/// Factory for constructing `OpenAiCompatProcessor` instances from TOML config.
#[derive(Debug)]
pub struct OpenAiCompatFactory;

impl voice_pipes_sdk::TextProcessorFactory for OpenAiCompatFactory {
    fn name(&self) -> &'static str {
        "openai-compat"
    }

    fn is_local(&self) -> bool {
        false
    }

    fn default_api_key_env(&self) -> Option<&'static str> {
        Some(crate::config::API_KEY_ENV_VAR)
    }

    fn validate(
        &self,
        cfg: &voice_pipes_sdk::TextProcessorConfig,
    ) -> Vec<voice_pipes_sdk::ConfigError> {
        config::validate(cfg)
    }

    fn build(
        &self,
        cfg: &voice_pipes_sdk::TextProcessorConfig,
        ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::TextProcessor>, voice_pipes_sdk::BuildError> {
        let audit_tx = ctx.audit_tx.clone();
        let param_store = ctx.params;
        let text_chunking = &ctx.text_chunking;
        let parsed =
            OpenAiCompatConfig::from_config(cfg).map_err(voice_pipes_sdk::options::refused)?;

        tracing::info!(
            base_url = %parsed.base_url,
            model = %parsed.model,
            has_api_key = !parsed.api_key.is_empty(),
            "building OpenAI-compat text processor"
        );

        let mut processor = OpenAiCompatProcessor::new(parsed, audit_tx, text_chunking.clone())?;

        // Adopt cells from the worker-wide ParamRegistry so PATCH
        // /debug/params updates take effect on the next turn -- same
        // pattern as the local `LlmFactory`.
        if let Some(store) = param_store {
            if let Some(cell) = store.int_cell("max_tokens") {
                processor = processor.with_max_tokens_cell(cell);
            }
            if let Some(cell) = store.float_cell("temperature") {
                processor = processor.with_temperature_cell(cell);
            }
            if let Some(cell) = store.float_cell("top_p") {
                processor = processor.with_top_p_cell(cell);
            }
            if let Some(cell) = store.string_cell("system_prompt") {
                processor = processor.with_system_prompt_cell(cell);
            }
            if let Some(cell) = store.string_cell("stop") {
                processor = processor.with_stop_cell(cell);
            }
        }

        Ok(Box::new(processor))
    }

    fn provider_schema(
        &self,
        cfg: &voice_pipes_sdk::TextProcessorConfig,
    ) -> voice_pipes_sdk::schema::ProviderSchema {
        schema::schema(cfg)
    }
}
