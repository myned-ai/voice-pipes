"""Authentication for the Admin BFF's own callers.

The BFF already holds ``VP_ADMIN_TOKEN`` (``core/config.py``) — that is the
credential it presents *to the orchestrator*.  Nothing protected the BFF's own
HTTP/SSE/WebSocket surface, so anyone who could reach port 8000 got session
transcripts and audio, live worker re-configuration and a WebSocket proxy into
the transport.  This module closes that hole with one ASGI middleware.

Configuration (environment):

``ADMIN_UI_TOKEN``
    Bearer token the browser (or any client) must present.  Distinct from
    ``VP_ADMIN_TOKEN`` on purpose: the two credentials have different scopes
    and rotating one must not require rotating the other.
``ADMIN_UI_AUTH_REQUIRED``
    Default ``true``.  When true and ``ADMIN_UI_TOKEN`` is unset the app
    refuses to start (fail closed).  Set to ``false`` only for a loopback dev
    setup; the app then logs a warning and runs open.

The credential is accepted from three carriers, in this order:

1. ``Authorization: Bearer <token>`` header — used by ``fetch``.
2. ``token`` query parameter — the only carrier the browser offers for
   ``EventSource``, ``WebSocket`` and ``<audio src>``, so it is accepted on
   those routes alone (``QUERY_TOKEN_PATHS`` and every WebSocket); a URL
   is written to access logs, and nothing else needs it there.
3. ``vp_admin_ui`` cookie — set by ``POST /api/v1/auth/login`` so ``<audio>``
   elements and SSE work without a token in every URL.  ``HttpOnly``,
   ``SameSite=Lax``; ``Secure`` when the request arrived over HTTPS.

Comparison is constant time (``hmac.compare_digest``).  Paths under
``EXEMPT_PATHS`` (liveness probe, login) are always open.
"""
from __future__ import annotations

import hashlib
import hmac
import logging
import os
import re
import secrets
import time
from urllib.parse import parse_qs

from starlette.types import ASGIApp, Receive, Scope, Send

log = logging.getLogger("admin.auth")

ADMIN_UI_TOKEN: str = os.getenv("ADMIN_UI_TOKEN", "").strip()
ADMIN_UI_AUTH_REQUIRED: bool = os.getenv("ADMIN_UI_AUTH_REQUIRED", "true").strip().lower() not in (
    "0",
    "false",
    "no",
    "off",
)

# Multi-tenant mode (e.g. shared Hugging Face Space): every user authenticates
# via Hugging Face OAuth or an isolated guest session and receives an
# HMAC-signed ``vp_tenant_session`` cookie binding all requests strictly to
# their own ``tenant_id``.
ADMIN_MULTI_TENANT: bool = os.getenv("ADMIN_MULTI_TENANT", "false").strip().lower() in (
    "1",
    "true",
    "yes",
    "on",
)
OAUTH_CLIENT_ID: str = os.getenv("OAUTH_CLIENT_ID", "").strip()
OAUTH_CLIENT_SECRET: str = os.getenv("OAUTH_CLIENT_SECRET", "").strip()
OPENID_PROVIDER_URL: str = os.getenv("OPENID_PROVIDER_URL", "https://huggingface.co").strip().rstrip("/")
OAUTH_SCOPES: str = os.getenv("OAUTH_SCOPES", "openid profile").strip() or "openid profile"
SPACE_HOST: str = os.getenv("SPACE_HOST", "").strip()

_ALLOW_GUEST_RAW = os.getenv("ADMIN_ALLOW_GUEST", "").strip().lower()
ADMIN_ALLOW_GUEST: bool = (
    _ALLOW_GUEST_RAW in ("1", "true", "yes", "on")
    if _ALLOW_GUEST_RAW
    else not bool(OAUTH_CLIENT_ID and OAUTH_CLIENT_SECRET)
)
ADMIN_SESSION_TTL_SECONDS: int = max(300, int(os.getenv("ADMIN_SESSION_TTL_SECONDS", "43200")))

_SESSION_SECRET: str = (
    os.getenv("ADMIN_SESSION_SECRET", "").strip()
    or OAUTH_CLIENT_SECRET
    or ADMIN_UI_TOKEN
    or secrets.token_hex(32)
)

COOKIE_NAME = "vp_admin_ui"
TENANT_COOKIE_NAME = "vp_tenant_session"
OAUTH_STATE_COOKIE_NAME = "vp_oauth_state"

_VALID_TENANT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")

# Paths that never require a credential.  Keep this list short: a liveness
# probe for the container orchestrator and the login/check endpoints the UI
# uses before it has a token.
EXEMPT_PATHS: frozenset[str] = frozenset(
    {
        "/livez",
        "/api/v1/auth/login",
        "/api/v1/auth/check",
        "/api/v1/auth/guest",
        "/api/v1/auth/hf/login",
        "/api/v1/auth/hf/callback",
    }
)

