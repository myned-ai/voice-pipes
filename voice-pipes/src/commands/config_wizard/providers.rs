//! The built-in providers' setup-wizard entries, and the state one wizard
//! run threads through its stages.
//!
//! A built-in entry is the same data a plugin's is (a [`WizardChoice`]), so
//! the wizard has one way to show and write a choice. Adding one: append it
//! to [`builtin_choices`] for its stage. `provider` is the value written as
//! `provider =` / `type =` / `mode =` and must resolve against the stage's
//! factories, and `feature` must be in the worker images' feature lists;
//! tests enforce both. Files the wizard downloads are a
//! `WizardDeploy::Registry` entry in `config/wizard_models.json`.

use anyhow::Result;
use voice_pipes_sdk::{WizardChoice, WizardDeploy, WizardField};

use super::prompt::Prompt;
use super::registry::Registry;
use crate::plugins::Stage;

/// The built-in entries of `stage`'s menu, each with the provider it
/// selects, in menu order. Each has a fixed rank below every plugin entry
/// the workspace has (5 and up), so plugins list after them; a provider that
/// moves into a plugin keeps the rank its entry had here (Parakeet's and
/// Piper's 1, Moonshine's 2). Only turn detection has built-ins left.
pub fn builtin_choices(stage: Stage) -> Vec<(String, WizardChoice)> {
    let turn = |label: &str, timeout_ms: &str, rank: u16| {
        WizardChoice::new(label, WizardDeploy::None)
            .with_fields(vec![WizardField::fixed_literal(
                "silence_timeout_ms",
                timeout_ms,
            )])
            .with_menu_rank(rank)
    };
    match stage {
        Stage::Turn => vec![
            (
                "layered".into(),
                turn(
                    "Layered (provider end-of-turn signal, else silence timeout) [default]",
                    "450",
                    1,
                ),
            ),
            (
                "silence".into(),
                turn("Silence (fixed silence timeout only)", "700", 2),
            ),
        ],
        // Every recogniser, voice, text processor and speech-to-speech
        // provider the wizard offers is a plugin's.
        Stage::Stt | Stage::Tts | Stage::Llm | Stage::Realtime => Vec::new(),
    }
}

/// Mutable state threaded through the emit functions of one wizard run.
pub struct EmitCtx<'a> {
    pub prompt: &'a dyn Prompt,
    pub registry: &'a Registry,
    /// The pipeline TOML being assembled.
    pub toml: String,
    /// API keys the user typed; saved to `.env` after the plan is built.
    pub env_to_save: Vec<(String, String)>,
}

impl std::fmt::Debug for EmitCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmitCtx").finish_non_exhaustive()
    }
}

impl<'a> EmitCtx<'a> {
    pub fn new(prompt: &'a dyn Prompt, registry: &'a Registry) -> Self {
        Self {
            prompt,
            registry,
            toml: String::new(),
            env_to_save: Vec::new(),
        }
    }

    fn push(&mut self, s: &str) {
        self.toml.push_str(s);
    }

    /// Append `key = ["a", "b"]` with every element rendered by the TOML
    /// serializer, so a quote, a backslash or a control character typed at a
    /// prompt lands in the file as data, never as syntax.
    fn push_kv_list(&mut self, key: &str, values: &[String]) {
        use std::fmt::Write;
        let items: Vec<String> = values.iter().map(|v| toml_str(v)).collect();
        let _ = writeln!(self.toml, "{key} = [{}]", items.join(", "));
    }

    /// The registry entry's `default_path`, or `fallback` when the entry is
    /// absent (e.g. a pruned external registry override).
    pub(super) fn default_path(&self, registry_key: &str, fallback: &str) -> String {
        self.registry
            .get(registry_key)
            .map_or_else(|| fallback.to_string(), |c| c.default_path.clone())
    }

    /// Queues an API key for `.env` persistence when the user typed one.
    /// The value is trimmed; a key containing whitespace or quotes is
    /// refused, since `.env` is read line by line by two launchers that
    /// would disagree on it.
    pub(super) fn save_key_if_set(&mut self, env_key: &str, value: &str) -> Result<()> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(());
        }
        if value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
        {
            anyhow::bail!(
                "{env_key}: an API key cannot contain spaces or quotes; paste the bare key"
            );
        }
        self.env_to_save
            .push((env_key.to_string(), value.to_string()));
        Ok(())
    }
}

/// A string rendered as a TOML string literal (quoted and escaped).
pub(super) fn toml_str(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

// ─── Realtime (speech-to-speech) ────────────────────────────────────────────
// Every speech-to-speech provider is a plugin that describes its own entry;
// the engine adds the settings that are its own.

/// The `Configure.options` keys a client may override. The VAD knobs are
/// always allowed (they only tune endpointing for that client). Letting a
/// client append to the system prompt is a separate decision the operator
/// makes explicitly, because it applies to anyone holding the client token.
pub(super) fn emit_client_overrides(ctx: &mut EmitCtx) -> Result<()> {
    let allow_instructions = ctx.prompt.confirm(
        "Allow connected clients to append to the system prompt? (anyone with the client token; useful for a trusted client on a private box)",
        false,
    )?;
    let mut keys = Vec::new();
    if allow_instructions {
        keys.push("instructions".to_string());
    }
    keys.extend(
        ["vad_threshold", "vad_silence_ms", "vad_prefix_padding_ms"]
            .iter()
            .map(ToString::to_string),
    );
    ctx.push_kv_list("client_overrides", &keys);
    ctx.push("\n");
    Ok(())
}
