//! Registration and construction of a speech-to-speech provider.
//!
//! A provider crate exports a [`RealtimeFactory`]; the binary registers it
//! with the engine, which resolves `[realtime_provider] provider = "..."`
//! by [`name`](RealtimeFactory::name) or alias and builds the protocol per
//! session from a [`RealtimeConfig`].

use std::collections::HashMap;

use super::protocol::RealtimeProtocol;
use crate::build::BuildContext;
use crate::config::ConfigError;
use crate::secrets::Secret;

/// The `[realtime_provider]` settings a provider builds from. Carries only
/// what the provider reads; the engine keeps its own session settings
/// (instructions, client overrides, the uplink observer) to itself.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct RealtimeConfig {
    /// The name the config selected the provider by (its name or an alias).
    pub provider: String,
    /// Provider-specific model identifier.
    pub model: Option<String>,
    /// Provider-specific voice.
    pub voice: Option<String>,
    /// The API key, which the engine reads from the configured variable (or
    /// the provider's [`default_api_key_env`](RealtimeFactory::default_api_key_env));
    /// `None` when that variable is unset or blank. A provider never reads
    /// the environment for it.
    pub api_key: Option<Secret>,
    /// The variable the key was read from, for messages that tell the
    /// operator what to set.
    pub api_key_var: String,
    /// `[realtime_provider.options]`: provider-specific keys.
    pub options: HashMap<String, serde_json::Value>,
}

/// The key is a [`Secret`] already, and an option may carry a credential,
/// so `Debug` redacts secret-named options too.
impl std::fmt::Debug for RealtimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("api_key", &self.api_key)
            .field("api_key_var", &self.api_key_var)
            .field("options", &crate::secrets::debug_options(&self.options))
            .finish()
    }
}

impl RealtimeConfig {
    /// A reader over this section's options (`realtime_provider.<key>` in
    /// errors), with the keys the engine itself reads already accounted for.
    pub fn options(&self) -> crate::options::ProviderOptions<'_> {
        crate::options::ProviderOptions::new("realtime_provider.options", &self.options)
            .with_engine_keys(&[crate::options::ALLOW_INSECURE_OPTION])
            .with_engine_keys(crate::options::REALTIME_ENGINE_OPTIONS)
    }

    /// Settings for `provider`, everything else unset.
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            ..Self::default()
        }
    }

    with_setters! {
        with_model => model: Option<String>,
        with_voice => voice: Option<String>,
        with_api_key => api_key: Option<Secret>,
        with_api_key_var => api_key_var: String,
        with_options => options: HashMap<String, serde_json::Value>,
    }

    /// The error for a missing API key, naming `service` and the variable to
    /// set; `None` when the key is present.
    pub fn missing_key_error(&self, service: &str) -> Option<ConfigError> {
        self.api_key.is_none().then(|| {
            ConfigError::new(
                "realtime_provider.api_key_env",
                format!(
                    "{service} requires the {} environment variable to be set",
                    self.api_key_var
                ),
            )
        })
    }

    /// `options[key]` as a string, when it is one.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, which reports a value of the wrong type instead of ignoring it"
    )]
    pub fn option_str(&self, key: &str) -> Option<&str> {
        self.options.get(key).and_then(serde_json::Value::as_str)
    }

    /// `options[key]` as a boolean, when it is one.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, which reports a value of the wrong type instead of ignoring it"
    )]
    pub fn option_bool(&self, key: &str) -> Option<bool> {
        self.options.get(key).and_then(serde_json::Value::as_bool)
    }

    /// `options[key]` as an unsigned integer, when it is one.
    #[deprecated(
        since = "1.1.0",
        note = "use `options()`, which reports a value of the wrong type instead of ignoring it"
    )]
    pub fn option_u64(&self, key: &str) -> Option<u64> {
        self.options.get(key).and_then(serde_json::Value::as_u64)
    }
}

/// Registration metadata and construction for one speech-to-speech
/// provider.
pub trait RealtimeFactory: Send + Sync {
    /// Canonical config name (e.g. `"openai-realtime"`).
    fn name(&self) -> &'static str;

    /// Other names the config may use.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Whether `n` is this provider's name or one of its aliases.
    fn matches(&self, n: &str) -> bool {
        self.name() == n || self.aliases().contains(&n)
    }

    /// Environment variable the engine reads the API key from unless the
    /// config names another.
    ///
    /// A provider that needs no key still names one here: the method is
    /// required in 1.x. Return a name of its own (`"MYVENDOR_API_KEY"`)
    /// and do not call [`RealtimeConfig::missing_key_error`] in `validate`;
    /// the key is then simply `None` when the variable is unset. The
    /// method becomes optional in 2.0.
    fn default_api_key_env(&self) -> &'static str;

    /// Check the settings before any session starts. Empty = valid.
    fn validate(&self, cfg: &RealtimeConfig) -> Vec<ConfigError>;

    /// Build the protocol mapper for one session.
    ///
    /// # Errors
    /// When the settings cannot produce a working protocol (a missing
    /// required option, say).
    fn build(
        &self,
        cfg: &RealtimeConfig,
        ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn RealtimeProtocol>, crate::BuildError>;
}

#[cfg(test)]
#[path = "../tests/s2s_factory.rs"]
mod tests;