# HTTP routes that accept the token as a ``token`` query parameter: the SSE
# streams and the audio files an ``<audio>`` element plays. WebSocket
# upgrades accept it too (``query_token_allowed``). Every other route takes
# the header or the cookie only.
QUERY_TOKEN_PATHS: tuple[re.Pattern[str], ...] = (
    re.compile(r"^/api/v1/stream/[^/]+$"),
    re.compile(r"^/api/v1/sessions/[^/]+/audio/.+$"),
    re.compile(r"^/api/v1/workers/[^/]+/probe/audio/[^/]+$"),
    re.compile(
        r"^/api/v1/benchmark/experiments/[^/]+/variants/[^/]+/scenarios/[^/]+/entries/[^/]+/audio$"
    ),
)


class AuthConfigError(RuntimeError):
    """Raised at import when auth is required but no token is configured."""


def hf_oauth_enabled() -> bool:
    return bool(OAUTH_CLIENT_ID and OAUTH_CLIENT_SECRET)


def auth_mode() -> str:
    if ADMIN_MULTI_TENANT:
        return "hf_oauth" if hf_oauth_enabled() else "guest"
    return "token"


def sanitize_tenant_id(raw: str) -> str:
    """Convert any username / subject into a path-safe ``TenantId`` matching
    ``^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$``."""
    cleaned = re.sub(r"[^A-Za-z0-9_-]+", "-", raw.strip()).lstrip("_-")[:64]
    if cleaned and _VALID_TENANT_RE.fullmatch(cleaned):
        return cleaned
    digest = hashlib.sha256(raw.encode("utf-8")).hexdigest()[:16]
    return f"u-{digest}"


