import json
from datetime import UTC, datetime

from ..captures.reader import is_empty_turn
from ..report.analysis import (
    _SEVERITY_WEIGHT,
    _apply_tool_context,
    _component_aggregates,
    _compute_verdict,
    _detect_anomalies,
    _flatten_turn,
    _health_model,
)
from ..report.turns import tool_ack_ms, tool_roundtrip_ms

# Report schema version. Bump on any breaking change to the JSON shape so
# consumers (custom dashboards, customer integrations) can guard against drift.
_REPORT_VERSION = "1.0"

# Per-metric display metadata so a consumer can render the numbers correctly
# without hardcoding units/direction. Keyed by the canonical `targets` key.
# `direction`: lower_better | higher_better | threshold. Must cover every key
# `_resolve_targets` can emit — `test_report_meta` asserts this so it can't drift.
_METRIC_META: dict[str, dict] = {
    "ttft_ms":                {"label": "LLM time-to-first-token",         "unit": "ms",    "direction": "lower_better"},
    "ttfb_ms":                {"label": "TTS time-to-first-byte",          "unit": "ms",    "direction": "lower_better"},
    "endpointing_ms":         {"label": "VAD endpointing (silence→commit)","unit": "ms",    "direction": "lower_better"},
    "stt_processing_ms":      {"label": "STT transcription latency",       "unit": "ms",    "direction": "lower_better"},
    "draining_handshake_ms":  {"label": "Draining handshake",              "unit": "ms",    "direction": "lower_better"},
    "rtf_max":                {"label": "TTS realtime factor",             "unit": "x",     "direction": "lower_better"},
    "tokens_per_sec_min":     {"label": "LLM token throughput",            "unit": "tok/s", "direction": "higher_better"},
    "barge_in_rtt_ms":        {"label": "Barge-in round-trip",             "unit": "ms",    "direction": "lower_better"},
    "self_barge_played_ms_max": {"label": "Self-barge played-audio threshold", "unit": "ms", "direction": "threshold"},
    "outlier_total_ms_factor":  {"label": "Turn total-time outlier factor",    "unit": "x",  "direction": "threshold"},
    "tool_roundtrip_ms":      {"label": "Client tool round-trip",          "unit": "ms",    "direction": "lower_better"},
    "emit_rtf_max":           {"label": "Output emit realtime factor",     "unit": "x",     "direction": "lower_better"},
    "post_budget_ratio_max":  {"label": "Post-processor budget ratio",     "unit": "x",     "direction": "lower_better"},
    "annotation_lag_budget_ms": {"label": "Annotation lag budget",         "unit": "ms",    "direction": "lower_better"},
    "unanswered_wait_ms":       {"label": "Unanswered-wait threshold",     "unit": "ms",    "direction": "threshold"},
}

# Verdict levels (v2 graded model) with a display icon + meaning. Single source
# of truth for the markdown header icon so a level can't render as the unknown
# glyph (the `fair` level used to). Ordered by severity via `rank`.
_LEVELS: dict[str, dict] = {
    "healthy":  {"rank": 0,  "icon": "✅", "meaning": "All metrics within target; no user-visible issues."},
    "fair":     {"rank": 1,  "icon": "\U0001F7E1", "meaning": "Minor/moderate on-critical-path breaches or slightly-high critical-path latency; usable but not ideal."},
    "degraded": {"rank": 2,  "icon": "⚠️", "meaning": "A severe on-critical-path breach or poor critical-path latency; user-visible slowness."},
    "broken":   {"rank": 3,  "icon": "❌", "meaning": "Session terminated mid-flight, or critical-path latency is unusable."},
    "unknown":  {"rank": -1, "icon": "❓", "meaning": "Verdict could not be computed."},
}

# Anomaly severity bands with their culprit-attribution weight (sourced from the
# analysis engine so the two never drift) + meaning.
_SEVERITY_MEANING: dict[str, str] = {
    "negligible": "Within ~15% of target; noise, not a user-felt defect.",
    "minor":      "Modestly over target.",
    "moderate":   "Clearly over target.",
    "severe":     "Far over target; a dominant contributor.",
}


def _build_legend(targets: dict) -> dict:
    """Self-describing display metadata so the report renders anywhere without
    hardcoding units, directions, or level/severity semantics."""
    return {
        "levels": _LEVELS,
        "severities": {
            name: {"weight": _SEVERITY_WEIGHT.get(name, 0.0), "meaning": meaning}
            for name, meaning in _SEVERITY_MEANING.items()
        },
        # One entry per resolved target key; fallback keeps it total if a new
        # target appears before its metadata is added (the test flags that).
        "metrics": {
            k: _METRIC_META.get(k, {"label": k, "unit": None, "direction": "unknown"})
            for k in targets
        },
    }



