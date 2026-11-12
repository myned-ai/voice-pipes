//! Process and container resource usage telemetry (safe, zero-overhead).
//!
//! Tracks process physical memory (RSS via `/proc/self/statm`), container
//! peak memory (`/sys/fs/cgroup/memory.peak` or `memory.current`), and CPU time
//! consumed across active turns without `unsafe` code or third-party dependencies.

use crate::api::types::TurnResourceUsage;
use std::time::Instant;

/// Point-in-time process resource snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSample {
    pub timestamp: Instant,
    pub cpu_time_ms: u64,
    pub rss_bytes: u64,
}

impl ProcessSample {
    /// Capture current process resources.
    ///
    /// Reads `/proc/self/statm` for resident memory and `/proc/self/stat` (or
    /// `/sys/fs/cgroup/cpu.stat`) for CPU time on Linux. Returns `None` on
    /// environments where `/proc` is unavailable (e.g. native Windows dev).
    pub fn capture() -> Option<Self> {
        let rss_bytes = read_linux_rss()?;
        let cpu_time_ms = read_cpu_time_ms().unwrap_or(0);
        Some(Self {
            timestamp: Instant::now(),
            cpu_time_ms,
            rss_bytes,
        })
    }
}

/// Compute turn resource metrics given turn-open and turn-close samples.
pub fn compute_turn_resources(
    start: Option<ProcessSample>,
    end: Option<ProcessSample>,
) -> Option<TurnResourceUsage> {
    let end = end.or_else(ProcessSample::capture)?;
    let container_peak_memory_bytes = read_container_peak_memory();

    let (cpu_percent, cpu_time_ms, rss_delta_bytes) = if let Some(start) = start {
        let wall_ms = end
            .timestamp
            .duration_since(start.timestamp)
            .as_millis()
            .max(1) as f32;
        let cpu_delta_ms = end.cpu_time_ms.saturating_sub(start.cpu_time_ms) as f32;
        let pct = (cpu_delta_ms / wall_ms) * 100.0;
        let delta = (end.rss_bytes as i64) - (start.rss_bytes as i64);
        (Some(pct), Some(cpu_delta_ms as u64), Some(delta))
    } else {
        (None, None, None)
    };

    Some(TurnResourceUsage {
        cpu_percent,
        cpu_time_ms,
        rss_bytes: Some(end.rss_bytes),
        rss_delta_bytes,
        container_peak_memory_bytes,
    })
}

// ── Linux / Container Safe Parsers ─────────────────────────────

/// Read physical RSS memory (in bytes) from `/proc/self/statm`.
pub fn read_linux_rss() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/self/statm").ok()?;
    parse_statm_rss(&contents)
}

/// Read total CPU time in milliseconds from cgroups or `/proc/self/stat`.
fn read_cpu_time_ms() -> Option<u64> {
    // Prefer cgroups v2 cpu.stat usage_usec if running inside container
    if let Ok(cgroup_cpu) = std::fs::read_to_string("/sys/fs/cgroup/cpu.stat")
        && let Some(ms) = parse_cgroup_cpu_usec(&cgroup_cpu)
    {
        return Some(ms);
    }

    let contents = std::fs::read_to_string("/proc/self/stat").ok()?;
    parse_stat_cpu_ticks(&contents)
}

/// Read peak container memory (in bytes) from cgroups v2 if available.
pub fn read_container_peak_memory() -> Option<u64> {
    if let Ok(peak) = std::fs::read_to_string("/sys/fs/cgroup/memory.peak")
        && let Some(bytes) = parse_cgroup_memory(&peak)
    {
        return Some(bytes);
    }

    if let Ok(current) = std::fs::read_to_string("/sys/fs/cgroup/memory.current")
        && let Some(bytes) = parse_cgroup_memory(&current)
    {
        return Some(bytes);
    }

    None
}

// ── Pure Parsing Logic (Testable & Safe) ───────────────────────

/// Parse resident pages from `/proc/self/statm` and convert to bytes.
/// Format: `total_size resident shared text lib data dt`
pub fn parse_statm_rss(content: &str) -> Option<u64> {
    let mut parts = content.split_whitespace();
    let _total = parts.next()?;
    let resident_pages = parts.next()?.parse::<u64>().ok()?;
    // Standard Linux page size: 4096 bytes (4 KB)
    Some(resident_pages.saturating_mul(4096))
}

