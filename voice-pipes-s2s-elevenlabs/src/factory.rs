//! Registration and construction of the `ElevenLabs` Agents protocol.
//!
//! The conversational brain (persona, LLM, voice, VAD, tool definitions)
//! lives in the `ElevenLabs` *agent* config, so the only required setting
//! here is the agent id: either a literal `[realtime_provider.options]
//! agent_id` or, when that is absent, the environment variable named by
//! `options.agent_id_env` (default `ELEVENLABS_AGENT_ID`), mirroring the
//! `api_key_env` convention.

use anyhow::Result;
use voice_pipes_sdk::s2s::{RealtimeConfig, RealtimeFactory, RealtimeProtocol};
use voice_pipes_sdk::{BuildContext, ConfigError};

use crate::{ElevenLabsProtocol, Env, process_env};

/// Factory for [`ElevenLabsProtocol`].
#[derive(Debug)]
pub struct ElevenLabsFactory;

/// Environment variable consulted for the agent id when `options.agent_id`
/// is not set.
const DEFAULT_AGENT_ID_ENV: &str = "ELEVENLABS_AGENT_ID";

/// The agent id: a literal `options.agent_id` wins; otherwise the variable
/// named by `options.agent_id_env` (default [`DEFAULT_AGENT_ID_ENV`]).
fn resolve_agent_id(cfg: &RealtimeConfig, env: Env<'_>) -> Option<String> {
    let mut o = cfg.options();
    if let Some(id) = o
        .string("agent_id")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(id);
    }
    let env_name = o
        .string("agent_id_env")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_AGENT_ID_ENV.to_string());
    env(&env_name)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The accepted range of `options.completion_idle_ms`.
const COMPLETION_IDLE_MS: std::ops::RangeInclusive<u64> = 200..=10_000;

/// The options `build` and the session setup read, typed and bounded;
/// any other key is refused (the ones the engine's session runner reads
/// are its own).
fn option_errors(cfg: &RealtimeConfig) -> Vec<ConfigError> {
    let mut o = cfg.options();
    let _ = o.string("agent_id");
    let _ = o.string("agent_id_env");
    let _ = o.string("endpoint");
    let _ = o.optional_bool("public_agent");
    let _ = o.optional_u64_in("completion_idle_ms", COMPLETION_IDLE_MS);
    let _ = o.optional_bool("override_prompt");
    let _ = o.string("language");
    let _ = o.string("first_message");
    let _ = o.string("user_id");
    if o.value("dynamic_variables").is_some_and(|v| !v.is_object()) {
        o.error("dynamic_variables", "must be a table of names to values");
    }
    o.into_errors()
}

impl ElevenLabsFactory {
    /// [`RealtimeFactory::validate`], reading the environment through `env`.
    pub(crate) fn validate_with(cfg: &RealtimeConfig, env: Env<'_>) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        if resolve_agent_id(cfg, env).is_none() {
            errors.push(ConfigError::new(
                "realtime_provider.options.agent_id",
                "ElevenLabs requires the id of an agent created in the \
                          ElevenLabs dashboard (agent_id = \"agent_...\", or set the \
                          ELEVENLABS_AGENT_ID environment variable / options.agent_id_env)",
            ));
        }
        // Public agents accept anonymous connections; opt out of the
        // key requirement with `options.public_agent = true`.
        let mut o = cfg.options();
        let public_agent = o.bool("public_agent", false);
        if let Some(ep) = o.string("endpoint")
            && let Some(e) = voice_pipes_net::credential_endpoint_error(
                "realtime_provider.options.endpoint",
                &ep,
                !public_agent && cfg.api_key.is_some(),
                &cfg.options,
            )
        {
            errors.push(e);
        }
        if !public_agent && let Some(mut e) = cfg.missing_key_error("ElevenLabs") {
            e.message.push_str(
                " (or [realtime_provider.options] public_agent = true for a public agent)",
            );
            errors.push(e);
        }
        errors.extend(option_errors(cfg));
        errors
    }

    /// [`RealtimeFactory::build`], reading the environment through `env`.
    pub(crate) fn build_with(
        cfg: &RealtimeConfig,
        env: Env<'_>,
    ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes_sdk::BuildError> {
        voice_pipes_sdk::options::validated(Self::validate_with(cfg, env))?;
        let agent_id = resolve_agent_id(cfg, env).ok_or_else(|| {
            voice_pipes_sdk::BuildError::failed(
                "elevenlabs requires an agent id -- set [realtime_provider.options] \
                 agent_id, or the ELEVENLABS_AGENT_ID environment variable \
                 (or the variable named by options.agent_id_env)",
            )
        })?;
        let mut p = ElevenLabsProtocol::new(agent_id);
        // A public agent connects anonymously. `validate_with` lets a
        // plain `ws://` endpoint through for one on that basis, so a key
        // resolved from the environment is never attached to its handshake.
        let mut o = cfg.options();
        if !o.bool("public_agent", false) {
            p = p.with_api_key(cfg.api_key.clone());
        }
        if let Some(endpoint) = o.string("endpoint") {
            p = p.with_endpoint(endpoint);
        }
        if let Some(ms) = o.optional_u64_in("completion_idle_ms", COMPLETION_IDLE_MS) {
            p = p.with_completion_idle_ms(ms);
        }
        // `model` overrides the agent's LLM (`agent.prompt.llm`) — the
        // "LLM" override must be enabled in the agent's Security tab.
        p = p.with_llm_override(cfg.model.clone());
        Ok(Box::new(p))
    }
}

impl RealtimeFactory for ElevenLabsFactory {
    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    fn aliases(&self) -> &[&str] {
        &["elevenlabs-agents", "11labs"]
    }

    fn default_api_key_env(&self) -> &'static str {
        "ELEVENLABS_API_KEY"
    }

    fn validate(&self, cfg: &RealtimeConfig) -> Vec<ConfigError> {
        Self::validate_with(cfg, &process_env)
    }

    fn build(
        &self,
        cfg: &RealtimeConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes_sdk::BuildError> {
        Self::build_with(cfg, &process_env)
    }
}
