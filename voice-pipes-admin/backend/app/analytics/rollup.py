"""Durable per-session analytics rollup store.

The ``/api/v1/analytics`` window scan used to re-parse every in-window
session's ``events.jsonl`` + ``*_turn_resolved.json`` files on every cache
miss.  This store memoises each session's *reduced contribution* (utterance
rows, latencies, ordered cost events, error counts, failed-session row and
anomaly verdict) keyed by the session's file mtimes, so an unchanged session
is parsed exactly once in its lifetime — and the result survives restarts
(unlike the in-process memo it replaces).

A row is only reused when BOTH the events.jsonl mtime AND the session-dir
mtime match (the dir mtime bumps when a new turn file is added; the events
mtime bumps when the live session appends), plus a schema version so a format
change transparently invalidates old rows.

The stored ``payload`` is the dict returned by
``analytics.compute._compute_session_rollup`` — all JSON-serialisable.
"""

from __future__ import annotations

import json
import logging
import sqlite3
import threading
from contextlib import closing
from pathlib import Path
from typing import Any

from ..core.state import state_dir

log = logging.getLogger("admin.rollup")

_LOCK = threading.Lock()
# The database file the schema was last created in; a different state
# directory (a test, a changed environment) is initialised afresh.
_INITIALIZED_AT: Path | None = None
_FAILURE_LOGGED = False


def _db_path() -> Path:
    # Sits next to issues.db in the backend's state directory.
    return state_dir() / "rollups.db"


def _conn() -> sqlite3.Connection:
    path = _db_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    c = sqlite3.connect(path, isolation_level=None)  # autocommit
    c.execute("PRAGMA journal_mode=WAL")
    c.row_factory = sqlite3.Row
    return c


def _report_failure(exc: BaseException) -> None:
    """A store that cannot be opened costs a re-parse of every session on
    every analytics call; say so in the log once rather than not at all."""
    global _FAILURE_LOGGED
    if not _FAILURE_LOGGED:
        _FAILURE_LOGGED = True
        log.error(
            "analytics rollup store at %s cannot be used (%s: %s); every analytics "
            "call re-reads every session until its directory is writable "
            "(ADMIN_STATE_DIR moves it)",
            _db_path(),
            type(exc).__name__,
            exc,
        )


def _init_if_needed() -> None:
    global _INITIALIZED_AT
    path = _db_path()
    if path == _INITIALIZED_AT:
        return
    with _LOCK:
        if path == _INITIALIZED_AT:
            return
        with closing(_conn()) as c:
            c.execute(
                """
                CREATE TABLE IF NOT EXISTS session_rollup (
                    session_id     TEXT PRIMARY KEY,
                    pipeline_name  TEXT,
                    start_ts       REAL,
                    events_mtime   REAL NOT NULL,
                    dir_mtime      REAL NOT NULL,
                    schema_ver     INTEGER NOT NULL,
                    payload        TEXT NOT NULL
                )
                """
            )
            # Partition index — single-pipeline / windowed queries can use it
            # once the read path filters server-side.
            c.execute(
                "CREATE INDEX IF NOT EXISTS idx_rollup_pipe_ts "
                "ON session_rollup (pipeline_name, start_ts)"
            )
        _INITIALIZED_AT = path


def get_fresh(
    session_id: str,
    events_mtime: float,
    dir_mtime: float,
    schema_ver: int,
    *,
    allow_lite: bool = False,
) -> dict | None:
    """Return the stored contribution iff the row matches all freshness keys,
    else None (caller recomputes).  Never raises — a store error just forces a
    recompute."""
    try:
        _init_if_needed()
        with closing(_conn()) as c:
            row = c.execute(
                "SELECT events_mtime, dir_mtime, schema_ver, payload "
                "FROM session_rollup WHERE session_id = ?",
                (session_id,),
            ).fetchone()
        if row is None:
            return None
        valid_schemas = (schema_ver, -schema_ver) if allow_lite else (schema_ver,)
        if (
            row["schema_ver"] not in valid_schemas
            or row["events_mtime"] != events_mtime
            or row["dir_mtime"] != dir_mtime
        ):
            return None
        return json.loads(row["payload"])
    except (sqlite3.Error, OSError) as exc:
        _report_failure(exc)
        return None
    except ValueError:
        # A stored payload that does not parse is a cache miss.
        return None


