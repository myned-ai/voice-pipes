"""Login / check / logout for the BFF's own credential.

``login`` exists so the browser can obtain the ``vp_admin_ui`` cookie: axios
sends the bearer header itself, but ``EventSource`` and ``<audio src>``
cannot set headers, and putting the token in every media URL would leak it
into access logs.  The cookie carries the same value; the middleware
(``core/auth.py``) accepts either.
"""
from __future__ import annotations

import logging
import secrets
from urllib.parse import urlencode

import httpx
from fastapi import APIRouter, HTTPException, Query, Request, Response
from fastapi.responses import HTMLResponse, RedirectResponse
from pydantic import BaseModel

from ..core import auth as _auth
from ..core import features as _features

router = APIRouter()
log = logging.getLogger("admin.auth")


class LoginBody(BaseModel):
    token: str


def _cookie_secure(request: Request) -> bool:
    proto = request.headers.get("x-forwarded-proto", request.url.scheme)
    return proto.split(",")[0].strip().lower() == "https"


def _cookie_samesite(request: Request) -> str:
    # Hugging Face Spaces embeds the app in a cross-site iframe on huggingface.co.
    # Browsers only send cookies inside a cross-site iframe when SameSite=None; Secure.
    return "none" if _cookie_secure(request) else "lax"


def _hf_redirect_uri(request: Request) -> str:
    if _auth.SPACE_HOST:
        return f"https://{_auth.SPACE_HOST}/api/v1/auth/hf/callback"
    scheme = "https" if _cookie_secure(request) else request.url.scheme
    host = request.headers.get("x-forwarded-host") or request.headers.get("host") or request.url.netloc
    return f"{scheme}://{host}/api/v1/auth/hf/callback"


def _set_tenant_cookie(response: Response, request: Request, tenant_id: str) -> None:
    response.set_cookie(
        key=_auth.TENANT_COOKIE_NAME,
        value=_auth.sign_tenant_cookie(tenant_id),
        httponly=True,
        samesite=_cookie_samesite(request),
        secure=_cookie_secure(request),
        max_age=_auth.ADMIN_SESSION_TTL_SECONDS,
        path="/",
    )


@router.get("/livez")
async def livez():
    """Liveness probe. Exempt from auth on purpose; says nothing about the
    orchestrator (that is ``/api/v1/health``, which is protected)."""
    return {"data": {"ok": True}, "error": None}


