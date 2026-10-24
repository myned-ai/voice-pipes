//! `voice-pipes models`: fetch and list the local model files, without the
//! wizard's questions.
//!
//! The entries are the wizard's model registry (`config/wizard_models.json`
//! plus what the linked plugins declare), each fetched to its default
//! location. With no names, `fetch` downloads the entries the registry
//! marks `"default": true`: the files `examples/minimal.toml` runs on.

use std::path::Path;

use anyhow::Result;

use super::config_wizard::registry::{
    DownloadSpec, Registry, download_file, load_model_registry, plan_missing_downloads,
};

/// The entries the registry marks as the default set, sorted.
fn default_set(registry: &Registry) -> Vec<&str> {
    let mut keys: Vec<&str> = registry
        .iter()
        .filter(|(_, c)| c.in_default_set)
        .map(|(k, _)| k.as_str())
        .collect();
    keys.sort_unstable();
    keys
}

/// Download every missing file of the named entries (of the default set
/// when `keys` is empty) under the working directory. Run from the
/// repository root it moves into `voice-pipes/`, where the example configs
/// resolve their model paths.
pub async fn run_models_fetch(keys: &[String]) -> Result<()> {
    enter_engine_dir()?;
    let registry = load_model_registry();
    let keys: Vec<&str> = if keys.is_empty() {
        default_set(&registry)
    } else {
        keys.iter().map(String::as_str).collect()
    };
    let specs = specs_for(&keys, &registry)?;
    let missing = plan_missing_downloads(&specs, &registry, Path::new(""));
    if missing.is_empty() {
        println!("✅ All files for {} are present.", keys.join(", "));
        return Ok(());
    }
    let mut failed = 0usize;
    for item in &missing {
        if let Err(e) = download_file(item.url, &item.dest).await {
            println!("❌ {}: {e}", item.dest.display());
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} of {} downloads failed", missing.len());
    }
    println!("✅ Fetched {}.", keys.join(", "));
    Ok(())
}

/// Print every registry entry, where it goes and whether it is complete.
pub fn run_models_list() -> Result<()> {
    enter_engine_dir()?;
    let registry = load_model_registry();
    let mut keys: Vec<&String> = registry.keys().collect();
    keys.sort();
    for key in keys {
        let component = &registry[key];
        let spec = [DownloadSpec {
            registry_key: key.clone(),
            dest: component.default_path.clone(),
        }];
        let missing = plan_missing_downloads(&spec, &registry, Path::new("")).len();
        let state = if missing == 0 {
            "present".to_string()
        } else {
            format!("{missing} of {} files missing", component.files.len())
        };
        let default = if component.in_default_set {
            " (default)"
        } else {
            ""
        };
        println!("{key:<16} {:<60} {state}{default}", component.default_path);
    }
    Ok(())
}

/// One download per key, at the entry's default location; an unknown key
/// is an error that lists the known ones.
fn specs_for(keys: &[&str], registry: &Registry) -> Result<Vec<DownloadSpec>> {
    keys.iter()
        .map(|key| {
            let component = registry.get(*key).ok_or_else(|| {
                let mut known: Vec<&str> = registry.keys().map(String::as_str).collect();
                known.sort_unstable();
                anyhow::anyhow!("unknown model {key:?}; known: {}", known.join(", "))
            })?;
            Ok(DownloadSpec {
                registry_key: (*key).to_string(),
                dest: component.default_path.clone(),
            })
        })
        .collect()
}

/// From the repository root, move into `voice-pipes/`: model paths are
/// relative to the directory the engine runs in, and that is the one the
/// examples and the Docker stack use.
fn enter_engine_dir() -> Result<()> {
    if !Path::new("config/wizard_models.json").exists()
        && Path::new("voice-pipes/config/wizard_models.json").exists()
    {
        std::env::set_current_dir("voice-pipes")?;
        println!("ℹ️  Using voice-pipes/ (model paths are relative to it; run the CLI from there).");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/models.rs"]
mod tests;
