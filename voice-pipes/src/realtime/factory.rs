//! Registry of speech-to-speech providers.
//!
//! Each provider crate exports a [`RealtimeFactory`]; the binary registers it
//! with [`register_realtime_factory`] before the engine runs, and the runner
//! resolves `[realtime_provider] provider = "..."` by name or alias.

use crate::config::RealtimeProviderConfig;

pub use voice_pipes_sdk::s2s::{RealtimeConfig, RealtimeFactory};

type Ctor = fn() -> Box<dyn RealtimeFactory>;

/// The engine has no speech-to-speech provider of its own: each is a
/// plugin crate the binary registers.
pub(crate) static REALTIME_FACTORIES: crate::plugins::Registry<dyn RealtimeFactory> =
    crate::plugins::Registry::new("speech-to-speech", Vec::new);

/// Register a speech-to-speech provider from outside the engine, so a
/// `[realtime_provider]` section can select it by name or alias. Call once
/// at startup, before any session starts. Thread-safe.
///
/// # Errors
/// When the factory's name or an alias already names a registered provider:
/// one would shadow the other.
pub fn register_realtime_factory(ctor: Ctor) -> Result<(), crate::plugins::RegistrationError> {
    REALTIME_FACTORIES.register(ctor)
}

/// Look up a speech-to-speech factory by name or alias.
pub fn lookup_realtime_factory(name: &str) -> Option<std::sync::Arc<dyn RealtimeFactory>> {
    REALTIME_FACTORIES.lookup(name)
}

/// Every registered speech-to-speech factory.
pub fn realtime_factories() -> Vec<std::sync::Arc<dyn RealtimeFactory>> {
    REALTIME_FACTORIES.all()
}

/// The provider's view of a `[realtime_provider]` section: what it builds
/// from, without the engine's own session settings. The API key is read
/// here, from the configured variable or `default_key_var` (the provider's
/// own default), so a provider never reads the environment for it.
pub fn realtime_config(cfg: &RealtimeProviderConfig, default_key_var: &str) -> RealtimeConfig {
    let key_var = cfg
        .api_key_env
        .clone()
        .unwrap_or_else(|| default_key_var.to_string());
    RealtimeConfig::new(cfg.provider.clone())
        .with_model(cfg.model.clone())
        .with_voice(cfg.voice.clone())
        .with_api_key(voice_pipes_sdk::secrets::resolve_key(
            None,
            Some(&key_var),
            &crate::config::credentials::process_env,
        ))
        .with_api_key_var(key_var)
        .with_options(cfg.options.clone())
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/factory.rs"]
mod tests;