# Item 9 — `_compute_session_metrics` predates the report bundle and emits
# camelCase keys consumed by the existing `/api/v1/sessions/{id}` endpoint.
# The report bundle is otherwise snake_case, so we re-key on the way out
# rather than touching the live UI contract.
_TOTALS_SNAKE_CASE_MAP: dict[str, str] = {
    "completedTurns": "completed_turns",
    "terminatedTurns": "terminated_turns",
    "totalTurns": "total_turns",
    "avgTtftMs": "avg_ttft_ms",
    "avgTtfbMs": "avg_ttfb_ms",
    "avgEndpointingMs": "avg_endpointing_ms",
    "avgProviderEndpointingMs": "avg_provider_endpointing_ms",
    "avgBargeInRttMs": "avg_barge_in_rtt_ms",
    "serverBargeIns": "server_barge_ins",
    "clientBargeIns": "client_barge_ins",
    "playbackBargeIns": "playback_barge_ins",
    "totalPromptTokens": "total_prompt_tokens",
    "totalOutputTokens": "total_output_tokens",
    "avgTokensPerSec": "avg_tokens_per_sec",
    "totalAudioSeconds": "total_audio_seconds",
    "avgRtf": "avg_rtf",
    "avgEmitRtf": "avg_emit_rtf",
    "maxEmitRtf": "max_emit_rtf",
    "turnsOverRealtime": "turns_over_realtime",
    "eventsAggregated": "events_aggregated",
}


def _totals_to_snake_case(totals: dict) -> dict:
    """Rekey the camelCase totals dict to snake_case for the report bundle.
    Unknown keys are passed through unchanged so future additions still flow."""
    if not totals:
        return {}
    out: dict = {}
    for k, v in totals.items():
        out[_TOTALS_SNAKE_CASE_MAP.get(k, k)] = v
    return out


