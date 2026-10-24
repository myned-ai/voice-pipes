"""Small cross-cutting helpers for the report/health engine."""

from datetime import datetime
from typing import Any


def _iso_to_ts(iso: Any) -> float | None:
    """Best-effort ISO-8601 → POSIX seconds.  Returns None on failure."""
    if not iso or not isinstance(iso, str):
        return None
    try:
        # Accept trailing Z by swapping to +00:00.
        return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp()
    except (TypeError, ValueError):
        return None
