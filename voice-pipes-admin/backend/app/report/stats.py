"""Small-n distribution statistics for per-component aggregates.

No numpy — the per-session bucket sizes are tiny (<= ~50 turns), so plain
Python is both sufficient and dependency-free.
"""


def _percentiles(vals: list[float]) -> dict[str, float | None]:
    """Min / p50 / p90 / p99 / max with no numpy dependency."""
    if not vals:
        return {"p50": None, "p90": None, "p99": None, "min": None, "max": None}
    s = sorted(vals)
    n = len(s)
    def pick(p: float) -> float:
        # nearest-rank percentile; sufficient for diagnostic ballparks
        idx = max(0, min(n - 1, round(p * (n - 1))))
        return s[idx]
    return {
        "min": s[0],
        "p50": pick(0.50),
        "p90": pick(0.90),
        "p99": pick(0.99),
        "max": s[-1],
    }


def _distribution_stats(vals: list[float]) -> dict[str, float | bool | str | None]:
    """Percentiles + jitter / variance signals.

    Adds:
      - `mean`, `stddev`        — basic dispersion
      - `iqr`                   — p90 - p50 (robust to outliers given small n)
      - `p99_p50_ratio`         — heavy-tail detector
      - `stability`             — "stable" | "variable" | "unstable" (string)
      - `unstable`              — bool, true when ratio > 5 (item 7 threshold)

    Designed for the small-n (<= ~50 turn) bucket sizes in a session;
    avoids numpy.  Returns nulls when fewer than 2 samples.
    """
    base = _percentiles(vals)
    n = len(vals)
    if n == 0:
        base.update({
            "mean": None, "stddev": None, "iqr": None,
            "p99_p50_ratio": None, "stability": None, "unstable": False,
        })
        return base

    mean = sum(vals) / n
    if n >= 2:
        var = sum((v - mean) ** 2 for v in vals) / (n - 1)
        stddev = var ** 0.5
    else:
        stddev = None

    p50 = base.get("p50")
    p90 = base.get("p90")
    p99 = base.get("p99")
    iqr = (p90 - p50) if (isinstance(p50, (int, float)) and isinstance(p90, (int, float))) else None
    ratio = (p99 / p50) if (isinstance(p50, (int, float)) and p50 > 0 and isinstance(p99, (int, float))) else None

    # Classify stability.  Thresholds are deliberately generous to avoid
    # flagging tiny per-turn jitter as "unstable" on small sessions.
    if ratio is None or n < 3:
        stability = "stable"
        unstable = False
    elif ratio > 5.0:
        stability, unstable = "unstable", True
    elif ratio > 2.0:
        stability, unstable = "variable", False
    else:
        stability, unstable = "stable", False

    base.update({
        "mean": mean,
        "stddev": stddev,
        "iqr": iqr,
        "p99_p50_ratio": ratio,
        "stability": stability,
        "unstable": unstable,
    })
    return base
