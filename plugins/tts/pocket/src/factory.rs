//! The `TtsFactory`: config validation, building the provider and its HTTP
//! client, and the parameter schema the live editor shows.

use tracing::{info, warn};

use voice_pipes_sdk::component::{ComponentCore, ComponentKind};
use voice_pipes_sdk::{TtsConfig, TtsFactory, TtsProvider};

use crate::config::{
    DEFAULT_ENDPOINT, DEFAULT_SILENT_START_MS, DEFAULT_SILENT_START_RETRIES, PocketHttpConfig,
    PocketOptions, SAMPLE_RATE,
};
use crate::provider::PocketHttpTts;

// ── Factory ─────────────────────────────────────────────────

/// Factory for the Pocket-TTS HTTP provider.
#[derive(Debug)]
pub(crate) struct PocketHttpTtsFactory;

impl TtsFactory for PocketHttpTtsFactory {
    fn output_sample_rate(&self, _cfg: &TtsConfig) -> Option<u32> {
        Some(SAMPLE_RATE)
    }

    fn name(&self) -> &'static str {
        "pocket-http"
    }

    fn is_local(&self) -> bool {
        // Local sidecar process -- no API key required.
        true
    }

    fn validate(&self, cfg: &TtsConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        let mut errors = vec![];
        if let Some(ep) = cfg.endpoint.as_deref()
            && !(ep.starts_with("http://") || ep.starts_with("https://"))
        {
            errors.push(voice_pipes_sdk::ConfigError::new(
                "endpoint",
                format!("pocket-http: endpoint must start with http:// or https:// (got {ep:?})"),
            ));
        }
        if let Some(voice) = cfg.voice.as_deref()
            && voice.is_empty()
        {
            errors.push(voice_pipes_sdk::ConfigError::new(
                "voice",
                "pocket-http: voice must not be empty",
            ));
        }
        if let Err(problems) = PocketOptions::read(cfg) {
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
        let mut config = PocketHttpConfig::from_tts_config(cfg)?;

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
            endpoint = %config.endpoint,
            voice = %*config.current_voice(),
            timeout_ms = u64::try_from(config.request_timeout.as_millis()).unwrap_or(u64::MAX),
            "Pocket-TTS HTTP provider initialized"
        );
        if config.endpoint == DEFAULT_ENDPOINT {
            warn!(
                "pocket-http: using default endpoint {DEFAULT_ENDPOINT}; \
                 ensure `pocket-tts serve` is running on the same host"
            );
        }

        Ok(Box::new(PocketHttpTts {
            client,
            config,
            audit_tx,
            core: std::sync::Arc::new(ComponentCore::new("tts:pocket-http", ComponentKind::Tts)),
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
        if let Some(e) = &cfg.endpoint {
            s.push(
                ParamDescriptor::static_str(
                    "endpoint",
                    "Endpoint",
                    "URL of the local pocket-tts HTTP sidecar.",
                ),
                serde_json::json!(e),
            );
        }
        if let Some(v) = &cfg.voice {
            s.push(
                ParamDescriptor::mutable_str(
                    "voice",
                    "Voice",
                    "Default voice / speaker ID. Changes apply on the next synthesis call.",
                ),
                serde_json::json!(v),
            );
        }
        if let Some(v) = cfg.options.get("request_timeout_ms") {
            s.push(
                // Fixed when the HTTP client is built, so changed by a
                // restart, not live.
                ParamDescriptor::static_int_display(
                    "request_timeout_ms",
                    "Request timeout (ms)",
                    "Longest wait for the sidecar's response or its next audio; a reply streams for as long as it keeps coming.",
                ),
                v.clone(),
            );
        }
        s.push(
            ParamDescriptor::static_int_display(
                "silent_start_ms",
                "Silent-start window (ms)",
                "Re-synthesize a fragment when the sidecar produces no sound in its \
                 first N ms (sampling checkpoints occasionally run to the length cap \
                 in silence). 0 disables.",
            ),
            cfg.options
                .get("silent_start_ms")
                .cloned()
                .unwrap_or(serde_json::json!(DEFAULT_SILENT_START_MS)),
        );
        s.push(
            ParamDescriptor::static_int_display(
                "silent_start_retries",
                "Silent-start retries",
                "Extra synthesis attempts after a silent start; the last one always \
                 streams to completion.",
            ),
            cfg.options
                .get("silent_start_retries")
                .cloned()
                .unwrap_or(serde_json::json!(DEFAULT_SILENT_START_RETRIES)),
        );
        s.passthrough_options(
            &cfg.options,
            &[
                "request_timeout_ms",
                "silent_start_ms",
                "silent_start_peak",
                "silent_start_retries",
            ],
            "pocket-http-specific option.",
        );
        s
    }
}
