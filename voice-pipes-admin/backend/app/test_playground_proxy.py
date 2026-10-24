"""The Playground's WebSocket proxy, end to end.

The browser opens ``/api/v1/playground/ws?worker_id=<id>``; the proxy asks the
orchestrator for that worker, refuses one it does not list, and relays to the
worker's ``/ws`` presenting ``VP_CLIENT_TOKEN`` itself. This runs a real
WebSocket echo server as the worker, a mock orchestrator, and the real app as
the proxy.
"""
from __future__ import annotations

import asyncio
import socket
import threading
import unittest
from contextlib import ExitStack
from unittest import mock

import httpx
import websockets
from starlette.websockets import WebSocketDisconnect

from app.test_auth import _fresh_app


class _EchoWorker:
    """A WebSocket server on a free local port that echoes every frame and
    records the path and Authorization header of each connection."""

    def __init__(self) -> None:
        self.paths: list[str] = []
        self.auth_headers: list[str | None] = []
        self.closed = threading.Event()
        self.port = 0
        self._ready = threading.Event()
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._run, daemon=True)

    async def _echo(self, ws) -> None:
        request = ws.request
        self.paths.append(request.path)
        self.auth_headers.append(request.headers.get("Authorization"))
        try:
            async for message in ws:
                if message == "close-4001":
                    await ws.close(4001, "worker says goodbye")
                    return
                await ws.send(message)
        finally:
            self.closed.set()

    def _run(self) -> None:
        asyncio.set_event_loop(self._loop)

        async def serve() -> None:
            # A short close handshake: on teardown the proxy may already be
            # gone, and the default 10 s wait for its answer outlasts the
            # shutdown bound below.
            self._server = await websockets.serve(self._echo, "127.0.0.1", 0, close_timeout=1)
            self.port = next(iter(self._server.sockets)).getsockname()[1]
            self._ready.set()
            await asyncio.Future()

        try:
            self._loop.run_until_complete(serve())
        except RuntimeError:
            pass  # the loop was stopped on teardown

    def __enter__(self) -> _EchoWorker:
        self._thread.start()
        assert self._ready.wait(5), "echo worker did not start"
        return self

    def __exit__(self, *exc) -> None:
        async def shutdown() -> None:
            self._server.close()
            try:
                await asyncio.wait_for(self._server.wait_closed(), 2)
            except TimeoutError:
                # A proxied connection still open when the test ends would
                # hold the close for the open and close timeouts; the test
                # is over, so drop it.
                for connection in list(self._server.all_connections):
                    connection.transport.abort()
                await self._server.wait_closed()

        asyncio.run_coroutine_threadsafe(shutdown(), self._loop).result(10)
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(5)


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class PlaygroundProxyTests(unittest.TestCase):
    def _client(self, workers: dict[str, dict], client_token: str = "", orchestrator_down: bool = False):
        """The app with the orchestrator answering for `workers` only."""
        from fastapi.testclient import TestClient

        main = _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "false"})
        from app.core import config
        from app.routers import health, playground

        self.lookups: list[str] = []

        def handler(request: httpx.Request) -> httpx.Response:
            if orchestrator_down:
                raise httpx.ConnectError("refused", request=request)
            path = request.url.raw_path.decode("ascii")
            self.lookups.append(path)
            if path == "/api/v1/dispatch":
                wid = next(iter(workers))
                return httpx.Response(200, json={"data": {
                    "worker_id": wid, "ws_url": "ws://worker-internal:8800/ws", "pipeline_name": "p",
                }, "error": None})
            prefix = "/api/v1/workers/"
            wid = path[len(prefix):] if path.startswith(prefix) else None
            if wid in workers:
                return httpx.Response(200, json={"data": workers[wid], "error": None})
            return httpx.Response(404, json={"data": None, "error": {"code": "NOT_FOUND", "message": "no"}})

        def orch(**kwargs):
            kwargs.pop("headers", None)
            return httpx.AsyncClient(transport=httpx.MockTransport(handler), **kwargs)

        stack = ExitStack()
        for module in (playground, health):
            stack.enter_context(mock.patch.object(module, "orch_client", orch))
            stack.enter_context(mock.patch.object(module, "ORCHESTRATOR_URL", "http://orch"))
        stack.enter_context(mock.patch.object(config, "CLIENT_TOKEN", client_token))
        stack.enter_context(mock.patch.object(playground, "LOOPBACK_HOST", ""))
        self.addCleanup(stack.close)
        return TestClient(main.app)

    @staticmethod
    def _worker(port: int, **extra) -> dict:
        return {"id": "w1", "transport_address": f"0.0.0.0:{port}", **extra}

    def test_text_and_binary_frames_go_through_both_ways(self):
        with _EchoWorker() as worker:
            client = self._client({"w1": self._worker(worker.port)})
            with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
                ws.send_text('{"type":"hello"}')
                self.assertEqual(ws.receive_text(), '{"type":"hello"}')
                audio = bytes(range(256)) * 8
                ws.send_bytes(audio)
                self.assertEqual(ws.receive_bytes(), audio)
        self.assertEqual(worker.paths, ["/ws"])
        self.assertEqual(self.lookups, ["/api/v1/workers/w1"])

    def test_the_worker_token_is_presented_upstream_and_never_to_the_browser(self):
        token = "worker-secret-5c1e"
        with _EchoWorker() as worker:
            client = self._client({"w1": self._worker(worker.port)}, client_token=token)
            dispatch = client.get("/api/v1/dispatch")
            self.assertNotIn(token, dispatch.text)
            browser_path = dispatch.json()["data"]["ws_path"]
            self.assertEqual(browser_path, "/api/v1/playground/ws?worker_id=w1")
            self.assertNotIn(token, browser_path)
            self.assertNotIn(token, client.get("/api/v1/auth/check").text)
            with client.websocket_connect(browser_path) as ws:
                ws.send_text("ping")
                self.assertEqual(ws.receive_text(), "ping")
        self.assertEqual(worker.auth_headers, [f"Bearer {token}"])
        self.assertEqual(worker.paths, ["/ws"])

    def test_dispatch_does_not_hand_out_the_workers_own_address(self):
        with _EchoWorker() as worker:
            client = self._client({"w 1/x": self._worker(worker.port)})
            data = client.get("/api/v1/dispatch").json()["data"]
        self.assertNotIn("ws_url", data)
        self.assertEqual(data["ws_path"], "/api/v1/playground/ws?worker_id=w+1%2Fx")

    def test_a_worker_the_orchestrator_does_not_list_is_refused(self):
        with _EchoWorker() as worker:
            client = self._client({"w1": self._worker(worker.port)})
            for wid in ("evil", "w1/../../admin", "127.0.0.1:8085"):
                with self.assertRaises(WebSocketDisconnect) as ctx:
                    with client.websocket_connect("/api/v1/playground/ws", params={"worker_id": wid}):
                        pass
                self.assertEqual(ctx.exception.code, 4403, wid)
            # Another parameter cannot steer the target either.
            with client.websocket_connect("/api/v1/playground/ws?worker_id=w1&url=ws://169.254.169.254/") as ws:
                ws.send_text("x")
                self.assertEqual(ws.receive_text(), "x")
        self.assertEqual(worker.paths, ["/ws"])
        self.assertIn("/api/v1/workers/w1%2F..%2F..%2Fadmin", self.lookups)

    def test_a_missing_worker_id_is_refused(self):
        client = self._client({})
        with self.assertRaises(WebSocketDisconnect) as ctx, client.websocket_connect("/api/v1/playground/ws"):
            pass
        self.assertEqual(ctx.exception.code, 4400)

    def test_an_unreachable_worker_closes_with_a_reason(self):
        client = self._client({"w1": self._worker(_free_port())})
        with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
            with self.assertRaises(WebSocketDisconnect) as ctx:
                ws.receive_text()
        self.assertEqual(ctx.exception.code, 4502)
        self.assertEqual(ctx.exception.reason, "worker unreachable")

    def test_an_unreachable_orchestrator_closes_with_a_reason(self):
        client = self._client({}, orchestrator_down=True)
        with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
            with self.assertRaises(WebSocketDisconnect) as ctx:
                ws.receive_text()
        self.assertEqual(ctx.exception.code, 4503)

    def test_the_workers_close_code_reaches_the_browser(self):
        with _EchoWorker() as worker:
            client = self._client({"w1": self._worker(worker.port)})
            with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
                ws.send_text("close-4001")
                with self.assertRaises(WebSocketDisconnect) as ctx:
                    ws.receive_text()
        self.assertEqual(ctx.exception.code, 4001)
        self.assertEqual(ctx.exception.reason, "worker says goodbye")

    def test_the_browser_leaving_closes_the_worker_connection(self):
        with _EchoWorker() as worker:
            client = self._client({"w1": self._worker(worker.port)})
            with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
                ws.send_text("hi")
                ws.receive_text()
            self.assertTrue(worker.closed.wait(5), "the proxy left the worker connection open")

    def test_an_oversized_browser_frame_is_refused(self):
        from app.routers import playground

        with _EchoWorker() as worker:
            client = self._client({"w1": self._worker(worker.port)})
            with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
                ws.send_bytes(b"\0" * (playground.MAX_CLIENT_FRAME_BYTES + 1))
                with self.assertRaises(WebSocketDisconnect) as ctx:
                    ws.receive_bytes()
        self.assertEqual(ctx.exception.code, 1009)


class WorkerAddressTests(unittest.TestCase):
    def test_public_address_wins_and_wildcards_become_reachable(self):
        from app.routers import playground

        with mock.patch.object(playground, "LOOPBACK_HOST", ""):
            self.assertEqual(
                playground.worker_ws_url({"public_transport_address": "voice.example:443",
                                          "transport_address": "0.0.0.0:8800"}),
                "ws://voice.example:443/ws",
            )
            self.assertEqual(playground.worker_ws_url({"transport_address": "0.0.0.0:8800"}),
                             "ws://127.0.0.1:8800/ws")
            self.assertIsNone(playground.worker_ws_url({"address": "worker-a:8085"}))
        with mock.patch.object(playground, "LOOPBACK_HOST", "host.docker.internal"):
            self.assertEqual(playground.worker_ws_url({"public_transport_address": "localhost:8801"}),
                             "ws://host.docker.internal:8801/ws")
            self.assertEqual(playground.worker_ws_url({"transport_address": "0.0.0.0:8800"}),
                             "ws://host.docker.internal:8800/ws")


if __name__ == "__main__":
    unittest.main()
