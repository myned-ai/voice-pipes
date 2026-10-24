"""Session-level health verdict (level + primary culprit + headline).

`_compute_verdict` reduces the flattened turns, per-component aggregates, and
graded anomalies into a single pre-computed verdict the downstream LLM is asked
to confirm or override.
"""

import os
from collections import Counter

from .targets import describe_custom_grading


# ── Health model selection ──
# v1 = the original binary verdict. v2 = graded (healthy/fair/degraded/broken),
# severity-aware, anchored on the critical-path latency estimate.
#
# Default is **v2**, validated against the real-session corpus. Set
# `VP_HEALTH_MODEL=v1` to revert, or pass
# `?health=v1` per request. v1 remains fully supported and pinned by tests.
def _health_model(override: str | None = None) -> str:
    m = (override or os.getenv("VP_HEALTH_MODEL") or "v2").strip().lower()
    return "v1" if m == "v1" else "v2"


# v2 experience bands on critical-path-latency p90 (ms) — the user's stopped-talking
# → first-audio wait. Calibrated to voice-UX norms and the corpus stats:
# <=1.5s good, <=3s acceptable-but-noticeable, >3s poor, >5s unusable.
# The estimate's stages are exclusive (STT tail folded into endpointing, TTS
# stage net of LLM TTFT — see turns.critical_path_latency), so it tracks the
# true speech-end → first-audio wait rather than a conservative over-count.
_CRITICAL_PATH_GOOD_P90_MS = 1500.0
_CRITICAL_PATH_POOR_P90_MS = 3000.0
_CRITICAL_PATH_UNUSABLE_P90_MS = 5000.0

# dominant critical-path stage → responsible component (culprit fallback when there
# are no anomalies but the critical-path latency estimate is high).
_STAGE_COMPONENT = {
    "endpointing_ms": "stt",
    "stt_critical_ms": "stt",
    "llm_ttft_ms": "llm",
    "tts_ttfb_ms": "tts",
    "output_wait_ms": "audio_post",
}

# Maps anomaly kinds → which component is responsible.  Used by the verdict
# logic to attribute blame.  Kept as a flat lookup so adding new anomaly
# types in `_detect_anomalies` only requires extending this table.
_ANOMALY_COMPONENT: dict[str, str] = {
    "llm.ttft_high": "llm",
    "llm.throughput_low": "llm",
    "tts.ttfb_high": "tts",
    "tts.rtf_high": "tts",
    "tts.audio_truncated": "tts",
    "stt.processing_high": "stt",
    "stt.endpointing_high": "stt",
    "bi.likely_self_barge": "stt",   # echo/self-barge is upstream of STT VAD
    "bi.rtt_high": "transport",
    "turn.total_outlier": "turn",
    "tool.roundtrip_high": "client_tools",
    "tool.unresolved": "client_tools",
    "tool.failed": "client_tools",
    # Throughput. These are *defaults*:
    # the detector stamps the real owner on each anomaly (`a["component"]`) —
    # a slow emit is the chain's (`audio_post:<name>`) when
    # emit_rtf − source_rtf is material, else the provider's (`tts` → `s2s`).
    "output.slower_than_realtime": "tts",
    "audio_post.over_budget": "audio_post",
    "client.starvation": "tts",
    "client.jitter": "transport",
    # Annotators are off the audio path: their own culprit, never collapsed.
    "annotator.lagging": "annotator",
    "annotator.degraded": "annotator",
    "annotator.unavailable": "annotator",
    # S2S anchor provenance: info-only,
    # owned by the model. `tts.ttfb_high` / `stt.endpointing_high` raised on
    # S2S turns keep their `tts` / `stt` defaults and collapse to `s2s` below.
    "s2s.anchor_proxy": "s2s",
    # Plausibility / measurement integrity faults (§4.4).
    "measurement.zero_with_work": "measurement",
    "measurement.breakdown_exceeds_stage": "measurement",
    "measurement.stages_disagree": "measurement",
    "measurement.missing": "measurement",
    "measurement.audio_inconsistent": "measurement",
}

# Components that are really one model in a speech-to-speech pipeline
# (providers.s2s). Blame collapses to "s2s" so the verdict can't name a
# TTS/STT/LLM component that doesn't independently exist there.
_S2S_COLLAPSED_COMPONENTS = {"stt", "llm", "tts"}

