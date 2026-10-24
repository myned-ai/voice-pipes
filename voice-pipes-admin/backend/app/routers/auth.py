"""Login / check / logout for the BFF's own credential.

``login`` exists so the browser can obtain the ``vp_admin_ui`` cookie: axios
sends the bearer header itself, but ``EventSource`` and ``<audio src>``
cannot set headers, and putting the token in every media URL would leak it
into access logs.  The cookie carries the same value; the middleware
(``core/auth.py``) accepts either.
"""
from __future__ import annotations

from fastapi import APIRouter, Request, Response
from pydantic import BaseModel

from ..core import auth as _auth

router = APIRouter()


class LoginBody(BaseModel):
    token: str


def _cookie_secure(request: Request) -> bool:
    proto = request.headers.get("x-forwarded-proto", request.url.scheme)
    return proto.split(",")[0].strip().lower() == "https"


@router.get("/livez")
async def livez():
    """Liveness probe. Exempt from auth on purpose; says nothing about the
    orchestrator (that is ``/api/v1/health``, which is protected)."""
    return {"data": {"ok": True}, "error": None}


@router.get("/api/v1/auth/check")
async def auth_check(request: Request):
    """Tell the UI whether it needs a token and whether the one it has works.

    Exempt from the middleware so an unauthenticated browser can ask; the
    answer reveals only the boolean state, never the token.
    """
    presented = _auth.credential_from_scope(request.scope)
    authenticated = (not _auth.auth_enabled()) or _auth.token_matches(presented)
    # The worker credential (VP_CLIENT_TOKEN) is not part of the answer for
    # anyone: the Playground reaches workers through /api/v1/playground/ws,
    # which presents it upstream itself.
    data: dict = {
        "auth_required": _auth.auth_enabled(),
        "authenticated": authenticated,
    }
    return {"data": data, "error": None}


@router.post("/api/v1/auth/login")
async def auth_login(body: LoginBody, request: Request, response: Response):
    if _auth.auth_enabled() and not _auth.token_matches(body.token):
        response.status_code = 401
        return {"data": None, "error": {"code": "UNAUTHORIZED", "message": "invalid token"}}
    if _auth.auth_enabled():
        response.set_cookie(
            key=_auth.COOKIE_NAME,
            value=body.token,
            httponly=True,
            samesite="lax",
            secure=_cookie_secure(request),
            path="/",
        )
    return {"data": {"ok": True}, "error": None}


@router.post("/api/v1/auth/logout")
async def auth_logout(response: Response):
    response.delete_cookie(key=_auth.COOKIE_NAME, path="/")
    return {"data": {"ok": True}, "error": None}