def _build_report_for_llm(
    session_id: str,
    metadata: dict,
    utterances: list[dict],
    session_metrics: dict | None,
    providers: dict,
    targets: dict[str, float],
    events: list[dict] | None = None,
    health_model: str | None = None,
    generated_at: str | None = None,
    client_tool_calls: list[dict] | None = None,
    grading: dict | None = None,
) -> dict:
    """Assemble the structured payload that's both emitted as JSON and
    embedded into the markdown narrative.

    `events` is optional: when supplied, a compacted timeline is embedded
    as `event_timeline[]` so the LLM can reason about ordering /
    propagation issues that aren't visible from the per-turn aggregates
    (item: opt-in event log on the diagnostic report).

    `client_tool_calls` is the paired call/result activity list from
    captures.reader (`_collect_client_tool_calls` + turn attribution). When
    supplied, tool legs are classified on the turn rows, the followup turn's
    TTFB is re-anchored at the tool result, and tool round-trips get their own
    `client_tools` component + anomalies (see turns._apply_tool_context).

    `generated_at` (ISO-8601) stamps when the report was computed; defaults to
    now (UTC). Injectable so tests stay deterministic."""
    if generated_at is None:
        generated_at = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    started_at = metadata.get("started_at")
    ended_at = metadata.get("ended_at")
    duration_s = None
    try:
        if started_at and ended_at:
            t0 = datetime.fromisoformat(started_at.replace("Z", "+00:00"))
            t1 = datetime.fromisoformat(ended_at.replace("Z", "+00:00"))
            duration_s = round((t1 - t0).total_seconds(), 2)
    except (AttributeError, TypeError, ValueError):
        # Unparseable timestamps: the report has no duration.
        pass

    # Drop empty/phantom turns (no metrics, no transcript, no interrupt/terminate)
    # so they don't pad the flattened rows, the verdict's "across N turns", or the
    # totals. Matches the filter in _compute_session_metrics.
    utterances = [u for u in utterances if not is_empty_turn(u)]

    # Resolve "which utterance id is turn #1" — captures sometimes use 0-based
    # ids and sometimes 1-based.  We treat the smallest id present as the
    # cold-start turn so latency anomalies on it are suppressed.
    first_utt_id = None
    utt_ids = [
        u.get("utterance_id") for u in utterances
        if isinstance(u.get("utterance_id"), int)
    ]
    if utt_ids:
        first_utt_id = min(utt_ids)

    flat_turns = [_flatten_turn(u, started_at, first_utt_id, providers) for u in utterances]
    _apply_tool_context(flat_turns, utterances, client_tool_calls)
    aggregates = _component_aggregates(flat_turns, events, client_tool_calls)
    anomalies = _detect_anomalies(flat_turns, targets, client_tool_calls)
    active_model = _health_model(health_model)
    verdict = _compute_verdict(flat_turns, aggregates, anomalies, providers,
                               model=active_model, grading=grading)
    # Dual-run preview: while v1 is active, also surface the graded v2 verdict so
    # operators can compare on live sessions before flipping the default. No
    # caller is forced to change.
    verdict_v2 = (
        _compute_verdict(flat_turns, aggregates, anomalies, providers, model="v2", grading=grading)
        if active_model != "v2" else None
    )
    timeline_rows = _compact_event_timeline(events, started_at) if events else None
    worst_turn = _worst_turn_timeline(flat_turns, events, started_at) if events else None

    tool_rows = [_tool_call_report_row(c) for c in (client_tool_calls or [])]

    totals = _totals_to_snake_case(session_metrics or {})
    # For s2s pipelines there is no cascade TTS, so the synthesis `avg_rtf` is
    # structurally null — exactly the metric that was 1.67 when a post-processor
    # starved the client. Populate it from the
    # streaming-phase `emit_rtf` there (same convention: wall ÷ audio, > 1.0 =
    # slower than real time), and say so via `avg_rtf_source`. Cascade sessions
    # keep the TTS meaning; `avg_emit_rtf` is available alongside in both.
    if (
        bool(providers.get("s2s")) if isinstance(providers, dict) else False
    ) and totals.get("avg_rtf") is None and totals.get("avg_emit_rtf") is not None:
        totals["avg_rtf"] = totals["avg_emit_rtf"]
        totals["avg_rtf_source"] = "emit_rtf"

    return {
        "report_version": _REPORT_VERSION,
        "generated_at": generated_at,
        "session": {
            "id": session_id,
            "pipeline_name": metadata.get("pipeline_name"),
            "pipeline_config_hash": metadata.get("pipeline_config_hash"),
            "stt_provider": metadata.get("stt_provider"),
            "tts_provider": metadata.get("tts_provider"),
            "worker_id": metadata.get("worker_id"),
            "status": metadata.get("status"),
            "started_at": started_at,
            "ended_at": ended_at,
            "duration_s": duration_s,
            "turn_count": metadata.get("turn_count"),
            "tags": metadata.get("tags") or [],
        },
        "providers": providers,
        "verdict": verdict,
        **({"verdict_v2": verdict_v2} if verdict_v2 is not None else {}),
        "totals": totals,
        "targets": targets,
        # The bands the level was graded on, and every rule that differs from
        # its default with where it came from. `custom` is empty for a session
        # graded by the defaults.
        "grading": {
            "levels": (grading or {}).get("levels"),
            "custom": (grading or {}).get("custom") or [],
        },
        "legend": _build_legend(targets),
        "component_aggregates": aggregates,
        "anomalies": anomalies,
        "turns": flat_turns,
        **({"client_tool_calls": tool_rows} if tool_rows else {}),
        **({"event_timeline": timeline_rows} if timeline_rows else {}),
        **({"worst_turn_timeline": worst_turn} if worst_turn else {}),
    }


def _tool_call_report_row(call: dict) -> dict:
    """Session-level projection of one client tool call for the report JSON:
    identity, arguments, outcome, round-trip, and the turn it belongs to."""
    row = {
        "call_id": call.get("call_id"),
        "name": call.get("name"),
        "arguments": call.get("arguments"),
        "status": call.get("status"),
        "after_utterance_id": call.get("after_utterance_id"),
    }
    rt = tool_roundtrip_ms(call)
    if rt is not None:
        row["roundtrip_ms"] = round(rt, 1)
    ack = tool_ack_ms(call)
    if ack is not None:
        row["ack_ms"] = round(ack, 1)
    for k in ("speak_on_result", "spoken"):
        if call.get(k) is not None:
            row[k] = call[k]
    return row


