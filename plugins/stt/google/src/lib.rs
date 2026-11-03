//! Google Cloud Speech-to-Text v2 streaming recogniser (`provider = "google"`),
//! as a plugin crate on `voice-pipes-sdk`.
//!
//! Speaks `google.cloud.speech.v2.Speech/StreamingRecognize` over gRPC with
//! a trimmed, hand-maintained proto (`proto/google_speech_v2.proto`). Adds
//! the languages no other provider covers — Croatian via `chirp_3` first.
//!
//! The binary that links this crate registers it:
//! `voice_pipes::stt::register_stt_factory(voice_pipes_stt_google::factory)`.

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

mod auth;
mod client;
mod config;
mod provider;
mod schema;

pub(crate) use config::GoogleSttConfig;
pub(crate) use provider::GoogleStt;

use voice_pipes_sdk::config::{ConfigError, SttConfig};
use voice_pipes_sdk::schema::ProviderSchema;
use voice_pipes_sdk::stt::{SttFactory, SttProvider};
use voice_pipes_sdk::wizard::{WizardChoice, WizardDeploy, WizardField};

/// Factory for [`GoogleStt`] provider instances.
#[derive(Debug)]
pub(crate) struct GoogleSttFactory;

impl SttFactory for GoogleSttFactory {
    fn name(&self) -> &'static str {
        "google"
    }

    fn aliases(&self) -> &[&str] {
        &["google-stt", "gcp-speech", "chirp"]
    }

    fn is_local(&self) -> bool {
        false
    }

    fn default_api_key_env(&self) -> Option<&'static str> {
        Some(crate::config::ACCESS_TOKEN_ENV_VAR)
    }

    fn validate(&self, cfg: &SttConfig) -> Vec<ConfigError> {
        match GoogleSttConfig::parse(cfg) {
            Ok(gc) => config_errors(&gc, cfg),
            Err(problems) => problems,
        }
    }

    fn build(
        &self,
        cfg: &SttConfig,
        ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(self.validate(cfg))?;
        let audit_tx = ctx.audit_tx.clone();
        // tonic's TLS and gcp_auth's HTTP client both go through rustls;
        // pin the process-wide crypto provider before either builds a
        // ClientConfig (a no-op if something already installed one).
        voice_pipes_net::install_crypto_provider();

        let gc = GoogleSttConfig::from_stt_config(cfg)?;
        let validation_errors = config_errors(&gc, cfg);
        if !validation_errors.is_empty() {
            return Err(validation_errors.into());
        }

        Ok(Box::new(GoogleStt::new(gc).with_audit_tx(audit_tx)))
    }

    fn provider_schema(&self, cfg: &SttConfig) -> ProviderSchema {
        schema::schema(cfg)
    }
}

/// The parsed config's own checks, plus the rule every hosted provider
/// follows: the OAuth token is sent on every stream, so a plain-HTTP
/// endpoint on an internet host is refused unless the operator opts in with
/// `allow_insecure_endpoint`.
fn config_errors(gc: &GoogleSttConfig, cfg: &SttConfig) -> Vec<ConfigError> {
    let mut errors = gc.validate();
    if let Some(e) =
        voice_pipes_net::credential_endpoint_error("stt.endpoint", &gc.endpoint, true, &cfg.options)
    {
        errors.push(e);
    }
    errors
}

/// What a binary registers:
/// `voice_pipes::stt::register_stt_factory(voice_pipes_stt_google::factory)`.
pub fn factory() -> Box<dyn SttFactory> {
    Box::new(GoogleSttFactory)
}

/// The setup wizard's entry. Authentication is Application Default
/// Credentials unless a service-account key path is given, so there is
/// no API key to ask for or store.
pub fn wizard_choices() -> Vec<voice_pipes_sdk::WizardChoice> {
    vec![WizardChoice::new("Google Cloud STT — Chirp 3 (Cloud API, 100+ languages)", WizardDeploy::Cloud {
            key_env: config::CREDENTIALS_ENV_VAR.into(),
        }).with_feature(Some("stt-google".into())).with_fields(vec![
            WizardField::fixed("model_name", "chirp_3"),
            WizardField::text("location", "Google Cloud location (chirp_3: us or eu)", "us"),
            WizardField::text(
                "project_id",
                "Google Cloud project id (blank = from credentials / GOOGLE_CLOUD_PROJECT)",
                "",
            )
            .optional(),
            WizardField::list(
                "language_codes",
                "Language codes, comma-separated (BCP-47)",
                "en-US",
            ),
            WizardField::text(
                "credentials_path",
                "Service-account key path as seen by the worker (blank = Application Default Credentials)",
                "",
            )
            .optional(),
        ]).with_notes(vec![
            "Google STT authenticates with Application Default Credentials unless".into(),
            "credentials_path points at a service-account key. In the Docker stack,".into(),
            "put the key under models/ (mounted at /app/models) and use a relative path".into(),
            "such as models/gcp/service-account.json. Enable speech.googleapis.com first.".into(),
        ]).with_menu_rank(10)]
}

#[cfg(test)]
#[path = "tests/wizard.rs"]
mod wizard_tests;

#[cfg(test)]
#[path = "tests/endpoint.rs"]
mod endpoint_tests;
