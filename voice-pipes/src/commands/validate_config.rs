//! `voice-pipes validate-config <PATH>`: load a config file, run the
//! [`crate::config::validate`] pass on the merged result, and print a
//! human-readable report. Exits non-zero when validation fails so it can be
//! wired into CI.

use std::path::Path;

use anyhow::{Context, Result};

use crate::config;

/// Output format for the validation report.
#[derive(Debug, Clone, Copy)]
pub enum ValidateConfigFormat {
    /// Human-readable bullet list.
    Text,
    /// Stable JSON shape: `{ ok: bool, errors: [{field, message}] }`.
    Json,
}

impl ValidateConfigFormat {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => anyhow::bail!("unknown format `{other}` (expected text|json)"),
        }
    }
}

/// Load `path`, run validation, print the report. Returns `Err` when the
/// config is invalid so the binary exits with a non-zero code.
pub fn run_validate_config(path: &Path, format: ValidateConfigFormat) -> Result<()> {
    let loaded = config::load(Some(path))
        .with_context(|| format!("failed to load config from {}", path.display()))?
        .ok_or_else(|| anyhow::anyhow!("no config found at {}", path.display()))?;

    // Merge with empty CLI overrides so defaults are applied the same way
    // every other subcommand sees them.
    let merged = config::merge(Some(loaded), config::CliOverrides::default());
    let errors = config::validate(&merged);

    match format {
        ValidateConfigFormat::Text => {
            if errors.is_empty() {
                println!("[ok] {} -- configuration is valid", path.display());
            } else {
                eprintln!(
                    "[x] {} -- {} validation error{}:",
                    path.display(),
                    errors.len(),
                    if errors.len() == 1 { "" } else { "s" }
                );
                for e in &errors {
                    eprintln!("  - {e}");
                }
            }
        }
        ValidateConfigFormat::Json => {
            let payload = serde_json::json!({
                "ok": errors.is_empty(),
                "path": path.display().to_string(),
                "errors": errors
                    .iter()
                    .map(|e| serde_json::json!({
                        "field": e.field,
                        "message": e.message,
                    }))
                    .collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} validation error(s)", errors.len())
    }
}
