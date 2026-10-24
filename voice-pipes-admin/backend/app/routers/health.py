import logging
from datetime import UTC, datetime
from typing import Any
from urllib.parse import urlencode

import httpx
from fastapi import APIRouter, HTTPException, Query, Request, Response

from ..core import config
from ..core.config import _CAPTURES_DIR_FALLBACK, ORCHESTRATOR_URL, orch_client
from ..core.upstream import upstream_error, upstream_path
from ..issues import record_issue

router = APIRouter()
log = logging.getLogger("admin.health")


# ── Health / Pipeline / Metrics (proxy to orchestrator) ──────

@router.get("/api/v1/health")
async def health():
    async with orch_client() as client:
        try:
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/health", timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}

@router.get("/api/v1/debug_paths")
async def debug_paths():
    """Internal hostnames and filesystem paths — only with ADMIN_DEBUG=1."""
    import os

    if os.getenv("ADMIN_DEBUG", "").strip().lower() not in ("1", "true", "yes", "on"):
        raise HTTPException(status_code=404, detail="debug endpoints disabled (set ADMIN_DEBUG=1)")
    return {
        "ORCHESTRATOR_URL": ORCHESTRATOR_URL,
        "CAPTURES_DIR": str(config.CAPTURES_DIR),
        "FALLBACK": str(_CAPTURES_DIR_FALLBACK)
    }


# ── Multi-Pipeline Routing Proxies ────────────────────────────

# In-memory snapshot of last-seen worker statuses keyed by worker_id.  Used by
# the /api/v1/workers proxy to detect status transitions and persist
# ``worker_death`` / ``worker_back`` issues.  Lives only in process memory —
# acceptable because the issues themselves are persisted in SQLite.
_LAST_WORKER_STATUS: dict[str, str] = {}


def _ingest_worker_snapshot(payload: Any) -> None:
    """Compare the current /workers payload against ``_LAST_WORKER_STATUS``
    and emit issues for transitions to/from a healthy state."""
    try:
        data = payload.get("data") if isinstance(payload, dict) else None
        workers_list = data.get("workers") if isinstance(data, dict) else data
        if not isinstance(workers_list, list):
            return
        seen: set[str] = set()
        for w in workers_list:
            if not isinstance(w, dict):
                continue
            wid = w.get("worker_id") or w.get("id")
            if not wid:
                continue
            seen.add(wid)
            status = (w.get("status") or "").lower()
            prev = _LAST_WORKER_STATUS.get(wid)
            if prev is None:
                _LAST_WORKER_STATUS[wid] = status
                continue
            if prev == status:
                continue
            _LAST_WORKER_STATUS[wid] = status
            healthy = {"healthy", "ready", "available", "active", "online"}
            unhealthy = {"unhealthy", "dead", "failed", "offline", "error"}
            now_ts = datetime.now(UTC).timestamp()
            ts_bucket = int(now_ts)  # 1s dedup granularity
            if prev in healthy and status in unhealthy:
                record_issue(
                    kind="worker_death",
                    severity="error",
                    dedup_key=f"worker_death:{wid}:{ts_bucket}",
                    summary=f"Worker {wid} transitioned {prev} → {status}",
                    pipeline_name=w.get("pipeline_name"),
                    payload={"worker_id": wid, "from": prev, "to": status},
                )
            elif prev in unhealthy and status in healthy:
                record_issue(
                    kind="worker_back",
                    severity="info",
                    dedup_key=f"worker_back:{wid}:{ts_bucket}",
                    summary=f"Worker {wid} recovered: {prev} → {status}",
                    pipeline_name=w.get("pipeline_name"),
                    payload={"worker_id": wid, "from": prev, "to": status},
                )
    except Exception:
        # Issue detection must never fail the workers proxy.
        log.warning("worker status snapshot could not be ingested", exc_info=True)