def _compact_event_timeline(events: list[dict], started_at: str | None) -> list[dict]:
    """Project the raw `events.jsonl` stream into a compact, LLM-friendly
    timeline.  Drops chunked / high-cardinality payloads and rewrites
    timestamps as `t_s` (seconds since session start) so the model can
    reason about ordering without parsing epoch nanoseconds.

    Cap is intentionally generous (~400 rows) — the LLM is reading the
    text, not enforcing a token budget.  Pipelines with longer sessions
    can re-paginate or fetch JSON directly.
    """
    # Event types worth showing.  Audio chunks, partial transcripts, and
    # captions are noisy and rarely diagnostic — skip them.
    KEEP = {
        "stt_timing", "stt_usage", "stt_final",
        "llm_request", "llm_timing", "llm_detail",
        "tts_timing", "tts_detail", "tts_usage",
        "client_timing", "barge_in_rtt",
        "client_tool_call", "client_tool_result",
        "session_started", "session_ended",
        "turn_resolved", "interrupt", "interrupted",
        "error", "warning",
    }

    t0_secs: float | None = None
    if started_at:
        try:
            t0_secs = datetime.fromisoformat(started_at.replace("Z", "+00:00")).timestamp()
        except (AttributeError, TypeError, ValueError):
            t0_secs = None

    def _ts_seconds(evt: dict, payload: dict) -> float | None:
        ts = evt.get("timestamp") or {}
        if isinstance(ts, dict):
            secs = ts.get("secs_since_epoch")
            nanos = ts.get("nanos_since_epoch") or 0
            if isinstance(secs, (int, float)):
                return float(secs) + float(nanos) / 1e9
        client_ms = payload.get("client_timestamp_ms")
        if isinstance(client_ms, (int, float)):
            return float(client_ms) / 1000.0
        return None

    # Client timing events reach the capture twice: as a trace event in
    # events.jsonl and as a pipeline event in pipeline_events.jsonl. Keep the
    # trace copy and drop its pipeline twin.
    def _client_timing_key(payload: dict) -> tuple:
        return (
            payload.get("utterance_id"),
            payload.get("event"),
            payload.get("client_timestamp_ms"),
        )

    trace_client_timings = {
        _client_timing_key(evt.get("payload") or {})
        for evt in events
        if (evt.get("event_type") or evt.get("type")) == "client_timing"
        and isinstance(evt.get("payload"), dict)
    }

    out: list[dict] = []
    for evt in events:
        etype = evt.get("event_type") or evt.get("type")
        if etype not in KEEP:
            continue
        payload = _event_payload(evt)
        if (
            etype == "client_timing"
            and not isinstance(evt.get("payload"), dict)
            and _client_timing_key(payload) in trace_client_timings
        ):
            continue
        abs_s = _ts_seconds(evt, payload)
        rel = round(abs_s - t0_secs, 3) if (abs_s is not None and t0_secs is not None) else None
        # Strip large fields (text bodies, audio data) — keep only the
        # diagnostic-relevant scalars.
        compact = {
            k: v for k, v in payload.items()
            if not isinstance(v, (list, dict))
            and (not isinstance(v, str) or len(v) <= 120)
        }
        # Always preserve utterance_id even if it's nested under usage; it
        # anchors the row to a turn.
        if "utterance_id" not in compact and isinstance(payload.get("utterance_id"), (int, str)):
            compact["utterance_id"] = payload["utterance_id"]
        row = {
            "t_s": rel,
            "type": etype,
            "component": evt.get("component"),
            "p": compact or None,
        }
        out.append(row)

    # Order by time; rows without one (pipeline lifecycle events carry no
    # timestamp) keep their relative order after the timed rows.
    out.sort(key=lambda r: (r["t_s"] is None, r["t_s"] or 0.0))
    return out[:400]


# Envelope keys of a pipeline event row merged in from pipeline_events.jsonl;
# everything else on the row is its payload.
_PIPELINE_ROW_KEYS = {"kind", "type", "component", "timestamp"}


def _event_payload(evt: dict) -> dict:
    """The payload of a timeline source row. Trace events (events.jsonl) nest
    it under `payload`; pipeline events (pipeline_events.jsonl) carry their
    fields at the top level, with `timing_event` naming a client timing."""
    payload = evt.get("payload")
    if isinstance(payload, dict):
        return payload
    flat = {k: v for k, v in evt.items() if k not in _PIPELINE_ROW_KEYS}
    if "timing_event" in flat and "event" not in flat:
        flat["event"] = flat.pop("timing_event")
    return flat


