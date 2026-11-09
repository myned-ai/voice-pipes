import logging
import os
import re
import time
from pathlib import Path

import httpx

log = logging.getLogger("admin.config")


def configure_logging() -> None:
    """Send every logger's records to stderr at ``LOG_LEVEL`` (default
    INFO). An unknown level name falls back to INFO. Uvicorn's own loggers
    keep their handlers and do not propagate, so nothing prints twice.
    httpx logs every request at INFO; it is held at WARNING unless the
    level is DEBUG, so orchestrator polling does not flood the log."""
    name = os.getenv("LOG_LEVEL", "INFO").strip().upper()
    level = logging.getLevelName(name)
    if not isinstance(level, int):
        level = logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    if level > logging.DEBUG:
        logging.getLogger("httpx").setLevel(max(level, logging.WARNING))


ORCHESTRATOR_URL = os.getenv("ORCHESTRATOR_URL", "http://host.docker.internal:9090")

# Admin credential presented to the orchestrator. Set the same
# VP_ADMIN_TOKEN the orchestrator enforces; unset/empty = no auth (dev).
ADMIN_TOKEN = os.getenv("VP_ADMIN_TOKEN", "").strip()
ORCH_HEADERS: dict = (
    {"Authorization": f"Bearer {ADMIN_TOKEN}"} if ADMIN_TOKEN else {}
)

# Worker WebSocket credential (VP_CLIENT_TOKEN on the workers). Used only by
# the /api/v1/playground/ws proxy, which presents it to the worker; it is
# never sent to the browser. Empty = workers run open (dev).
CLIENT_TOKEN = os.getenv("VP_CLIENT_TOKEN", "").strip()


async def probe_orchestrator() -> str:
    """Check the orchestrator is reachable and our credential is accepted.

    An empty fleet in the console is indistinguishable from a healthy
    orchestrator with no workers registered, so a rejected credential looks
    exactly like "nothing is running" -- the console renders an empty list and
    says nothing. This turns that into one loud line at startup.

    Returns a short status string for the health endpoint; never raises, since
    a console that cannot reach the orchestrator should still serve captures.
    """
    try:
        async with orch_client() as client:
            resp = await client.get(f"{ORCHESTRATOR_URL}/api/v1/workers", timeout=5.0)
    except Exception as exc:  # noqa: BLE001 - reported, not handled
        log.warning(
            "orchestrator UNREACHABLE at %s: %s. "
            "The console will show an empty fleet. Check ORCHESTRATOR_URL.",
            ORCHESTRATOR_URL,
            exc,
        )
        return "unreachable"

    if resp.status_code in (401, 403):
        have = "set" if ADMIN_TOKEN else "UNSET"
        log.warning(
            "orchestrator REJECTED our credential (%s) at %s. "
            "VP_ADMIN_TOKEN is %s. The console will show an empty fleet "
            "until it matches the orchestrator's token.",
            resp.status_code,
            ORCHESTRATOR_URL,
            have,
        )
        return "unauthorized"

    if resp.status_code >= 400:
        log.warning(
            "orchestrator returned %s at %s; the console may show an empty fleet.",
            resp.status_code,
            ORCHESTRATOR_URL,
        )
        return f"http_{resp.status_code}"

    return "ok"


def orch_client(**kwargs) -> httpx.AsyncClient:
    """`httpx.AsyncClient` pre-configured with the orchestrator admin
    credential. Use for every orchestrator call so enabling
    VP_ADMIN_TOKEN needs no per-call-site changes."""
    kwargs.setdefault("headers", ORCH_HEADERS)
    return httpx.AsyncClient(**kwargs)


# Fallback only — resolved from orchestrator config at startup when possible.
_CAPTURES_DIR_FALLBACK = Path(os.getenv("CAPTURES_DIR", "/app/captures"))
CAPTURES_DIR: Path = _CAPTURES_DIR_FALLBACK

# Throttle re-resolution on the hot path. When the orchestrator is
# unreachable at startup, CAPTURES_DIR stays == the fallback and
# `_captures_signature` re-attempts resolution on EVERY analytics request (even
# cache hits), each paying the orchestrator connect timeout.  We cap that to one
# attempt per cooldown so recovery still happens but the hot path stays fast.
_RESOLVE_COOLDOWN_S = float(os.getenv("CAPTURES_RESOLVE_COOLDOWN_S", "30"))
_last_resolve_attempt_mono = 0.0


def _resolve_captures_dir(force: bool = False, timeout: float = 2.0) -> Path:
    """Fetch captures_dir from the orchestrator's config endpoint, falling
    back to the CAPTURES_DIR env-var / default. Blocking: the startup hook
    runs it in a thread.

    The orchestrator's directory wins when it exists here; otherwise an
    existing fallback directory does (the container's volume mount);
    otherwise the orchestrator's directory is created.

    Throttled (see _RESOLVE_COOLDOWN_S): callers on the per-request hot path
    (``_captures_signature``) would otherwise pay the orchestrator connect
    timeout on every request while the orchestrator is down.  We attempt the
    round-trip at most once per cooldown; once it succeeds CAPTURES_DIR no
    longer equals the fallback and the hot path stops calling this entirely.
    Pass ``force=True`` to bypass the cooldown.
    """
    global CAPTURES_DIR, _last_resolve_attempt_mono
    if not force:
        now = time.monotonic()
        if now - _last_resolve_attempt_mono < _RESOLVE_COOLDOWN_S:
            return CAPTURES_DIR
        _last_resolve_attempt_mono = now
    try:
        with httpx.Client(headers=ORCH_HEADERS) as client:
            resp = client.get(
                f"{ORCHESTRATOR_URL}/api/v1/paths", timeout=timeout
            )
            resp.raise_for_status()
            paths_cfg = resp.json().get("data", {})
            remote = paths_cfg.get("captures_dir")
            if remote:
                remote_path = Path(remote)
                if remote_path.exists() and remote_path.is_dir():
                    CAPTURES_DIR = remote_path
                    return CAPTURES_DIR
                if _CAPTURES_DIR_FALLBACK.exists() and _CAPTURES_DIR_FALLBACK.is_dir():
                    CAPTURES_DIR = _CAPTURES_DIR_FALLBACK
                    return CAPTURES_DIR

                remote_path.mkdir(parents=True, exist_ok=True)
                CAPTURES_DIR = remote_path
                return CAPTURES_DIR
    except Exception:  # noqa: BLE001, S110 - orchestrator unreachable or its answer unusable: the fallback applies
        pass
    return _CAPTURES_DIR_FALLBACK


# ── Regex for per-utterance capture files ────────────────────

_UTT_FILE_RE = re.compile(
    r"^(?P<utt>\d+)_(?P<kind>mic_raw|mic_16k|stt|llm_input|llm_output|tts_frag_\d+(?:_spoken)?|turn_resolved|realtime_usage|playback)\.(?P<ext>wav|json|txt)$"
)
