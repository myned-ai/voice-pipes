import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fastapi import HTTPException
from fastapi.testclient import TestClient

from app.captures.reader import _read_session_metadata, _resolve_session_dir
from app.core import config
from app.test_auth import _fresh_app


class MultitenantCapturesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        main = _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "false"})
        cls.client = TestClient(main.app)

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.captures_dir = Path(self.temp_dir.name)
        self.patcher = mock.patch.object(config, "CAPTURES_DIR", self.captures_dir)
        self.patcher.start()

        # Create tenant-alpha session
        self.alpha_dir = self.captures_dir / "tenant-alpha" / "sess-100"
        self.alpha_dir.mkdir(parents=True, exist_ok=True)
        with open(self.alpha_dir / "metadata.json", "w", encoding="utf-8") as f:
            json.dump({"session_id": "sess-100", "tenant_id": "tenant-alpha", "pipeline_name": "alpha-pipe"}, f)

        # Create tenant-beta session
        self.beta_dir = self.captures_dir / "tenant-beta" / "sess-200"
        self.beta_dir.mkdir(parents=True, exist_ok=True)
        with open(self.beta_dir / "metadata.json", "w", encoding="utf-8") as f:
            json.dump({"session_id": "sess-200", "tenant_id": "tenant-beta", "pipeline_name": "beta-pipe"}, f)

        # Create flat legacy session
        self.flat_dir = self.captures_dir / "sess-legacy"
        self.flat_dir.mkdir(parents=True, exist_ok=True)
        with open(self.flat_dir / "metadata.json", "w", encoding="utf-8") as f:
            json.dump({"session_id": "sess-legacy", "pipeline_name": "legacy-pipe"}, f)

    def tearDown(self):
        self.patcher.stop()
        self.temp_dir.cleanup()

    def test_resolve_session_dir_scoped_to_tenant(self):
        # Tenant alpha resolving its own session
        res = _resolve_session_dir("sess-100", tenant_id="tenant-alpha")
        self.assertEqual(res, self.alpha_dir)

        # Tenant beta resolving its own session
        res = _resolve_session_dir("sess-200", tenant_id="tenant-beta")
        self.assertEqual(res, self.beta_dir)

    def test_cross_tenant_isolation(self):
        # Tenant beta trying to resolve tenant alpha's session must resolve to tenant-beta path (which does not exist)
        res = _resolve_session_dir("sess-100", tenant_id="tenant-beta")
        self.assertEqual(res, self.captures_dir / "tenant-beta" / "sess-100")
        self.assertFalse(res.exists())

    def test_unscoped_discovery_fallback(self):
        # Unscoped resolution finds flat session
        res = _resolve_session_dir("sess-legacy", tenant_id=None)
        self.assertEqual(res, self.flat_dir)

        # Unscoped resolution falls back to finding partitioned session
        res = _resolve_session_dir("sess-100", tenant_id=None)
        self.assertEqual(res, self.alpha_dir)

    def test_path_traversal_denied(self):
        with self.assertRaises(HTTPException) as ctx:
            _resolve_session_dir("sess-100", tenant_id="../escape")
        self.assertEqual(ctx.exception.status_code, 400)

        with self.assertRaises(HTTPException) as ctx:
            _resolve_session_dir("../../etc/passwd", tenant_id="tenant-alpha")
        self.assertEqual(ctx.exception.status_code, 400)

        with self.assertRaises(HTTPException) as ctx:
            _resolve_session_dir("sess-100; rm -rf", tenant_id="tenant-alpha")
        self.assertEqual(ctx.exception.status_code, 400)

    def test_read_metadata_tenant_scoped(self):
        meta = _read_session_metadata("sess-100", tenant_id="tenant-alpha")
        self.assertEqual(meta.get("tenant_id"), "tenant-alpha")
        self.assertEqual(meta.get("pipeline_name"), "alpha-pipe")

        # Wrong tenant gets empty metadata because path doesn't exist
        meta_wrong = _read_session_metadata("sess-100", tenant_id="tenant-beta")
        self.assertEqual(meta_wrong, {})

    def test_api_session_detail_tenant_isolation(self):
        client = self.client

        # Requesting tenant-alpha's session with matching header succeeds
        resp = client.get("/api/v1/sessions/sess-100", headers={"x-tenant-id": "tenant-alpha"})
        self.assertEqual(resp.status_code, 200)
        data = resp.json().get("data", {})
        self.assertEqual(data.get("metadata", {}).get("tenant_id"), "tenant-alpha")

        # Requesting tenant-alpha's session with mismatched header returns 404
        resp_mismatch = client.get("/api/v1/sessions/sess-100", headers={"x-tenant-id": "tenant-beta"})
        self.assertEqual(resp_mismatch.status_code, 404)
