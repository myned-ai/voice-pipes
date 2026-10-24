import asyncio
import logging

import httpx
from fastapi import APIRouter, HTTPException, Query
from fastapi.responses import PlainTextResponse

from ..captures.reader import _read_capture_artifacts, _read_session_metadata, is_empty_turn
from ..core.config import ORCHESTRATOR_URL, orch_client
from ..core.upstream import upstream_error, upstream_path
from ..report.analysis import (
    _apply_tool_context,
    _component_aggregates,
    _compute_verdict,
    _detect_anomalies,
    _extract_provider_identity,
    _flatten_turn,
    resolve_grading,
    resolve_turn_ttft,
)
from ..report.render import _build_report_for_llm, _render_report_markdown

router = APIRouter()
log = logging.getLogger("admin.sessions")

# Session fields the orchestrator's record owns. Its value wins when it has
# one; a null there (a record written before the field existed, or by a
# worker that could not know it) must not hide what the capture's
# metadata.json recorded.
_ORCHESTRATOR_SESSION_FIELDS = (
    "status",
    "started_at",
    "ended_at",
    "pipeline_name",
    "pipeline_config_hash",
    "stt_provider",
    "tts_provider",
    "worker_id",
    "input_sample_rate",
    "output_sample_rate",
)


def merge_session_metadata(local_meta: dict, orch_data: dict) -> dict:
    """Overlay the orchestrator's session record on the capture metadata,
    keeping the capture's value wherever the orchestrator's is null."""
    merged = dict(local_meta)
    for key in _ORCHESTRATOR_SESSION_FIELDS:
        value = orch_data.get(key)
        if value is not None:
            merged[key] = value
    merged["turn_count"] = orch_data.get("turn_count", 0)
    merged["tags"] = orch_data.get("tags") or local_meta.get("tags") or []
    return merged




# ── Sessions Endpoints (proxy to orchestrator + local captures) ──

@router.get("/api/v1/sessions")
async def list_sessions(
    page: int = Query(1, ge=1),
    per_page: int = Query(50, ge=1, le=500),
    status: str | None = Query(None),
    pipeline_name: str | None = Query(None),
    pipeline_hash: str | None = Query(None),
    tag: str | None = Query(None),
    exclude_tag: str | None = Query(None),
    worker_id: str | None = Query(None),
    min_turns: int | None = Query(None, ge=0),
    since: str | None = Query(None, description="RFC3339: only sessions started at/after"),
    until: str | None = Query(None, description="RFC3339: only sessions started at/before"),
):
    """List sessions from the orchestrator's session store."""
    params: dict = {"page": page, "per_page": per_page}
    if status:
        params["status"] = status
    if pipeline_name:
        params["pipeline_name"] = pipeline_name
    if pipeline_hash:
        params["pipeline_hash"] = pipeline_hash
    if tag:
        params["tag"] = tag
    if exclude_tag:
        params["exclude_tag"] = exclude_tag
    if worker_id:
        params["worker_id"] = worker_id
    if min_turns is not None:
        params["min_turns"] = min_turns
    if since:
        params["since"] = since
    if until:
        params["until"] = until

    async with orch_client() as client:
        try:
            response = await client.get(
                f"{ORCHESTRATOR_URL}/api/v1/sessions", params=params, timeout=5.0
            )
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/sessions/{session_id}")
async def get_session_detail(
    session_id: str,
    events_offset: int = Query(0, ge=0),
    events_limit: int = Query(200, ge=1, le=5000),
    wait_for_playback_report: bool = Query(False, description="Wait for PlaybackReport to resolve barge-ins before returning"),
):
    """Session detail: orchestrator metadata + turns, merged with local capture artifacts."""
    
    if wait_for_playback_report:
        # Briefly poll for up to 500ms to allow orchestrator to write turn_resolved.json
        # and pipeline_events.jsonl for any recently resolved barge-ins.
        for _ in range(10):
            await asyncio.sleep(0.05)

    # Fetch session + turns from orchestrator
    orch_data = None
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/sessions/{session_id}", session_id=session_id), timeout=5.0
            )
            response.raise_for_status()
            orch_data = response.json().get("data")
        except Exception:  # noqa: BLE001, S110 - orchestrator unreachable: the capture alone is served
            pass

    # The capture read and the verdict are blocking file I/O and CPU work;
    # on the event loop they would stall every other request until done.
    result = await asyncio.to_thread(
        _local_session_detail, session_id, events_offset, events_limit, orch_data
    )
    return {"data": result}


