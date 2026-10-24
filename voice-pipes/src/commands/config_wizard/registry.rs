//! Model download registry.
//!
//! `config/wizard_models.json` is the single source of truth for what files
//! each local component needs and where they come from. It is embedded at
//! compile time (so the binary can never drift from the repo copy); if a
//! `config/wizard_models.json` exists relative to the wizard's working
//! directory it overrides the embedded copy, letting ops patch URLs without a
//! rebuild.
//!
//! Registry entry format:
//! - `default_path` — where the component lives by default. With
//!   `layout = "dir"` it names a directory the files are placed into; with
//!   `layout = "file"` it names the *primary* file (secondary files land next
//!   to it).
//! - `default` — `true` on the entries `voice-pipes models fetch` downloads
//!   when given no names (what `examples/minimal.toml` needs).
//! - `files[].alt_names` — alternative file names that also satisfy a file
//!   (e.g. a quantized variant already on disk); if any exists, the download
//!   is skipped.
//!
//! A plugin that declares its files ([`voice_pipes_sdk::WizardDeploy::Download`])
//! adds an entry under its provider name, unless the file already has one.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::StreamExt as _;
use serde::Deserialize;

/// How a registry entry's `default_path` is interpreted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// `default_path` is a directory; every file goes inside it.
    #[default]
    Dir,
    /// `default_path` is the primary file; secondary files go next to it.
    File,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WizardFile {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub alt_names: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WizardComponent {
    pub default_path: String,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub files: Vec<WizardFile>,
    /// Part of what `voice-pipes models fetch` downloads when given no
    /// names: the files `examples/minimal.toml` runs on.
    #[serde(default, rename = "default")]
    pub in_default_set: bool,
}

pub type Registry = HashMap<String, WizardComponent>;

/// The registry JSON compiled into the binary.
pub const EMBEDDED_REGISTRY: &str = include_str!("../../../config/wizard_models.json");

/// Loads the registry: external `config/wizard_models.json` if present and
/// valid, else the embedded copy (which is guaranteed to parse by tests),
/// plus the download entries the registered plugins declare.
pub fn load_model_registry() -> Registry {
    let mut registry = load_registry_file();
    merge_plugin_downloads(&mut registry, super::plugin_wizard_choices());
    registry
}

fn load_registry_file() -> Registry {
    let external = Path::new("config/wizard_models.json");
    if let Ok(content) = fs::read_to_string(external) {
        match serde_json::from_str(&content) {
            Ok(registry) => return registry,
            Err(e) => eprintln!(
                "⚠️  Ignoring malformed {}: {e}; using the built-in registry.",
                external.display()
            ),
        }
    }
    serde_json::from_str(EMBEDDED_REGISTRY)
        .expect("embedded wizard_models.json parses (guarded by registry tests)")
}

/// Adds an entry for every plugin choice that declares downloads: a plain
/// download under its provider's name, a sidecar's model under the model's
/// own name. An entry the registry file already has wins, so an operator can
/// repoint a plugin's URLs without a rebuild; presets sharing a model add it
/// once.
pub fn merge_plugin_downloads(
    registry: &mut Registry,
    choices: impl IntoIterator<Item = (String, voice_pipes_sdk::WizardChoice)>,
) {
    for (provider, choice) in choices {
        if let Some((key, component)) = plugin_component(&provider, &choice) {
            registry.entry(key).or_insert(component);
        }
    }
}

/// The registry key and entry a plugin choice describes, or `None` when it
/// declares no downloads. A plain download is keyed by `provider` and
/// defaults to the choice's first path field (no path field, no entry); a
/// sidecar's model carries its own key and default location.
pub fn plugin_component(
    provider: &str,
    choice: &voice_pipes_sdk::WizardChoice,
) -> Option<(String, WizardComponent)> {
    use voice_pipes_sdk::{WizardDeploy, WizardFieldKind};
    match &choice.deploy {
        WizardDeploy::Download { layout, files } => {
            let default_path = choice
                .fields
                .iter()
                .find(|f| f.kind == WizardFieldKind::Path)?
                .default
                .clone();
            Some((
                provider.to_string(),
                component(default_path, *layout, files),
            ))
        }
        WizardDeploy::DockerSidecarModel { model, .. } => Some((
            model.key.clone(),
            component(model.default_path.clone(), model.layout, &model.files),
        )),
        _ => None,
    }
}

fn component(
    default_path: String,
    layout: voice_pipes_sdk::WizardLayout,
    files: &[voice_pipes_sdk::WizardDownload],
) -> WizardComponent {
    WizardComponent {
        default_path,
        layout: match layout {
            voice_pipes_sdk::WizardLayout::File => Layout::File,
            _ => Layout::Dir,
        },
        files: files
            .iter()
            .map(|f| WizardFile {
                name: f.name.clone(),
                url: f.url.clone(),
                alt_names: f.alt_names.clone(),
            })
            .collect(),
        in_default_set: false,
    }
}

/// A component the wizard selected whose files may need downloading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadSpec {
    pub registry_key: String,
    /// User-chosen path: a directory for [`Layout::Dir`] entries, the primary
    /// file for [`Layout::File`] entries.
    pub dest: String,
}

