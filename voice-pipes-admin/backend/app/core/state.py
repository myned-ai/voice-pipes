"""Where the backend keeps its own state: the issues feed and the analytics
rollup cache, two SQLite files.

``ADMIN_STATE_DIR`` names the directory outright. Otherwise
``ADMIN_STATE_ROOT`` names a shared, world-writable parent (the image sets
``/app/state``, a volume) and the directory is ``uid-<uid>`` inside it, made
by whichever uid the process runs as: compose runs the container as the host
user, not the image's own uid, so a directory prepared in the image for that
uid would not be writable. With neither set it is ``tmp/`` beside the
``app`` package, which suits a checkout.

Read on every call rather than once at import, so a test, or a process whose
environment changes, gets the directory it names now.
"""
from __future__ import annotations

import os
from pathlib import Path

_DEFAULT_STATE_DIR = Path(__file__).resolve().parent.parent.parent / "tmp"


def _uid() -> str:
    getuid = getattr(os, "getuid", None)
    return str(getuid()) if getuid is not None else "user"


def state_dir() -> Path:
    raw = os.getenv("ADMIN_STATE_DIR", "").strip()
    if raw:
        return Path(raw)
    root = os.getenv("ADMIN_STATE_ROOT", "").strip()
    if root:
        return Path(root) / f"uid-{_uid()}"
    return _DEFAULT_STATE_DIR
