import asyncio
from datetime import UTC, datetime

from fastapi import APIRouter, BackgroundTasks, Query
from pydantic import BaseModel

from ..analytics.compute import (
    _ANALYTICS_CACHE_TTL_S,
    _analytics_cache,
    _analytics_inflight,
    _analytics_scan_sem,
    _captures_signature,
    _compute_analytics_window,
    _fetch_orchestrator_overlay,
    _persist_analytics_issues,
    clear_analytics_data,
)
from ..issues import recent_issues, store_failure
from ..pricing import get_pricing_book, reload_pricing

router = APIRouter()


class ClearAnalyticsBody(BaseModel):
    pipeline: str | None = None
    session_id: str | None = None
    delete_captures: bool = False


# ── Analytics Endpoint ───────────────────────────────────────

@router.get("/api/v1/pricing")
async def get_pricing():
    """Return the active pricing book (per-provider unit prices and caps).

    Frontend uses this to render quota chips and validate that the configured
    providers/models are priced.
    """
    return {"data": get_pricing_book()}


@router.post("/api/v1/pricing/reload")
async def reload_pricing_endpoint():
    """Re-read ``pricing.yaml`` from disk without a process restart."""
    book = reload_pricing()
    return {"data": {
        "llm": dict(book["llm"]),
        "stt": dict(book["stt"]),
        "tts": dict(book["tts"]),
        "monthly_caps_usd": dict(book["monthly_caps_usd"]),
        "local_providers": sorted(book["local_providers"]),
    }}


@router.post("/api/v1/analytics/clear")
async def clear_analytics_endpoint(body: ClearAnalyticsBody | None = None):
    """Clear dashboard metrics globally, for one pipeline, or for one session.

    When ``delete_captures`` is True, also removes matching session directories
    from ``CAPTURES_DIR``.
    """
    req = body or ClearAnalyticsBody()
    result = await asyncio.to_thread(
        clear_analytics_data,
        pipeline=req.pipeline,
        session_id=req.session_id,
        delete_captures=req.delete_captures,
    )
    return {"data": result}


# ── Issues feed (P3 #18) ──────────────────────────────────────

@router.get("/api/v1/issues")
async def get_issues(
    limit: int = Query(50, ge=1, le=500),
    kinds: str | None = Query(None, description="Comma-separated kinds filter"),
    days: int = Query(7, ge=1, le=365),
):
    """Recent persisted issues (failed sessions, worker deaths, anomaly
    sessions, quota breaches, provider changes)."""
    kind_list = [k.strip() for k in kinds.split(",")] if kinds else None
    since_ts = datetime.now(UTC).timestamp() - days * 86400
    rows = recent_issues(limit=limit, kinds=kind_list, since_ts=since_ts)
    error = None
    if store_failure() is not None:
        # The store's path and the OS error are in the backend log; an
        # empty feed must not read as "no issues".
        error = {
            "code": "ISSUES_STORE_UNAVAILABLE",
            "message": "the issues store cannot be opened, so nothing is being recorded; "
                       "see the admin backend's log",
        }
    return {"data": {"issues": rows, "count": len(rows)}, "error": error}


