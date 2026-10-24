"""Persistent issues feed (P3 #18).

A tiny SQLite-backed store that survives admin-backend restarts and
collects mixed-kind issues from multiple sources:

* ``failed_session`` — emitted by ``/api/v1/analytics`` when a session
  records errors or produced no turns.
* ``worker_death`` / ``worker_back`` — emitted by the workers proxy when a
  poll observes a status transition.
* ``provider_change`` — emitted by ``/api/v1/analytics`` for each
  consecutive-session provider mutation.
* ``anomaly_session`` — emitted by ``/api/v1/analytics`` for any session
  whose anomaly count crosses a configurable threshold.

The schema is intentionally small.  Each issue is keyed by a synthetic
``dedup_key`` so re-running analytics does not duplicate rows.

The database lives in the backend's state directory (``core/state.py``).
When it cannot be opened, writes are dropped, the log says so once, and
``store_failure()`` reports it so the feed can say so too.
"""

from __future__ import annotations

import json
import logging
import sqlite3
import threading
from contextlib import closing
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .core.state import state_dir

log = logging.getLogger("admin.issues")

_LOCK = threading.Lock()
# The database file the schema was last created in; a different state
# directory (a test, a changed environment) is initialised afresh.
_INITIALIZED_AT: Path | None = None

# Why the store could not be used on the last attempt, or None when it could.
_FAILURE: str | None = None
_FAILURE_LOGGED = False


def _db_path() -> Path:
    return state_dir() / "issues.db"


def _conn() -> sqlite3.Connection:
    path = _db_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    c = sqlite3.connect(path, isolation_level=None)  # autocommit
    c.execute("PRAGMA journal_mode=WAL")
    c.row_factory = sqlite3.Row
    return c


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
                CREATE TABLE IF NOT EXISTS issues (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    dedup_key     TEXT UNIQUE NOT NULL,
                    kind          TEXT NOT NULL,
                    severity      TEXT NOT NULL DEFAULT 'warn',
                    ts            REAL NOT NULL,
                    session_id    TEXT,
                    pipeline_name TEXT,
                    summary       TEXT,
                    payload       TEXT,
                    created_at    REAL NOT NULL
                )
                """
            )
            c.execute(
                "CREATE INDEX IF NOT EXISTS idx_issues_ts ON issues(ts DESC)"
            )
            c.execute(
                "CREATE INDEX IF NOT EXISTS idx_issues_kind ON issues(kind)"
            )
        _INITIALIZED_AT = path


def _record_failure(exc: BaseException) -> None:
    """Remember that the store is unusable, and say so in the log once.

    Every caller treats the feed as best effort and carries on, so without
    this a store that could not be opened (a state directory the process
    cannot write) left the feed empty with nothing anywhere saying why.
    """
    global _FAILURE, _FAILURE_LOGGED
    _FAILURE = f"{type(exc).__name__}: {exc}"
    if not _FAILURE_LOGGED:
        _FAILURE_LOGGED = True
        log.error(
            "issues store at %s cannot be used (%s); the issues feed stays empty "
            "until its directory is writable (ADMIN_STATE_DIR moves it)",
            _db_path(),
            _FAILURE,
        )


def _record_success() -> None:
    global _FAILURE
    _FAILURE = None


def store_failure() -> str | None:
    """Why the issues store could not be used on the last attempt, or None."""
    return _FAILURE


def record_issue(
    *,
    kind: str,
    dedup_key: str,
    summary: str,
    severity: str = "warn",
    ts: float | None = None,
    session_id: str | None = None,
    pipeline_name: str | None = None,
    payload: dict[str, Any] | None = None,
) -> bool:
    """Insert an issue if ``dedup_key`` is unseen.  Returns True if inserted,
    False for a duplicate or when the store cannot be used."""
    now = datetime.now(UTC).timestamp()
    try:
        _init_if_needed()
        with _LOCK, closing(_conn()) as c:
            try:
                c.execute(
                    """INSERT INTO issues
                           (dedup_key, kind, severity, ts, session_id,
                            pipeline_name, summary, payload, created_at)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                    (
                        dedup_key,
                        kind,
                        severity,
                        ts if ts is not None else now,
                        session_id,
                        pipeline_name,
                        summary,
                        json.dumps(payload) if payload is not None else None,
                        now,
                    ),
                )
                inserted = True
            except sqlite3.IntegrityError:
                inserted = False
    except (sqlite3.Error, OSError) as exc:
        _record_failure(exc)
        return False
    _record_success()
    return inserted


def recent_issues(
    limit: int = 50,
    kinds: list[str] | None = None,
    since_ts: float | None = None,
) -> list[dict[str, Any]]:
    """Return the most recent issues, newest first; empty when the store
    cannot be used (``store_failure()`` then says why)."""
    sql = "SELECT * FROM issues"
    where: list[str] = []
    params: list[Any] = []
    if kinds:
        placeholders = ",".join("?" * len(kinds))
        where.append(f"kind IN ({placeholders})")
        params.extend(kinds)
    if since_ts is not None:
        where.append("ts >= ?")
        params.append(since_ts)
    if where:
        sql += " WHERE " + " AND ".join(where)
    sql += " ORDER BY ts DESC LIMIT ?"
    params.append(int(limit))
    out: list[dict[str, Any]] = []
    try:
        _init_if_needed()
        with closing(_conn()) as c:
            rows = c.execute(sql, params).fetchall()
    except (sqlite3.Error, OSError) as exc:
        _record_failure(exc)
        return []
    _record_success()
    for row in rows:
        d = dict(row)
        if d.get("payload"):
            try:
                d["payload"] = json.loads(d["payload"])
            except (TypeError, ValueError):
                # Not JSON: the payload is returned as the stored text.
                pass
        out.append(d)
    return out


def prune_older_than(days: int) -> int:
    """Delete issues older than ``days``.  Returns the number of rows removed."""
    _init_if_needed()
    cutoff = datetime.now(UTC).timestamp() - days * 86400
    with _LOCK, closing(_conn()) as c:
        cur = c.execute("DELETE FROM issues WHERE ts < ?", (cutoff,))
        return cur.rowcount or 0


def clear_issues(
    *,
    pipeline: str | None = None,
    session_id: str | None = None,
) -> int:
    """Delete persisted issues matching the given scope (or all when both are None)."""
    try:
        _init_if_needed()
        with _LOCK, closing(_conn()) as c:
            if session_id:
                cur = c.execute("DELETE FROM issues WHERE session_id = ?", (session_id,))
            elif pipeline:
                cur = c.execute("DELETE FROM issues WHERE pipeline_name = ?", (pipeline,))
            else:
                cur = c.execute("DELETE FROM issues")
            return cur.rowcount or 0
    except (sqlite3.Error, OSError) as exc:
        _record_failure(exc)
        return 0

