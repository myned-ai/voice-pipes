"""Voice-Pipes Admin BFF — application entrypoint.

The implementation was split out of this file into a package:
  core/      shared config + captures-dir state
  captures/  local capture reading + per-session metrics
  report/    diagnostic report (anomaly detection + markdown render)
  analytics/ window aggregation + cache + orchestrator overlay
  routers/   the HTTP/WS endpoints, grouped by concern

This module only creates the app, wires middleware + startup, and mounts
the routers.  Launch unchanged:  uvicorn app.main:app
"""
import asyncio
import os

from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from fastapi.middleware.gzip import GZipMiddleware

from .core import auth as _auth
from .core import limits as _limits
from .core.config import _resolve_captures_dir, configure_logging, probe_orchestrator
from .routers import analytics as _analytics
from .routers import audio as _audio
from .routers import auth as _auth_router
from .routers import benchmark as _benchmark
from .routers import health as _health
from .routers import playground as _playground
from .routers import sessions as _sessions
from .routers import stream as _stream

configure_logging()

# Refuse to start when auth is required but no token is configured.
_auth.validate_startup_config()

app = FastAPI(title="Voice-Pipes Admin BFF")


# Innermost: refuse an oversized request body before a route reads it.
# Inside the auth gate, so an anonymous caller gets 401 whatever it sends.
app.add_middleware(_limits.BodyLimitMiddleware)

# 0c — compress responses (the analytics utterances payload is large and very
# repetitive; gzip cuts transfer ~8-10x).  Browsers auto-decompress, so the
# frontend is unchanged.  Added before CORS so the CORS headers wrap the
# already-compressed response.  Starlette leaves ``text/event-stream``
# uncompressed, so the SSE routes are not held in zlib's buffer.
app.add_middleware(GZipMiddleware, minimum_size=1024)

# Every HTTP/SSE/WebSocket request must carry ADMIN_UI_TOKEN (header,
# ``token`` query param, or the login cookie).  Added after GZip so it sits
# outside it, and before CORS so CORS is the outermost layer and still
# answers preflights and decorates the 401.
app.add_middleware(_auth.AdminAuthMiddleware)

# CORS: allowed origins come from ADMIN_CORS_ORIGINS (comma-separated). The
# default covers the local stack (Docker frontend on :3000, native Vite dev
# server on :5173). "*" is accepted for throwaway setups, but then
# credentialed requests are disabled — wildcard + credentials is forbidden
# by the CORS spec and would otherwise have Starlette echo any Origin back
# with credentials allowed.
_cors_origins = [
    o.strip()
    for o in os.getenv(
        "ADMIN_CORS_ORIGINS",
        "http://localhost:3000,http://127.0.0.1:3000,"
        "http://localhost:5173,http://127.0.0.1:5173",
    ).split(",")
    if o.strip()
]
app.add_middleware(
    CORSMiddleware,
    allow_origins=_cors_origins,
    allow_credentials="*" not in _cors_origins,
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.on_event("startup")
async def _on_startup():
    await asyncio.to_thread(_resolve_captures_dir, force=True, timeout=5.0)
    # Say so at startup when the fleet will be empty for a reason other than
    # "no workers": unreachable orchestrator, or a credential it rejects.
    await probe_orchestrator()


# Routers — order is immaterial; every path is distinct after the duplicate
# /workers and /components handlers were de-shadowed during the split.
app.include_router(_auth_router.router)
app.include_router(_health.router)
app.include_router(_sessions.router)
app.include_router(_audio.router)
app.include_router(_analytics.router)
app.include_router(_playground.router)
app.include_router(_benchmark.router)
app.include_router(_stream.router)