/// Parse total CPU ticks (utime + stime) from `/proc/self/stat` and convert to ms.
/// Fields 14 and 15 (1-based) are utime and stime.
pub fn parse_stat_cpu_ticks(content: &str) -> Option<u64> {
    // comm can contain spaces and parentheses; find the last ')' to safely skip comm
    let after_comm_idx = content.rfind(')')?;
    let rest = content.get(after_comm_idx.saturating_add(1)..)?;
    let fields = rest.split_whitespace();

    // After ')':
    // index 0: state
    // index 1..10: ppid, pgrp, session, tty_nr, tpgid, flags, minflt, cminflt, majflt, cmajflt
    // index 11: utime
    // index 12: stime
    let mut iter = fields.skip(11);
    let utime = iter.next()?.parse::<u64>().ok()?;
    let stime = iter.next()?.parse::<u64>().ok()?;
    let total_ticks = utime.saturating_add(stime);

    // Standard POSIX user ticks is 100 Hz (1 tick = 10ms)
    Some(total_ticks.saturating_mul(10))
}

/// Parse `usage_usec` from cgroups v2 `cpu.stat` and convert to ms.
pub fn parse_cgroup_cpu_usec(content: &str) -> Option<u64> {
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(key), Some(val)) = (parts.next(), parts.next())
            && key == "usage_usec"
        {
            let usec = val.parse::<u64>().ok()?;
            return Some(usec / 1000);
        }
    }
    None
}

/// Parse cgroups memory bytes (`memory.peak` or `memory.current`).
pub fn parse_cgroup_memory(content: &str) -> Option<u64> {
    content.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parse_statm_calculates_bytes_accurately() {
        // 16384 resident pages * 4096 = 67,108,864 bytes (64 MB)
        let sample = "32768 16384 4096 128 0 8192 0\n";
        assert_eq!(parse_statm_rss(sample), Some(67_108_864));
    }

    #[test]
    fn parse_statm_handles_empty_or_invalid() {
        assert_eq!(parse_statm_rss(""), None);
        assert_eq!(parse_statm_rss("invalid_string"), None);
    }

    #[test]
    fn parse_stat_cpu_ticks_with_special_comm() {
        // comm with spaces and parentheses: (voice pipes (worker))
        let sample = "1234 (voice pipes (worker)) S 1 1234 1234 0 -1 4194304 100 0 0 0 150 50 0 0";
        // utime = 150, stime = 50 -> 200 ticks * 10 = 2000 ms
        assert_eq!(parse_stat_cpu_ticks(sample), Some(2000));
    }

    #[test]
    fn parse_cgroup_cpu_extracts_usage_usec() {
        let sample = "usage_usec 4500000\nuser_usec 3500000\nsystem_usec 1000000\n";
        // 4500000 usec = 4500 ms
        assert_eq!(parse_cgroup_cpu_usec(sample), Some(4500));
    }

    #[test]
    fn parse_cgroup_memory_trims_and_parses() {
        let sample = "  83886080\n";
        assert_eq!(parse_cgroup_memory(sample), Some(83_886_080));
    }

    #[test]
    fn compute_turn_resources_calculates_delta_and_percent() {
        let t0 = Instant::now();
        let start = ProcessSample {
            timestamp: t0,
            cpu_time_ms: 1000,
            rss_bytes: 60_000_000,
        };
        let end = ProcessSample {
            timestamp: t0 + Duration::from_millis(500),
            cpu_time_ms: 1250, // 250ms CPU time in 500ms wall time = 50% CPU
            rss_bytes: 62_000_000,
        };

        let usage = compute_turn_resources(Some(start), Some(end))
            .expect("should compute resource usage");

        assert_eq!(usage.cpu_time_ms, Some(250));
        assert_eq!(usage.rss_bytes, Some(62_000_000));
        assert_eq!(usage.rss_delta_bytes, Some(2_000_000));
        if let Some(pct) = usage.cpu_percent {
            assert!((pct - 50.0).abs() < 1.0, "CPU percent should be ~50%");
        }
    }
}