#[derive(Debug)]
pub struct DownloadItem<'a> {
    pub url: &'a str,
    pub dest: PathBuf,
}

/// Computes which of the selected components' files are absent and where they
/// should be downloaded to. `root` is prepended to every (relative) path —
/// the working directory in production, a temp dir in tests.
///
/// For [`Layout::File`] entries, if the user redirected the primary file away
/// from the registry's file name they are supplying their own model, so the
/// whole entry is skipped.
pub fn plan_missing_downloads<'a>(
    specs: &[DownloadSpec],
    registry: &'a Registry,
    root: &Path,
) -> Vec<DownloadItem<'a>> {
    let mut missing = Vec::new();
    for spec in specs {
        let Some(component) = registry.get(&spec.registry_key) else {
            continue;
        };
        match component.layout {
            Layout::Dir => {
                let dir = root.join(&spec.dest);
                for file in &component.files {
                    let dest = dir.join(&file.name);
                    if !is_satisfied(file, &dest) {
                        missing.push(DownloadItem {
                            url: &file.url,
                            dest,
                        });
                    }
                }
            }
            Layout::File => {
                let primary_name = Path::new(&component.default_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let user_file = Path::new(&spec.dest);
                let user_name = user_file.file_name().map(|n| n.to_string_lossy());
                if user_name.as_deref() != Some(primary_name.as_str()) {
                    continue; // user brings their own model files
                }
                let side_dir = user_file.parent().unwrap_or_else(|| Path::new("models"));
                for file in &component.files {
                    let dest = if file.name == primary_name {
                        root.join(user_file)
                    } else {
                        root.join(side_dir.join(&file.name))
                    };
                    if !is_satisfied(file, &dest) {
                        missing.push(DownloadItem {
                            url: &file.url,
                            dest,
                        });
                    }
                }
            }
        }
    }
    missing
}

fn is_satisfied(file: &WizardFile, dest: &Path) -> bool {
    if dest.exists() {
        return true;
    }
    let dir = dest.parent().unwrap_or_else(|| Path::new(""));
    file.alt_names.iter().any(|alt| dir.join(alt).exists())
}