def _hmac_sig(payload: str) -> str:
    return hmac.new(
        _SESSION_SECRET.encode("utf-8"),
        payload.encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()


def sign_tenant_cookie(tenant_id: str, ttl_s: int | None = None) -> str:
    safe_tid = sanitize_tenant_id(tenant_id)
    exp = int(time.time()) + (ttl_s if ttl_s is not None else ADMIN_SESSION_TTL_SECONDS)
    payload = f"{safe_tid}.{exp}"
    return f"{payload}.{_hmac_sig(payload)}"


def verify_tenant_cookie(value: str | None) -> str | None:
    if not value:
        return None
    parts = value.split(".")
    if len(parts) != 3:
        return None
    tenant_id, exp_str, sig = parts
    if not _VALID_TENANT_RE.fullmatch(tenant_id):
        return None
    try:
        exp = int(exp_str)
    except ValueError:
        return None
    if exp < int(time.time()):
        return None
    expected = _hmac_sig(f"{tenant_id}.{exp_str}")
    if not hmac.compare_digest(sig, expected):
        return None
    return tenant_id


def validate_startup_config() -> None:
    """Fail closed: required + unset token is a configuration error."""
    if ADMIN_MULTI_TENANT:
        log.info(
            "admin BFF running in MULTI-TENANT mode (auth_mode=%s, allow_guest=%s)",
            auth_mode(),
            ADMIN_ALLOW_GUEST,
        )
        return
    if ADMIN_UI_AUTH_REQUIRED and not ADMIN_UI_TOKEN:
        raise AuthConfigError(
            "ADMIN_UI_TOKEN is not set but ADMIN_UI_AUTH_REQUIRED=true. "
            "Set ADMIN_UI_TOKEN to a random secret (e.g. `openssl rand -hex 32`) "
            "or, for a loopback-only dev setup, set ADMIN_UI_AUTH_REQUIRED=false."
        )
    if not ADMIN_UI_TOKEN:
        log.warning(
            "admin BFF auth is DISABLED (ADMIN_UI_AUTH_REQUIRED=false, no ADMIN_UI_TOKEN); "
            "every route is reachable without credentials"
        )


def auth_enabled() -> bool:
    return ADMIN_MULTI_TENANT or bool(ADMIN_UI_TOKEN)


def token_matches(candidate: str | None) -> bool:
    if not candidate or not ADMIN_UI_TOKEN:
        return False
    return hmac.compare_digest(candidate.encode("utf-8"), ADMIN_UI_TOKEN.encode("utf-8"))


def _header(scope: Scope, name: bytes) -> str | None:
    for k, v in scope.get("headers", ()):
        if k.lower() == name:
            return v.decode("latin-1")
    return None


def _bearer_from_scope(scope: Scope) -> str | None:
    auth = _header(scope, b"authorization")
    if auth and auth[:7].lower() == "bearer ":
        return auth[7:].strip()
    return None


def _query_token(scope: Scope) -> str | None:
    raw = scope.get("query_string", b"")
    if not raw:
        return None
    vals = parse_qs(raw.decode("latin-1")).get("token")
    return vals[0] if vals else None


def _cookie_value(scope: Scope, name: str) -> str | None:
    cookie = _header(scope, b"cookie")
    if not cookie:
        return None
    for part in cookie.split(";"):
        k, _, v = part.strip().partition("=")
        if k == name:
            return v
    return None


def _cookie_token(scope: Scope) -> str | None:
    return _cookie_value(scope, COOKIE_NAME)


def tenant_from_scope(scope: Scope) -> str | None:
    """Return the verified tenant_id from the signed session cookie, if any."""
    return verify_tenant_cookie(_cookie_value(scope, TENANT_COOKIE_NAME))


def resolve_request_tenant(request, query_tenant: str | None = None) -> str | None:
    """Authoritative tenant resolution for HTTP and WebSocket endpoints.

    When ``AdminAuthMiddleware`` verified a signed multi-tenant session cookie
    and bound ``request.state.tenant_id``, that verified tenant ALWAYS wins —
    client-supplied headers or query parameters can never spoof another user.
    """
    if request is not None and hasattr(request, "state"):
        bound = getattr(request.state, "tenant_id", None)
        if isinstance(bound, str) and bound:
            return bound
    if ADMIN_MULTI_TENANT:
        if request is not None and hasattr(request, "scope"):
            return tenant_from_scope(request.scope)
        return None
    if isinstance(query_tenant, str):
        return query_tenant
    if request is not None and hasattr(request, "headers"):
        return request.headers.get("x-tenant-id")
    return None


def require_operator(request) -> None:
    """Reject global operator/mutation endpoints for non-admin users in multi-tenant mode."""
    import sys

    live_auth = sys.modules.get("app.core.auth")
    multi_tenant = getattr(live_auth, "ADMIN_MULTI_TENANT", ADMIN_MULTI_TENANT)
    if not multi_tenant:
        return
    if request is not None and hasattr(request, "scope"):
        if token_matches(credential_from_scope(request.scope)):
            return
    from fastapi import HTTPException

    raise HTTPException(
        status_code=403,
        detail="Operator action is disabled in multi-tenant mode",
    )


def query_token_allowed(scope: Scope) -> bool:
    """Whether this request may carry the token in its URL.

    Only a WebSocket, an SSE stream or an audio file: what ``WebSocket``,
    ``EventSource`` and ``<audio src>`` request cannot carry a header. On any
    other route a ``?token=`` is ignored, so a ``curl`` user who puts the
    token in a URL is refused rather than having it written to access logs
    on every call.
    """
    if scope.get("type") == "websocket":
        return True
    path = scope.get("path", "")
    return any(p.match(path) for p in QUERY_TOKEN_PATHS)


def credential_from_scope(scope: Scope) -> str | None:
    """First credential found, header > query (where allowed) > cookie."""
    query = _query_token(scope) if query_token_allowed(scope) else None
    return _bearer_from_scope(scope) or query or _cookie_token(scope)


def is_authorized(scope: Scope) -> bool:
    if not auth_enabled():
        return True
    if scope.get("path", "") in EXEMPT_PATHS:
        return True
    if ADMIN_MULTI_TENANT:
        tid = tenant_from_scope(scope)
        if tid is not None:
            scope.setdefault("state", {})["tenant_id"] = tid
            return True
        return False
    return token_matches(credential_from_scope(scope))


_UNAUTHORIZED_BODY = b'{"data":null,"error":{"code":"UNAUTHORIZED","message":"missing or invalid ADMIN_UI_TOKEN"}}'


class AdminAuthMiddleware:
    """Pure ASGI middleware so HTTP, SSE and WebSocket share one gate.

    A rejected WebSocket is closed with 4401 *before* accept, so the client
    sees a failed handshake rather than an open-then-closed socket.
    """

    def __init__(self, app: ASGIApp) -> None:
        self.app = app

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        kind = scope.get("type")
        if kind not in ("http", "websocket") or is_authorized(scope):
            await self.app(scope, receive, send)
            return

        if kind == "websocket":
            await send({"type": "websocket.close", "code": 4401, "reason": "unauthorized"})
            return

        # CORS preflights carry no credentials by design; let CORSMiddleware
        # (outer) answer them.  Any other unauthenticated request is refused.
        if scope.get("method") == "OPTIONS":
            await self.app(scope, receive, send)
            return

        await send(
            {
                "type": "http.response.start",
                "status": 401,
                "headers": [
                    (b"content-type", b"application/json"),
                    (b"www-authenticate", b'Bearer realm="voice-pipes-admin"'),
                    (b"cache-control", b"no-store"),
                ],
            }
        )
        await send({"type": "http.response.body", "body": _UNAUTHORIZED_BODY})
