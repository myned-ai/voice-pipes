//! The `TtsFactory` that validates config and builds the provider, and the
//! provider schema it reports.

use tracing::info;

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{TtsFactory, TtsProvider};

use crate::config::{DEFAULT_SAMPLE_RATE, ElevenLabsConfig, ElevenLabsOptions};
use crate::provider::ElevenLabsTts;

// ── Factory ─────────────────────────────────────────────────

/// Factory for the `ElevenLabs` TTS provider.
#[derive(Debug)]
pub(crate) struct ElevenLabsTtsFactory;

impl TtsFactory for ElevenLabsTtsFactory {
    /// The configured `sample_rate` option, or the default.
    fn output_sample_rate(&self, cfg: &TtsConfig) -> Option<u32> {
        Some(
            cfg.options
                .get("sample_rate")
                .and_then(serde_json::Value::as_u64)
                .map_or(DEFAULT_SAMPLE_RATE, |r| {
                    u32::try_from(r).unwrap_or(u32::MAX)
                }),
        )
    }

    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    fn is_local(&self) -> bool {
        false
    }

    fn default_api_key_env(&self) -> Option<&'static str> {
        Some("ELEVENLABS_API_KEY")
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        let mut errors = vec![];

        if voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).is_none() {
            errors.push(voice_pipes_sdk::ConfigError::new(
                "api_key",
                "ElevenLabs TTS requires `api_key` or the ELEVENLABS_API_KEY env var",
            ));
        }

        if let Err(problems) = ElevenLabsOptions::read(cfg) {
            errors.extend(problems);
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
        let mut config = ElevenLabsConfig::from_tts_config(cfg)?;

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
            model = %config.model_id,
            voice = %*config.current_voice(),
            endpoint = %config.endpoint,
            sample_rate = config.sample_rate,
            word_timestamps = config.word_timestamps,
            "ElevenLabs TTS provider initialized"
        );

        Ok(Box::new(ElevenLabsTts {
            client,
            config,
            audit_tx,
            core: std::sync::Arc::new(ComponentCore::new("tts:elevenlabs", ComponentKind::Tts)),
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
                    "ElevenLabs model id (e.g. `eleven_flash_v2_5`, `eleven_multilingual_v2`).",
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
                    "Custom ElevenLabs API endpoint (regional residency; leave unset for default).",
                ),
                serde_json::json!(e),
            );
        }
        s.push(
            ParamDescriptor::static_bool(
                "api_key_set",
                "API key",
                "Whether an API key is configured (env: `ELEVENLABS_API_KEY`).",
            ),
            serde_json::json!(cfg.api_key.is_some()),
        );
        s.passthrough_options(
            &cfg.options,
            &[
                "sample_rate",
                "language_code",
                "speed",
                "stability",
                "similarity_boost",
                "style",
                "use_speaker_boost",
                "word_timestamps",
            ],
            "ElevenLabs TTS option.",
        );
        s
    }
}

/// What a binary registers to make `provider = "elevenlabs"` available:
/// `voice_pipes::tts::register_tts_factory(voice_pipes_tts_elevenlabs::factory)`.
pub fn factory() -> Box<dyn TtsFactory> {
    Box::new(ElevenLabsTtsFactory)
}
