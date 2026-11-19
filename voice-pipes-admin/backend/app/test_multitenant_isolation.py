"""Tests for multi-tenant HF Space auth, per-user analytics/session isolation,
and automatic TTL/disk-cap capture cleanup.
"""
from __future__ import annotations

import json
import os
import tempfile
import time
import unittest
from datetime import UTC, datetime
from pathlib import Path
from unittest import mock

from fastapi.testclient import TestClient

from app.analytics.compute import _analytics_cache
from app.core import cleanup, config
from app.test_auth import _fresh_app


def _write_session(
    root: Path,
    tenant: str,
    sid: str,
    pipeline_name: str,
    *,
    age_seconds: float = 0.0,
    extra_bytes: int = 0,
) -> Path:
    sdir = root / tenant / sid
    sdir.mkdir(parents=True, exist_ok=True)
    now_iso = datetime.now(UTC).isoformat().replace("+00:00", "Z")
    meta = {
        "id": sid,
        "session_id": sid,
        "tenant_id": tenant,
        "pipeline_name": pipeline_name,
        "start_time": now_iso,
        "started_at": now_iso,
        "stt_provider": "moonshine",
        "tts_provider": "piper",
    }
    meta_path = sdir / "metadata.json"
    meta_path.write_text(json.dumps(meta), encoding="utf-8")
    ev_path = sdir / "events.jsonl"
    ev_path.write_text("", encoding="utf-8")
    if extra_bytes > 0:
        (sdir / "001_mic_raw.wav").write_bytes(b"\x00" * extra_bytes)
    if age_seconds > 0:
        old_ts = time.time() - age_seconds
        for f in sdir.iterdir():
            os.utime(f, (old_ts, old_ts))
        os.utime(sdir, (old_ts, old_ts))
    return sdir


class MultiTenantIsolationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.env = {
            "ADMIN_MULTI_TENANT": "true",
            "ADMIN_SESSION_SECRET": "test-secret-0123456789abcdef0123456789abcdef",
            "ADMIN_ALLOW_GUEST": "true",
            "ADMIN_UI_AUTH_REQUIRED": "false",
        }
        cls.main = _fresh_app(cls.env)

    @classmethod
    def tearDownClass(cls):
        # Restore default single-tenant module state for other test modules
        _fresh_app({"ADMIN_MULTI_TENANT": "false", "ADMIN_UI_AUTH_REQUIRED": "false"})

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.captures_dir = Path(self.temp_dir.name)
        self.patcher = mock.patch.object(config, "CAPTURES_DIR", self.captures_dir)
        self.patcher.start()
        _analytics_cache.clear()

        self.alpha_dir = _write_session(self.captures_dir, "hf-alice", "sess-a1", "alice-pipe")
        self.beta_dir = _write_session(self.captures_dir, "hf-bob", "sess-b1", "bob-pipe")

    def tearDown(self):
        self.patcher.stop()
        self.temp_dir.cleanup()
        _analytics_cache.clear()

    def _client_for(self, tenant_id: str) -> TestClient:
        from app.core import auth as auth_mod

        client = TestClient(self.main.app)
        client.cookies.set(auth_mod.TENANT_COOKIE_NAME, auth_mod.sign_tenant_cookie(tenant_id))
        return client

    def test_unauthenticated_request_is_rejected(self):
        client = TestClient(self.main.app)
        self.assertEqual(client.get("/api/v1/sessions").status_code, 401)
        self.assertEqual(client.get("/api/v1/analytics").status_code, 401)

        check = client.get("/api/v1/auth/check")
        self.assertEqual(check.status_code, 200)
        data = check.json()["data"]
        self.assertTrue(data["auth_required"])
        self.assertFalse(data["authenticated"])
        self.assertTrue(data["allow_guest"])

    def test_guest_session_mints_isolated_cookie(self):
        client = TestClient(self.main.app)
        resp = client.post("/api/v1/auth/guest")
        self.assertEqual(resp.status_code, 200)
        tid = resp.json()["data"]["tenant_id"]
        self.assertTrue(tid.startswith("u-"))

        check = client.get("/api/v1/auth/check")
        self.assertTrue(check.json()["data"]["authenticated"])
        self.assertEqual(check.json()["data"]["tenant_id"], tid)

    def test_tampered_or_expired_cookie_rejected(self):
        from app.core import auth as auth_mod

        client = TestClient(self.main.app)
        client.cookies.set(auth_mod.TENANT_COOKIE_NAME, "hf-alice.9999999999.deadbeef")
        self.assertEqual(client.get("/api/v1/analytics").status_code, 401)

        expired = auth_mod.sign_tenant_cookie("hf-alice", ttl_s=-10)
        client.cookies.set(auth_mod.TENANT_COOKIE_NAME, expired)
        self.assertEqual(client.get("/api/v1/analytics").status_code, 401)

    def test_analytics_and_pipelines_strictly_isolated_per_user(self):
        alice = self._client_for("hf-alice")
        bob = self._client_for("hf-bob")

        # Alice only sees alice-pipe, even if she tries to spoof x-tenant-id or ?tenant_id=hf-bob
        resp_a = alice.get(
            "/api/v1/analytics?days=7&tenant_id=hf-bob",
            headers={"x-tenant-id": "hf-bob"},
        )
        self.assertEqual(resp_a.status_code, 200)
        data_a = resp_a.json()["data"]
        self.assertEqual(data_a["available_pipelines"], ["alice-pipe"])
        self.assertEqual(data_a["kpi"]["total_sessions"], 1)

        # Bob only sees bob-pipe
        resp_b = bob.get("/api/v1/analytics?days=7")
        self.assertEqual(resp_b.status_code, 200)
        data_b = resp_b.json()["data"]
        self.assertEqual(data_b["available_pipelines"], ["bob-pipe"])
        self.assertEqual(data_b["kpi"]["total_sessions"], 1)

    def test_session_detail_cannot_be_spoofed_across_tenants(self):
        alice = self._client_for("hf-alice")

        # Alice can read her own session
        ok = alice.get("/api/v1/sessions/sess-a1")
        self.assertEqual(ok.status_code, 200)

        # Alice cannot read Bob's session even by spoofing header/query
        denied = alice.get(
            "/api/v1/sessions/sess-b1?tenant_id=hf-bob",
            headers={"x-tenant-id": "hf-bob"},
        )
        self.assertEqual(denied.status_code, 404)

    def test_clear_analytics_only_deletes_calling_tenants_captures(self):
        alice = self._client_for("hf-alice")
        resp = alice.post("/api/v1/analytics/clear", json={"delete_captures": True})
        self.assertEqual(resp.status_code, 200)
        self.assertEqual(resp.json()["data"]["deleted_captures"], 1)

        self.assertFalse(self.alpha_dir.exists())
        self.assertTrue(self.beta_dir.exists())


class CaptureJanitorTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.captures_dir = Path(self.temp_dir.name)
        self.patcher = mock.patch.object(config, "CAPTURES_DIR", self.captures_dir)
        self.patcher.start()

    def tearDown(self):
        self.patcher.stop()
        self.temp_dir.cleanup()

    def test_ttl_sweep_deletes_expired_and_prunes_empty_tenant(self):
        old_dir = _write_session(self.captures_dir, "hf-old", "sess-old", "p", age_seconds=3600)
        fresh_dir = _write_session(self.captures_dir, "hf-fresh", "sess-fresh", "p", age_seconds=60)

        res = cleanup.sweep_captures_once(ttl_minutes=30, max_mb=0)
        self.assertEqual(res["ttl_deleted"], 1)
        self.assertFalse(old_dir.exists())
        self.assertFalse((self.captures_dir / "hf-old").exists())
        self.assertTrue(fresh_dir.exists())

    def test_disk_cap_evicts_oldest_sessions_first(self):
        oldest = _write_session(
            self.captures_dir, "hf-a", "sess-1", "p", age_seconds=600, extra_bytes=700_000
        )
        newest = _write_session(
            self.captures_dir, "hf-b", "sess-2", "p", age_seconds=10, extra_bytes=700_000
        )

        # Total is ~1.4 MB; cap at 1.0 MB -> evicts oldest only
        res = cleanup.sweep_captures_once(ttl_minutes=0, max_mb=1.0)
        self.assertEqual(res["cap_deleted"], 1)
        self.assertFalse(oldest.exists())
        self.assertTrue(newest.exists())


if __name__ == "__main__":
    unittest.main()
