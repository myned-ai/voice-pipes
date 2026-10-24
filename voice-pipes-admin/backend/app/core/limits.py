"""A ceiling on how much of a request body this process will hold.

Starlette reads a body into memory in full: ``await request.body()`` on the
probe route, and FastAPI's own parse of every JSON body. Nothing bounded
either, so one request could hold as much memory as a client cared to send,
and a probe body stayed held for as long as the probe ran (up to three
minutes).

``BodyLimitMiddleware`` refuses a request whose ``Content-Length`` is over the
route's ceiling before anything is read, and stops a body sent without one
(chunked) at the first chunk that crosses it. Either way the answer is 413
with the standard error envelope.
"""
from __future__ import annotations

import json
import re

from starlette.types import ASGIApp, Message, Receive, Scope, Send

# The worker refuses probe audio over 16 kHz * 2 bytes * 30 s (960 000 bytes).
# The ceiling here leaves room for a WAV header and a higher-rate recording
# the worker will refuse with its own message, and no more.
MAX_PROBE_BYTES = 2 * 1024 * 1024

# Every other body is JSON a person typed or the console built: a parameter
# edit, a benchmark suite, an experiment.
MAX_JSON_BYTES = 1024 * 1024

_PROBE_PATH = re.compile(r"^/api/v1/workers/[^/]+/probe/[^/]+$")


def body_limit_for(path: str) -> int:
    return MAX_PROBE_BYTES if _PROBE_PATH.match(path) else MAX_JSON_BYTES


class _TooLarge(Exception):
    pass


def _too_large_body(limit: int) -> bytes:
    return json.dumps(
        {
            "data": None,
            "error": {
                "code": "PAYLOAD_TOO_LARGE",
                "message": f"the request body is over the {limit}-byte limit for this route",
            },
        }
    ).encode("utf-8")


class BodyLimitMiddleware:
    def __init__(self, app: ASGIApp) -> None:
        self.app = app

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope.get("type") != "http":
            await self.app(scope, receive, send)
            return

        limit = body_limit_for(scope.get("path", ""))
        for name, value in scope.get("headers", ()):
            if name.lower() == b"content-length":
                try:
                    declared = int(value)
                except ValueError:
                    declared = limit + 1
                if declared > limit:
                    await self._refuse(send, limit)
                    return
                break

        received = 0
        response_started = False

        async def counting_receive() -> Message:
            nonlocal received
            message = await receive()
            if message.get("type") == "http.request":
                received += len(message.get("body", b""))
                if received > limit:
                    raise _TooLarge()
            return message

        async def tracking_send(message: Message) -> None:
            nonlocal response_started
            if message.get("type") == "http.response.start":
                response_started = True
            await send(message)

        try:
            await self.app(scope, counting_receive, tracking_send)
        except _TooLarge:
            if response_started:
                raise
            await self._refuse(send, limit)

    @staticmethod
    async def _refuse(send: Send, limit: int) -> None:
        await send(
            {
                "type": "http.response.start",
                "status": 413,
                "headers": [(b"content-type", b"application/json"), (b"connection", b"close")],
            }
        )
        await send({"type": "http.response.body", "body": _too_large_body(limit)})
