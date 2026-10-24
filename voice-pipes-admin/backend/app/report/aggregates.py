"""Per-component aggregates (count, cold-starts, errors, distribution stats)."""

from .stats import _distribution_stats
from .turns import tool_roundtrip_ms

# A connect-dominated LLM TTFT is excluded from the component distribution only
# when its absolute value exceeds this — i.e. a genuine cold-socket / routing
# stall, not the normal case where streaming APIs return headers and the first
# token together (`connect_ms ≈ first_token_ms` on every warm turn). Set well
# above any normal cloud TTFT (cloud targets top out ~1500ms).
_CONNECT_DOMINATED_OUTLIER_MS = 2000.0


def _component_aggregates(
    turns: list[dict],
    events: list[dict] | None = None,
    client_tool_calls: list[dict] | None = None,
) -> list[dict]:
    """Group flattened turns by component and compute per-component stats.

    When `events` is supplied, real error counts are derived by scanning
    `events.jsonl` for `{stt,llm,tts}_error` rows (item 10).  When omitted
    or no error rows exist, the `errors` field is dropped from the output
    rather than reporting a misleading zero.

    When `client_tool_calls` is supplied and non-empty, a `client_tools`
    component is appended: `roundtrip_ms` (dispatch → client result) over the
    resolved calls, plus `failed`/`pending` outcome counts. Tool latency is a
    first-class component so a slow client tool shows up under its own name
    instead of leaking into the TTS TTFB series (see turns._apply_tool_context).
    """
    buckets: dict[str, dict] = {
        "stt": {"vals": [], "count": 0, "cold_starts": 0, "primary_metric": "processing_ms"},
        "llm": {"vals": [], "count": 0, "cold_starts": 0, "primary_metric": "ttft_ms"},
        "tts": {"vals": [], "count": 0, "cold_starts": 0, "primary_metric": "ttfb_ms"},
    }
    # Batch recognisers are aggregated on `endpointing_ms`, not `processing_ms`.
    #
    # For a batch recogniser `processing_ms` runs from the start of the user's
    # audio to the final transcript, so it *contains the user's speech*: a
    # long sentence produces a large value and a short one a small value, and
    # nobody waited for either. Its spread is the spread of how long people
    # talked. Aggregating it made the stability check report "response
    # latency is unstable (p99/p50 > 5)" on a session whose real response
    # time varied by 38 ms across five turns, because one sentence was long.
    #
    # What a batch recogniser costs the person waiting is the wait for the
    # commit, which is `endpointing_ms` -- the same judgement the anomaly
    # rules already make when they mark `stt.processing_high` as off the
    # critical path. The row names the metric it used, so a reader is never
    # shown one number under another's label.
    if any((t.get("stt") or {}).get("mode") == "batch" for t in turns):
        buckets["stt"]["primary_metric"] = "endpointing_ms"

    for t in turns:
        interrupted = bool(t.get("interrupted"))
        for kind, b in buckets.items():
            comp = t.get(kind) or {}
            v = comp.get(b["primary_metric"])
            # Skip TTS ttfb_ms on interrupted turns: synthesis was cut
            # short (`synthesis_ms` is null) and ttfb_ms then includes
            # upstream LLM-wait + interrupt-handling stalls rather than
            # TTS slowness.  Including these inflates p99/p50 and
            # falsely flags TTS as `unstable` in the verdict (the
            # session was healthy; one interrupted turn dragged the
            # whole component into the unstable bucket).
            if (
                kind == "tts"
                and interrupted
                and not isinstance(comp.get("synthesis_ms"), (int, float))
            ):
                v = None
            # Skip LLM ttft_ms when a cold TLS/TCP connect dominated
            # the measurement (`connect_ms / first_token_ms > 0.9`).
            # That latency belongs to the cloud provider's network
            # path, not to the model itself — including it inflates
            # p99/p50 and falsely flags the LLM as `unstable` even
            # when every model-side first-token came back promptly.
            # Drop a connect-dominated TTFT from the LLM distribution only when
            # it is *also* an absolute outlier — a genuine cold-socket / routing
            # stall. For streaming APIs `connect_ms ≈ first_token_ms` on EVERY
            # warm turn (response headers and the first token arrive together),
            # so excluding all connect-dominated turns emptied the aggregate
            # (count: 0). Normal sub-threshold turns stay in; only egregious
            # stalls (e.g. a 7s cold TLS handshake) are excluded.
            if kind == "llm":
                connect_ms = comp.get("connect_ms")
                first_token_ms = comp.get("first_token_ms") or v
                if (
                    isinstance(connect_ms, (int, float))
                    and isinstance(first_token_ms, (int, float))
                    and first_token_ms > _CONNECT_DOMINATED_OUTLIER_MS
                    and connect_ms / first_token_ms > 0.9
                ):
                    v = None
            if isinstance(v, (int, float)):
                b["vals"].append(float(v))
                b["count"] += 1
            if comp.get("cold_start"):
                b["cold_starts"] += 1

    # Item 10 — real error counts from events.jsonl.  Only populate the
    # field when the event stream was provided; absent field means "not
    # measured" rather than "verified zero".
    error_counts: dict[str, int] | None = None
    if events is not None:
        error_counts = {"stt": 0, "llm": 0, "tts": 0}
        for e in events:
            etype = e.get("event_type") or e.get("type") or ""
            if etype.endswith("_error"):
                comp = etype[: -len("_error")]
                if comp in error_counts:
                    error_counts[comp] += 1
            elif etype == "error":
                # Generic error events carry the failing component in payload.
                comp = ((e.get("payload") or {}).get("component") or "").lower()
                if comp in error_counts:
                    error_counts[comp] += 1

    out: list[dict] = []
    for kind, b in buckets.items():
        if b["count"] == 0 and b["cold_starts"] == 0:
            continue
        row = {
            "kind": kind,
            "primary_metric": b["primary_metric"],
            "count": b["count"],
            "cold_starts": b["cold_starts"],
        }
        if error_counts is not None:
            row["errors"] = error_counts[kind]
        row.update(_distribution_stats(b["vals"]))
        out.append(row)

    if client_tool_calls:
        roundtrips = [
            round(rt, 1)
            for rt in (tool_roundtrip_ms(c) for c in client_tool_calls)
            if rt is not None
        ]
        row = {
            "kind": "client_tools",
            "primary_metric": "roundtrip_ms",
            "count": len(client_tool_calls),
            "failed": sum(1 for c in client_tool_calls if c.get("status") == "failed"),
            "pending": sum(1 for c in client_tool_calls if c.get("status") == "pending"),
        }
        row.update(_distribution_stats(roundtrips))
        out.append(row)
    return out