def _local_session_detail(
    session_id: str, events_offset: int, events_limit: int, orch_data: dict | None
) -> dict:
    """Read the session's capture artifacts, merge the orchestrator record
    over them and add the provider identity and verdict. Blocking."""
    artifacts = _read_capture_artifacts(session_id, events_offset, events_limit)
    local_meta = _read_session_metadata(session_id)

    if orch_data:
        # Merge orchestrator session data with local artifacts
        result = {
            "id": session_id,
            "metadata": merge_session_metadata(local_meta, orch_data),
            "turns": orch_data.get("turns", []),
            **artifacts,
        }
    else:
        # Fallback to captures-only mode (orchestrator down)
        result = {
            "id": session_id,
            "metadata": local_meta,
            "turns": [],
            **artifacts,
        }

    # ── Provider identity + per-utterance enrichment ──
    # Derive a single `providers` block (model names, streaming mode) so the
    # UI can render a header strip without re-implementing the same logic.
    # Also enrich each utterance summary with `stt_mode` and `ttft_estimated`
    # so TurnCards can render "(streaming)" and "(est.)" hints without
    # cross-referencing other event payloads.
    utts = result.get("utterances") or []
    providers = _extract_provider_identity(utts, result.get("metadata") or {})
    result["providers"] = providers

    stt_mode = (providers.get("stt") or {}).get("mode") or "unknown"
    for utt in utts:
        summary = utt.get("summary")
        if summary is None:
            continue
        summary.setdefault("stt_mode", stt_mode)
        # Cold-turn TTFT fallback: shared with turns._flatten_turn (P1.7).
        if summary.get("ttft_ms") is None:
            resolved_ttft, est = resolve_turn_ttft(
                summary.get("ttft_ms"),
                utt.get("llm_detail"),
                generation_ms=summary.get("generation_ms"),
            )
            if resolved_ttft is not None:
                summary["ttft_ms"] = resolved_ttft
                if est:
                    summary["ttft_estimated"] = True

    # ── Verdict (item 8) ──
    # Compute the same session-level health verdict that the LLM-bundle
    # uses, so the UI can show a coloured chip in the header without
    # round-tripping through `/report-for-llm`.  Reuses the same
    # provider-aware target resolution.
    try:
        # Mirror _build_report_for_llm: drop phantom turns and resolve which
        # utterance id is the cold-start turn (ids are 0- or 1-based depending
        # on capture config).  Passing first_utt_id=None left every turn
        # unmarked, so the cold-start turn's warm-up latency was included in
        # the critical-path percentiles and could pin the chip to "broken"
        # while /report-for-llm said "fair".
        verdict_utts = [u for u in utts if not is_empty_turn(u)]
        utt_ids = [
            u.get("utterance_id") for u in verdict_utts
            if isinstance(u.get("utterance_id"), int)
        ]
        first_utt_id = min(utt_ids) if utt_ids else None
        flat = [
            _flatten_turn(u, result["metadata"].get("started_at"), first_utt_id, providers)
            for u in verdict_utts
        ]
        tool_calls = result.get("client_tool_calls") or []
        _apply_tool_context(flat, verdict_utts, tool_calls)
        # The same resolver the report uses, so the session page and the
        # report cannot grade one session two ways.
        grading = resolve_grading(providers, result.get("metadata"))
        aggs = _component_aggregates(flat, client_tool_calls=tool_calls)
        anoms = _detect_anomalies(flat, grading["targets"], tool_calls)
        result["verdict"] = _compute_verdict(flat, aggs, anoms, providers, grading=grading)
    except Exception:
        # The verdict is decorative; it must not break the page.
        log.warning("verdict for session %s could not be computed", session_id, exc_info=True)
        result["verdict"] = None

    return result


