"""Request bodies have a ceiling, and the benchmark bodies have a shape.

The probe route read its body whole with ``await request.body()`` and held it
for the length of the probe; the benchmark routes took ``body: dict``. Neither
had a size bound. These tests send a body one byte over each ceiling, with and
without a ``Content-Length``, and check it is refused with 413 before anything
reaches the orchestrator.
"""
from __future__ import annotations

import json
import unittest
from contextlib import ExitStack
from unittest import mock

import httpx

from app.core.limits import MAX_JSON_BYTES, MAX_PROBE_BYTES
from app.test_auth import _fresh_app


class _ProxyCase(unittest.TestCase):
    """The real app, with the orchestrator replaced by a recorder."""

    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient

        main = _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "false"})
        cls.client = TestClient(main.app)

    def setUp(self):
        from app.routers import benchmark, health

        self.forwarded: list[httpx.Request] = []

        def handler(request: httpx.Request) -> httpx.Response:
            self.forwarded.append(request)
            return httpx.Response(200, json={"data": {}, "error": None}, request=request)

        def client(**kwargs):
            kwargs.pop("headers", None)
            return httpx.AsyncClient(transport=httpx.MockTransport(handler), **kwargs)

        stack = ExitStack()
        for module in (health, benchmark):
            stack.enter_context(mock.patch.object(module, "orch_client", client))
            stack.enter_context(mock.patch.object(module, "ORCHESTRATOR_URL", "http://orch"))
        self.addCleanup(stack.close)


class BodyLimitTests(_ProxyCase):
    def _post_probe(self, size: int, chunked: bool = False):
        body = b"\0" * size
        content = iter([body[i : i + 65536] for i in range(0, size, 65536)]) if chunked else body
        return self.client.post(
            "/api/v1/workers/w1/probe/stt",
            content=content,
            headers={"content-type": "audio/wav"},
        )

    def test_a_probe_body_at_the_ceiling_is_forwarded(self):
        resp = self._post_probe(MAX_PROBE_BYTES)
        self.assertEqual(resp.status_code, 200)
        self.assertEqual(len(self.forwarded), 1)
        self.assertEqual(len(self.forwarded[0].content), MAX_PROBE_BYTES)

    def test_a_probe_body_one_byte_over_is_413(self):
        resp = self._post_probe(MAX_PROBE_BYTES + 1)
        self.assertEqual(resp.status_code, 413)
        self.assertEqual(resp.json()["error"]["code"], "PAYLOAD_TOO_LARGE")
        self.assertEqual(self.forwarded, [])

    def test_a_chunked_probe_body_one_byte_over_is_413(self):
        resp = self._post_probe(MAX_PROBE_BYTES + 1, chunked=True)
        self.assertEqual(resp.status_code, 413)
        self.assertEqual(self.forwarded, [])

    def test_a_json_body_one_byte_over_is_413(self):
        padding = "x" * MAX_JSON_BYTES
        resp = self.client.post(
            "/api/v1/benchmark/suites",
            content=('{"name": "s", "description": "' + padding + '"}').encode(),
            headers={"content-type": "application/json"},
        )
        self.assertEqual(resp.status_code, 413)
        self.assertEqual(self.forwarded, [])
        resp = self.client.patch(
            "/api/v1/workers/w1/params",
            content=('{"system_prompt": "' + padding + '"}').encode(),
            headers={"content-type": "application/json"},
        )
        self.assertEqual(resp.status_code, 413)
        self.assertEqual(self.forwarded, [])


class BenchmarkBodyShapeTests(_ProxyCase):
    def test_a_suite_without_a_name_is_refused_before_forwarding(self):
        resp = self.client.post("/api/v1/benchmark/suites", json={"scenarios": []})
        self.assertEqual(resp.status_code, 422)
        self.assertEqual(self.forwarded, [])

    def test_an_experiment_with_too_many_variants_is_refused(self):
        body = {"name": "e", "test_suite_id": "s", "variants": [{}] * 65}
        resp = self.client.post("/api/v1/benchmark/experiments", json=body)
        self.assertEqual(resp.status_code, 422)
        self.assertEqual(self.forwarded, [])

    def test_a_valid_suite_is_forwarded_as_sent(self):
        body = {"name": "s", "scenarios": [{"id": "a", "input": {"type": "text_prompt", "text": "hi"}}],
                "future_field": 1}
        resp = self.client.put("/api/v1/benchmark/suites/s1", json=body)
        self.assertEqual(resp.status_code, 200)
        self.assertEqual(len(self.forwarded), 1)


        self.assertEqual(json.loads(self.forwarded[0].content), body)


if __name__ == "__main__":
    unittest.main()
