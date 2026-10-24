"""SLA target resolution for the health engine.

Effective targets = defaults → hardcoded provider table → provider-declared
descriptor budgets → speech-to-speech defaults (S2S sessions only) →
caller `?targets=` overrides (last wins). Operator rules (console env, the
pipeline's `[grading]`) are laid over that by `resolve_grading`.
"""

import logging

logger = logging.getLogger(__name__)


# Default SLA targets.  Tuned to the values called out in the production
# go-live checklist and the per-turn metrics registry.
# All numeric; units are baked into the field name.  These are the *baseline*
# targets — `_resolve_targets` may relax/tighten them based on the resolved
# provider+model identity (e.g. cloud LLMs get a higher `ttft_ms` budget).
_DEFAULT_TARGETS: dict[str, float] = {
    "ttft_ms": 300,
    "ttfb_ms": 400,
    "endpointing_ms": 500,
    "stt_processing_ms": 800,
    "draining_handshake_ms": 1500,
    "rtf_max": 0.6,
    "tokens_per_sec_min": 30,
    "barge_in_rtt_ms": 400,
    "self_barge_played_ms_max": 200,  # interrupted with played_ms below this is likely echo/self-barge
    "outlier_total_ms_factor": 2.0,    # turn flagged when total_ms > factor * session median
    # Client tool dispatch → client result. Tools do real-world client work
    # (navigation, media start), so the budget is deliberately looser than the
    # speech-latency targets; a breach only escalates the verdict when spoken
    # output was gated on the call (see anomalies._tool_call_anomalies).
    "tool_roundtrip_ms": 2000,
    # Throughput. rtf = wall ÷ audio,
    # > 1.0 = slower than real time. `emit_rtf` is the streaming phase after
    # the post-audio chain; the anomaly fires at > 1.0 (the client will starve
    # after its prebuffer), the budget is the 0.9 the descriptor advertises.
    "emit_rtf_max": 0.9,
    # Per inline post-processor: mean call latency ÷ mean chunk audio duration.
    # > 1.0 = cannot keep up with its own input; 0.8 leaves headroom for jitter.
    "post_budget_ratio_max": 0.8,
    # Annotators: how late an annotation may reach
    # the client relative to the audio it describes. Defaults to the client's
    # playback prebuffer — anything later than that is already playing.
    "annotation_lag_budget_ms": 200,
    # How much longer the caller may wait than the server timed before the
    # difference is a lost attempt to speak rather than clock noise. Client
    # and server clocks are aligned by the ping offset to within tens of
    # milliseconds; three seconds is a whole re-try.
    "unanswered_wait_ms": 3000,
}

# Per-provider target overrides applied before any caller `?targets=` overrides.
# Keyed by lowercase provider id; sub-keys may match against `model.startswith(...)`.
# Only fields present here override the defaults — everything else stays as-is.
_PROVIDER_TARGETS: dict[str, dict] = {
    # OpenAI cloud streaming: a healthy gpt-4o-mini call streams ~25 tok/s
    # with a 600-1000ms TTFT to East Coast US.  Holding it to 300ms / 30 tok/s
    # produces noise.
    "openai-compat": {
        "_default": {"ttft_ms": 800, "tokens_per_sec_min": 18},
        "gpt-4o-mini": {"ttft_ms": 800, "tokens_per_sec_min": 20},
        "gpt-4o":      {"ttft_ms": 1000, "tokens_per_sec_min": 15},
        "gpt-4":       {"ttft_ms": 1500, "tokens_per_sec_min": 12},
        "gpt-3.5":     {"ttft_ms": 600, "tokens_per_sec_min": 40},
    },
    "openai": {  # alias
        "_default": {"ttft_ms": 800, "tokens_per_sec_min": 18},
    },
}


# Speech-to-speech sessions: one cloud realtime model is the whole
# STT + LLM + TTS stack, and a healthy one (Gemini Live, OpenAI Realtime,
# ElevenLabs Agents) answers 0.8-1.3 s after the end of speech and notices
# that end 0.5-1 s late, the latter bounded below by the turn detector's own
# silence timeout. The cascade defaults (ttfb 400 ms, endpointing 500 ms)
# and the engine's generic realtime descriptor (ttfb 600 ms) would flag
# every turn of every healthy session. These flag only a turn slower than a
# healthy cloud one. Applied after descriptor budgets, because the realtime
# descriptor's budget is the engine's generic one, not a provider's; the
# console's ADMIN_GRADING_* and the pipeline's own [grading] still win.
_S2S_TARGETS: dict[str, float] = {
    "ttfb_ms": 1200,
    "endpointing_ms": 1200,
}


