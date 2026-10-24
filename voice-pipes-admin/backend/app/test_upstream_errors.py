"""A failed orchestrator call must not tell the browser where the orchestrator is.

``str()`` of an ``httpx`` error carries the URL it was fetching, and a refused
connect often the host and port. Every proxy route used to return that text as
the error message, and the fleet stream put it on the SSE wire. These tests
point the routes at an orchestrator whose host name is distinctive, make it
fail both ways (no connection; an error status), and look for the name in what
comes back.
"""
from __future__ import annotations

import asyncio
import unittest
from contextlib import ExitStack
from unittest import mock

import httpx

from app.test_auth import _fresh_app

HOST = "orch-internal-7f3a.cluster.local"
ORCH_URL = f"http://{HOST}:9090"

# GET routes that proxy the orchestrator and answer with the error envelope.
ENVELOPE_ROUTES = (
    "/api/v1/health",
    "/api/v1/workers",
    "/api/v1/workers/w1",
    "/api/v1/dispatch",
    "/api/v1/pipelines",
    "/api/v1/pipelines/diff?hash_a=a&hash_b=b",
    "/api/v1/metrics",
    "/api/v1/components",
    "/api/v1/workers/w1/components",
    "/api/v1/sessions",
    "/api/v1/sessions/s1/turns",
    "/api/v1/turns",
    "/api/v1/turns/t1",
    "/api/v1/benchmark/configs",
    "/api/v1/benchmark/experiments",
    "/api/v1/benchmark/experiments/e1",
    "/api/v1/benchmark/experiments/e1/results",
    "/api/v1/benchmark/suites",
    "/api/v1/benchmark/suites/s1",
)

# Routes that answer a failure with an HTTP error carrying the envelope.
RAISING_ROUTES = (
    "/api/v1/workers/w1/probe/fixtures",
    "/api/v1/workers/w1/probe/audio/a.wav",
    "/api/v1/benchmark/experiments/e1/variants/v1/scenarios/s1/entries/n1/audio",
)


def _refusing(request: httpx.Request) -> httpx.Response:
    raise httpx.ConnectError(
        f"[Errno 111] Connect call failed ('{HOST}', 9090)", request=request
    )


def _failing(request: httpx.Request) -> httpx.Response:
    return httpx.Response(500, content=b"", request=request)


class UpstreamErrorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient

        main = _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "false"})
        cls.client = TestClient(main.app)

    def _orchestrator(self, handler):
        """Point every proxy module at a mock orchestrator served by `handler`."""
        from app.routers import benchmark, health, sessions, stream

        def client(**kwargs):
            kwargs.pop("headers", None)
            return httpx.AsyncClient(transport=httpx.MockTransport(handler), **kwargs)

        stack = ExitStack()
        for module in (health, benchmark, sessions, stream):
            stack.enter_context(mock.patch.object(module, "orch_client", client))
            stack.enter_context(mock.patch.object(module, "ORCHESTRATOR_URL", ORCH_URL))
        self.addCleanup(stack.close)

    def _assert_no_host(self, path: str, text: str) -> None:
        self.assertNotIn(HOST, text, f"{path} leaked the orchestrator host")
        self.assertNotIn("9090", text, f"{path} leaked the orchestrator port")

    def test_an_unreachable_orchestrator_is_reported_without_its_address(self):
        self._orchestrator(_refusing)
        for path in ENVELOPE_ROUTES:
            resp = self.client.get(path)
            self._assert_no_host(path, resp.text)
            error = resp.json()["error"]
            self.assertEqual(error["code"], "ORCHESTRATOR_UNREACHABLE", path)
            self.assertTrue(error["message"], path)
        for path in RAISING_ROUTES:
            resp = self.client.get(path)
            self.assertEqual(resp.status_code, 502, path)
            self._assert_no_host(path, resp.text)
            self.assertEqual(resp.json()["detail"]["error"]["code"], "ORCHESTRATOR_UNREACHABLE", path)

    def test_an_error_status_is_reported_without_the_url(self):
        self._orchestrator(_failing)
        for path in ("/api/v1/health", "/api/v1/workers", "/api/v1/sessions", "/api/v1/benchmark/configs"):
            resp = self.client.get(path)
            self._assert_no_host(path, resp.text)
            self.assertEqual(
                resp.json()["error"], {
                    "code": "ORCHESTRATOR_ERROR",
                    "status": 500,
                    "message": "the orchestrator answered HTTP 500",
                },
                path,
            )

    def test_the_pipeline_graph_is_an_error_not_an_invented_graph(self):
        # The route used to answer an unreachable orchestrator with a
        # hard-coded demo pipeline and no error, which the Live Pipeline page
        # drew as if a worker were running it.
        self._orchestrator(_refusing)
        for path in ("/api/v1/pipeline", "/api/v1/pipeline?worker_id=w1"):
            body = self.client.get(path).json()
            self.assertIsNone(body["data"], path)
            self.assertEqual(body["error"]["code"], "ORCHESTRATOR_UNREACHABLE", path)
        self._orchestrator(_failing)
        body = self.client.get("/api/v1/pipeline").json()
        self.assertIsNone(body["data"])
        self.assertEqual(body["error"]["code"], "ORCHESTRATOR_ERROR")

    def test_the_fleet_stream_carries_no_address(self):
        from app.routers import stream

        self._orchestrator(_refusing)
        snap = asyncio.run(stream._FleetStream()._fetch())
        self.assertTrue(snap["error"])
        self.assertNotIn(HOST, snap["error"])


if __name__ == "__main__":
    unittest.main()