@router.get("/api/v1/auth/check")
async def auth_check(request: Request):
    """Tell the UI whether it needs auth and whether the caller is authenticated.

    Exempt from the middleware so an unauthenticated browser can ask; the
    answer reveals only the boolean state and (in multi-tenant mode) the
    caller's own tenant identifier, never any secret token.
    """
    if _auth.ADMIN_MULTI_TENANT:
        tenant_id = _auth.tenant_from_scope(request.scope)
        data: dict = {
            "auth_required": True,
            "authenticated": tenant_id is not None,
            "auth_mode": _auth.auth_mode(),
            "allow_guest": _auth.ADMIN_ALLOW_GUEST,
            "tenant_id": tenant_id,
        }
        if _auth.hf_oauth_enabled():
            data["hf_oauth_url"] = "/api/v1/auth/hf/login"
        return {"data": data, "error": None}

    presented = _auth.credential_from_scope(request.scope)
    authenticated = (not _auth.auth_enabled()) or _auth.token_matches(presented)
    # The worker credential (VP_CLIENT_TOKEN) is not part of the answer for
    # anyone: the Playground reaches workers through /api/v1/playground/ws,
    # which presents it upstream itself.
    data = {
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


@router.post("/api/v1/auth/guest")
async def auth_guest(request: Request, response: Response):
    """Issue an isolated ephemeral tenant cookie for a browser session."""
    if not _auth.ADMIN_MULTI_TENANT or not _auth.ADMIN_ALLOW_GUEST:
        raise HTTPException(status_code=403, detail="Guest sessions are disabled")
    existing = _auth.tenant_from_scope(request.scope)
    tenant_id = existing or f"u-{secrets.token_hex(6)}"
    _set_tenant_cookie(response, request, tenant_id)
    return {"data": {"ok": True, "tenant_id": tenant_id}, "error": None}


_POPUP_SUCCESS_HTML = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Signed in — Voice-Pipes</title>
  <style>
    body {
      margin: 0; font-family: system-ui, -apple-system, sans-serif;
      background: #020617; color: #f8fafc;
      display: grid; place-items: center; min-height: 100vh; padding: 1.5rem;
    }
    .card {
      max-width: 24rem; background: #0f172a; border: 1px solid #1e293b;
      border-radius: 0.75rem; padding: 1.5rem; text-align: center;
    }
    h1 { font-size: 1.125rem; margin: 0 0 0.5rem; }
    p { font-size: 0.875rem; color: #cbd5e1; margin: 0 0 1rem; line-height: 1.5; }
    a {
      display: inline-block; padding: 0.5rem 0.9rem; border-radius: 0.375rem;
      background: #4f46e5; color: #fff; text-decoration: none; font-size: 0.875rem;
    }
  </style>
</head>
<body>
  <div class="card">
    <h1>Signed in to Voice-Pipes</h1>
    <p>You can close this window and return to the Space.</p>
    <a href="/">Continue in this tab</a>
  </div>
  <script>
    try {
      const ch = new BroadcastChannel("vp-admin-auth");
      ch.postMessage("signed-in");
      ch.close();
    } catch (_) {}
    window.close();
  </script>
</body>
</html>"""


@router.get("/api/v1/auth/hf/login")
async def auth_hf_login(request: Request, popup: bool = Query(False)):
    """Start Hugging Face OpenID Connect / OAuth2 sign-in flow."""
    if not _auth.ADMIN_MULTI_TENANT or not _auth.hf_oauth_enabled():
        raise HTTPException(status_code=404, detail="Hugging Face OAuth is not configured")
    state = secrets.token_urlsafe(24)
    params = urlencode(
        {
            "client_id": _auth.OAUTH_CLIENT_ID,
            "redirect_uri": _hf_redirect_uri(request),
            "response_type": "code",
            "scope": _auth.OAUTH_SCOPES,
            "state": state,
        }
    )
    authorize_url = f"{_auth.OPENID_PROVIDER_URL}/oauth/authorize?{params}"
    resp = RedirectResponse(url=authorize_url, status_code=302)
    state_cookie = f"1:{state}" if popup else state
    resp.set_cookie(
        key=_auth.OAUTH_STATE_COOKIE_NAME,
        value=state_cookie,
        httponly=True,
        samesite=_cookie_samesite(request),
        secure=_cookie_secure(request),
        max_age=600,
        path="/",
    )
    return resp


@router.get("/api/v1/auth/hf/callback")
async def auth_hf_callback(
    request: Request,
    code: str | None = Query(None),
    state: str | None = Query(None),
    error: str | None = Query(None),
):
    """Complete Hugging Face OAuth2 sign-in and bind the user's HF subject ID as tenant_id."""
    if not _auth.ADMIN_MULTI_TENANT or not _auth.hf_oauth_enabled():
        raise HTTPException(status_code=404, detail="Hugging Face OAuth is not configured")
    raw_state_cookie = request.cookies.get(_auth.OAUTH_STATE_COOKIE_NAME) or ""
    is_popup = raw_state_cookie.startswith("1:")
    expected_state = raw_state_cookie[2:] if is_popup else raw_state_cookie
    if error:
        log.warning("HF OAuth returned error: %s", error)
        if is_popup:
            return HTMLResponse(
                "<!doctype html><title>Sign-in cancelled</title>"
                "<p>Sign-in was cancelled. You can close this window.</p>"
                "<script>window.close();</script>",
                status_code=200,
            )
        return RedirectResponse(url="/?auth_error=oauth_denied", status_code=302)
    if not code or not state or not expected_state or not secrets.compare_digest(state, expected_state):
        raise HTTPException(status_code=400, detail="Invalid OAuth state")

    token_url = f"{_auth.OPENID_PROVIDER_URL}/oauth/token"
    userinfo_url = f"{_auth.OPENID_PROVIDER_URL}/oauth/userinfo"

    try:
        async with httpx.AsyncClient(timeout=10.0) as client:
            tok_resp = await client.post(
                token_url,
                data={
                    "grant_type": "authorization_code",
                    "code": code,
                    "redirect_uri": _hf_redirect_uri(request),
                    "client_id": _auth.OAUTH_CLIENT_ID,
                    "client_secret": _auth.OAUTH_CLIENT_SECRET,
                },
                headers={"Accept": "application/json"},
            )
            tok_resp.raise_for_status()
            access_token = (tok_resp.json() or {}).get("access_token")
            if not access_token:
                raise HTTPException(status_code=502, detail="OAuth token response missing access_token")

            ui_resp = await client.get(
                userinfo_url,
                headers={"Authorization": f"Bearer {access_token}", "Accept": "application/json"},
            )
            ui_resp.raise_for_status()
            profile = ui_resp.json() or {}
    except HTTPException:
        raise
    except Exception as exc:
        log.warning("HF OAuth exchange failed: %s: %s", type(exc).__name__, exc)
        raise HTTPException(status_code=502, detail="Hugging Face OAuth exchange failed") from exc

    raw_user = profile.get("preferred_username") or profile.get("name") or profile.get("sub")
    if not isinstance(raw_user, str) or not raw_user.strip():
        raise HTTPException(status_code=502, detail="OAuth profile missing username")

    tenant_id = _auth.sanitize_tenant_id(f"hf-{raw_user}")
    resp: HTMLResponse | RedirectResponse = (
        HTMLResponse(_POPUP_SUCCESS_HTML) if is_popup else RedirectResponse(url="/", status_code=302)
    )
    resp.delete_cookie(key=_auth.OAUTH_STATE_COOKIE_NAME, path="/")
    _set_tenant_cookie(resp, request, tenant_id)
    return resp


@router.get("/api/v1/features")
async def get_features():
    """Return active UI feature flags (all enabled by default)."""
    return {"data": _features.get_features(), "error": None}


@router.post("/api/v1/auth/logout")
async def auth_logout(response: Response):
    response.delete_cookie(key=_auth.COOKIE_NAME, path="/")
    response.delete_cookie(key=_auth.TENANT_COOKIE_NAME, path="/")
    return {"data": {"ok": True}, "error": None}
