//! `AssemblyAI` V3 streaming speech-to-text provider.
//!
//! This module connects the pipeline to `AssemblyAI`'s real-time
//! transcription service via WebSocket, supporting turn detection,
//! keyterms prompting, and automatic reconnection.

#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_truncation
    )
)]

mod config;
mod messages;
mod provider;
mod schema;
mod ws_client;

pub(crate) use config::AssemblyAiConfig;
pub(crate) use provider::AssemblyAiStt;

use voice_pipes_sdk::{ConfigError, SttConfig};
use voice_pipes_sdk::{SttFactory, SttProvider};

/// Factory for creating [`AssemblyAiStt`] provider instances.
#[derive(Debug)]
pub(crate) struct AssemblyAiFactory;

impl SttFactory for AssemblyAiFactory {
    fn name(&self) -> &'static str {
        "assemblyai"
    }

    fn aliases(&self) -> &[&str] {
        &["assembly-ai", "aai"]
    }

    fn is_local(&self) -> bool {
        false
    }

    fn default_api_key_env(&self) -> Option<&'static str> {
        Some(crate::config::API_KEY_ENV_VAR)
    }

    fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError> {
        let mut errors = Vec::new();

        // API key: must be in config or environment.
        if voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).is_none() {
            errors.push(ConfigError::new(
                "api_key",
                "AssemblyAI requires `api_key` or ASSEMBLYAI_API_KEY env var",
            ));
        }

        if let Some(ep) = cfg.endpoint.as_deref()
            && let Some(e) = voice_pipes_net::credential_endpoint_error(
                "stt.endpoint",
                ep,
                voice_pipes_sdk::secrets::configured_key(cfg.api_key.as_ref()).is_some(),
                &cfg.options,
            )
        {
            errors.push(e);
        }

        // model_name is required (speech_model in the API).
        if cfg.model_name.is_none() {
            errors.push(ConfigError::new(
                "model_name",
                "AssemblyAI requires `model_name` (e.g. \"u3-rt-pro\")",
            ));
        }

        // Every option problem, whether or not a key is set here.
        if let Err(problems) = config::AaiOptions::read(cfg) {
            errors.extend(problems);
        }

        // Attempt full config parse to surface deeper issues.
        if errors.is_empty() {
            match AssemblyAiConfig::from_stt_config(cfg) {
                Ok(ac) => errors.extend(ac.validate()),
                Err(e) => errors.push(ConfigError::new("stt", e.to_string())),
            }
        }

        errors
    }

    fn build(
        &self,
        cfg: &SttConfig,
        ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let audit_tx = ctx.audit_tx.clone();
        voice_pipes_net::install_crypto_provider();

        let ac = AssemblyAiConfig::from_stt_config(cfg)?;

        let validation_errors = ac.validate();
        if !validation_errors.is_empty() {
            return Err(validation_errors.into());
        }

        Ok(Box::new(AssemblyAiStt::new(ac).with_audit_tx(audit_tx)))
    }

    fn provider_schema(&self, cfg: &SttConfig) -> voice_pipes_sdk::ProviderSchema {
        schema::schema(cfg)
    }
}

/// What a binary registers to make `provider = "assemblyai"` available:
/// `voice_pipes::stt::register_stt_factory(voice_pipes_stt_assemblyai::factory)`.
pub fn factory() -> Box<dyn SttFactory> {
    Box::new(AssemblyAiFactory)
}

/// How this provider offers itself in the setup wizard.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};
    vec![
        WizardChoice::new(
            "AssemblyAI (Cloud API)",
            WizardDeploy::Cloud {
                key_env: "ASSEMBLYAI_API_KEY".into(),
            },
        )
        .with_feature(Some("stt-assemblyai".into()))
        .with_fields(vec![
            WizardField::fixed("model_name", "u3-rt-pro"),
            WizardField::secret("ASSEMBLYAI_API_KEY", "Enter ASSEMBLYAI_API_KEY"),
        ])
        .with_stage_default(true)
        .with_menu_rank(30),
    ]
}

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;
