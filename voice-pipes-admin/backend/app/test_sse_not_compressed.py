"""Responses are gzipped, except the fleet SSE stream.

The backend relies on Starlette's ``GZipMiddleware`` for both halves: it
compresses large JSON (the analytics payload shrinks about tenfold), and it
leaves ``text/event-stream`` alone by default. Compressing the stream would
hold small frames in zlib's buffer, so an EventSource stays connected and
never sees an event. Both are pinned here for the real app with a browser's
``Accept-Encoding``, so a replacement middleware cannot drop either.
"""
from __future__ import annotations

import json
import unittest
from unittest import mock

from fastapi.testclient import TestClient

from app.test_auth import TOKEN, _fresh_app

BROWSER = {"Authorization": f"Bearer {TOKEN}", "Accept-Encoding": "gzip, deflate, br"}


class CompressionTests(unittest.TestCase):
    def test_fleet_stream_is_not_gzipped(self) -> None:
        main = _fresh_app({"ADMIN_UI_TOKEN": TOKEN, "ADMIN_UI_AUTH_REQUIRED": "true"})
        from app.routers import stream

        snap = {"workers": [], "components": [], "error": None}

        async def one_snapshot(self):
            yield snap

        with mock.patch.object(stream._FleetStream, "subscribe", one_snapshot):
            client = TestClient(main.app)
            r = client.get("/api/v1/stream/fleet", headers=BROWSER)
        self.assertEqual(r.status_code, 200)
        self.assertTrue(r.headers["content-type"].startswith("text/event-stream"))
        self.assertNotIn("content-encoding", r.headers)
        self.assertEqual(r.text, f"data: {json.dumps(snap)}\n\n")

    def test_a_large_json_response_is_gzipped(self) -> None:
        main = _fresh_app({"ADMIN_UI_TOKEN": TOKEN, "ADMIN_UI_AUTH_REQUIRED": "true"})
        payload = {"utterances": [{"id": i, "stt_ms": 120.5, "tts_ms": 80.25} for i in range(200)]}

        @main.app.get("/api/v1/test-large-json")
        async def large_json() -> dict:
            return payload

        client = TestClient(main.app)
        r = client.get("/api/v1/test-large-json", headers=BROWSER)
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.headers.get("content-encoding"), "gzip")
        self.assertIn("Accept-Encoding", r.headers.get("vary", ""))
        self.assertEqual(r.json(), payload)
        # The client decodes the body; Content-Length is what went on the wire.
        self.assertLess(int(r.headers["content-length"]), len(json.dumps(payload)) // 4)


if __name__ == "__main__":
    unittest.main()
