"""Tests for the Admin BFF Playground FIFO queue integration and /api/v1/capacity."""
from __future__ import annotations

import json
import os
import unittest
from contextlib import ExitStack
from unittest import mock

import httpx
from fastapi.testclient import TestClient

from app.test_auth import _fresh_app
from app.test_playground_proxy import _EchoWorker


class PlaygroundQueueTests(unittest.TestCase):
    def _setup_client(self, orch_handler, *, multi_tenant: bool = False):
        env = {
            "ADMIN_MULTI_TENANT": "true" if multi_tenant else "false",
            "ADMIN_SESSION_SECRET": "test-secret-0123456789abcdef0123456789abcdef",
            "ADMIN_UI_AUTH_REQUIRED": "false",
            "PLAYGROUND_ADMISSION_ENABLED": "true",
        }
        main = _fresh_app(env)
        from app.core import auth as auth_mod
        from app. routers import health, playground

        def orch(**kwargs):
            kwargs.pop("headers", None)
            return httpx.AsyncClient(transport=httpx.MockTransport(orch_handler), **kwargs)

        stack = ExitStack()
        for mod in (playground, health):
            stack.enter_context(mock.patch.object(mod, "orch_client", orch))
            stack.enter_context(mock.patch.object(mod, "ORCHESTRATOR_URL", "http://orch"))
        stack.enter_context(mock.patch.object(playground, "LOOPBACK_HOST", ""))
        stack.enter_context(mock.patch.object(playground, "QUEUE_POLL_WAIT_MS", 50))
        stack.enter_context(
            mock.patch.dict(os.environ, {"PLAYGROUND_ADMISSION_ENABLED": "true"}, clear=False)
        )
        self.addCleanup(stack.close)

        client = TestClient(main.app)
        if multi_tenant:
            client.cookies.set(auth_mod.TENANT_COOKIE_NAME, auth_mod.sign_tenant_cookie("hf-alice"))
        return client

    @classmethod
    def tearDownClass(cls):
        _fresh_app({"ADMIN_MULTI_TENANT": "false", "ADMIN_UI_AUTH_REQUIRED": "false"})

    def test_queued_caller_receives_queue_status_then_promotes_to_worker(self):
        with _EchoWorker() as worker:
            polls = 0

            def handler(request: httpx.Request) -> httpx.Response:
                nonlocal polls
                path = request.url.path
                if path == "/api/v1/workers/w1":
                    return httpx.Response(
                        200,
                        json={"data": {"id": "w1", "transport_address": f"127.0.0.1:{worker.port}"}},
                    )
                if path == "/api/v1/dispatch/acquire":
                    return httpx.Response(
                        202,
                        json={
                            "data": {
                                "status": "queued",
                                "ticket_id": "vp_ticket_1",
                                "position": 2,
                                "ahead": 1,
                                "queue_depth": 2,
                                "active_sessions": 3,
                                "capacity_sessions": 3,
                                "estimated_wait_s": 30,
                            }
                        },
                    )
                if path == "/api/v1/dispatch/tickets/vp_ticket_1":
                    polls += 1
                    if polls == 1:
                        return httpx.Response(
                            200,
                            json={
                                "data": {
                                    "status": "queued",
                                    "ticket_id": "vp_ticket_1",
                                    "position": 1,
                                    "ahead": 0,
                                    "queue_depth": 1,
                                    "active_sessions": 3,
                                    "capacity_sessions": 3,
                                    "estimated_wait_s": 15,
                                }
                            },
                        )
                    return httpx.Response(
                        200,
                        json={
                            "data": {
                                "status": "granted",
                                "worker_id": "w1",
                                "ws_url": f"ws://127.0.0.1:{worker.port}/ws",
                                "lease_id": "vp_lease_1",
                                "expires_in_s": 15,
                            }
                        },
                    )
                return httpx.Response(404)

            client = self._setup_client(handler)
            with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
                msg1 = json.loads(ws.receive_text())
                self.assertEqual(msg1["type"], "queue_status")
                self.assertEqual(msg1["position"], 2)

                msg2 = json.loads(ws.receive_text())
                self.assertEqual(msg2["type"], "queue_status")
                self.assertEqual(msg2["position"], 1)

                msg3 = json.loads(ws.receive_text())
                self.assertEqual(msg3["type"], "queue_granted")
                self.assertEqual(msg3["worker_id"], "w1")

                # Now spliced directly to the upstream worker echo server
                ws.send_text("hello-after-queue")
                self.assertEqual(ws.receive_text(), "hello-after-queue")

    def test_429_from_orchestrator_closes_with_4429(self):
        def handler(request: httpx.Request) -> httpx.Response:
            path = request.url.path
            if path == "/api/v1/workers/w1":
                return httpx.Response(
                    200, json={"data": {"id": "w1", "transport_address": "127.0.0.1:8800"}}
                )
            if path == "/api/v1/dispatch/acquire":
                return httpx.Response(
                    429,
                    json={
                        "data": None,
                        "error": {
                            "code": "TENANT_LIMIT_REACHED",
                            "message": "per-user concurrency limit (1) reached",
                        },
                    },
                )
            return httpx.Response(404)

        client = self._setup_client(handler)
        with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
            msg = ws.receive()
            self.assertEqual(msg["type"], "websocket.close")
            self.assertEqual(msg["code"], 4429)
            self.assertIn("concurrency limit", msg["reason"])

    def test_capacity_endpoint_scrubs_per_tenant_ids_in_multitenant_mode(self):
        def handler(request: httpx.Request) -> httpx.Response:
            if request.url.path == "/api/v1/capacity":
                return httpx.Response(
                    200,
                    json={
                        "data": {
                            "admission_enabled": True,
                            "active_sessions": 2,
                            "capacity_sessions": 3,
                            "queue_depth": 1,
                            "per_tenant_active": {"hf-alice": 1, "hf-bob": 1},
                        },
                        "error": None,
                    },
                )
            return httpx.Response(404)

        client = self._setup_client(handler, multi_tenant=True)
        resp = client.get("/api/v1/capacity")
        self.assertEqual(resp.status_code, 200)
        data = resp.json()["data"]
        self.assertEqual(data["active_sessions"], 2)
        self.assertEqual(data["queue_depth"], 1)
        self.assertNotIn("per_tenant_active", data)

    def test_failed_upstream_connect_releases_lease_with_tenant_id(self):
        deleted_leases: list[tuple[str, str | None]] = []

        def handler(request: httpx.Request) -> httpx.Response:
            path = request.url.path
            if path == "/api/v1/workers/w1":
                # Unreachable port 1 so _connect_upstream fails after lease grant
                return httpx.Response(
                    200, json={"data": {"id": "w1", "transport_address": "127.0.0.1:1"}}
                )
            if path == "/api/v1/dispatch/acquire":
                return httpx.Response(
                    200,
                    json={
                        "data": {
                            "status": "granted",
                            "worker_id": "w1",
                            "ws_url": "ws://127.0.0.1:1/ws",
                            "lease_id": "vp_lease_fail",
                            "expires_in_s": 15,
                        }
                    },
                )
            if path == "/api/v1/dispatch/tickets/vp_lease_fail" and request.method == "DELETE":
                deleted_leases.append((path, request.headers.get("x-tenant-id")))
                return httpx.Response(200, json={"data": {"id": "vp_lease_fail", "cancelled": True}})
            return httpx.Response(404)

        client = self._setup_client(handler, multi_tenant=True)
        with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
            msg = ws.receive()
            self.assertEqual(msg["type"], "websocket.close")
            self.assertEqual(msg["code"], 4502)

        self.assertEqual(
            deleted_leases,
            [("/api/v1/dispatch/tickets/vp_lease_fail", "hf-alice")],
        )

    def test_session_close_releases_unredeemed_lease_with_tenant_id(self):
        deleted_leases: list[tuple[str, str | None]] = []
        with _EchoWorker() as worker:

            def handler(request: httpx.Request) -> httpx.Response:
                path = request.url.path
                if path == "/api/v1/workers/w1":
                    return httpx.Response(
                        200,
                        json={"data": {"id": "w1", "transport_address": f"127.0.0.1:{worker.port}"}},
                    )
                if path == "/api/v1/dispatch/acquire":
                    return httpx.Response(
                        200,
                        json={
                            "data": {
                                "status": "granted",
                                "worker_id": "w1",
                                "ws_url": f"ws://127.0.0.1:{worker.port}/ws",
                                "lease_id": "vp_lease_close",
                                "expires_in_s": 15,
                            }
                        },
                    )
                if path == "/api/v1/dispatch/tickets/vp_lease_close" and request.method == "DELETE":
                    deleted_leases.append((path, request.headers.get("x-tenant-id")))
                    return httpx.Response(
                        200, json={"data": {"id": "vp_lease_close", "cancelled": True}}
                    )
                return httpx.Response(404)

            client = self._setup_client(handler, multi_tenant=True)
            with client.websocket_connect("/api/v1/playground/ws?worker_id=w1") as ws:
                ws.send_text("ping")
                self.assertEqual(ws.receive_text(), "ping")

        self.assertEqual(
            deleted_leases,
            [("/api/v1/dispatch/tickets/vp_lease_close", "hf-alice")],
        )


if __name__ == "__main__":
    unittest.main()
