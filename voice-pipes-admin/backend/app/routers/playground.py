"""The Playground's WebSocket proxy: browser <-> one registered worker's ``/ws``.

The browser never holds ``VP_CLIENT_TOKEN``. It opens
``/api/v1/playground/ws?worker_id=<id>`` on this backend (authenticated by the
admin cookie like every other route); the proxy asks the orchestrator for that
worker, refuses an id the orchestrator does not list, and connects to the
worker's transport presenting the client token itself.

Only a worker the orchestrator lists can be reached, so the proxy cannot be
pointed at an arbitrary host. The address comes from the worker's registration,
the same one ``GET /api/v1/dispatch`` hands out: the public transport address
when it registered one, else its transport bind address.

Close codes the browser can see:

``4400``  no ``worker_id`` (refused before the handshake completes)
``4403``  the orchestrator does not list that worker (refused likewise)
``4502``  the worker's transport could not be reached
``4503``  the orchestrator could not be asked
``1009``  a browser frame over ``MAX_CLIENT_FRAME_BYTES``

When the worker closes first, its close code and reason are passed on.
"""
from __future__ import annotations

import asyncio
import logging
import os

import anyio
import httpx
import websockets
from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from ..core import config as _config
from ..core.config import ORCHESTRATOR_URL, orch_client
from ..core.upstream import upstream_path

log = logging.getLogger("admin.playground")

router = APIRouter()

# A worker that registered a loopback or wildcard address is reachable at
# that address only from its own host. When this backend runs in a container
# (the admin compose file sets this to host.docker.internal), that host is
# reached under another name. Empty = connect to loopback as registered.
LOOPBACK_HOST = os.getenv("PLAYGROUND_LOOPBACK_HOST", "").strip()

# The worker refuses a client message over 1 MiB; refusing it here saves
# holding and forwarding it.
MAX_CLIENT_FRAME_BYTES = 1024 * 1024
# Worker -> browser frames are audio chunks and JSON events; 16 MiB is far
# above either and still a bound.
MAX_UPSTREAM_FRAME_BYTES = 16 * 1024 * 1024
# Frames websockets buffers from the worker before it stops reading the socket.
UPSTREAM_QUEUE_FRAMES = 64

LOOKUP_TIMEOUT_S = 5.0
CONNECT_TIMEOUT_S = 10.0

_LOOPBACK = {"localhost", "127.0.0.1", "::1", "[::1]"}
_WILDCARD = {"0.0.0.0", "::", "[::]"}


class _Refused(Exception):
    def __init__(self, code: int, reason: str, before_accept: bool) -> None:
        super().__init__(reason)
        self.code = code
        self.reason = reason
        self.before_accept = before_accept


def _split_host_port(address: str) -> tuple[str, str]:
    host, sep, port = address.rpartition(":")
    if not sep or not port.isdigit():
        return address, ""
    return host, port


def worker_ws_url(worker: dict) -> str | None:
    """The ``ws://`` URL this backend connects to for a worker record."""
    address = worker.get("public_transport_address") or worker.get("transport_address")
    if not isinstance(address, str) or not address.strip():
        return None
    host, port = _split_host_port(address.strip())
    if not port:
        return None
    if host in _WILDCARD:
        host = LOOPBACK_HOST or "127.0.0.1"
    elif host in _LOOPBACK and LOOPBACK_HOST:
        host = LOOPBACK_HOST
    return f"ws://{host}:{port}/ws"


async def _lookup_worker(worker_id: str) -> dict:
    """The orchestrator's record of ``worker_id``; refuses ids it does not list."""
    try:
        async with orch_client() as client:
            resp = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/workers/{worker_id}", worker_id=worker_id),
                timeout=LOOKUP_TIMEOUT_S,
            )
    except httpx.HTTPError as exc:
        log.warning("playground: worker lookup failed: %s: %s", type(exc).__name__, exc)
        raise _Refused(4503, "orchestrator unreachable", before_accept=False) from exc
    if resp.status_code == 404:
        raise _Refused(4403, "unknown worker", before_accept=True)
    if resp.status_code >= 400:
        log.warning("playground: worker lookup answered HTTP %s", resp.status_code)
        raise _Refused(4503, f"orchestrator answered HTTP {resp.status_code}", before_accept=False)
    try:
        worker = (resp.json() or {}).get("data")
    except ValueError as exc:
        raise _Refused(4503, "orchestrator answer unreadable", before_accept=False) from exc
    if not isinstance(worker, dict):
        raise _Refused(4403, "unknown worker", before_accept=True)
    return worker


def _upstream_headers() -> dict[str, str]:
    # The worker's /ws takes the client token as a bearer header as well as a
    # query parameter; the header keeps it out of the worker's access log.
    if _config.CLIENT_TOKEN:
        return {"Authorization": f"Bearer {_config.CLIENT_TOKEN}"}
    return {}


