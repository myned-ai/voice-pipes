//! The `TtsFactory` that validates a config and builds the provider, and
//! the parameter schema the console shows for it.

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{TtsFactory, TtsProvider};

use crate::config::{GeminiConfig, GeminiOptions};
use crate::provider::{GeminiTtsProvider, SAMPLE_RATE};

// ── Factory ─────────────────────────────────────────────────

/// Factory for the Gemini TTS provider.
#[derive(Debug)]
pub(crate) struct GeminiTtsFactory;

impl TtsFactory for GeminiTtsFactory {
    fn output_sample_rate(&self, _cfg: &TtsConfig) -> Option<u32> {
        Some(SAMPLE_RATE)
    }

    fn name(&self) -> &'static str {
        "gemini"
    }

    fn is_local(&self) -> bool {
        false
    }

    fn default_api_key_env(&self) -> Option<&'static str> {
        Some("GOOGLE_API_KEY")
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        let mut errors = vec![];

        if voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).is_none() {
            errors.push(voice_pipes_sdk::ConfigError::new(
                "api_key",
                "Gemini TTS requires `api_key` or GOOGLE_API_KEY env var",
            ));
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
        if let Err(problems) = GeminiOptions::read(cfg) {
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
        let mut config = GeminiConfig::from_tts_config(cfg)?;
        // Adopt the shared voice cell from the param store when present, so
        // an edit from the console applies to the next synthesis.
        config.voice = voice_pipes_sdk::tts::adopt_voice(ctx.params, config.voice);

        let client = voice_pipes_net::http_client(&voice_pipes_net::HttpOptions::request(
            config.request_timeout,
        ))
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;

        tracing::info!(
            model = %config.model_name,
            voice = %config.voice.load(),
            endpoint = %config.endpoint,
            "Gemini TTS provider initialized"
        );

        Ok(Box::new(GeminiTtsProvider {
            client,
            config,
            audit_tx,
            core: std::sync::Arc::new(ComponentCore::new("tts:gemini", ComponentKind::Tts)),
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
                    "Gemini TTS model identifier (e.g. `gemini-2.5-flash-preview-tts`).",
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
                    "Custom Gemini API endpoint (leave unset for default).",
                ),
                serde_json::json!(e),
            );
        }
        s.push(
            ParamDescriptor::static_bool(
                "api_key_set",
                "API key",
                "Whether an API key is configured (env: `GOOGLE_API_KEY`).",
            ),
            serde_json::json!(cfg.api_key.is_some()),
        );
        s.passthrough_options(&cfg.options, &[], "Gemini TTS option.");
        s
    }
}

/// What a binary registers to make `provider = "gemini"` available:
/// `voice_pipes::tts::register_tts_factory(voice_pipes_tts_gemini::factory)`.
pub fn factory() -> Box<dyn TtsFactory> {
    Box::new(GeminiTtsFactory)
}
