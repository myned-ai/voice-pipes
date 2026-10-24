"""Path parameters reach the orchestrator as one encoded segment each.

The browser sends ``/api/v1/workers/a%3Fb``; the route receives the decoded
``a?b``. Interpolated as it is, the upstream URL became ``/workers/a?b``:
worker ``a`` with a query string. These tests run each proxy against a mock
orchestrator and read the raw path it was asked for.
"""
from __future__ import annotations

import unittest
from contextlib import ExitStack
from unittest import mock

import httpx

from app.core.upstream import upstream_path
from app.test_auth import _fresh_app


class UpstreamPathTests(unittest.TestCase):
    def test_reserved_characters_and_dot_segments_are_encoded(self):
        self.assertEqual(upstream_path("/w/{id}", id="a?b"), "/w/a%3Fb")
        self.assertEqual(upstream_path("/w/{id}", id="a#b/c"), "/w/a%23b%2Fc")
        self.assertEqual(upstream_path("/w/{id}", id=".."), "/w/%2E%2E")
        self.assertEqual(upstream_path("/w/{id}", id="."), "/w/%2E")
        self.assertEqual(upstream_path("/w/{id}", id="worker-a_1"), "/w/worker-a_1")


class ProxiedPathTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient

        main = _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "false"})
        cls.client = TestClient(main.app)

    def setUp(self):
        from app.routers import benchmark, health, sessions

        self.seen: list[str] = []

        def handler(request: httpx.Request) -> httpx.Response:
            self.seen.append(request.url.raw_path.decode("ascii"))
            return httpx.Response(200, json={"data": {}, "error": None}, request=request)

        def client(**kwargs):
            kwargs.pop("headers", None)
            return httpx.AsyncClient(transport=httpx.MockTransport(handler), **kwargs)

        stack = ExitStack()
        for module in (health, benchmark, sessions):
            stack.enter_context(mock.patch.object(module, "orch_client", client))
            stack.enter_context(mock.patch.object(module, "ORCHESTRATOR_URL", "http://orch"))
        self.addCleanup(stack.close)

    def _call(self, method: str, path: str, **kwargs) -> str:
        self.seen.clear()
        self.client.request(method, path, **kwargs)
        self.assertEqual(len(self.seen), 1, f"{method} {path} made {len(self.seen)} upstream calls")
        return self.seen[0]

    def test_a_worker_id_with_a_question_mark_stays_in_the_path(self):
        self.assertEqual(
            self._call("GET", "/api/v1/workers/a%3Fb/components"),
            "/api/v1/workers/a%3Fb/components",
        )
        self.assertEqual(self._call("GET", "/api/v1/workers/a%3Fb"), "/api/v1/workers/a%3Fb")
        self.assertEqual(
            self._call("PATCH", "/api/v1/workers/a%3Fb/params", json={"k": 1}),
            "/api/v1/workers/a%3Fb/params",
        )
        self.assertEqual(
            self._call("GET", "/api/v1/workers/a%3Fb/probe/fixtures"),
            "/api/v1/workers/a%3Fb/probe/fixtures",
        )
        self.assertEqual(
            self._call("GET", "/api/v1/workers/a%3Fb/probe/audio/x%23y.wav"),
            "/api/v1/workers/a%3Fb/probe/audio/x%23y.wav",
        )

    def test_session_turn_and_benchmark_ids_stay_in_the_path(self):
        self.assertEqual(
            self._call("GET", "/api/v1/sessions/s%3Fx/turns"), "/api/v1/sessions/s%3Fx/turns"
        )
        self.assertEqual(self._call("GET", "/api/v1/turns/t%23x"), "/api/v1/turns/t%23x")
        self.assertEqual(
            self._call("GET", "/api/v1/benchmark/experiments/e%3Fx/results"),
            "/api/v1/benchmark/experiments/e%3Fx/results",
        )
        self.assertEqual(
            self._call("DELETE", "/api/v1/benchmark/suites/s%3Fx"),
            "/api/v1/benchmark/suites/s%3Fx",
        )


if __name__ == "__main__":
    unittest.main()
