//! Pipeline-config file discovery.
//!
//! Walks an `examples/`-style directory and enumerates every `*.toml`
//! file, returning lightweight metadata suitable for transport over the
//! wire (worker → orchestrator) and display in the admin UI.
//!
//! Extracted from `api::orchestrator::benchmark` so workers can scan
//! their own CWD at startup and advertise their available configs to
//! the orchestrator in `WorkerRegistration`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A discovered pipeline-config file.
///
/// `path` is the scan-root prefix plus the root-relative file path
/// (e.g. `examples/avatar/foo.toml`), normalized to `/` separators. For
/// the standard relative `examples/` root this makes it resolvable from
/// the worker's CWD — the contract `Variant.config_path` relies on when
/// a benchmark hands it back as `--config <path>` — and it never
/// contains a host-absolute prefix, so the same repo file yields the
/// same entry on every worker regardless of filesystem layout. `name`
/// is the file stem and is used as a display label in dropdowns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigEntry {
    pub path: String,
    pub name: String,
}

/// Walk `root` recursively (up to `depth_limit` levels) and collect every
/// `*.toml` file as a [`ConfigEntry`]. Hidden entries (starting with `.`)
/// are skipped. Results are sorted by path for stable UI ordering.
///
/// Emitted paths are `<prefix>/<root-relative path>` where `prefix` is
/// `root` as passed when it is relative (preserving CWD-resolvability),
/// or `root`'s final component when it is absolute — an absolute scan
/// root must not leak the host directory layout into the wire payload
/// (workers advertise these entries to the orchestrator, which
/// aggregates them across the fleet keyed by path).
pub fn scan_configs(root: &Path, depth_limit: usize) -> Vec<ConfigEntry> {
    fn walk(
        dir: &Path,
        root: &Path,
        prefix: &Path,
        out: &mut Vec<ConfigEntry>,
        depth: usize,
        limit: usize,
    ) {
        if depth > limit {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            let file_name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if file_name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, prefix, out, depth + 1, limit);
            } else if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                let portable = match path.strip_prefix(root) {
                    Ok(rel) => prefix.join(rel),
                    // Unreachable (the walk starts at `root`), but never
                    // silently emit a host-absolute path if it happens.
                    Err(_) => prefix.join(&file_name),
                };
                let rel = portable.to_string_lossy().replace('\\', "/");
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&file_name)
                    .to_string();
                out.push(ConfigEntry { path: rel, name });
            }
        }
    }

    // Relative roots pass through verbatim (byte-identical output to the
    // historical behavior); absolute roots collapse to their final
    // component so payloads stay host-agnostic.
    let prefix: PathBuf = if root.is_absolute() {
        root.file_name().map(PathBuf::from).unwrap_or_default()
    } else {
        root.to_path_buf()
    };

    let mut entries: Vec<ConfigEntry> = Vec::new();
    walk(root, root, &prefix, &mut entries, 0, depth_limit);
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}
