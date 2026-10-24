"""The issues feed and rollup cache live where the process can write.

The image chowned ``/app`` (and so the stores under ``/app/tmp``) to its own
uid, while compose runs the container as the host user's uid. The stores could
not be created, every write was swallowed, and the dashboard's issues feed was
empty with nothing saying why. The stores now live in a state directory the
running uid makes for itself (``uid-<uid>`` under the image's world-writable
``ADMIN_STATE_ROOT`` volume, or ``ADMIN_STATE_DIR``), and a store that cannot
be opened is logged once and reported on the feed.
"""
from __future__ import annotations

import os
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import app.core.config as config
from app import issues
from app.test_analytics import _make_session
from app.test_auth import _fresh_app

BACKEND = Path(__file__).resolve().parent.parent


class StateDirTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient

        main = _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "false"})
        cls.client = TestClient(main.app)

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        root = Path(self._tmp.name)
        self.caps = root / "captures"
        self.caps.mkdir()
        old_caps = config.CAPTURES_DIR
        config.CAPTURES_DIR = self.caps
        self.addCleanup(setattr, config, "CAPTURES_DIR", old_caps)
        self.state = root / "state"
        env = mock.patch.dict(os.environ, {"ADMIN_STATE_DIR": str(self.state)})
        env.start()
        self.addCleanup(env.stop)
        # A fresh failure state per test.
        for name, value in (("_FAILURE", None), ("_FAILURE_LOGGED", False)):
            patch = mock.patch.object(issues, name, value)
            patch.start()
            self.addCleanup(patch.stop)

    def _issues_after_analytics(self) -> dict:
        # A session that recorded an error is a failed session, which the
        # analytics scan records in the feed off the response path.
        _make_session(self.caps, "s-failed",
                      events=[{"event_type": "stt_error", "payload": {"message": "boom"}}])
        self.assertEqual(self.client.get("/api/v1/analytics?days=365").status_code, 200)
        body: dict = {}
        for _ in range(50):
            body = self.client.get("/api/v1/issues?days=365").json()
            if body["data"]["count"] or body.get("error"):
                break
            time.sleep(0.05)
        return body

    def test_the_feed_records_into_the_configured_state_dir(self):
        body = self._issues_after_analytics()
        self.assertIsNone(body["error"])
        self.assertGreater(body["data"]["count"], 0)
        self.assertEqual(body["data"]["issues"][0]["kind"], "failed_session")
        self.assertTrue((self.state / "issues.db").is_file())
        self.assertTrue((self.state / "rollups.db").is_file())

    def test_an_unusable_store_is_logged_once_and_reported_on_the_feed(self):
        # A file where the directory should be: nothing can be created in it,
        # whatever uid the test runs as.
        self.state.write_text("not a directory")
        with self.assertLogs("admin.issues", level="ERROR") as logs:
            body = self._issues_after_analytics()
            self.client.get("/api/v1/issues")
        self.assertEqual(len(logs.records), 1, logs.output)
        self.assertEqual(body["data"]["count"], 0)
        self.assertEqual(body["error"]["code"], "ISSUES_STORE_UNAVAILABLE")
        self.assertNotIn(str(self.state), body["error"]["message"])


class StateRootTests(unittest.TestCase):
    def test_a_state_root_gives_each_uid_its_own_directory(self):
        from app.core.state import state_dir

        with tempfile.TemporaryDirectory() as root:
            env = {"ADMIN_STATE_ROOT": root, "ADMIN_STATE_DIR": ""}
            with mock.patch.dict(os.environ, env), \
                    mock.patch.object(issues, "_FAILURE", None), \
                    mock.patch.object(issues, "_FAILURE_LOGGED", False):
                uid = os.getuid() if hasattr(os, "getuid") else "user"
                self.assertEqual(state_dir(), Path(root) / f"uid-{uid}")
                self.assertTrue(issues.record_issue(kind="k", dedup_key="d", summary="s"))
                self.assertEqual(len(issues.recent_issues()), 1)
                db = Path(root) / f"uid-{uid}" / "issues.db"
                self.assertTrue(db.is_file())
                if hasattr(os, "getuid"):
                    self.assertEqual(db.parent.stat().st_uid, os.getuid())

    def test_the_image_and_compose_file_provide_a_shared_state_root(self):
        dockerfile = (BACKEND / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("ADMIN_STATE_ROOT=/app/state", dockerfile)
        self.assertIn("chmod 1777 /app/state", dockerfile)
        compose = (BACKEND.parent / "docker-compose.yml").read_text(encoding="utf-8")
        self.assertIn("admin-state:/app/state", compose)


if __name__ == "__main__":
    unittest.main()
