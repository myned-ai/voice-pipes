"""Automatic TTL and disk-budget janitor for session captures and logs.

Designed for ephemeral / disk-constrained environments like Hugging Face Spaces
where saved WAV captures and JSONL event logs would otherwise accumulate until
the container runs out of space.

Configuration (environment):
  ADMIN_CAPTURE_TTL_MINUTES       Delete session capture dirs whose newest file
                                  is older than this many minutes (0 = disabled).
  ADMIN_CAPTURES_MAX_MB           Hard disk cap for CAPTURES_DIR in MiB; oldest
                                  sessions are evicted first when exceeded
                                  (0 = disabled).
  ADMIN_CLEANUP_INTERVAL_SECONDS  How often the background janitor runs
                                  (default: 300 seconds, minimum: 10).
"""
from __future__ import annotations

import asyncio
import logging
import os
import shutil
import time
from pathlib import Path

from ..analytics import rollup
from ..analytics.compute import _analytics_cache, _iter_capture_session_dirs
from ..issues import clear_issues
from . import config

log = logging.getLogger("admin.cleanup")


def capture_ttl_minutes() -> float:
    try:
        return max(0.0, float(os.getenv("ADMIN_CAPTURE_TTL_MINUTES", "0")))
    except ValueError:
        return 0.0


def captures_max_mb() -> float:
    try:
        return max(0.0, float(os.getenv("ADMIN_CAPTURES_MAX_MB", "0")))
    except ValueError:
        return 0.0


def cleanup_interval_seconds() -> float:
    try:
        return max(10.0, float(os.getenv("ADMIN_CLEANUP_INTERVAL_SECONDS", "300")))
    except ValueError:
        return 300.0


def _dir_stats(path: Path) -> tuple[float, int]:
    """Return ``(newest_mtime, total_bytes)`` across all files in ``path``."""
    try:
        newest = path.stat().st_mtime
    except OSError:
        newest = 0.0
    total_bytes = 0
    try:
        for root, _, files in os.walk(path):
            root_path = Path(root)
            for fname in files:
                fpath = root_path / fname
                try:
                    st = fpath.stat()
                    total_bytes += st.st_size
                    if st.st_mtime > newest:
                        newest = st.st_mtime
                except OSError:
                    continue
    except OSError:
        pass
    return newest, total_bytes


def _remove_session_dir(session_dir: Path, base: Path) -> bool:
    """Safely remove a session directory under ``base`` and clean related state."""
    try:
        resolved = session_dir.resolve()
        if base not in resolved.parents:
            return False
    except OSError:
        return False

    sid = session_dir.name
    parent = session_dir.parent
    tenant_id = parent.name if parent.resolve() != base else None

    shutil.rmtree(session_dir, ignore_errors=True)
    try:
        if tenant_id:
            rollup.clear_rollups(session_id=f"{tenant_id}:{sid}")
        rollup.clear_rollups(session_id=sid)
        clear_issues(session_id=sid)
    except Exception:
        log.debug("best-effort rollup/issue cleanup failed for %s", sid, exc_info=True)

    # Prune empty tenant folder.
    if tenant_id and parent.exists():
        try:
            if not any(parent.iterdir()):
                parent.rmdir()
        except OSError:
            pass
    return True


def _iter_all_capture_session_dirs() -> list[Path]:
    """Return all session capture directories (including partial/empty ones) under ``CAPTURES_DIR``."""
    dirs: list[Path] = []
    if not config.CAPTURES_DIR.exists():
        return dirs
    try:
        for entry in config.CAPTURES_DIR.iterdir():
            if not entry.is_dir():
                continue
            if (entry / "metadata.json").exists() or (entry / "events.jsonl").exists():
                dirs.append(entry)
                continue
            try:
                subs = [sub for sub in entry.iterdir() if sub.is_dir()]
            except OSError:
                continue
            if subs:
                dirs.extend(subs)
            else:
                # Empty flat or empty tenant dir
                dirs.append(entry)
    except OSError:
        pass
    return dirs


def sweep_captures_once(
    *,
    ttl_minutes: float | None = None,
    max_mb: float | None = None,
    now_ts: float | None = None,
) -> dict[str, int]:
    """Run one synchronous sweep of ``config.CAPTURES_DIR``.

    Returns counts of ``ttl_deleted``, ``cap_deleted``, and ``bytes_freed``.
    """
    eff_ttl = capture_ttl_minutes() if ttl_minutes is None else ttl_minutes
    eff_max_mb = captures_max_mb() if max_mb is None else max_mb
    if eff_ttl <= 0 and eff_max_mb <= 0:
        return {"ttl_deleted": 0, "cap_deleted": 0, "bytes_freed": 0}
    if not config.CAPTURES_DIR.exists():
        return {"ttl_deleted": 0, "cap_deleted": 0, "bytes_freed": 0}

    now = time.time() if now_ts is None else now_ts
    base = config.CAPTURES_DIR.resolve()

    sessions: list[tuple[float, int, Path]] = []
    for sdir in _iter_all_capture_session_dirs():
        newest, size = _dir_stats(sdir)
        sessions.append((newest, size, sdir))

    ttl_deleted = 0
    cap_deleted = 0
    bytes_freed = 0
    remaining: list[tuple[float, int, Path]] = []

    cutoff = now - eff_ttl * 60.0 if eff_ttl > 0 else None
    for newest, size, sdir in sessions:
        if cutoff is not None and newest < cutoff:
            if _remove_session_dir(sdir, base):
                ttl_deleted += 1
                bytes_freed += size
        else:
            remaining.append((newest, size, sdir))

    if eff_max_mb > 0 and remaining:
        max_bytes = int(eff_max_mb * 1024 * 1024)
        total_bytes = sum(size for _, size, _ in remaining)
        if total_bytes > max_bytes:
            # Evict oldest sessions first until under disk budget.
            remaining.sort(key=lambda item: item[0])
            for _, size, sdir in remaining:
                if total_bytes <= max_bytes:
                    break
                if _remove_session_dir(sdir, base):
                    cap_deleted += 1
                    bytes_freed += size
                    total_bytes -= size

    if ttl_deleted or cap_deleted:
        _analytics_cache.clear()
        log.info(
            "capture janitor swept ttl_deleted=%d cap_deleted=%d bytes_freed=%d",
            ttl_deleted,
            cap_deleted,
            bytes_freed,
        )

    return {
        "ttl_deleted": ttl_deleted,
        "cap_deleted": cap_deleted,
        "bytes_freed": bytes_freed,
    }


async def run_cleanup_loop() -> None:
    """Background asyncio task that periodically sweeps expired captures."""
    interval = cleanup_interval_seconds()
    log.info(
        "capture janitor started (ttl_minutes=%s, max_mb=%s, interval_s=%s)",
        capture_ttl_minutes(),
        captures_max_mb(),
        interval,
    )
    try:
        while True:
            try:
                await asyncio.to_thread(sweep_captures_once)
            except Exception:
                log.warning("capture janitor sweep failed", exc_info=True)
            await asyncio.sleep(interval)
    except asyncio.CancelledError:
        pass
