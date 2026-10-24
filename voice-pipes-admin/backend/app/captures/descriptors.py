"""Single source of truth metric descriptors for the admin backend.

Loaded from `metric_descriptors.json`, which is generated from the Rust
registry in `voice-pipes/src/pipeline/metrics.rs`.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

DESCRIPTORS_PATH = Path(__file__).parent / "metric_descriptors.json"

_CACHED_DESCRIPTORS: list[dict[str, Any]] | None = None


def get_metric_descriptors() -> list[dict[str, Any]]:
    """Return all canonical component metric descriptors."""
    global _CACHED_DESCRIPTORS
    if _CACHED_DESCRIPTORS is None:
        if not DESCRIPTORS_PATH.exists():
            return []
        _CACHED_DESCRIPTORS = json.loads(DESCRIPTORS_PATH.read_text(encoding="utf-8"))
    return _CACHED_DESCRIPTORS


def get_all_metric_keys() -> set[str]:
    """Return all metric keys across all components."""
    keys: set[str] = set()
    for comp in get_metric_descriptors():
        for field in comp.get("fields", []):
            keys.add(field["key"])
    return keys


def get_component_descriptor(kind_or_id: str) -> dict[str, Any] | None:
    """Find a component descriptor by kind or component_id."""
    for comp in get_metric_descriptors():
        if comp.get("kind") == kind_or_id or comp.get("component_id") == kind_or_id:
            return comp
    return None


def get_field_descriptor(kind_or_id: str, key: str) -> dict[str, Any] | None:
    """Find a metric field descriptor by component kind/id and field key."""
    comp = get_component_descriptor(kind_or_id)
    if not comp:
        return None
    for field in comp.get("fields", []):
        if field.get("key") == key:
            return field
    return None