# Maps a descriptor field `key` → the target name the health engine uses.
# Lets a component *declare* its own budget (via `health.budget`) so the engine
# no longer needs the hardcoded `_PROVIDER_TARGETS` table for it.
_DESCRIPTOR_BUDGET_TO_TARGET: dict[str, str] = {
    "ttft_ms": "ttft_ms",
    "ttfb_ms": "ttfb_ms",
    "endpointing_ms": "endpointing_ms",
    "processing_ms": "stt_processing_ms",
    "rtf": "rtf_max",
    # `output` / `audio_post:*` / `annotator:*` descriptors.
    "emit_rtf": "emit_rtf_max",
    "budget_ratio": "post_budget_ratio_max",
    "lag_max_ms": "annotation_lag_budget_ms",
}


def _targets_from_descriptors(metric_descriptors: list | None) -> dict[str, float]:
    """Extract provider-declared budgets from `metric_descriptors`.

    Reads each field's optional `health.budget` metadata. Empty for
    captures/providers that don't declare health metadata — the caller then
    falls back to the built-in tables. This is the
    generic replacement for `_PROVIDER_TARGETS`: a provider owns its own budget.
    """
    out: dict[str, float] = {}
    for comp in (metric_descriptors or []):
        for f in (comp.get("fields") or []):
            budget = (f.get("health") or {}).get("budget")
            target_key = _DESCRIPTOR_BUDGET_TO_TARGET.get(f.get("key"))
            if budget is not None and target_key is not None:
                out[target_key] = float(budget)
    return out


def _resolve_targets(
    providers: dict,
    override_raw: str | None,
    metric_descriptors: list | None = None,
) -> dict[str, float]:
    """Resolve effective targets: defaults → hardcoded provider table →
    **provider-declared descriptor budgets** → the speech-to-speech defaults
    for an S2S session → caller `?targets=` overrides (last wins). Descriptor budgets override the hardcoded table so a provider
    that ships `health.budget` no longer needs an entry in `_PROVIDER_TARGETS`;
    the table remains the fallback for captures/providers that don't declare
    them (logged so we can see when it's still relied on)."""
    targets: dict[str, float] = dict(_DEFAULT_TARGETS)

    llm = providers.get("llm") or {}
    llm_provider = (llm.get("provider") or "").lower()
    llm_model = (llm.get("model") or "").lower()
    if llm_provider in _PROVIDER_TARGETS:
        ptable = _PROVIDER_TARGETS[llm_provider]
        # First apply `_default`, then any matching model-family entry.
        targets.update(ptable.get("_default") or {})
        for family, overrides in ptable.items():
            if family == "_default":
                continue
            if llm_model.startswith(family.lower()):
                targets.update(overrides)
                break

    # Provider-declared budgets win over the hardcoded table.
    declared = _targets_from_descriptors(metric_descriptors)
    if declared:
        targets.update(declared)
    elif metric_descriptors:
        # Descriptors present but none declared budgets → still on the fallback
        # table. A quiet signal for the Phase-6 deprecation of the tables.
        logger.debug("health targets: no descriptor budgets; using fallback table")

    if providers.get("s2s"):
        targets.update(_S2S_TARGETS)

    if override_raw:
        for piece in override_raw.split(","):
            if ":" not in piece:
                continue
            k, v = piece.split(":", 1)
            k = k.strip()
            try:
                targets[k] = float(v.strip())
            except ValueError:
                continue
    return targets


# Kept for backward compatibility; new callers should use `_resolve_targets`.
def _parse_targets_override(raw: str | None) -> dict[str, float]:
    return _resolve_targets({}, raw)


# ── Operator grading rules ──────────────────────────────────
#
# The defaults above describe a good conversational agent in general. A
# deployment may have decided otherwise on purpose -- a 700 ms silence timeout
# so callers are never cut off mid-thought, a larger model that answers more
# slowly -- and graded against the defaults it reports `fair` on every session
# for ever. A verdict that is always amber stops being read. So an operator may
# set their own, from two places:
#
#   console   ADMIN_GRADING_TARGETS / ADMIN_GRADING_LEVELS in the console's
#             environment: a house default for every pipeline.
#   pipeline  `[grading]` in the pipeline TOML, recorded into each session's
#             `metadata.json`: that pipeline's own rules. Wins over the console.
#
# The pipeline's rules travel in the capture, so a session is graded by what
# was in force when it ran and editing the TOML never re-grades history.
#
# One rule is not negotiable: a report graded by custom rules says so, naming
# every value that differs from its default. Relaxing a target changes the
# grade. It must never be able to change it quietly.

# Bands on critical-path p90 (ms): at or under `good` a session can be healthy,
# over `poor` it is degraded, over `unusable` it is broken.
DEFAULT_LEVELS: dict[str, float] = {"good": 1500.0, "poor": 3000.0, "unusable": 5000.0}

_LEVEL_ORDER = ("good", "poor", "unusable")


