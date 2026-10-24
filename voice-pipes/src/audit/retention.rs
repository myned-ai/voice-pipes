//! Bounded retention for session captures.
//!
//! When `[pipeline.audit] enabled = true`, every session leaves raw audio of
//! both sides plus transcripts under `captures_dir/<session_id>/`, which would
//! otherwise grow without bound. [`sweep_expired_captures`] removes session directories
//! whose newest file is older than `retention_days`, and is run at worker
//! start and every [`SWEEP_INTERVAL`] after that (see `commands::serve`).
//! Several workers may share one captures directory and sweep concurrently;
//! a directory that vanishes under us is counted as already removed, not as
//! an error.
//!
//! Only directories that look like a session capture are touched: a direct
//! child of `captures_dir` holding `metadata.json`, an events log or a mic
//! recording (a session whose metadata write failed still leaves the
//! latter). Symbolic links are never followed or removed. Anything else in
//! the directory is left alone, so pointing `captures_dir` at a shared
//! folder cannot delete unrelated data.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How often a running worker repeats the sweep after the one at start.
pub const SWEEP_INTERVAL: Duration = Duration::from_hours(6);

/// Whether `dir` holds a session capture: metadata, an events log, or a
/// mic recording from a session whose metadata write never landed.
fn is_capture_dir(dir: &Path) -> bool {
    if dir.join("metadata.json").is_file()
        || dir.join("events.jsonl").is_file()
        || dir.join("pipeline_events.jsonl").is_file()
    {
        return true;
    }
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.file_type().is_ok_and(|t| t.is_file())
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with("_mic_raw.wav"))
        })
    })
}

/// Outcome of one sweep, for the startup log line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Session directories removed.
    pub removed: usize,
    /// Session directories younger than the cutoff.
    pub kept: usize,
    /// Directories skipped because they are not session captures.
    pub skipped: usize,
    /// Bytes on disk freed (best effort; files that could not be stat'ed count 0).
    pub bytes_freed: u64,
    /// Session directories we failed to remove (name, error). The sweep
    /// continues past these.
    pub errors: Vec<(String, String)>,
}

/// Newest modification time of any regular file directly inside `dir`
/// (the directory's own mtime is unreliable: appends do not bump it on
/// every platform). Falls back to the directory mtime when it holds no files.
fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            // Entry metadata never follows a link, so a link's own
            // timestamp is what counts and its target is never read.
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            if let Ok(m) = meta.modified() {
                newest = Some(newest.map_or(m, |n| n.max(m)));
            }
        }
    }
    newest.or_else(|| std::fs::symlink_metadata(dir).ok()?.modified().ok())
}

/// Bytes under `dir`, without following symbolic links (a link to a
/// large or cyclic tree counts as nothing).
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_file() {
                total += e.metadata().map_or(0, |m| m.len());
            } else if kind.is_dir() {
                total += dir_size(&e.path());
            }
        }
    }
    total
}

/// Remove session capture directories under `captures_dir` whose newest file
/// is older than `retention_days`. `retention_days == 0` disables the sweep
/// (returns an empty report). Synchronous: call from `spawn_blocking`.
pub fn sweep_expired_captures(captures_dir: &Path, retention_days: u32) -> SweepReport {
    sweep_expired_captures_at(captures_dir, retention_days, SystemTime::now())
}

/// [`sweep_expired_captures`] with an injectable clock, for tests.
pub fn sweep_expired_captures_at(
    captures_dir: &Path,
    retention_days: u32,
    now: SystemTime,
) -> SweepReport {
    let mut report = SweepReport::default();
    if retention_days == 0 {
        return report;
    }
    let cutoff = now
        .checked_sub(Duration::from_secs(u64::from(retention_days) * 86_400))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let Ok(entries) = std::fs::read_dir(captures_dir) else {
        return report; // nothing recorded yet
    };
    // Real directories only: a symbolic link at the top level is never
    // followed, so a link into another tree cannot get that tree removed.
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
        .map(|e| e.path())
        .collect();
    candidates.sort();

    for dir in candidates {
        if !is_capture_dir(&dir) {
            report.skipped += 1;
            continue;
        }
        let Some(mtime) = newest_mtime(&dir) else {
            report.skipped += 1;
            continue;
        };
        if mtime >= cutoff {
            report.kept += 1;
            continue;
        }
        let size = dir_size(&dir);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                report.removed += 1;
                report.bytes_freed += size;
            }
            // Another worker got there first.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.removed += 1,
            Err(e) => report.errors.push((
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                e.to_string(),
            )),
        }
    }
    report
}

#[cfg(test)]
#[path = "tests/retention.rs"]
mod tests;
