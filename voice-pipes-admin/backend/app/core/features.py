"""Runtime feature flags for the Admin Console UI and BFF.

All optional UI sections are enabled by default so local and self-hosted
deployments keep full functionality. Hosted / public demos (such as shared
multi-tenant sandboxes) can disable heavy or admin-only workflows via
environment variables without rebuilding the frontend bundle:

  ADMIN_FEATURE_BENCHMARKS   Enable the Benchmarks UI & /api/v1/benchmark/experiments*
                             routes (default: true).
  ADMIN_FEATURE_TEST_SUITES  Enable the Test Suites UI & /api/v1/benchmark/suites*
                             routes (default: true).
"""
from __future__ import annotations

import os

_FALSY = frozenset({"0", "false", "no", "off"})


def _env_flag(name: str, *, default: bool = True) -> bool:
    raw = os.getenv(name)
    if raw is None or not raw.strip():
        return default
    return raw.strip().lower() not in _FALSY


def benchmarks_enabled() -> bool:
    return _env_flag("ADMIN_FEATURE_BENCHMARKS", default=True)


def test_suites_enabled() -> bool:
    return _env_flag("ADMIN_FEATURE_TEST_SUITES", default=True)


def get_features() -> dict[str, bool]:
    return {
        "benchmarks": benchmarks_enabled(),
        "test_suites": test_suites_enabled(),
    }