def put(
    session_id: str,
    pipeline_name: str,
    start_ts: float,
    events_mtime: float,
    dir_mtime: float,
    schema_ver: int,
    payload: dict[str, Any],
) -> None:
    """Upsert a session's contribution.  Never raises — persistence is a cache,
    not a correctness dependency."""
    try:
        _init_if_needed()
        blob = json.dumps(payload, separators=(",", ":"))
        with _LOCK:  # serialise writers (WAL allows concurrent readers)
            with closing(_conn()) as c:
                c.execute(
                    """
                    INSERT INTO session_rollup
                        (session_id, pipeline_name, start_ts, events_mtime,
                         dir_mtime, schema_ver, payload)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(session_id) DO UPDATE SET
                        pipeline_name = excluded.pipeline_name,
                        start_ts      = excluded.start_ts,
                        events_mtime  = excluded.events_mtime,
                        dir_mtime     = excluded.dir_mtime,
                        schema_ver    = excluded.schema_ver,
                        payload       = excluded.payload
                    """,
                    (session_id, pipeline_name, start_ts, events_mtime,
                     dir_mtime, schema_ver, blob),
                )
    except (sqlite3.Error, OSError) as exc:
        _report_failure(exc)
    except (TypeError, ValueError):
        log.warning("rollup for session %s not cached: payload is not JSON", session_id, exc_info=True)


def clear_rollups(
    *,
    pipeline: str | None = None,
    session_id: str | None = None,
) -> int:
    """Delete cached session rollups matching the given scope (or all when both are None)."""
    try:
        _init_if_needed()
        with _LOCK, closing(_conn()) as c:
            if session_id:
                cur = c.execute("DELETE FROM session_rollup WHERE session_id = ?", (session_id,))
            elif pipeline:
                cur = c.execute("DELETE FROM session_rollup WHERE pipeline_name = ?", (pipeline,))
            else:
                cur = c.execute("DELETE FROM session_rollup")
            return cur.rowcount or 0
    except (sqlite3.Error, OSError) as exc:
        _report_failure(exc)
        return 0


def _cleared_path() -> Path:
    return state_dir() / "analytics_cleared.json"


def cleared_mtime() -> float:
    """Return mtime of analytics_cleared.json (0.0 when absent) for cache signatures."""
    p = _cleared_path()
    try:
        return p.stat().st_mtime if p.exists() else 0.0
    except OSError:
        return 0.0


def get_cleared_state() -> dict[str, Any]:
    """Return the persisted clear watermarks and dismissed session IDs."""
    p = _cleared_path()
    default = {"global_cleared_at": 0.0, "pipelines": {}, "sessions": set()}
    if not p.exists():
        return default
    try:
        raw = json.loads(p.read_text(encoding="utf-8"))
        if not isinstance(raw, dict):
            return default
        global_ts = float(raw.get("global_cleared_at") or 0.0)
        pipes_raw = raw.get("pipelines")
        pipes = (
            {str(k): float(v) for k, v in pipes_raw.items() if isinstance(v, (int, float))}
            if isinstance(pipes_raw, dict)
            else {}
        )
        sess_raw = raw.get("sessions")
        sessions = (
            {str(x) for x in sess_raw if isinstance(x, str)}
            if isinstance(sess_raw, list)
            else set()
        )
        return {
            "global_cleared_at": global_ts,
            "pipelines": pipes,
            "sessions": sessions,
        }
    except (OSError, ValueError, TypeError):
        return default


def record_cleared(
    *,
    pipeline: str | None = None,
    session_id: str | None = None,
    ts: float | None = None,
) -> dict[str, Any]:
    """Record a clear action (all, single pipeline, or single session) in analytics_cleared.json."""
    from datetime import UTC, datetime

    now_ts = ts if ts is not None else datetime.now(UTC).timestamp()
    with _LOCK:
        cur = get_cleared_state()
        if session_id:
            cur["sessions"].add(session_id)
        elif pipeline is not None:
            cur["pipelines"][pipeline] = now_ts
        else:
            cur["global_cleared_at"] = now_ts
            cur["pipelines"] = {}
            cur["sessions"] = set()
        serializable = {
            "global_cleared_at": cur["global_cleared_at"],
            "pipelines": cur["pipelines"],
            "sessions": sorted(cur["sessions"]),
        }
        try:
            p = _cleared_path()
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps(serializable), encoding="utf-8")
        except OSError as exc:
            _report_failure(exc)
        return serializable

