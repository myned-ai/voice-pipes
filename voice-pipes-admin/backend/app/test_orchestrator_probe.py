"""The startup probe must name why the fleet will look empty.

An empty worker list in the console is indistinguishable from a healthy
orchestrator with nothing registered, so a rejected credential or an
unreachable orchestrator renders as "no workers" and says nothing. These
pin the three cases apart.
"""

import asyncio
import unittest
from unittest import mock

import httpx

from app.core import config as cfg


def _probe_with(response=None, exc=None):
    class _Client:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *_):
            return False

        async def get(self, *_a, **_k):
            if exc is not None:
                raise exc
            return response

    with mock.patch.object(cfg, "orch_client", lambda **_: _Client()):
        return asyncio.run(cfg.probe_orchestrator())


def _resp(status):
    return httpx.Response(status, request=httpx.Request("GET", "http://x/"))


class OrchestratorProbeTest(unittest.TestCase):
    def test_reachable_and_authorised_is_ok(self):
        self.assertEqual(_probe_with(_resp(200)), "ok")

    def test_rejected_credential_is_reported_as_unauthorized(self):
        self.assertEqual(_probe_with(_resp(401)), "unauthorized")
        self.assertEqual(_probe_with(_resp(403)), "unauthorized")

    def test_unreachable_orchestrator_is_reported(self):
        self.assertEqual(
            _probe_with(exc=httpx.ConnectError("refused")), "unreachable"
        )

    def test_other_errors_are_not_silently_ok(self):
        self.assertEqual(_probe_with(_resp(500)), "http_500")

    def test_a_probe_failure_never_raises(self):
        # The console must still serve captures when the orchestrator is down.
        self.assertEqual(_probe_with(exc=RuntimeError("boom")), "unreachable")


if __name__ == "__main__":
    unittest.main()