@router.get("/api/v1/workers")
async def list_workers():
    """Proxy to orchestrator: list registered workers with pipeline info."""
    async with orch_client() as client:
        try:
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/workers", timeout=5.0)
            response.raise_for_status()
            payload = response.json()
            _ingest_worker_snapshot(payload)
            return payload
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/workers/{worker_id}")
async def get_worker(worker_id: str):
    """Proxy to orchestrator: get worker detail with pipeline info."""
    async with orch_client() as client:
        try:
            response = await client.get(ORCHESTRATOR_URL + upstream_path("/api/v1/workers/{worker_id}", worker_id=worker_id), timeout=5.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


PROBE_KINDS = ("vad", "stt", "tts", "llm")


def _probe_payload(worker_id: str, response) -> dict:
    """The JSON a probe answered with, or an error that names what went wrong.

    A worker built before the probe endpoints existed answers them with a bare
    404, and an orchestrator of the same age relays that as it is. Parsing it
    raised a decoder error ("Expecting value: line 1 column 1") that reached
    the console verbatim and said nothing about the worker.
    """
    try:
        return response.json()
    except ValueError:
        pass
    if response.status_code == 404:
        code, message = "PROBE_UNSUPPORTED", (
            f"Worker {worker_id} has no probe endpoints: it is running a build from "
            "before they were added. Rebuild and restart that worker."
        )
    else:
        code, message = "BAD_PROBE_RESPONSE", (
            f"Worker {worker_id} answered the probe with HTTP {response.status_code} "
            "and a body that is not JSON."
        )
    raise HTTPException(
        status_code=501 if response.status_code == 404 else 502,
        detail={"data": None, "error": {"code": code, "message": message}},
    )
# A probe may load a model on its first call; the worker caps its own run.
PROBE_TIMEOUT_S = 180.0


@router.get("/api/v1/workers/{worker_id}/probe/fixtures")
async def get_worker_probe_fixtures(worker_id: str):
    """Proxy to orchestrator: list the fixtures a worker can be probed with.

    Read from the worker's committed manifest so the console's list cannot
    drift from the fixtures that actually exist on that worker.
    """
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/workers/{worker_id}/probe/fixtures", worker_id=worker_id),
                timeout=10.0,
            )
            payload = _probe_payload(worker_id, response)
            if response.status_code >= 400:
                raise HTTPException(status_code=response.status_code, detail=payload)
            return payload
        except HTTPException:
            raise
        except Exception as e:
            raise HTTPException(
                status_code=502,
                detail={"data": None, "error": upstream_error(e)},
            ) from e


@router.get("/api/v1/workers/{worker_id}/probe/audio/{name}")
async def get_worker_probe_audio(worker_id: str, name: str):
    """Proxy to orchestrator: fetch audio a worker's TTS probe synthesised.

    Returned as the bytes the worker sent, with the worker's content type,
    rather than wrapped in JSON: the console plays it in an audio element, and
    a relabelled WAV is one a browser refuses to play.
    """
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/workers/{worker_id}/probe/audio/{name}", worker_id=worker_id, name=name),
                timeout=30.0,
            )
        except Exception as e:
            raise HTTPException(
                status_code=502,
                detail={"data": None, "error": upstream_error(e)},
            ) from e
    if response.status_code >= 400:
        try:
            payload = response.json()
        except ValueError:
            payload = {"raw": response.text}
        raise HTTPException(status_code=response.status_code, detail=payload)
    return Response(
        content=response.content,
        media_type=response.headers.get("content-type", "application/octet-stream"),
    )


@router.post("/api/v1/workers/{worker_id}/probe/{kind}")
async def post_worker_probe(worker_id: str, kind: str, request: Request):
    """Proxy to orchestrator: run one component against a known input.

    The console cannot call a worker's debug API directly -- it is on an
    internal port behind the internal token, which the browser must never
    hold. This forwards the request unchanged through the orchestrator, which
    attaches the credential.

    The body is passed through as raw bytes rather than parsed: the audio
    probes take a WAV and the text probes take JSON, and this layer has no
    reason to understand either.
    """
    if kind not in PROBE_KINDS:
        raise HTTPException(
            status_code=404,
            detail={"data": None, "error": {"code": "UNKNOWN_PROBE",
                                            "message": f"no probe named {kind!r}"}},
        )
    body = await request.body()
    async with orch_client() as client:
        try:
            response = await client.post(
                ORCHESTRATOR_URL + upstream_path("/api/v1/workers/{worker_id}/probe/{kind}", worker_id=worker_id, kind=kind),
                params=dict(request.query_params),
                content=body,
                headers={"content-type": request.headers.get(
                    "content-type", "application/octet-stream")},
                timeout=PROBE_TIMEOUT_S,
            )
            payload = _probe_payload(worker_id, response)
            if response.status_code >= 400:
                raise HTTPException(status_code=response.status_code, detail=payload)
            return payload
        except HTTPException:
            raise
        except Exception as e:
            raise HTTPException(
                status_code=502,
                detail={"data": None, "error": upstream_error(e)},
            ) from e