/// Returns a `HuggingFace` access token from the environment, if set.
///
/// Checks `HF_TOKEN` first, then `HUGGING_FACE_HUB_TOKEN` (the names the
/// official `HuggingFace` tooling uses). Empty/whitespace values are ignored.
fn hf_token() -> Option<String> {
    ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"]
        .into_iter()
        .find_map(|var| std::env::var(var).ok())
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// Attach the `HuggingFace` bearer token to a request builder when the URL
/// points at huggingface.co and a token is configured (needed for gated repos).
fn with_hf_auth(url: &str, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if url.contains("huggingface.co")
        && let Some(token) = hf_token()
    {
        return builder.bearer_auth(token);
    }
    builder
}

/// Best-effort probe of the remote file size via a HEAD request. Returns `None`
/// if the server doesn't support HEAD or omits `Content-Length`; callers then
/// fall back to verifying against the GET response's length.
async fn remote_content_length(client: &reqwest::Client, url: &str) -> Option<u64> {
    let resp = with_hf_auth(url, client.head(url)).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.content_length()
}

pub async fn download_file(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    // Public model files, fetched without a credential; Hugging Face answers
    // `resolve/` URLs with a redirect to its CDN, which the provider clients'
    // no-redirect policy would refuse.
    #[expect(
        clippy::disallowed_methods,
        reason = "public downloads that must follow CDN redirects and carry no credential"
    )]
    let client = reqwest::Client::new();

    // Probe the expected size up-front so we can skip an already-complete file
    // (avoids re-downloading multi-GB GGUFs on every wizard run) and verify the
    // download afterwards.
    let expected_size = remote_content_length(&client, url).await;
    if let (Some(expected), Ok(meta)) = (expected_size, fs::metadata(dest))
        && meta.len() == expected
    {
        println!(
            "✅ Already present, skipping: {} ({})",
            dest.display(),
            bytes_to_human(expected)
        );
        return Ok(());
    }

    println!("📥 Downloading {url}...");
    let response = with_hf_auth(url, client.get(url)).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!(
                "Failed to download {url}: HTTP {status}. This repo is likely gated — \
                 accept its license on HuggingFace and set HF_TOKEN to an access token \
                 (https://huggingface.co/settings/tokens)."
            );
        }
        anyhow::bail!("Failed to download {url}: HTTP {status}");
    }

    let total_size = response.content_length().or(expected_size).unwrap_or(0);

    // Stream into a temporary `.part` file and only rename into place once the
    // download is verified — an interrupted/truncated download therefore never
    // leaves a corrupt file at `dest` that later "looks present".
    let tmp = dest.with_extension("part");
    let downloaded = match stream_to_file(response, &tmp, total_size).await {
        Ok(n) => n,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };

    if total_size > 0 && downloaded != total_size {
        let _ = fs::remove_file(&tmp);
        anyhow::bail!(
            "Download incomplete for {url}: got {} of {} — try again",
            bytes_to_human(downloaded),
            bytes_to_human(total_size)
        );
    }

    fs::rename(&tmp, dest)?;
    println!(
        "💾 Saved to: {} ({})",
        dest.display(),
        bytes_to_human(downloaded)
    );
    Ok(())
}

/// Streams a response body to `dest`, returning the number of bytes written and
/// printing a progress line. Errors propagate so the caller can clean up.
async fn stream_to_file(response: reqwest::Response, dest: &Path, total_size: u64) -> Result<u64> {
    let mut file = fs::File::create(dest)?;
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    let mut last_reported = std::time::Instant::now();

    while let Some(item) = stream.next().await {
        let chunk = item?;
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;

        if total_size > 0 && last_reported.elapsed().as_millis() > 500 {
            let percentage = (downloaded as f64 / total_size as f64) * 100.0;
            print!(
                "\rProgress: {:.1}% ({}/{})",
                percentage,
                bytes_to_human(downloaded),
                bytes_to_human(total_size)
            );
            std::io::stdout().flush()?;
            last_reported = std::time::Instant::now();
        }
    }
    file.flush()?;

    if total_size > 0 {
        println!(
            "\rProgress: 100.0% ({}/{})",
            bytes_to_human(total_size.max(downloaded)),
            bytes_to_human(total_size)
        );
    } else {
        println!("Downloaded {}", bytes_to_human(downloaded));
    }
    Ok(downloaded)
}

fn bytes_to_human(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
