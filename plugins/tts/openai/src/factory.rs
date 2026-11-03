//! The `TtsFactory`: config validation, provider construction, and the
//! schema the live parameter editor shows.

use tracing::info;

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{TtsFactory, TtsProvider};

use crate::config::{OpenAiConfig, OpenAiOptions};
use crate::provider::{OpenAiTts, SAMPLE_RATE};

/// Factory for the `OpenAI` TTS provider.
#[derive(Debug)]
pub(crate) struct OpenAiTtsFactory;

impl TtsFactory for OpenAiTtsFactory {
    fn output_sample_rate(&self, _cfg: &TtsConfig) -> Option<u32> {
        Some(SAMPLE_RATE)
    }

    fn name(&self) -> &'static str {
        "openai"
    }

    fn is_local(&self) -> bool {
        false
    }

    fn default_api_key_env(&self) -> Option<&'static str> {
        Some("OPENAI_API_KEY")
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        let mut errors = vec![];

        let is_openai = cfg
            .endpoint
            .as_deref()
            .is_none_or(|ep| ep.contains("api.openai.com"));

        if is_openai && voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).is_none() {
            errors.push(voice_pipes_sdk::ConfigError::new("api_key", "OpenAI TTS requires `api_key` or OPENAI_API_KEY env var when using the default OpenAI endpoint"));
        }

        if let Some(ep) = cfg.endpoint.as_deref()
            && let Some(e) = voice_pipes_net::credential_endpoint_error(
                "tts.endpoint",
                ep,
                voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).is_some(),
                &cfg.options,
            )
        {
            errors.push(e);
        }
        if let Err(problems) = OpenAiOptions::read(cfg) {
            errors.extend(problems);
        }
        errors
    }

    fn build(
        &self,
        cfg: &TtsConfig,
        ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn TtsProvider>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let audit_tx = ctx.audit_tx.clone();
        let params = ctx.params;
        let mut config = OpenAiConfig::from_tts_config(cfg)?;

        // Adopt the shared voice cell from the param store when present so
        // PATCH /debug/params updates take effect on the next synthesis call.
        config.voice = voice_pipes_sdk::tts::adopt_voice(params, config.voice);

        // The reply is read as it plays, so it lasts as long as its audio: the
        // timeout bounds each wait for more bytes, not the whole reply.
        let client = voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::streaming(
            config.request_timeout,
        ))
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;

        info!(
            model = %config.model_name,
            voice = %*config.current_voice(),
            endpoint = %config.endpoint,
            "OpenAI TTS provider initialized"
        );

        Ok(Box::new(OpenAiTts {
            client,
            config,
            audit_tx,
            core: std::sync::Arc::new(ComponentCore::new("tts:openai", ComponentKind::Tts)),
            resolved_model: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
        }))
    }

    fn provider_schema(&self, cfg: &TtsConfig) -> voice_pipes_sdk::ProviderSchema {
        use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};
        let mut s = ProviderSchema::default();
        s.push(
            ParamDescriptor::static_str(
                "provider",
                "Provider",
                "The TTS framework or service used for speech synthesis.",
            ),
            serde_json::json!(cfg.provider),
        );
        if let Some(n) = &cfg.model_name {
            s.push(
                ParamDescriptor::static_str(
                    "model_name",
                    "Model",
                    "OpenAI TTS model identifier (e.g. `tts-1`, `tts-1-hd`).",
                ),
                serde_json::json!(n),
            );
        }
        if let Some(v) = &cfg.voice {
            s.push(
                ParamDescriptor::mutable_str(
                    "voice",
                    "Voice",
                    "Default voice ID. Changes apply on the next synthesis call.",
                ),
                serde_json::json!(v),
            );
        }
        if let Some(e) = &cfg.endpoint {
            s.push(
                ParamDescriptor::static_str(
                    "endpoint",
                    "Endpoint",
                    "Custom OpenAI API endpoint (leave unset for default).",
                ),
                serde_json::json!(e),
            );
        }
        s.push(
            ParamDescriptor::static_bool(
                "api_key_set",
                "API key",
                "Whether an API key is configured (env: `OPENAI_API_KEY`).",
            ),
            serde_json::json!(cfg.api_key.is_some()),
        );
        s.passthrough_options(&cfg.options, &["speed"], "OpenAI TTS option.");
        s
    }
}
