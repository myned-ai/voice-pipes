"""What a failed orchestrator call tells the browser.

An ``httpx`` exception's text names the URL it was fetching (``Server error
'500 ...' for url 'http://orchestrator:9090/api/v1/...'``) and, for a failed
connect, often the host and port. That is internal topology, so it stays in
this process's log; the browser and the SSE wire get a fixed code, the HTTP
status when there was one, and a message built from those two alone.
"""
from __future__ import annotations

import logging
from urllib.parse import quote

import httpx

log = logging.getLogger("admin.upstream")


def upstream_error(exc: BaseException) -> dict:
    """The ``error`` member of the response envelope for a failed proxy call.

    ``code`` is ``ORCHESTRATOR_ERROR`` when the orchestrator answered with an
    error status (``status`` carries it) and ``ORCHESTRATOR_UNREACHABLE`` for
    everything else: refused or timed-out connections and bodies that would
    not parse. The exception itself is logged here, with its URL, for the
    operator.
    """
    log.warning("orchestrator call failed: %s: %s", type(exc).__name__, exc)
    if isinstance(exc, httpx.HTTPStatusError):
        status = exc.response.status_code
        return {
            "code": "ORCHESTRATOR_ERROR",
            "status": status,
            "message": f"the orchestrator answered HTTP {status}",
        }
    if isinstance(exc, httpx.TimeoutException):
        message = "the orchestrator did not answer in time"
    elif isinstance(exc, httpx.TransportError):
        message = "the orchestrator could not be reached"
    else:
        message = "the orchestrator's answer could not be read"
    return {"code": "ORCHESTRATOR_UNREACHABLE", "status": None, "message": message}


def _segment(value: object) -> str:
    """One path segment, percent-encoded so it stays one segment.

    ``quote(.., safe="")`` encodes ``/``, ``?`` and ``#``, which would
    otherwise end the segment, start a query or start a fragment. A bare
    ``.`` or ``..`` survives quoting and would be resolved as a dot segment
    by the client, so its dots are encoded too.
    """
    encoded = quote(str(value), safe="")
    if encoded in (".", ".."):
        encoded = encoded.replace(".", "%2E")
    return encoded


def upstream_path(template: str, **params: object) -> str:
    """``template`` with each ``{name}`` replaced by its encoded parameter.

    Path parameters arrive decoded from the browser's URL, so a worker id of
    ``a?b`` interpolated as it is would reach the orchestrator as worker
    ``a`` with a query string ``b``.
    """
    return template.format(**{k: _segment(v) for k, v in params.items()})
