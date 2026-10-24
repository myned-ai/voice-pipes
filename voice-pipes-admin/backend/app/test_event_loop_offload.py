"""A slow capture read must not stall the event loop.

The session and audio routes read capture files and concatenate WAVs. Run
on the event loop, a slow disk held every other request, the liveness
probe included, until the read finished. Each test starts a request whose
capture read blocks for ``SLOW_S`` and, while it is in flight, times a
``/livez`` request on the same loop.
"""
from __future__ import annotations

import asyncio
import time
import unittest
from unittest import mock

import httpx
from fastapi import FastAPI

from app.routers import audio, auth, sessions

SLOW_S = 1.0
# Far below SLOW_S, far above what /livez costs on a loaded machine.
LIVEZ_BUDGET_S = 0.5


class _FailingOrchClient:
    """Stands in for the orchestrator client: every call fails at once."""

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return False

    async def get(self, *args, **kwargs):
        raise httpx.ConnectError("orchestrator not reachable in this test")


def _slow_read_artifacts(session_id, events_offset=0, events_limit=200):
    time.sleep(SLOW_S)
    return {"utterances": []}


def _app() -> FastAPI:
    app = FastAPI()
    app.include_router(auth.router)
    app.include_router(sessions.router)
    app.include_router(audio.router)
    return app


async def _livez_while(path: str) -> tuple[float, int]:
    """Start a request to ``path``, then time ``/livez`` while it runs.
    Returns (livez seconds, status of the slow request)."""
    transport = httpx.ASGITransport(app=_app())
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        # Timed from before the slow request starts: a read that blocks the
        # loop runs inside the yield below and delays everything after it.
        t0 = time.perf_counter()
        slow = asyncio.create_task(client.get(path))
        # Let the slow request reach its blocking read.
        await asyncio.sleep(0.05)
        r = await client.get("/livez")
        elapsed = time.perf_counter() - t0
        assert r.status_code == 200
        slow_resp = await slow
        return elapsed, slow_resp.status_code


class EventLoopOffloadTests(unittest.TestCase):
    def setUp(self) -> None:
        patches = [
            mock.patch.object(sessions, "orch_client", lambda **kw: _FailingOrchClient()),
            mock.patch.object(sessions, "_read_capture_artifacts", _slow_read_artifacts),
            mock.patch.object(sessions, "_read_session_metadata", lambda sid: {}),
        ]
        for p in patches:
            p.start()
            self.addCleanup(p.stop)

    def test_session_detail_read_does_not_block_livez(self) -> None:
        elapsed, status = asyncio.run(_livez_while("/api/v1/sessions/sess-1"))
        self.assertEqual(status, 200)
        self.assertLess(elapsed, LIVEZ_BUDGET_S)

    def test_report_read_does_not_block_livez(self) -> None:
        # No utterances and no orchestrator record: the route answers 404,
        # after the slow read.
        elapsed, status = asyncio.run(
            _livez_while("/api/v1/sessions/sess-1/report-for-llm")
        )
        self.assertEqual(status, 404)
        self.assertLess(elapsed, LIVEZ_BUDGET_S)

    def test_wav_concat_does_not_block_livez(self) -> None:
        from pathlib import Path

        def slow_frags(session_dir, utterance_id):
            time.sleep(SLOW_S)
            return [Path("a.wav"), Path("b.wav")]

        def concat(frags):
            return (1, 2, 16000), b"\x00\x00" * 160

        with mock.patch.object(audio, "_resolve_session_dir", lambda sid: Path(".")), \
                mock.patch.object(audio, "_turn_frags", slow_frags), \
                mock.patch.object(audio, "_concat_frag_frames", concat):
            elapsed, status = asyncio.run(
                _livez_while("/api/v1/sessions/sess-1/audio/turn/1/tts_concat.wav")
            )
        self.assertEqual(status, 200)
        self.assertLess(elapsed, LIVEZ_BUDGET_S)


if __name__ == "__main__":
    unittest.main()