@router.patch("/api/v1/workers/{worker_id}/params")
async def patch_worker_params(worker_id: str, body: dict):
    """Proxy to orchestrator: live-edit a worker parameter (e.g. system_prompt).

    Forwards the request body unchanged to the orchestrator's transparent
    proxy, which in turn forwards to the worker's `PATCH /debug/params`
    endpoint. Validation runs entirely on the worker side via the
    schema-driven `ParamRegistry`.
    """
    async with orch_client() as client:
        try:
            response = await client.patch(
                ORCHESTRATOR_URL + upstream_path("/api/v1/workers/{worker_id}/params", worker_id=worker_id),
                json=body,
                timeout=10.0,
            )
            # Forward the response body verbatim so the frontend can read
            # `data.error.message` on validation failures.
            try:
                payload = response.json()
            except ValueError:
                payload = {"raw": response.text}
            if response.status_code >= 400:
                raise HTTPException(status_code=response.status_code, detail=payload)
            return payload
        except HTTPException:
            raise
        except Exception as e:
            raise HTTPException(
                status_code=502,
                detail={"data": None, "error": upstream_error(e)},
            ) from e


@router.get("/api/v1/dispatch")
async def dispatch(
    pipeline_name: str | None = Query(None),
    pipeline_hash: str | None = Query(None),
    tags: str | None = Query(None),
    policy: str | None = Query(None),
):
    """Proxy to orchestrator: pick a worker, and the path to reach it by.

    The orchestrator answers with the worker's own ``ws_url``. The console
    does not connect there: a token-protected worker would refuse it, and the
    worker token stays on this side. The answer carries ``ws_path`` instead,
    the Playground proxy for that worker, relative to this backend.
    """
    params = {k: v for k, v in {
        "pipeline_name": pipeline_name,
        "pipeline_hash": pipeline_hash,
        "tags": tags,
        "policy": policy,
    }.items() if v is not None}
    async with orch_client() as client:
        try:
            response = await client.get(
                f"{ORCHESTRATOR_URL}/api/v1/dispatch", params=params, timeout=5.0
            )
            response.raise_for_status()
            payload = response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}
    data = payload.get("data") if isinstance(payload, dict) else None
    if isinstance(data, dict):
        data.pop("ws_url", None)
        worker_id = data.get("worker_id")
        if isinstance(worker_id, str) and worker_id:
            data["ws_path"] = "/api/v1/playground/ws?" + urlencode({"worker_id": worker_id})
    return payload


@router.get("/api/v1/pipelines")
async def list_pipelines():
    """Proxy to orchestrator: workers grouped by pipeline config hash."""
    async with orch_client() as client:
        try:
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/pipelines", timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/pipelines/diff")
async def pipeline_diff(
    hash_a: str = Query(...),
    hash_b: str = Query(...),
):
    """Proxy to orchestrator: structural diff between two pipeline configs."""
    async with orch_client() as client:
        try:
            response = await client.get(
                f"{ORCHESTRATOR_URL}/api/v1/pipelines/diff",
                params={"hash_a": hash_a, "hash_b": hash_b},
                timeout=5.0,
            )
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/pipeline")
async def get_pipeline(worker_id: str = Query(None)):
    async with orch_client() as client:
        try:
            params = {}
            if worker_id:
                params["worker_id"] = worker_id
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/pipeline", params=params, timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/metrics")
async def get_metrics(worker_id: str = Query(None)):
    async with orch_client() as client:
        try:
            params = {}
            if worker_id:
                params["worker_id"] = worker_id
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/metrics", params=params, timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/components")
async def list_components():
    """Proxy: per-worker component health rollup across the fleet."""
    async with orch_client() as client:
        try:
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/components", timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/workers/{worker_id}/components")
async def get_worker_components(worker_id: str):
    """Proxy: component snapshots + aggregate health for a single worker."""
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/workers/{worker_id}/components", worker_id=worker_id),
                timeout=5.0,
            )
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}