async def _connect_upstream(url: str):
    try:
        return await websockets.connect(
            url,
            additional_headers=_upstream_headers(),
            max_size=MAX_UPSTREAM_FRAME_BYTES,
            max_queue=UPSTREAM_QUEUE_FRAMES,
            open_timeout=CONNECT_TIMEOUT_S,
            close_timeout=2.0,
        )
    except (TimeoutError, OSError, websockets.exceptions.WebSocketException) as exc:
        # The URL is not logged: it is internal topology, and nothing about
        # the failure needs it beyond the exception's own text.
        log.warning("playground: worker transport connect failed: %s: %s", type(exc).__name__, exc)
        raise _Refused(4502, "worker unreachable", before_accept=False) from exc


class _ClientFrameTooLarge(Exception):
    pass


async def _client_to_upstream(client_ws: WebSocket, upstream) -> None:
    while True:
        msg = await client_ws.receive()
        # Starlette reports the browser leaving as a message, not an
        # exception; receiving again after it raises.
        if msg.get("type") == "websocket.disconnect":
            return
        text = msg.get("text")
        data = msg.get("bytes")
        if text is not None:
            if len(text.encode("utf-8")) > MAX_CLIENT_FRAME_BYTES:
                raise _ClientFrameTooLarge()
            await upstream.send(text)
        elif data is not None:
            if len(data) > MAX_CLIENT_FRAME_BYTES:
                raise _ClientFrameTooLarge()
            await upstream.send(data)


async def _upstream_to_client(client_ws: WebSocket, upstream) -> None:
    async for msg in upstream:
        if isinstance(msg, bytes):
            await client_ws.send_bytes(msg)
        else:
            await client_ws.send_text(msg)


async def _close(client_ws: WebSocket, code: int, reason: str) -> None:
    try:
        await client_ws.close(code=code, reason=reason)
    except (Exception, asyncio.CancelledError):  # noqa: BLE001, S110 - the browser may already be gone; nothing to tell it
        pass


@router.websocket("/api/v1/playground/ws")
async def playground_ws_proxy(client_ws: WebSocket):
    worker_id = client_ws.query_params.get("worker_id", "").strip()
    if not worker_id:
        await client_ws.close(code=4400, reason="worker_id is required")
        return

    try:
        worker = await _lookup_worker(worker_id)
        url = worker_ws_url(worker)
        if url is None:
            raise _Refused(4502, "worker has no transport address", before_accept=False)
        upstream = await _connect_upstream(url)
    except _Refused as refused:
        if not refused.before_accept:
            # Accept first so the browser sees the code and reason; a
            # handshake refused outright reaches it only as 1006.
            await client_ws.accept()
        await _close(client_ws, refused.code, refused.reason)
        return

    await client_ws.accept()
    pumps = {
        asyncio.create_task(_client_to_upstream(client_ws, upstream), name="client->worker"),
        asyncio.create_task(_upstream_to_client(client_ws, upstream), name="worker->client"),
    }
    close_code, close_reason = 1000, ""
    try:
        done, pending = await asyncio.wait(pumps, return_when=asyncio.FIRST_COMPLETED)
        for task in pending:
            task.cancel()
        # Wait for the cancelled pump to unwind, so neither outlives the
        # sockets it reads from.
        await asyncio.gather(*pending, return_exceptions=True)
        for task in done:
            exc = task.exception()
            if isinstance(exc, _ClientFrameTooLarge):
                close_code, close_reason = 1009, "frame too large"
            elif isinstance(exc, (WebSocketDisconnect, websockets.exceptions.ConnectionClosed)):
                pass
            elif exc is not None:
                log.warning("playground: %s ended with %s: %s", task.get_name(), type(exc).__name__, exc)
                close_code, close_reason = 1011, "proxy error"
        if close_code == 1000 and upstream.close_code is not None:
            # The worker ended the session: tell the browser what it said.
            close_code = upstream.close_code
            close_reason = upstream.close_reason or ""
            if close_code in (1005, 1006, 1015):
                # Reserved codes that may not be sent in a close frame.
                close_code, close_reason = 4502, "worker connection lost"
    except asyncio.CancelledError:
        pass
    finally:
        with anyio.CancelScope(shield=True):
            for task in pumps:
                task.cancel()
            await asyncio.gather(*pumps, return_exceptions=True)
            try:
                await asyncio.wait_for(upstream.close(), timeout=2.0)
            except (Exception, asyncio.CancelledError):  # noqa: BLE001, S110 - worker might already be closed or unreachable
                pass
            await _close(client_ws, close_code, close_reason)