@router.get("/api/v1/sessions/{session_id}/report-for-llm")
async def session_report_for_llm(
    session_id: str,
    format: str = Query("markdown", pattern="^(markdown|json)$"),
    targets: str | None = Query(
        None,
        description='Override defaults, e.g. "ttft_ms:250,ttfb_ms:350,rtf_max:0.5"',
    ),
    include_events: bool = Query(
        False,
        description=(
            "When true, embed a compact `event_timeline[]` derived from "
            "events.jsonl (timing/lifecycle events with stripped payloads). "
            "Useful when pasting into an LLM to debug ordering / propagation "
            "issues that aren't visible from the per-turn aggregates."
        ),
    ),
    health: str | None = Query(
        None,
        pattern="^(v1|v2)$",
        description="Health model: v1 (binary) or v2 (graded). Defaults to VP_HEALTH_MODEL env.",
    ),
):
    """Build a compact diagnostic bundle suitable for pasting into an LLM
    prompt to ask "is this pipeline healthy?".

    Reuses the same data sources as `GET /api/v1/sessions/{session_id}`
    but reshapes them for analysis: per-turn flat rows, per-component
    aggregates, precomputed anomalies, and explicit SLA targets.
    """
    # Fetch orchestrator session for metadata (best-effort).
    orch_data = None
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/sessions/{session_id}", session_id=session_id), timeout=5.0
            )
            response.raise_for_status()
            orch_data = response.json().get("data")
        except Exception:  # noqa: BLE001, S110 - orchestrator unreachable: the capture alone is served
            pass

    # Reading every capture artifact and building the report is blocking
    # work; it runs off the event loop like the session detail's.
    return await asyncio.to_thread(
        _report_for_llm_response, session_id, orch_data, format, targets, include_events, health
    )


def _report_for_llm_response(
    session_id: str,
    orch_data: dict | None,
    format: str,
    targets: str | None,
    include_events: bool,
    health: str | None,
):
    # Pull the FULL local artifacts (no pagination).
    artifacts = _read_capture_artifacts(session_id, events_offset=0, events_limit=5000)
    local_meta: dict = _read_session_metadata(session_id)

    metadata: dict = merge_session_metadata(local_meta, orch_data) if orch_data else dict(local_meta)

    if not artifacts.get("utterances") and not orch_data:
        raise HTTPException(
            status_code=404,
            detail=f"No data found for session {session_id} (orchestrator unreachable and no local captures)",
        )

    utterances = artifacts.get("utterances") or []

    # Resolve provider identity (model names, streaming mode) from the
    # richest event payload available, then derive provider-aware SLA
    # targets before applying the caller's `?targets=` overrides.
    providers = _extract_provider_identity(utterances, metadata)
    grading = resolve_grading(providers, metadata, targets)
    parsed_targets = grading["targets"]

    report = _build_report_for_llm(
        session_id=session_id,
        metadata=metadata,
        utterances=utterances,
        session_metrics=artifacts.get("session_metrics"),
        providers=providers,
        targets=parsed_targets,
        events=(artifacts.get("events") if include_events else None),
        health_model=health,
        client_tool_calls=artifacts.get("client_tool_calls"),
        grading=grading,
    )

    if format == "json":
        return {"data": report}
    return PlainTextResponse(
        _render_report_markdown(report),
        media_type="text/markdown; charset=utf-8",
    )


@router.get("/api/v1/sessions/{session_id}/turns")
async def session_turns(session_id: str):
    """Proxy to orchestrator: list turns for a session."""
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/sessions/{session_id}/turns", session_id=session_id), timeout=5.0
            )
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


# ── Turns Endpoints (proxy to orchestrator) ──────────────────

@router.get("/api/v1/turns")
async def list_turns(
    page: int = Query(1, ge=1),
    per_page: int = Query(50, ge=1, le=500),
    status: str | None = Query(None),
    pipeline_name: str | None = Query(None),
    worker_id: str | None = Query(None),
    search: str | None = Query(None),
):
    """Proxy to orchestrator: flat list of all turns."""
    params: dict = {"page": page, "per_page": per_page}
    if status:
        params["status"] = status
    if pipeline_name:
        params["pipeline_name"] = pipeline_name
    if worker_id:
        params["worker_id"] = worker_id
    if search:
        params["search"] = search

    async with orch_client() as client:
        try:
            response = await client.get(
                f"{ORCHESTRATOR_URL}/api/v1/turns", params=params, timeout=5.0
            )
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/turns/{turn_id}")
async def get_turn(turn_id: str):
    """Proxy to orchestrator: get a single turn by ID."""
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/turns/{turn_id}", turn_id=turn_id), timeout=5.0
            )
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}