def _parse_pairs(raw: str | None) -> dict[str, float]:
    """`"a:1,b:2"` → `{"a": 1.0, "b": 2.0}`, dropping anything malformed."""
    out: dict[str, float] = {}
    for piece in (raw or "").split(","):
        if ":" not in piece:
            continue
        k, v = piece.split(":", 1)
        try:
            out[k.strip()] = float(v.strip())
        except ValueError:
            continue
    return out


def _clean_numbers(raw, allowed) -> dict[str, float]:
    """Keep the entries of `raw` that name something in `allowed` and hold a
    positive finite number. A rule this cannot read is dropped and logged --
    never guessed at, and never allowed to take the report down."""
    if not isinstance(raw, dict):
        return {}
    out: dict[str, float] = {}
    for k, v in raw.items():
        ok = (
            k in allowed
            and isinstance(v, (int, float))
            and not isinstance(v, bool)
            and v == v  # not NaN
            and 0 < v < float("inf")
        )
        if ok:
            out[k] = float(v)
        else:
            logger.warning("grading: ignoring %r = %r (unknown name or not a positive number)", k, v)
    return out


def _console_grading() -> tuple[dict[str, float], dict[str, float]]:
    """The console's house rules, from its environment. Read on each call: it
    is two small strings, and a cached copy would outlive a test's override."""
    import os
    targets = _clean_numbers(_parse_pairs(os.getenv("ADMIN_GRADING_TARGETS")), _DEFAULT_TARGETS)
    levels = _clean_numbers(_parse_pairs(os.getenv("ADMIN_GRADING_LEVELS")), DEFAULT_LEVELS)
    return targets, levels


def resolve_grading(
    providers: dict,
    metadata: dict | None,
    override_raw: str | None = None,
) -> dict:
    """Everything a session is graded by, and where each custom value came from.

    Layered, last wins: built-in and component-declared defaults → the
    console's house rules → the pipeline's own `[grading]` → a `?targets=` on
    the request. Returns::

        {"targets": {...}, "levels": {...},
         "custom": [{"name", "value", "default", "source"}, ...]}

    `custom` lists every value that differs from what the session would have
    been graded by with no operator rules at all. It is the disclosure: a
    report shows it whenever it is non-empty, so a lenient grade always
    arrives with the leniency spelled out.

    Every caller that grades a session goes through here -- the report, the
    session page and the analytics rollup -- so they cannot disagree about one
    session.
    """
    metadata = metadata or {}
    base = _resolve_targets(providers, None, metric_descriptors=metadata.get("metric_descriptors"))
    targets = dict(base)
    levels = dict(DEFAULT_LEVELS)
    source: dict[str, str] = {}

    console_targets, console_levels = _console_grading()
    pipeline = metadata.get("grading") if isinstance(metadata.get("grading"), dict) else {}
    layers = (
        ("console", console_targets, console_levels),
        ("pipeline config", _clean_numbers(pipeline.get("targets"), _DEFAULT_TARGETS),
         _clean_numbers(pipeline.get("levels"), DEFAULT_LEVELS)),
        ("request", _parse_pairs(override_raw), {}),
    )
    for name, layer_targets, layer_levels in layers:
        for k, v in layer_targets.items():
            targets[k] = v
            source[k] = name
        for k, v in layer_levels.items():
            levels[k] = v
            source[f"levels.{k}"] = name

    # Bands out of order would make a level unreachable. The pipeline's own are
    # validated at worker startup; this catches a console rule that, laid over
    # a pipeline's, breaks the order between them.
    ordered = [levels[k] for k in _LEVEL_ORDER]
    if ordered != sorted(ordered) or len(set(ordered)) != len(ordered):
        logger.warning("grading: level bands %s are out of order; using the defaults", levels)
        levels = dict(DEFAULT_LEVELS)
        source = {k: v for k, v in source.items() if not k.startswith("levels.")}

    custom = [
        {"name": k, "value": targets[k], "default": base.get(k), "source": source[k]}
        for k in sorted(targets) if k in source and targets[k] != base.get(k)
    ] + [
        {"name": f"levels.{k}", "value": levels[k], "default": DEFAULT_LEVELS[k],
         "source": source[f"levels.{k}"]}
        for k in _LEVEL_ORDER
        if f"levels.{k}" in source and levels[k] != DEFAULT_LEVELS[k]
    ]
    return {"targets": targets, "levels": levels, "custom": custom}


def describe_custom_grading(custom: list[dict]) -> str | None:
    """One sentence naming every rule that differs from its default, or None."""
    if not custom:
        return None

    def _n(v) -> str:
        return f"{v:g}" if isinstance(v, (int, float)) else "none"

    parts = [
        f"{c['name']} {_n(c['value'])} (default {_n(c['default'])}, from {c['source']})"
        for c in custom
    ]
    return "Graded against custom rules: " + "; ".join(parts) + "."
