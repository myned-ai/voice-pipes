"""A probe that cannot be relayed must say which worker and why.

A worker built before the probe endpoints existed answers them with a bare
404. Parsed as JSON that raised "Expecting value: line 1 column 1 (char 0)",
which the console showed as it was: true, and no use to anyone deciding what
to do next.
"""

import unittest

import httpx
from fastapi import HTTPException

from app.routers.health import _probe_payload


def _resp(status, content=b"", content_type="application/json"):
    return httpx.Response(
        status,
        content=content,
        headers={"content-type": content_type},
        request=httpx.Request("GET", "http://x/"),
    )


class ProbePayloadTest(unittest.TestCase):
    def test_json_is_returned_whatever_the_status(self):
        body = b'{"data": null, "error": {"code": "BUSY", "message": "one at a time"}}'
        self.assertEqual(_probe_payload("w", _resp(429, body))["error"]["code"], "BUSY")

    def test_bare_404_names_the_worker_and_the_remedy(self):
        with self.assertRaises(HTTPException) as caught:
            _probe_payload("worker-old", _resp(404))
        self.assertEqual(caught.exception.status_code, 501)
        error = caught.exception.detail["error"]
        self.assertEqual(error["code"], "PROBE_UNSUPPORTED")
        self.assertIn("worker-old", error["message"])
        self.assertIn("Rebuild", error["message"])
        self.assertNotIn("Expecting value", error["message"])

    def test_other_non_json_is_a_gateway_error_that_says_so(self):
        with self.assertRaises(HTTPException) as caught:
            _probe_payload("w", _resp(500, b"<html>oops</html>", "text/html"))
        self.assertEqual(caught.exception.status_code, 502)
        self.assertIn("not JSON", caught.exception.detail["error"]["message"])


if __name__ == "__main__":
    unittest.main()
