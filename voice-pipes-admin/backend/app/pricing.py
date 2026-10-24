"""Pricing book + per-component cost computation.

Loads ``pricing.yaml`` once at startup and exposes small pure helpers that
compute USD cost for STT / TTS / LLM usage.  Local providers are treated as
zero-cost.  Helpers tolerate ``None`` inputs to keep callers concise.
"""
from __future__ import annotations

from pathlib import Path
from threading import Lock
from typing import Any

import yaml

_PRICING_PATH = Path(__file__).parent / "pricing.yaml"
_BOOK: dict[str, Any] | None = None
_LOCK = Lock()


def _load() -> dict[str, Any]:
    """Load and cache the pricing book.  Returns an empty book on failure so
    callers always get a usable structure."""
    global _BOOK
    if _BOOK is not None:
        return _BOOK
    with _LOCK:
        if _BOOK is not None:
            return _BOOK
        try:
            with open(_PRICING_PATH, encoding="utf-8") as f:
                data = yaml.safe_load(f) or {}
        except (OSError, yaml.YAMLError):
            data = {}
        _BOOK = {
            "llm": data.get("llm") or {},
            "stt": data.get("stt") or {},
            "tts": data.get("tts") or {},
            "monthly_caps_usd": data.get("monthly_caps_usd") or {},
            "local_providers": {p.lower() for p in (data.get("local_providers") or [])},
        }
        return _BOOK


def reload_pricing() -> dict[str, Any]:
    """Drop the cache and re-read the file (useful for ops without a restart)."""
    global _BOOK
    with _LOCK:
        _BOOK = None
    return _load()


def get_pricing_book() -> dict[str, Any]:
    """Public, immutable-ish view of the loaded pricing book."""
    book = _load()
    return {
        "llm": dict(book["llm"]),
        "stt": dict(book["stt"]),
        "tts": dict(book["tts"]),
        "monthly_caps_usd": dict(book["monthly_caps_usd"]),
        "local_providers": sorted(book["local_providers"]),
    }


def is_local_provider(provider: str | None) -> bool:
    if not provider:
        return False
    return provider.lower() in _load()["local_providers"]


def _provider_root(provider: str | None) -> str:
    """Return the canonical root key used by ``monthly_caps_usd`` for *provider*.

    For ``"openai/gpt-4o-mini"`` returns ``"openai"``.  For a bare name like
    ``"elevenlabs"`` returns it unchanged (lowercased)."""
    if not provider:
        return ""
    return provider.split("/", 1)[0].lower()


def llm_cost_usd(model: str | None, prompt_tokens: int | None, output_tokens: int | None) -> float | None:
    """USD cost for one LLM turn.  Returns 0 for local providers, ``None`` when
    the model is unknown to the price book (caller can treat as untracked)."""
    if not model:
        return None
    if is_local_provider(model.split("/", 1)[0]):
        return 0.0
    entry = _load()["llm"].get(model)
    if entry is None:
        # Try a looser match: providers may emit the bare model name
        # ("gpt-4o-mini") without the vendor prefix.
        for key, val in _load()["llm"].items():
            if key.split("/", 1)[-1] == model:
                entry = val
                break
    if entry is None:
        return None
    p = (prompt_tokens or 0) * (entry.get("input_per_mtok") or 0) / 1_000_000
    o = (output_tokens or 0) * (entry.get("output_per_mtok") or 0) / 1_000_000
    return round(p + o, 6)


def stt_cost_usd(provider: str | None, audio_seconds: float | None) -> float | None:
    if not provider or not audio_seconds:
        return 0.0 if is_local_provider(provider) else None
    if is_local_provider(provider):
        return 0.0
    entry = _load()["stt"].get(provider)
    if entry is None:
        return None
    minutes = float(audio_seconds) / 60.0
    return round(minutes * (entry.get("per_audio_minute") or 0), 6)


def tts_cost_usd(provider: str | None, chars: int | None) -> float | None:
    if not provider or not chars:
        return 0.0 if is_local_provider(provider) else None
    if is_local_provider(provider):
        return 0.0
    entry = _load()["tts"].get(provider)
    if entry is None:
        return None
    return round(float(chars) * (entry.get("per_million_chars") or 0) / 1_000_000, 6)


def monthly_cap_for(provider: str | None) -> float | None:
    """Return the configured monthly USD cap for *provider* (root key), or None."""
    root = _provider_root(provider)
    if not root:
        return None
    cap = _load()["monthly_caps_usd"].get(root)
    if cap is None:
        return None
    try:
        return float(cap)
    except (TypeError, ValueError):
        return None