def _worst_turn_timeline(
    flat_turns: list[dict],
    events: list[dict],
    started_at: str | None,
) -> dict | None:
    """Item 12 — pick the worst turn (by `e2e_perceived_ms`, then `total_ms`)
    and return a *filtered* compact event timeline scoped to that turn.

    Returns `{utterance_id, metric, value_ms, rows}` or `None` when there
    are no turns / no events.  Rows reuse `_compact_event_timeline`'s
    KEEP-set + scalar payload stripping; we additionally filter by
    `p.utterance_id` so the LLM gets only the relevant events.  Capped at
    50 rows so the report stays paste-friendly."""
    if not flat_turns or not events:
        return None

    def _score(t: dict) -> tuple[float, str, float]:
        e2e = t.get("e2e_perceived_ms")
        if isinstance(e2e, (int, float)):
            return (float(e2e), "e2e_perceived_ms", float(e2e))
        total = t.get("total_ms")
        if isinstance(total, (int, float)):
            return (float(total), "total_ms", float(total))
        return (-1.0, "", 0.0)

    scored = [(t, _score(t)) for t in flat_turns]
    scored = [s for s in scored if s[1][0] >= 0]
    if not scored:
        return None
    worst_t, (_, metric, value) = max(scored, key=lambda s: s[1][0])
    worst_id = worst_t.get("utt")
    if worst_id is None:
        return None

    # Reuse the compact-timeline projection, then filter to this turn's
    # rows.  `_compact_event_timeline` already preserves utterance_id in
    # `p` whenever the source payload exposes it.
    timeline = _compact_event_timeline(events, started_at)
    rows = [
        r for r in timeline
        if isinstance(r.get("p"), dict)
        and str((r["p"] or {}).get("utterance_id")) == str(worst_id)
    ]

    return {
        "utterance_id": worst_id,
        "metric": metric,
        "value_ms": round(value),
        "rows": rows[:50],
    }


def _transcript_block(turn: dict) -> str:
    """One turn's transcript lines, annotated with tool-leg context so a
    tool-call turn reads as 'invoked a tool' instead of a missing transcript."""
    kind = turn.get("kind")
    kind_label = {
        "tool_invocation": " *(tool-call leg)*",
        "tool_followup": " *(post-tool followup)*",
    }.get(kind, "")
    agent = turn.get("agent_transcript")
    if not agent:
        agent = (
            "(no spoken output — invoked client tool)"
            if kind == "tool_invocation"
            else "(no agent transcript)"
        )
    lines = [
        f"- **Turn {turn.get('utt')}**{kind_label}\n",
        f"  - User: {turn.get('user_transcript') or '(no user transcript)'}\n",
        f"  - Agent: {agent}\n",
    ]
    for tc in turn.get("tool_calls") or []:
        rt = tc.get("roundtrip_ms")
        rt_label = f" in {rt:.0f}ms" if isinstance(rt, (int, float)) else ""
        ack = tc.get("ack_ms")
        ack_label = f" (acked in {ack:.0f}ms)" if isinstance(ack, (int, float)) else ""
        spoken = " (spoken confirmation)" if tc.get("spoken") else ""
        lines.append(
            f"  - Tool: `{tc.get('name')}` → {tc.get('status')}{rt_label}{ack_label}{spoken}\n"
        )
    return "".join(lines)


