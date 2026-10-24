"""The Python the backend is tested on, and the refusal to start on another.

CI, the image and the lock file (``requirements.txt``, resolved for this
version) all use one minor version. Another interpreter may install
different builds of the compiled dependencies, or none at all, and nothing
here has run on it, so the backend says so at import instead of failing
later in a way that looks like a bug.
"""
from __future__ import annotations

import logging
import os
import sys

SUPPORTED_PYTHON = (3, 12)

# Set to 1 to start on another version anyway (logged as unsupported).
ALLOW_ENV = "ADMIN_ALLOW_UNTESTED_PYTHON"

log = logging.getLogger("admin.runtime")


def python_problem(version: tuple[int, ...]) -> str | None:
    """Why ``version`` is not the tested Python, or ``None`` when it is."""
    if tuple(version[:2]) == SUPPORTED_PYTHON:
        return None
    want = ".".join(map(str, SUPPORTED_PYTHON))
    have = ".".join(map(str, version[:3]))
    return (
        f"the admin backend is tested on Python {want} and this is Python {have}. "
        f"Create its environment with Python {want} (see docs/guides/admin-console.md), "
        f"or set {ALLOW_ENV}=1 to start anyway, untested."
    )


def require_supported_python() -> None:
    """Exit with the reason on an untested Python, unless overridden."""
    problem = python_problem(sys.version_info)
    if problem is None:
        return
    if os.getenv(ALLOW_ENV, "").strip() == "1":
        log.warning("UNSUPPORTED: %s", problem)
        return
    raise SystemExit(f"error: {problem}")
