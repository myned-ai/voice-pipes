"""Fleet health/metrics stream (P14 — kill the 5s poll storm).

Every dashboard client used to poll the orchestrator's /workers + /components
every 5s; with N clients × M workers that multiplies orchestrator load.  Here
the BFF runs a SINGLE poller and fans the combined snapshot out to all
connected clients over Server-Sent Events, so orchestrator load is O(1) in the
number of dashboard tabs.  The poller is lazy: it runs only while ≥1 client is
subscribed, and stops when the last disconnects.

No orchestrator changes — this is pure BFF fan-out.  The poller also performs
the worker-death/recovery ingestion that the /workers proxy did, so that signal
keeps flowing when clients use the stream instead of polling.
"""
import asyncio
import json
import os

from fastapi import APIRouter
from fastapi.responses import StreamingResponse

from ..core.config import ORCHESTRATOR_URL, orch_client
from ..core.upstream import upstream_error
from .health import _ingest_worker_snapshot

router = APIRouter()

_FLEET_POLL_S = float(os.getenv("FLEET_STREAM_POLL_S", "5"))


class _FleetStream:
    def __init__(self) -> None:
        self._subs: set[asyncio.Queue] = set()
        self._latest: dict | None = None
        self._task: asyncio.Task | None = None
        self._lock = asyncio.Lock()

    async def _fetch(self) -> dict:
        snap: dict = {"workers": [], "components": [], "error": None}
        async with orch_client() as client:
            try:
                wr = await client.get(f"{ORCHESTRATOR_URL}/api/v1/workers", timeout=5.0)
                wr.raise_for_status()
                payload = wr.json()
                _ingest_worker_snapshot(payload)  # keep death/recovery detection alive
                wdata = payload.get("data") or {}
                snap["workers"] = wdata.get("items") or wdata.get("workers") or []
            except Exception as e:  # noqa: BLE001 - any upstream failure goes into the snapshot's error
                snap["error"] = upstream_error(e)["message"]
            try:
                cr = await client.get(f"{ORCHESTRATOR_URL}/api/v1/components", timeout=5.0)
                cr.raise_for_status()
                cdata = cr.json().get("data")
                if isinstance(cdata, list):
                    snap["components"] = cdata
                elif isinstance(cdata, dict):
                    snap["components"] = cdata.get("items") or cdata.get("components") or []
            except Exception:  # noqa: BLE001, S110 - components are optional; the workers error above covers a down orchestrator
                pass
        return snap

    async def _loop(self) -> None:
        try:
            while True:
                snap = await self._fetch()
                self._latest = snap
                for q in list(self._subs):
                    try:
                        q.put_nowait(snap)
                    except asyncio.QueueFull:
                        pass  # slow client — it'll get the next snapshot
                await asyncio.sleep(_FLEET_POLL_S)
        except asyncio.CancelledError:
            pass

    async def subscribe(self):
        q: asyncio.Queue = asyncio.Queue(maxsize=8)
        async with self._lock:
            self._subs.add(q)
            if self._task is None or self._task.done():
                self._task = asyncio.create_task(self._loop())
        try:
            if self._latest is not None:
                yield self._latest  # don't make a fresh client wait a full interval
            while True:
                yield await q.get()
        finally:
            async with self._lock:
                self._subs.discard(q)
                if not self._subs and self._task is not None:
                    self._task.cancel()
                    self._task = None


_fleet_stream = _FleetStream()


@router.get("/api/v1/stream/fleet")
async def stream_fleet():
    async def gen():
        async for snap in _fleet_stream.subscribe():
            yield f"data: {json.dumps(snap)}\n\n"

    return StreamingResponse(
        gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",  # disable proxy buffering (nginx) for SSE
        },
    )
