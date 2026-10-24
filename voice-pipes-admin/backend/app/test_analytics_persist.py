"""The issues an analytics scan finds are in the store once the request is done.

They were written by a task the route created and dropped: nothing held a
reference to it (the event loop keeps only a weak one), nothing awaited it,
and an error in it was never seen. The write is now a background task of the
request, which the server finishes before the request is complete.
"""
from __future__ import annotations

import os
import time
import unittest
from tempfile import TemporaryDirectory
from unittest import mock

from fastapi import FastAPI
from fastapi.testclient import TestClient

from app.analytics import compute
from app.issues import recent_issues
from app.routers import analytics


def _scan(days, offset_days, pipeline=None):
    return {
        "response": {"kpi": {"spend_by_provider": {}}},
        "issue_inputs": {
            "failed_sessions": [
                {
                    "session_id": "sess-failed",
                    "timestamp": "2026-10-05T10:00:00Z",
                    "reason": "errors",
                    "errors_total": 3,
                    "turn_count": 2,
                }
            ],
            "provider_changes": [],
            "anomaly_sessions": [],
        },
    }


async def _no_overlay(days):
    return None


class AnalyticsPersistTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        real_persist = compute._persist_analytics_issues

        def slow_persist(issue_inputs, spend):
            # A store that takes a moment, as SQLite on a busy volume does.
            time.sleep(0.3)
            real_persist(issue_inputs, spend)

        patches = [
            mock.patch.dict(os.environ, {"ADMIN_STATE_DIR": self._tmp.name}),
            mock.patch.object(analytics, "_captures_signature", lambda: (1, 1.0)),
            mock.patch.object(analytics, "_compute_analytics_window", _scan),
            mock.patch.object(analytics, "_fetch_orchestrator_overlay", _no_overlay),
            mock.patch.object(analytics, "_persist_analytics_issues", slow_persist),
        ]
        for p in patches:
            p.start()
            self.addCleanup(p.stop)
        analytics._analytics_cache.clear()
        self.addCleanup(analytics._analytics_cache.clear)

    def test_rows_exist_after_the_response(self) -> None:
        app = FastAPI()
        app.include_router(analytics.router)
        with TestClient(app) as client:
            r = client.get("/api/v1/analytics", params={"days": 3})
            self.assertEqual(r.status_code, 200)
            rows = recent_issues(limit=10)
        self.assertEqual([row["session_id"] for row in rows], ["sess-failed"])
        self.assertEqual(rows[0]["kind"], "failed_session")


if __name__ == "__main__":
    unittest.main()
