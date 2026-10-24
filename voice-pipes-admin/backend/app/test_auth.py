"""The BFF's own auth gate.

Runs the real app with ``ADMIN_UI_TOKEN`` set, then walks every registered
route and asserts it is closed without a credential and open with one.  Also
covers the three carriers (header, query, cookie), the WebSocket proxy, the
fail-closed startup rule, and constant-time comparison.
"""
from __future__ import annotations

import importlib
import os
import sys
import unittest
from unittest import mock

TOKEN = "test-admin-ui-token-0123456789abcdef"


def _fresh_app(env: dict[str, str]):
    """Import ``app.main`` under ``env`` with the auth module re-evaluated."""
    for mod in list(sys.modules):
        if mod == "app.main" or mod.startswith(("app.core.auth", "app.routers.auth")):
            sys.modules.pop(mod, None)
    # `from .core import auth` resolves through the package attribute first,
    # so the stale module object must go from the package too.
    for pkg, attr in (("app.core", "auth"), ("app.routers", "auth")):
        if pkg in sys.modules and hasattr(sys.modules[pkg], attr):
            delattr(sys.modules[pkg], attr)
    with mock.patch.dict(os.environ, env, clear=False):
        for k in ("ADMIN_UI_TOKEN", "ADMIN_UI_AUTH_REQUIRED"):
            if k not in env:
                os.environ.pop(k, None)
        importlib.import_module("app.core.auth")
        return importlib.import_module("app.main")


class AuthGateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient

        cls.main = _fresh_app({"ADMIN_UI_TOKEN": TOKEN, "ADMIN_UI_AUTH_REQUIRED": "true"})
        cls.client = TestClient(cls.main.app)
        # The debug_paths gate is read per request, so the flag must
        # outlive _fresh_app's patch.dict; it doubles as the cheap local probe.
        os.environ["ADMIN_DEBUG"] = "1"

    @classmethod
    def tearDownClass(cls):
        os.environ.pop("ADMIN_DEBUG", None)

    def _http_routes(self):
        """Every HTTP route as (path, methods), from the OpenAPI schema.

        The schema is public and lists each route with its router prefix
        applied. `app.routes` stopped doing that in FastAPI 0.120, which
        keeps one wrapper per included router instead of the routes. A route
        declared with `include_in_schema=False` is not in the schema, so it
        needs its own test here.
        """
        for path, operations in self.main.app.openapi()["paths"].items():
            yield path, {method.upper() for method in operations}

    def test_every_http_route_is_closed_without_a_token(self):
        from app.core.auth import EXEMPT_PATHS

        checked = 0
        for path, methods in self._http_routes():
            if path in EXEMPT_PATHS or "{" in path:
                continue
            for method in methods - {"HEAD", "OPTIONS"}:
                resp = self.client.request(method, path)
                self.assertEqual(
                    resp.status_code, 401, f"{method} {path} answered {resp.status_code} without a token"
                )
                self.assertEqual(resp.json()["error"]["code"], "UNAUTHORIZED")
                self.assertIn("www-authenticate", {k.lower() for k in resp.headers})
                checked += 1
        self.assertGreater(checked, 10, "route walk found suspiciously few routes")

    def test_parameterised_routes_are_closed_without_a_token(self):
        for path in (
            "/api/v1/sessions/abc",
            "/api/v1/sessions/abc/audio/1_mic_raw.wav",
            "/api/v1/workers/w1",
        ):
            self.assertEqual(self.client.get(path).status_code, 401, path)
        self.assertEqual(
            self.client.patch("/api/v1/workers/w1/params", json={"x": 1}).status_code, 401
        )

    def test_bearer_header_opens_the_gate(self):
        resp = self.client.get("/api/v1/debug_paths", headers={"Authorization": f"Bearer {TOKEN}"})
        self.assertEqual(resp.status_code, 200)

    def test_query_param_is_refused_on_an_ordinary_route(self):
        # A token in a URL ends up in access logs; only the routes a browser
        # cannot send a header to take it there.
        resp = self.client.get(f"/api/v1/debug_paths?token={TOKEN}")
        self.assertEqual(resp.status_code, 401)
        resp = self.client.get(f"/api/v1/sessions?token={TOKEN}")
        self.assertEqual(resp.status_code, 401)

    def test_query_param_opens_the_sse_stream_audio_and_websocket_routes(self):
        from app.core.auth import is_authorized

        def scope(path: str, kind: str = "http") -> dict:
            return {"type": kind, "path": path, "query_string": f"token={TOKEN}".encode(), "headers": []}

        for path in (
            "/api/v1/stream/fleet",
            "/api/v1/sessions/abc/audio/1_mic_raw.wav",
            "/api/v1/sessions/abc/audio/turn/1/tts_concat.wav",
            "/api/v1/workers/w1/probe/audio/probe-tts-1.wav",
            "/api/v1/benchmark/experiments/e/variants/v/scenarios/s/entries/n/audio",
        ):
            self.assertTrue(is_authorized(scope(path)), path)
        self.assertTrue(is_authorized(scope("/api/v1/playground/ws", "websocket")))
        self.assertFalse(is_authorized(scope("/api/v1/debug_paths")))
        self.assertFalse(is_authorized(scope("/api/v1/sessions/abc")))

        # Through the app: the audio route answers past the gate (404, no
        # capture) where the plain route answers 401. (The SSE stream never
        # ends, and the test client buffers a whole response, so it is
        # checked at the gate above.)
        resp = self.client.get(f"/api/v1/sessions/abc/audio/1_mic_raw.wav?token={TOKEN}")
        self.assertNotEqual(resp.status_code, 401)

    def test_login_sets_cookie_and_cookie_opens_the_gate(self):
        from app.core.auth import COOKIE_NAME

        bad = self.client.post("/api/v1/auth/login", json={"token": "nope"})
        self.assertEqual(bad.status_code, 401)
        self.assertNotIn(COOKIE_NAME, bad.cookies)

        ok = self.client.post("/api/v1/auth/login", json={"token": TOKEN})
        self.assertEqual(ok.status_code, 200)
        self.assertEqual(ok.cookies.get(COOKIE_NAME), TOKEN)
        set_cookie = ok.headers.get("set-cookie", "")
        self.assertIn("HttpOnly", set_cookie)
        self.assertIn("SameSite=lax", set_cookie)

        resp = self.client.get("/api/v1/debug_paths", cookies={COOKIE_NAME: TOKEN})
        self.assertEqual(resp.status_code, 200)

        out = self.client.post("/api/v1/auth/logout")
        self.assertEqual(out.status_code, 200)
        self.assertIn(f"{COOKIE_NAME}=", out.headers.get("set-cookie", ""))

    def test_wrong_token_is_rejected_on_every_carrier(self):
        from app.core.auth import COOKIE_NAME

        self.assertEqual(
            self.client.get("/api/v1/debug_paths", headers={"Authorization": "Bearer wrong"}).status_code, 401
        )
        self.assertEqual(self.client.get("/api/v1/debug_paths?token=wrong").status_code, 401)
        self.assertEqual(
            self.client.get("/api/v1/debug_paths", cookies={COOKIE_NAME: "wrong"}).status_code, 401
        )

    def test_check_reports_state_without_leaking_token(self):
        anon = self.client.get("/api/v1/auth/check").json()["data"]
        self.assertEqual(anon, {"auth_required": True, "authenticated": False})
        good = self.client.get(
            "/api/v1/auth/check", headers={"Authorization": f"Bearer {TOKEN}"}
        ).json()["data"]
        self.assertEqual(good, {"auth_required": True, "authenticated": True})
        self.assertNotIn(TOKEN, self.client.get("/api/v1/auth/check").text)

    def test_the_worker_token_is_never_handed_out(self):
        # The Playground goes through the backend's proxy, which presents
        # VP_CLIENT_TOKEN itself; no caller, signed in or not, receives it.
        from app.core import config as cfg

        with mock.patch.object(cfg, "CLIENT_TOKEN", "worker-secret"):
            anon = self.client.get("/api/v1/auth/check")
            good = self.client.get("/api/v1/auth/check", headers={"Authorization": f"Bearer {TOKEN}"})
        self.assertEqual(good.json()["data"], {"auth_required": True, "authenticated": True})
        for resp in (anon, good):
            self.assertNotIn("worker-secret", resp.text)
            self.assertNotIn("client_token", resp.json()["data"])

    def test_debug_paths_is_hidden_without_admin_debug(self):
        # Internal hostnames/paths are not served unless ADMIN_DEBUG=1
        # (the suite runs with it set so debug_paths can double as a probe).
        with mock.patch.dict(os.environ, {"ADMIN_DEBUG": "0"}):
            resp = self.client.get("/api/v1/debug_paths", headers={"Authorization": f"Bearer {TOKEN}"})
        self.assertEqual(resp.status_code, 404)

    def test_livez_is_open(self):
        self.assertEqual(self.client.get("/livez").status_code, 200)

    def test_websocket_proxy_is_closed_without_a_token(self):
        from starlette.websockets import WebSocketDisconnect

        with self.assertRaises(WebSocketDisconnect) as ctx:
            with self.client.websocket_connect("/api/v1/playground/ws"):
                pass
        self.assertEqual(ctx.exception.code, 4401)

    def test_cors_preflight_is_answered_without_a_token(self):
        resp = self.client.options(
            "/api/v1/sessions",
            headers={
                "Origin": "http://localhost:3000",
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Headers": "authorization",
            },
        )
        self.assertEqual(resp.status_code, 200)
        self.assertEqual(resp.headers.get("access-control-allow-origin"), "http://localhost:3000")

    def test_comparison_is_constant_time(self):
        import inspect

        from app.core import auth

        self.assertIn("compare_digest", inspect.getsource(auth.token_matches))


class StartupConfigTests(unittest.TestCase):
    def tearDown(self):
        # Leave the module state as the other tests expect it.
        _fresh_app({"ADMIN_UI_TOKEN": TOKEN, "ADMIN_UI_AUTH_REQUIRED": "true"})

    def test_required_and_unset_refuses_to_start(self):
        with self.assertRaises(Exception) as ctx:
            _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "true"})
        self.assertIn("ADMIN_UI_TOKEN", str(ctx.exception))

    def test_required_defaults_to_true(self):
        # AuthConfigError is re-created with each fresh import; its base is stable.
        with self.assertRaises(RuntimeError):
            _fresh_app({})

    def test_not_required_runs_open(self):
        from fastapi.testclient import TestClient

        main = _fresh_app({"ADMIN_UI_AUTH_REQUIRED": "false"})
        client = TestClient(main.app)
        with mock.patch.dict(os.environ, {"ADMIN_DEBUG": "1"}):
            self.assertEqual(client.get("/api/v1/debug_paths").status_code, 200)
        self.assertEqual(
            client.get("/api/v1/auth/check").json()["data"],
            {"auth_required": False, "authenticated": True},
        )


if __name__ == "__main__":
    unittest.main()