# Components that live *around* the model and must survive the s2s collapse:
# an inline post-processor (`audio_post:<name>`) sits between the model and
# the transport — folding its time into the model would mis-blame it — and
# an annotator (`annotator:<name>`) is off the audio path entirely.
_S2S_COLLAPSE_EXEMPT_PREFIXES = ("audio_post", "annotator")

# Kinds whose winning the culprit means "the culprit delivers audio slower
# than real time" rather than "is the primary (latency) bottleneck".
_THROUGHPUT_KINDS = {"output.slower_than_realtime", "client.starvation"}


def _kind_p50(anoms: list[dict], kind: str) -> float | None:
    """Median `observed` across the anomalies of one kind (the finding's own
    magnitude, e.g. "TTFB p50 across the turns that breached")."""
    vals = sorted(
        float(a["observed"]) for a in anoms
        if a.get("kind") == kind and isinstance(a.get("observed"), (int, float))
    )
    return vals[len(vals) // 2] if vals else None


def _kind_anchor_source(anoms: list[dict], kind: str) -> str:
    """Most common `anchor_source` stamped on the anomalies of one kind."""
    c = Counter(a.get("anchor_source") for a in anoms if a.get("kind") == kind and a.get("anchor_source"))
    return c.most_common(1)[0][0] if c else "provider"


def _anomaly_component(a: dict) -> str:
    """Owner of one anomaly: the detector's per-anomaly stamp when present,
    else the static kind table."""
    comp = a.get("component")
    if isinstance(comp, str) and comp:
        return comp
    return _ANOMALY_COMPONENT.get(a.get("kind") or "", "other")


def _is_metadata_component(comp: str) -> bool:
    """Annotator components degrade metadata (visemes/blendshapes), never
    speech: they get their own culprit but may only cap the level at `fair`."""
    return comp == "annotator" or comp.startswith("annotator:")


def _collapses_to_s2s(comp: str) -> bool:
    return comp in _S2S_COLLAPSED_COMPONENTS and not comp.startswith(_S2S_COLLAPSE_EXEMPT_PREFIXES)


def _component_label(comp: str) -> str | None:
    """Display label for the named (non stt/llm/tts) culprits; None → default."""
    if comp == "audio_post":
        return "POST-PROCESSOR"
    if comp.startswith("audio_post:"):
        return f"POST-PROCESSOR ({comp[len('audio_post:'):]})"
    if comp == "annotator":
        return "ANNOTATOR — metadata only, audio unaffected"
    if comp.startswith("annotator:"):
        return f"ANNOTATOR ({comp[len('annotator:'):]}) — metadata only, audio unaffected"
    if comp == "client_tools":
        return "CLIENT TOOLS (app-side tool execution)"
    if comp == "turn_detection":
        return "TURN DETECTION (the configured silence timeout)"
    return None


# Severity → culprit-attribution weight. Ranks the primary culprit by
# *user-visible impact* rather than raw flag count, so many negligible /
# off-critical-path flags (e.g. batch STT `processing_ms` that overlaps speech)
# don't outrank a few severe on-critical-path ones (e.g. TTS TTFB). Mirrors the
# ordering `_verdict_level_v2` uses to grade the level; keep monotonic with the
# severity bands in `_anomaly_severity`.
_SEVERITY_WEIGHT: dict[str, float] = {
    "negligible": 0.0,
    "minor": 1.0,
    "moderate": 3.0,
    "severe": 8.0,
}


def _is_user_felt(a: dict) -> bool:
    """Whether a flag marks something a person could have noticed.

    Not when it is off the critical path (it never reached them) and not when
    it is within noise of its target. The same two conditions under which the
    graded verdict gives a flag no weight, so the count in the headline and
    the level beside it are judged alike.
    """
    if a.get("on_critical_path") is False:
        return False
    return (a.get("severity") or "negligible") != "negligible"


def _flag_count_phrase(model: str, total: int, felt: int, diagnostic: int) -> str:
    """The "N anomaly flag(s)" part of a headline."""
    if model != "v2":
        return f"{total} anomaly flag(s)"
    if diagnostic == 0:
        return f"{felt} anomaly flag(s)"
    return (
        f"{felt} anomaly flag(s) (plus {diagnostic} diagnostic, off the critical "
        f"path or within noise of target)"
    )


def _critical_path_percentiles(turns: list[dict]) -> tuple[float | None, float | None]:
    """p50/p90 of per-turn critical-path total, excluding cold-start, terminated,
    and agent-initiated turns (the experience anchor for the v2 verdict).
    Agent-initiated turns have no user-utterance anchor — their latencies are
    measured from turn-open, not end-of-user-speech — so folding them in would
    skew the user-perceived percentiles."""
    vals = sorted(
        v for t in turns
        if not t.get("terminated")
        and not ((t.get("llm") or {}).get("cold_start"))
        and t.get("initiator") != "agent"
        for v in [(t.get("critical_path") or {}).get("total_ms")]
        if isinstance(v, (int, float))
    )
    if not vals:
        return None, None

    def _pct(q: float) -> float:
        return vals[min(len(vals) - 1, int(q * len(vals)))]

    return _pct(0.5), _pct(0.9)


def _verdict_level_v2(
    warn_anoms: list[dict],
    *,
    terminated: int,
    n_turns: int,
    crit_p90: float | None,
    levels: dict[str, float] | None = None,
) -> str:
    """Graded level from critical-path latency + on-critical-path anomaly severity.

    Only anomalies that actually reach the user (on the critical path) escalate:
    an off-critical-path breach (e.g. STT processing that overlaps speech) is a
    diagnostic, not a defect — the generic replacement for the streaming/overlap
    special-cases. Near-target noise (severity == negligible) never escalates.
    Instability is deliberately *not* a driver here; it is surfaced separately as
    an `unstable_components` note, consistent with v1's "unstable-alone stays
    healthy" rule.
    """
    crit_anoms = [a for a in warn_anoms if a.get("on_critical_path") is not False]
    n_severe = sum(1 for a in crit_anoms if a.get("severity") == "severe")
    n_material = sum(
        1 for a in crit_anoms if a.get("severity") in ("minor", "moderate", "severe")
    )
    # The operator's bands when they set any (see targets.resolve_grading),
    # else the defaults above.
    levels = levels or {}
    good = levels.get("good", _CRITICAL_PATH_GOOD_P90_MS)
    poor = levels.get("poor", _CRITICAL_PATH_POOR_P90_MS)
    unusable = levels.get("unusable", _CRITICAL_PATH_UNUSABLE_P90_MS)
    if terminated > 0 and terminated >= n_turns * 0.5:
        return "broken"
    if crit_p90 is not None and crit_p90 > unusable:
        return "broken"
    if n_severe > 0 or (crit_p90 is not None and crit_p90 > poor):
        return "degraded"
    if n_material > 0 or (crit_p90 is not None and crit_p90 > good):
        return "fair"
    return "healthy"


def _compute_verdict(
    turns: list[dict],
    aggregates: list[dict],
    anomalies: list[dict],
    providers: dict,
    model: str | None = None,
    grading: dict | None = None,
) -> dict:
    """Pre-computed session-level health verdict.

    `grading` is the result of `targets.resolve_grading`. Its `levels` set the
    bands the graded model uses, and its `custom` list -- every rule that
    differs from its default -- is carried onto the verdict and into the
    headline, so a grade reached by relaxed rules never travels without them.

    `model` selects the health engine (defaults to `VP_HEALTH_MODEL` env, else
    v1). v1 = the original binary `healthy|degraded|broken`. v2 = graded
    `healthy|fair|degraded|broken`, anchored on the critical-path latency estimate + anomaly
    severity so internal-metric noise that never reached the user doesn't pin
    the verdict.

    Output shape (v2 adds `critical_path_p50_ms`/`critical_path_p90_ms`):
        {
          level, primary_culprit, headline,
          supporting_anomaly_kinds: [str], unstable_components: [str],
        }

    The synthesis is deliberately simple; the downstream LLM is still asked to
    confirm or override the verdict — this just primes the read.
    """
    model = _health_model(model)

    # Nothing to grade is not the same as nothing wrong.
    #
    # Every rule below reads per-turn metrics. A session whose turns carry no
    # metrics breaches no target, raises no anomaly, and so arrived at
    # "healthy — no anomalies flagged" on the strength of having no evidence
    # at all. That is the most misleading answer available: it was produced
    # for a session whose single turn took sixteen seconds, and again for one
    # that recorded a turn and kept no row for it.
    #
    # A turn counts as gradeable if it reported any measurement at all,
    # wherever it sits -- a stage latency, a total, a critical-path estimate.
    # Enumerating the places a metric may live would go stale the first time
    # a stage is added, and the question here is only "is there evidence",
    # so look for a number anywhere and ignore the keys that identify the
    # turn rather than measure it.
    _IDENTIFIERS = {"utt", "utterance_id", "t_offset_s"}

    def _has_metric(obj) -> bool:
        if isinstance(obj, bool):
            return False
        if isinstance(obj, (int, float)):
            return True
        if isinstance(obj, dict):
            return any(k not in _IDENTIFIERS and _has_metric(v)
                       for k, v in obj.items())
        if isinstance(obj, (list, tuple)):
            return any(_has_metric(v) for v in obj)
        return False

    def _gradeable(t: dict) -> bool:
        return _has_metric(t)

    # Per-component aggregates are themselves reduced from measured turns, so
    # a populated aggregate is evidence even when the turn rows that produced
    # it are not in hand.
    _aggregates_measured = any(
        isinstance(a.get("count"), int) and a["count"] > 0 for a in aggregates
    )

    if turns and not _aggregates_measured and not any(_gradeable(t) for t in turns):
        return {
            "level": "unknown",
            "primary_culprit": "none",
            "headline": (
                f"No per-turn metrics for {len(turns)} recorded turn(s) — "
                f"the session cannot be graded. This is missing evidence, "
                f"not a clean bill of health."
            ),
            "supporting_anomaly_kinds": [],
            "unstable_components": [],
            "model": model,
            **({"critical_path_p50_ms": None, "critical_path_p90_ms": None}
               if model == "v2" else {}),
        }

    # Severity bucket: only `warn` anomalies count toward the verdict.
    # `info`-level rows (estimated TTFT, self-barge hints) are noise here.
    warn_anoms = [a for a in anomalies if a.get("level") != "info"]
    # Annotator anomalies are a separate dimension (metadata, not audio): they
    # never drive the level beyond `fair` and never out-rank an audio culprit.
    audio_anoms = [a for a in warn_anoms if not _is_metadata_component(_anomaly_component(a))]
    meta_anoms = [a for a in warn_anoms if _is_metadata_component(_anomaly_component(a))]

    by_component: dict[str, int] = {}
    # Largest target-overage per component, used to break culprit ties:
    # when two components have the same warn count, the one that breached
    # its target by the wider margin is the more likely bottleneck.
    # Deviation is direction-agnostic — latency/rtf are "higher is worse"
    # while throughput is "lower is worse", so max(obs/tgt, tgt/obs) >= 1
    # expresses "how many multiples past target" for either direction.
    overage_by_component: dict[str, float] = {}
    # User-visible impact per component: severity weight, zeroed for
    # off-critical-path breaches (they never reached the user). This — not the
    # raw flag count — is the primary culprit key, mirroring `_verdict_level_v2`.
    impact_by_component: dict[str, float] = {}
    # The single worst finding per component. Summing alone let a component
    # with several moderate flags outrank one with a single severe flag: a
    # session whose caller waited thirteen seconds for a lost turn was
    # headlined for its silence timeout, because the timeout flagged on three
    # turns and the lost turn on one. What the caller noticed is the worst
    # thing that happened, so that ranks first, and the sum breaks ties.
    worst_by_component: dict[str, float] = {}
    # (component, kind) → (impact, count): which kind carries a culprit's blame,
    # so the headline can say "delivers audio slower than real time" when the
    # dominant finding is throughput rather than latency.
    impact_by_component_kind: dict[tuple[str, str], list[float]] = {}
    for a in warn_anoms:
        comp = _anomaly_component(a)
        by_component[comp] = by_component.get(comp, 0) + 1
        weight = _SEVERITY_WEIGHT.get(a.get("severity") or "negligible", 0.0)
        # An off-critical-path breach is a diagnostic, not a user-felt defect —
        # exclude it from culprit weight (same rule `_verdict_level_v2` applies).
        if a.get("on_critical_path") is False:
            weight = 0.0
        impact_by_component[comp] = impact_by_component.get(comp, 0.0) + weight
        worst_by_component[comp] = max(worst_by_component.get(comp, 0.0), weight)
        ck = impact_by_component_kind.setdefault((comp, a.get("kind") or ""), [0.0, 0.0])
        ck[0] += weight
        ck[1] += 1
        obs, tgt = a.get("observed"), a.get("target")
        if (
            isinstance(obs, (int, float))
            and isinstance(tgt, (int, float))
            and obs > 0
            and tgt > 0
        ):
            deviation = max(obs / tgt, tgt / obs)
            overage_by_component[comp] = max(
                overage_by_component.get(comp, 0.0), deviation
            )

    unstable = [
        agg.get("kind") for agg in aggregates
        if agg.get("unstable") and agg.get("kind")
    ]

    # Determine level.  `broken` if anything terminated mid-flight or
    # > 30% of turns hit at least one warn anomaly.  Pure stability
    # (no warn anomalies, just a high p99/p50 ratio) is surfaced as
    # `healthy` with an `unstable_components` note rather than
    # `degraded`: jitter alone is not a user-visible defect when no
    # individual turn breached its target.  This avoids cold-socket
    # outliers (already demoted to info via connect-dominant
    # detection) from pinning the verdict to `degraded`.
    n_turns = max(len(turns), 1)
    terminated = sum(1 for t in turns if t.get("terminated"))
    # Headline count. v1 counts every warn-level flag, unchanged. v2 counts
    # the ones a person could have felt, and names the rest as diagnostics.
    #
    # The v2 level and culprit already ignore a flag that is off the critical
    # path or within noise of its target -- both weigh nothing above. Counting
    # them in the headline anyway had it contradict the verdict beside it:
    # "8 anomaly flag(s)" on a session where four were `stt.processing_high`
    # rows the engine had itself marked `on_critical_path: false`, severity
    # negligible, contributing zero. A reader has no way to know that half the
    # number is bookkeeping.
    total_warn = sum(by_component.values())
    total_warn_audio = len(audio_anoms)
    felt_warn = sum(1 for a in warn_anoms if _is_user_felt(a))
    diagnostic_warn = total_warn - felt_warn
    p50, p90 = _critical_path_percentiles(turns)
    if model == "v2":
        level = _verdict_level_v2(
            audio_anoms, terminated=terminated, n_turns=n_turns, crit_p90=p90,
            levels=(grading or {}).get("levels"),
        )
    else:
        # v1 (unchanged): binary; any warn → degraded.
        if terminated > 0 and terminated >= n_turns * 0.5:
            level = "broken"
        elif total_warn_audio == 0:
            level = "healthy"
        elif total_warn_audio / n_turns > 1.0 or unstable:
            level = "degraded"
        else:
            level = "degraded"
    # Annotator cap: an annotator-only
    # problem degrades metadata, not speech — at most `fair`, in both models.
    if level == "healthy" and meta_anoms:
        level = "fair"

    # A `broken` that is purely "the turn(s) were terminated mid-response" —
    # no metric anomaly, critical-path latency not unusable — has no
    # bottleneck to name. Skip the dominant-stage fallback below so the
    # headline can say so instead of "NONE had issues" / "<stage> is the
    # primary bottleneck; 0 anomaly flag(s)".
    terminated_only = (
        level == "broken"
        and terminated > 0
        and total_warn_audio == 0
        and not (p90 is not None and p90 > _CRITICAL_PATH_UNUSABLE_P90_MS)
    )

    # Primary culprit: component with the greatest user-visible impact
    # (severity-weighted, on-critical-path). Ties break on widest target-overage,
    # then raw warn count, then instability. Ranking by impact rather than raw
    # count stops many negligible / off-critical-path flags (e.g. batch STT
    # `processing_ms` that overlaps speech) from outranking a few severe on-path
    # breaches (e.g. TTS TTFB) — the mis-attribution the count-only key caused.
    # Annotator components rank in a lower tier: they only win when no audio
    # component has any flag, so "audio unaffected" is always true when said.
    if level == "healthy":
        culprit = "none"
    else:
        ranked = sorted(
            by_component.items(),
            key=lambda kv: (
                0 if _is_metadata_component(kv[0]) else 1,  # 0) audio before metadata
                worst_by_component.get(kv[0], 0.0),       # 1) the worst single finding
                impact_by_component.get(kv[0], 0.0),      # 2) severity × critical-path impact, summed
                overage_by_component.get(kv[0], 0.0),     # 2) widest target overage
                kv[1],                                    # 3) raw warn count
                1 if kv[0] in unstable else 0,            # 4) instability
            ),
            reverse=True,
        )
        culprit = ranked[0][0] if ranked else (unstable[0] if unstable else "none")
        # v2 fallback: a fair/degraded verdict driven purely by critical-path latency
        # latency (no anomalies) still needs a culprit — attribute it to the
        # component owning the dominant critical-path stage across turns.
        if culprit == "none" and not terminated_only:
            dom = Counter(
                (t.get("critical_path") or {}).get("dominant_stage")
                for t in turns
                if not t.get("terminated") and not ((t.get("llm") or {}).get("cold_start"))
            )
            dom.pop(None, None)
            if dom:
                culprit = _STAGE_COMPONENT.get(dom.most_common(1)[0][0], "none")

    # Speech-to-speech collapse: with one model doing STT+LLM+TTS, blaming a
    # "TTS" component is meaningless (and misleading — `ttfb_ms` there is the
    # model's whole response latency). Collapse per-stage blame to "s2s".
    # `audio_post:<name>` / `annotator:<name>` survive the collapse: the chain
    # is not the model (folding it in would mis-blame the model), and
    # annotators are off the audio path.
    s2s = bool(providers.get("s2s")) if isinstance(providers, dict) else False
    if s2s and _collapses_to_s2s(culprit):
        culprit = "s2s"

    # Headline.  Short, factual, no recommendations — the LLM adds those.
    if level == "healthy":
        if unstable:
            unstable_label = ", ".join(c.upper() for c in unstable)
            headline = (
                f"Healthy session — {n_turns} turn(s), no warn-level anomalies; "
                f"{unstable_label} latency is jittery (p99/p50 > 5) but no "
                "individual turn breached its target."
            )
        else:
            headline = f"Healthy session — {n_turns} turn(s), no anomalies flagged."
    elif terminated_only and culprit == "none":
        term_ids = [t.get("utt") for t in turns if t.get("terminated") and t.get("utt") is not None]
        if len(term_ids) == 1:
            which = f"turn {term_ids[0]}"
        elif term_ids:
            which = "turns " + ", ".join(str(i) for i in term_ids)
        else:
            which = f"{terminated} turn(s)"
        headline = (
            f"Broken session — {which} terminated mid-response (client disconnect / "
            f"interruption); no metric anomalies across {n_turns} turn(s)."
        )
    else:
        prov_label = ""
        culprit_label = culprit.upper()
        if culprit == "s2s":
            blocks = providers if isinstance(providers, dict) else {}
            ident = next(
                (
                    (blocks.get(k) or {}).get("model") or (blocks.get(k) or {}).get("provider")
                    for k in ("llm", "tts", "stt")
                    if (blocks.get(k) or {}).get("model") or (blocks.get(k) or {}).get("provider")
                ),
                None,
            )
            culprit_label = (
                f"the {ident} speech-to-speech model" if ident
                else "the speech-to-speech model"
            )
        elif _component_label(culprit) is not None:
            culprit_label = _component_label(culprit)  # type: ignore[assignment]
        elif culprit in {"llm", "stt", "tts"}:
            block = (providers.get(culprit) or {}) if isinstance(providers, dict) else {}
            ident = block.get("model") or block.get("provider") or block.get("voice")
            if ident:
                prov_label = f" ({ident})"
        # In an S2S pipeline the tts/llm/stt aggregate buckets all measure the
        # same model, so instability in any of them is the culprit's.
        culprit_kinds = _S2S_COLLAPSED_COMPONENTS | {"s2s"} if culprit == "s2s" else {culprit}
        unstable_clause = (
            " Response latency is unstable (p99/p50 > 5)."
            if any(c in culprit_kinds for c in unstable) else ""
        )
        # The finding that carries the culprit's blame (by impact, then count).
        top_kind = None
        top_key = (-1.0, -1.0)
        for (comp, kind), (imp, cnt) in impact_by_component_kind.items():
            if comp in culprit_kinds and (imp, cnt) > top_key:
                top_key, top_kind = (imp, cnt), kind
        if _is_metadata_component(culprit):
            late_turns = len({a.get("utt") for a in meta_anoms})
            headline = (
                f"{level.title()} session — {culprit_label}; annotations were late or "
                f"missing on {late_turns} turn(s), "
                f"{_flag_count_phrase(model, total_warn, felt_warn, diagnostic_warn)} across "
                f"{n_turns} turn(s).{unstable_clause}"
            )
        else:
            if culprit == "none":
                verb = "had issues"
            elif top_kind in _THROUGHPUT_KINDS and culprit in {"tts", "s2s"}:
                # The provider itself streamed behind real time (chain overhead
                # negligible). A post-processor culprit keeps the plain
                # "primary bottleneck" wording (e.g. "POST-PROCESSOR (grpc) is
                # the primary bottleneck").
                verb = "delivers audio slower than real time"
            elif culprit == "s2s" and top_kind == "tts.ttfb_high":
                # The model's response latency from the end of speech: name
                # the anchor the number was measured from — a proxy anchor
                # under-reports.
                p50 = _kind_p50(warn_anoms, top_kind)
                src = _kind_anchor_source(warn_anoms, top_kind)
                verb = (
                    f"answers late (TTFB p50 {p50:.0f} ms from {src})" if p50 is not None
                    else f"answers late (TTFB measured from {src})"
                )
            elif culprit == "s2s" and top_kind == "stt.endpointing_high":
                # The provider's VAD/transcription is late to notice the user
                # stopped — a VAD-tuning / provider-choice fix, not a model
                # latency one, so the headline must differ.
                p50 = _kind_p50(warn_anoms, top_kind)
                verb = (
                    f"is slow to notice the end of speech (endpointing p50 {p50:.0f} ms)"
                    if p50 is not None else "is slow to notice the end of speech"
                )
            elif culprit == "turn_detection":
                # A setting, not a component that ran slowly: say what it is
                # set to and what that costs, so nobody goes looking for a
                # slow recogniser.
                p50 = _kind_p50(warn_anoms, "stt.endpointing_high")
                floors = [
                    a.get("silence_timeout_ms") for a in warn_anoms
                    if isinstance(a.get("silence_timeout_ms"), (int, float))
                ]
                floor = f"{floors[0]:.0f} ms" if floors else "its configured timeout"
                verb = (
                    f"holds each turn for {floor} of silence before committing "
                    f"(endpointing p50 {p50:.0f} ms) — a setting, not a slow component"
                    if p50 is not None else
                    f"holds each turn for {floor} of silence before committing — "
                    f"a setting, not a slow component"
                )
            else:
                verb = "is the primary bottleneck"
            headline = (
                f"{level.title()} session — {culprit_label}{prov_label} {verb}; "
                f"{_flag_count_phrase(model, total_warn, felt_warn, diagnostic_warn)} "
                f"across {n_turns} turn(s).{unstable_clause}"
            )

    # The disclosure. Appended to the headline rather than left to a field a
    # reader may never open: the headline is what gets quoted, screenshotted
    # and pasted into a status update, and it must not be quotable without it.
    custom = list((grading or {}).get("custom") or [])
    disclosure = describe_custom_grading(custom)
    if disclosure:
        headline = f"{headline} {disclosure}"

    out = {
        "level": level,
        "primary_culprit": culprit,
        "headline": headline,
        "supporting_anomaly_kinds": sorted({a.get("kind") for a in warn_anoms if a.get("kind")}),
        "unstable_components": unstable,
    }
    if custom:
        out["custom_grading"] = custom
    if model == "v2":
        # Surface the experience anchor so the UI/report can explain the level.
        out["model"] = "v2"
        out["critical_path_p50_ms"] = round(p50, 1) if p50 is not None else None
        out["critical_path_p90_ms"] = round(p90, 1) if p90 is not None else None
    return out
