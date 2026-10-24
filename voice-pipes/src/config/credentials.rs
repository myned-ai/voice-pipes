//! Credentials reach a provider one way: the engine resolves each key
//! here, from the config or the environment variable the provider names,
//! and hands the provider a config whose `api_key` is already set. No
//! provider reads the environment for a key, so a provider's tests supply
//! one in the config and never depend on the machine they run on.

use voice_pipes_sdk::secrets::resolve_key;
use voice_pipes_sdk::{SttConfig, SttFactory, TextProcessorConfig, TextProcessorFactory};
use voice_pipes_sdk::{TtsConfig, TtsFactory};

/// An environment lookup: the process's, or a test's.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The process environment, as an [`EnvLookup`].
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// `cfg` with its key resolved: the configured one when not blank, else
/// the factory's variable, else none.
pub fn stt(cfg: &SttConfig, factory: &dyn SttFactory, env: EnvLookup<'_>) -> SttConfig {
    let mut cfg = cfg.clone();
    cfg.api_key = resolve_key(cfg.api_key.as_ref(), factory.default_api_key_env(), env);
    cfg
}

/// [`stt`] for a TTS section.
pub fn tts(cfg: &TtsConfig, factory: &dyn TtsFactory, env: EnvLookup<'_>) -> TtsConfig {
    let mut cfg = cfg.clone();
    cfg.api_key = resolve_key(cfg.api_key.as_ref(), factory.default_api_key_env(), env);
    cfg
}

/// [`stt`] for a text processor, whose `api_key_env` may name another
/// variable than the factory's.
pub fn text_processor(
    cfg: &TextProcessorConfig,
    factory: &dyn TextProcessorFactory,
    env: EnvLookup<'_>,
) -> TextProcessorConfig {
    let mut cfg = cfg.clone();
    let var = cfg
        .api_key_env
        .as_deref()
        .or_else(|| factory.default_api_key_env());
    cfg.api_key = resolve_key(cfg.api_key.as_ref(), var, env);
    cfg
}

#[cfg(test)]
#[path = "tests/credentials.rs"]
mod tests;