def _render_report_markdown(report: dict) -> str:
    """Wrap the JSON report in a front-matter narrative that primes the
    LLM with context, the SLA rubric, and reading guidance.

    Kept terse on purpose: the JSON block carries the data; the prose
    only tells the LLM how to interpret it."""
    sess = report["session"]
    tgt = report["targets"]
    provs = report.get("providers") or {}
    llm_p = provs.get("llm") or {}
    stt_p = provs.get("stt") or {}
    tts_p = provs.get("tts") or {}
    llm_label = (
        f"{llm_p.get('provider') or '?'}"
        + (f"/{llm_p['model']}" if llm_p.get("model") else "")
        + (f" (v:{llm_p['model_version']})" if llm_p.get("model_version") else "")
    )
    stt_label = (
        f"{stt_p.get('provider') or sess.get('stt_provider') or '?'}"
        + (f"/{stt_p['model']}" if stt_p.get("model") else "")
        + (f" (v:{stt_p['model_version']})" if stt_p.get("model_version") else "")
        + (f" ({stt_p['mode']})" if stt_p.get("mode") and stt_p["mode"] != "unknown" else "")
    )
    tts_label = (
        f"{tts_p.get('provider') or sess.get('tts_provider') or '?'}"
        + (f"/{tts_p['model']}" if tts_p.get("model") else "")
        + (f" (v:{tts_p['model_version']})" if tts_p.get("model_version") else "")
        + (f" voice={tts_p['voice']}" if tts_p.get("voice") else "")
    )
    n_anom = len(report.get("anomalies") or [])
    n_turns = len(report.get("turns") or [])
    is_s2s = bool(provs.get("s2s"))
    has_input_audio = any(
        isinstance(t, dict) and t.get("input_audio") for t in (report.get("turns") or [])
    )
    has_tools = bool(report.get("client_tool_calls"))
    verdict = report.get("verdict") or {}
    verdict_level = verdict.get("level") or "unknown"
    # Drive the icon from the shared level legend so every v2 level (incl. `fair`)
    # renders correctly instead of falling back to the unknown glyph.
    verdict_icon = (_LEVELS.get(verdict_level) or _LEVELS["unknown"])["icon"]
    verdict_headline = verdict.get("headline") or "(no verdict computed)"
    unstable_list = verdict.get("unstable_components") or []
    unstable_clause = (
        f"  unstable components: {', '.join(unstable_list)}\n" if unstable_list else ""
    )
    # Said to the reader that is asked to "confirm or override" the verdict:
    # left alone it would override a deliberate grade back to general norms,
    # which is the operator's decision being quietly undone.
    custom_rules = (report.get("grading") or {}).get("custom") or []
    custom_clause = (
        "- NOTE: some targets below were set by this deployment's operator "
        "(see the verdict headline for which, and their defaults). That is a "
        "deliberate decision about what is acceptable here: grade against the "
        "values shown, and do not override the verdict merely because a value "
        "is looser than general norms. Do say so if a custom value looks like "
        "a mistake rather than a choice.\n"
        if custom_rules else ""
    )
    return (
        f"# Voice pipeline session diagnostic\n"
        f"\n"
        f"## Verdict {verdict_icon} {verdict_level.upper()}\n"
        f"{verdict_headline}\n"
        f"  primary_culprit: {verdict.get('primary_culprit') or 'n/a'}\n"
        f"{unstable_clause}"
        f"\n"
        f"session_id: {sess.get('id')}\n"
        f"report_version: {report.get('report_version')}  generated_at: {report.get('generated_at')}\n"
        f"pipeline:   {sess.get('pipeline_name')} (config={sess.get('pipeline_config_hash')})"
        + (" [speech-to-speech]" if is_s2s else "") + "\n"
        f"providers:  llm={llm_label}  stt={stt_label}  tts={tts_label}\n"
        f"started:    {sess.get('started_at')}  duration_s: {sess.get('duration_s')}\n"
        f"turns:      {n_turns}  flagged anomalies: {n_anom}\n"
        f"\n"
        f"## Goal\n"
        f"Tell me whether this session ran healthy, and if not which component is\n"
        f"the likely culprit and why.  Reference the per-turn rows when citing\n"
        f"evidence.  Treat the `anomalies[]` array as candidate findings to\n"
        f"confirm, refine, or dismiss with a brief rationale.  The `verdict`\n"
        f"block above is a pre-computed first-pass synthesis \u2014 confirm or\n"
        f"override it explicitly in your response.\n"
        f"\n"
        f"## Targets (interpret latencies against these; lower = better)\n"
        f"{custom_clause}"
        f"- ttft_ms <= {tgt['ttft_ms']:.0f} (LLM time-to-first-token)\n"
        f"- ttfb_ms <= {tgt['ttfb_ms']:.0f} (TTS time-to-first-byte; for s2s: end of speech → first audio)\n"
        f"- stt_processing_ms <= {tgt['stt_processing_ms']:.0f}\n"
        f"- endpointing_ms <= {tgt['endpointing_ms']:.0f} (VAD silence-to-commit; for s2s: provider_endpointing_ms)\n"
        f"- rtf <= {tgt['rtf_max']} (TTS realtime factor; >1.0 = worse than realtime)\n"
        f"- tokens_per_sec >= {tgt['tokens_per_sec_min']:.0f}\n"
        f"- barge_in_rtt_ms <= {tgt['barge_in_rtt_ms']:.0f}\n"
        f"- emit_rtf <= {tgt.get('emit_rtf_max', 0.9)} (streaming-phase output realtime factor: wall time\n"
        f"  first→last audio chunk sent ÷ audio duration; >1.0 = the client starves after its prebuffer)\n"
        f"- post_budget_ratio <= {tgt.get('post_budget_ratio_max', 0.8)} (per inline post-processor: mean call\n"
        f"  latency ÷ mean chunk audio duration; >1.0 = cannot keep up with its input)\n"
        f"- annotation_lag_ms <= {tgt.get('annotation_lag_budget_ms', 200):.0f} (annotators are off the audio path;\n"
        f"  a breach degrades metadata only, never speech)\n"
        f"\n"
        f"## Notes\n"
        f"- The first turn typically pays a cold-start cost; ignore it for averages\n"
        f"  but call it out if it is dramatically slower than the rest.  Cold-turn\n"
        f"  TTFT may carry `ttft_estimated: true` (derived from\n"
        f"  `llm_detail.first_token_ms` because the live metric was not captured).\n"
        f"- `interrupted=true` with `bi.played_ms < {tgt['self_barge_played_ms_max']:.0f}`\n"
        f"  and `bi.trigger == \"server_vad\"` is usually echo/self-barge, not a real\n"
        f"  user barge-in.\n"
        f"- For streaming STT (`stt.mode == \"streaming\"`) `processing_ms` is null by\n"
        f"  design \u2014 transcripts arrive incrementally during the user's speech.\n"
        f"  `stream_duration_ms` is the audio-start \u2192 final-transcript wall time.\n"
        f"- For batch STT `processing_ms` is the audio-start \u2192 final-transcript wall\n"
        f"  time \u2014 it spans the user's speech, so a long utterance inflates it\n"
        f"  without any user-felt wait. The turn commit is gated on the final\n"
        f"  transcript and `endpointing_ms` is measured at commit, so the user-felt\n"
        f"  STT cost lives in `endpointing_ms`; judge STT turn-taking on that, not\n"
        f"  on raw `processing_ms` (see `on_critical_path`/`on_path_observed` on\n"
        f"  the anomaly rows).\n"
        f"- `tts.ttfb_ms` is commit-anchored (utterance-commit \u2192 first audio chunk\n"
        f"  to the client) and therefore *contains* `llm.ttft_ms`; the TTS-only\n"
        f"  share is roughly `ttfb_ms - ttft_ms` (pre-pacer value:\n"
        f"  `tts.internal_ttfb_ms`). A slow LLM turn inflates `ttfb_ms` without TTS\n"
        f"  being at fault. `critical_path` stages are exclusive: its\n"
        f"  `tts_ttfb_ms` stage is already net of TTFT, and `total_ms` is the\n"
        f"  server-side speech-end \u2192 first-audio estimate. When `llm.ttft_ms` is\n"
        f"  0 alongside positive LLM work (`prompt_ingest_ms`, `generate_ms`,\n"
        f"  `generation_ms`, or `completion_tokens`), it is flagged as\n"
        f"  `measurement.zero_with_work` and treated as missing (using\n"
        f"  `perceived_ttft_ms` / `prompt_ingest_ms` when available, or leaving the\n"
        f"  wait unattributed — never charging it to TTS by subtracting 0).\n"
        f"- Targets above are *provider-aware*: cloud LLMs get a relaxed `ttft_ms`\n"
        f"  and `tokens_per_sec_min` budget so that normal cloud latency does not\n"
        f"  flag as anomalous.  See `targets` in the JSON for the resolved values.\n"
        f"- All times are milliseconds unless suffixed `_s` (seconds) or labelled rtf.\n"
        f"- Throughput is separate from latency: a turn can start on time and\n"
        f"  then trickle. Per-turn `output.emit_rtf` is the streaming-phase wall\n"
        f"  time ÷ audio duration *after* the post-audio chain, `output.source_rtf`\n"
        f"  isolates what the model/TTS delivered on its own. The wall time\n"
        f"  between the two (≈ `emit_rtf - source_rtf` × audio) is split:\n"
        f"  `output.chain_overhead_ms` is the post-processors' share (their summed\n"
        f"  call time; 0 when the turn ran none) and `output.pacer_wait_ms` the\n"
        f"  rest — mostly the realtime pacer holding audio to playback speed, so\n"
        f"  a provider faster than realtime shows a large pacer wait and\n"
        f"  `emit_rtf` near 1.0, which is by design. `audio_post.<name>` rows\n"
        f"  carry each processor's p50 / budget_ratio / timeouts.\n"
        f"  `playback.arrival_rtf` is\n"
        f"  the same measurement on the client: underruns with arrival_rtf > 1\n"
        f"  are server starvation (`client.starvation`), underruns with\n"
        f"  arrival_rtf <= 1 are network/prebuffer jitter (`client.jitter`).\n"
        f"  `annotators.<name>` rows are off the audio path: their anomalies\n"
        f"  (`annotator.*`) degrade metadata only and cap the verdict at `fair`.\n"
        + (
            "- `turns[].input_audio` is the user's speech as the server received it\n"
            "  (after browser AGC), metered by vp's own uplink VAD: `speech_dbfs`\n"
            "  (pooled RMS of the speech frames, dBFS), `noise_dbfs` (the last ~1 s\n"
            "  of non-speech audio up to the end of speech), `snr_db`, `peak_dbfs`,\n"
            "  `clipped_ratio` (samples within 0.1 dB of full scale ÷ speech\n"
            "  samples), `speech_ms` and `level_source` (which VAD classified the\n"
            "  frames). Use it to separate input problems from model problems: a\n"
            "  wrong or wrong-language user transcript on a turn with speech below\n"
            "  about -35 dBFS, SNR below about 15 dB, or clipping above 0.5 % is\n"
            "  plausibly the microphone, not the model; the same transcript on clean\n"
            "  input is the model's. These thresholds are provisional and are not\n"
            "  raised as anomalies yet.\n"
            if has_input_audio else ""
        )
        + (
            "- providers.s2s = true: this is a speech-to-speech pipeline — one\n"
            "  realtime model IS the whole STT+LLM+TTS stack. `llm.ttft_ms` is\n"
            "  structurally null; the user-felt wait decomposes into\n"
            "  `stt.endpointing_ms` (= `anchor.provider_endpointing_ms`: how long\n"
            "  the provider's VAD/transcription took to notice the user stopped,\n"
            "  measured against vp's own uplink VAD) + `tts.ttfb_ms` (provider\n"
            "  anchor → first AUDIO delta; `anchor.first_text_ms` is the\n"
            "  diagnostic first-transcript instant, usually earlier — the model\n"
            "  started but had not flushed audio). Per-stage blame is not\n"
            "  meaningful, so the verdict attributes latency to `s2s` (the model),\n"
            "  never to a separate TTS. Read every S2S latency against its\n"
            "  `anchor.ttfb_anchor_source`: `provider_speech_stopped` (OpenAI) and\n"
            "  `vp_vad` / `client_speech_end` are real ends of speech;\n"
            "  `input_transcript_proxy` (Gemini) lands 0.5–1 s late, so TTFB/E2E\n"
            "  under-report unless a vp/client stamp corrected them (see\n"
            "  `s2s.anchor_proxy`); `provider` is a legacy capture whose source\n"
            "  was not recorded; `turn_start` means NO anchor — that turn's\n"
            "  `ttfb_ms` is ~0 by construction and is not a latency.\n"
            "  `e2e_anchor_source` says the same for `e2e_perceived_ms`.\n"
            "  For s2s pipelines `rtf` is the streaming-phase `emit_rtf`\n"
            "  (`totals.avg_rtf` is populated from it, `avg_rtf_source`);\n"
            "  `source_rtf` isolates the model, `chain_overhead_ms` the\n"
            "  post-processors and `pacer_wait_ms` the pacer. A post-processor\n"
            "  that cannot keep up is blamed as `audio_post:<name>`, never\n"
            "  folded into the model.\n"
            if is_s2s else ""
        )
        + (
            "- This session used client-side tools (see `client_tool_calls[]`).\n"
            "  A `kind: \"tool_invocation\"` turn is the model dispatching a tool —\n"
            "  no spoken output is expected there. Per-call `roundtrip_ms`\n"
            "  (dispatch → client result) is APPLICATION-side execution time —\n"
            "  media startup, UI navigation, a human resolving the card — not\n"
            "  pipeline latency: a long round-trip is reported as `info` and never\n"
            "  degrades the verdict (`ack_ms`, when present, is the client's\n"
            "  started/progress acknowledgement — the integration-responsiveness\n"
            "  signal). The pipeline's own share of a tool exchange is the\n"
            "  `kind: \"tool_followup\"` turn's `tts.ttfb_ms`, re-anchored at the\n"
            "  tool-result arrival (`ttfb_anchor`, raw value in `ttfb_pre_tool_ms`)\n"
            "  so the tool wait is never misattributed to TTS. `e2e_perceived_ms`\n"
            "  still spans the full user-felt wait *including* the tool round-trip\n"
            "  (`tool_context.excluded_wait_ms`).\n"
            if has_tools else ""
        )
        + (
            "- An `event_timeline[]` is embedded (opt-in via `?include_events=true`).\n"
            "  Each row is `{t_s, type, component, p}` with `t_s` relative to\n"
            "  `session.started_at` and high-cardinality payloads stripped.  Use it\n"
            "  to audit ordering / propagation bugs that the aggregates can't show.\n"
            if report.get("event_timeline") else ""
        ) +
        "\n## Transcripts\n"
        + "".join([_transcript_block(turn) for turn in (report.get("turns") or [])]) +
        f"\n"
        f"## Data\n"
        f"```json\n"
        f"{json.dumps(report, indent=2, default=str)}\n"
        f"```\n"
    )