async def _build_analytics(
    days: int,
    offset_days: int,
    pipeline: str | None,
    background_tasks: BackgroundTasks,
    *,
    lite: bool = False,
) -> dict:
    """Run the scan + orchestrator overlay merge for one window and return the
    response payload.  No caching/locking here — that's the caller's job."""
    # Kick off the orchestrator overlay in parallel with the scan.  Skip for
    # prior-period windows — the orchestrator only knows "now".
    overlay_task: asyncio.Task | None = None
    if offset_days == 0:
        overlay_task = asyncio.create_task(_fetch_orchestrator_overlay(days))

    async with _analytics_scan_sem:  # bound simultaneous disk walks
        if lite:
            scan = await asyncio.to_thread(
                _compute_analytics_window, days, offset_days, pipeline, lite=True,
            )
        else:
            scan = await asyncio.to_thread(
                _compute_analytics_window, days, offset_days, pipeline,
            )
    response = scan["response"]
    issue_inputs = scan["issue_inputs"]

    # Merge orchestrator overlay into kpi (if available).
    overlay = await overlay_task if overlay_task is not None else None
    if overlay is not None:
        kpi = response.get("kpi") or {}
        orch_pipeline = overlay.get("pipeline") or {}
        orch_requests = int(orch_pipeline.get("requests_total") or 0)
        orch_errors = int(orch_pipeline.get("errors_total") or 0)
        orch_active = overlay.get("workers_active") or 0
        failed_sessions_n = len(response.get("failed_sessions") or [])
        total_sessions_n = (kpi.get("total_sessions") or 0)
        kpi["orchestrator_errors_total"] = orch_errors
        kpi["orchestrator_requests_total"] = orch_requests
        kpi["workers_active"] = orch_active
        combined_failed = max(failed_sessions_n, orch_errors)
        combined_total = max(total_sessions_n, orch_requests)
        kpi["combined_failed_sessions"] = combined_failed
        kpi["combined_total_sessions"] = combined_total
        kpi["combined_error_rate_pct"] = (
            round(combined_failed / combined_total * 1000) / 10
            if combined_total > 0 else None
        )
        response["orchestrator_metrics"] = {
            "window": overlay.get("window"),
            "timestamp": overlay.get("timestamp"),
            "pipeline": orch_pipeline,
            "workers_active": orch_active,
        }

    payload = {"data": response}

    # Persist issues after the response is sent (skipped in lite mode).
    if not lite:
        spend = (response.get("kpi") or {}).get("spend_by_provider") or {}
        background_tasks.add_task(_persist_analytics_issues, issue_inputs, spend)
    return payload


@router.get("/api/v1/analytics")
async def get_analytics(
    background_tasks: BackgroundTasks,
    days: int = Query(7, ge=1, le=365),
    offset_days: int = Query(0, ge=0, le=365),
    pipeline: str | None = Query(None, description="Restrict to one pipeline_name (1b). Omit for all."),
    lite: bool = Query(False, description="Skip anomaly artifact walks and issue persistence for fast load."),
):
    """Aggregate latency, LLM perf, and provider-change data across sessions.

    Cached for ``_ANALYTICS_CACHE_TTL_S`` seconds keyed by
    ``(days, offset_days, captures_signature)`` so the dashboard's repeat
    requests (current + prior period) only pay the scan cost once.

    0a — single-flight is keyed *per window* (``_analytics_inflight``) rather
    than a single global lock, so the dashboard's current+prior requests run
    concurrently and one slow window never head-of-line-blocks another.
    Identical concurrent requests still share one in-flight scan.

    `offset_days` shifts the analysis window backwards by that many days so
    callers can request the immediately-preceding period for delta
    comparisons (`/api/v1/analytics?days=7&offset_days=7`).
    """
    sig = await asyncio.to_thread(_captures_signature)
    cache_key = (days, offset_days, pipeline, lite, sig)
    loop = asyncio.get_event_loop()

    # Fast path — fresh cache hit.
    cached = _analytics_cache.get(cache_key)
    if cached is not None and loop.time() - cached[0] < _ANALYTICS_CACHE_TTL_S:
        return cached[1]

    # Per-key single-flight: share an in-flight scan for the SAME window, but
    # let distinct windows proceed concurrently.  The get/set pair below has no
    # await between it, so it is race-free on the single-threaded event loop.
    fut = _analytics_inflight.get(cache_key)
    if fut is not None:
        return await fut

    fut = loop.create_future()
    _analytics_inflight[cache_key] = fut
    try:
        payload = await _build_analytics(days, offset_days, pipeline, background_tasks, lite=lite)
        _analytics_cache[cache_key] = (loop.time(), payload)
        # Bound the cache: drop entries with stale signatures so it doesn't
        # grow unbounded as new sessions land.  (sig is the last key element.)
        if len(_analytics_cache) > 32:
            for k in list(_analytics_cache.keys()):
                if k[-1] != sig:
                    _analytics_cache.pop(k, None)
        fut.set_result(payload)
        return payload
    except Exception as exc:
        fut.set_exception(exc)
        raise
    finally:
        _analytics_inflight.pop(cache_key, None)

