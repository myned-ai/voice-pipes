#!/usr/bin/env bash
# Run the admin console natively (no Docker): backend on http://localhost:8000,
# frontend dev server on http://localhost:5173, against an orchestrator on
# http://localhost:9090 (the Docker launcher's, or a native one).
#
# It installs only what CI tests: Python 3.12 with every package from the
# hash-checked lock (backend/requirements.txt) in backend/.venv, and the
# frontend's package-lock.json on Node 22 or 24. A re-run reinstalls only
# when a lock file changed.
#
# --backend-only / --frontend-only: run one half.  --reinstall: rebuild the
# venv and node_modules from scratch.

set -euo pipefail

BACKEND_ONLY=0
FRONTEND_ONLY=0
REINSTALL=0
for arg in "$@"; do
    case "$arg" in
        --backend-only) BACKEND_ONLY=1 ;;
        --frontend-only) FRONTEND_ONLY=1 ;;
        --reinstall) REINSTALL=1 ;;
        *) echo "Unknown option: $arg (supported: --backend-only, --frontend-only, --reinstall)"; exit 2 ;;
    esac
done

ADMIN="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$ADMIN")"
BACKEND="$ADMIN/backend"
FRONTEND="$ADMIN/frontend"

fail() { echo "error: $*" >&2; exit 1; }

file_hash() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Export KEY=VALUE lines of an env file (quotes stripped) that are not
# already set; only the keys named after the file, or all when none are.
load_env() {
    local file="$1"; shift
    [ -f "$file" ] || return 0
    local line key value
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in ''|\#*) continue ;; esac
        key="${line%%=*}"; value="${line#*=}"
        case "$value" in \"*\") value="${value#\"}"; value="${value%\"}" ;; \'*\') value="${value#\'}"; value="${value%\'}" ;; esac
        if [ "$#" -gt 0 ]; then
            case " $* " in *" $key "*) ;; *) continue ;; esac
        fi
        if [ -n "$key" ] && [ -n "$value" ] && [ -z "${!key:-}" ]; then export "$key=$value"; fi
    done < "$file"
}

# ── Settings ───────────────────────────────────────────────────────────
# voice-pipes-admin/.env holds ADMIN_UI_TOKEN (generated once, as the Docker
# launcher does); the engine tokens come from voice-pipes/.env, where the
# launcher keeps them. Values already set in this shell win.
ADMIN_ENV="$ADMIN/.env"
if ! grep -qs '^ADMIN_UI_TOKEN=..*' "$ADMIN_ENV"; then
    token=$(openssl rand -hex 32 2>/dev/null || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
    printf 'ADMIN_UI_TOKEN=%s\n' "$token" >> "$ADMIN_ENV"
    chmod 600 "$ADMIN_ENV"
    echo "Generated ADMIN_UI_TOKEN into $ADMIN_ENV"
fi
load_env "$ADMIN_ENV"
load_env "$REPO/voice-pipes/.env" VP_ADMIN_TOKEN VP_CLIENT_TOKEN
# A native backend reaches the orchestrator and the captures on this host,
# not under the container names the image defaults to.
export ORCHESTRATOR_URL="${ORCHESTRATOR_URL:-http://localhost:9090}"
export CAPTURES_DIR="${CAPTURES_DIR:-$REPO/voice-pipes/captures}"

# ── Backend: Python 3.12 and the locked packages ───────────────────────
VENV="$BACKEND/.venv"
VENV_PY="$VENV/bin/python"
if [ "$FRONTEND_ONLY" = 0 ]; then
    PYTHON=""
    for candidate in python3.12 python3; do
        if command -v "$candidate" >/dev/null 2>&1 \
            && [ "$("$candidate" -c 'import sys; print("%d.%d" % sys.version_info[:2])')" = "3.12" ]; then
            PYTHON="$(command -v "$candidate")"; break
        fi
    done
    [ -n "$PYTHON" ] || fail "Python 3.12 not found. Install it (Ubuntu 24.04: sudo apt install python3 python3-venv; elsewhere: your package manager, pyenv or uv) and run this again. The backend is tested on 3.12 only."

    if [ -d "$VENV" ] && [ "$REINSTALL" = 0 ]; then
        have="$("$VENV_PY" -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null || true)"
        if [ "$have" != "3.12" ]; then
            echo "backend/.venv is Python '$have', not 3.12: recreating it."
            REINSTALL=1
        fi
    fi
    if [ "$REINSTALL" = 1 ]; then rm -rf "$VENV"; fi
    if [ ! -x "$VENV_PY" ]; then
        echo "Creating backend/.venv with $PYTHON"
        "$PYTHON" -m venv "$VENV" || fail "could not create backend/.venv (Debian/Ubuntu: sudo apt install python3.12-venv)"
    fi
    LOCK="$BACKEND/requirements.txt"
    MARKER="$VENV/.installed-lock-sha256"
    if [ ! -f "$MARKER" ] || [ "$(cat "$MARKER")" != "$(file_hash "$LOCK")" ]; then
        # An environment made by uv has no pip; Python's own ensurepip adds it.
        if ! "$VENV_PY" -m pip --version >/dev/null 2>&1; then
            "$VENV_PY" -m ensurepip --upgrade >/dev/null 2>&1 || fail "backend/.venv has no pip and ensurepip failed; run again with --reinstall"
        fi
        echo "Installing the backend's locked packages..."
        "$VENV_PY" -m pip install --disable-pip-version-check --require-hashes -r "$LOCK" \
            || fail "pip install from backend/requirements.txt failed (see above)"
        file_hash "$LOCK" > "$MARKER"
    fi
fi

# ── Frontend: Node 22 or 24 and the locked packages ────────────────────
if [ "$BACKEND_ONLY" = 0 ]; then
    command -v node >/dev/null 2>&1 || fail "Node.js not found. Install Node 24 LTS (nodejs.org, or a version manager: fnm install 24) and run this again."
    version="$(node --version)"; version="${version#v}"
    major="${version%%.*}"; rest="${version#*.}"; minor="${rest%%.*}"
    if ! { [ "$major" = 22 ] && [ "$minor" -ge 13 ]; } && [ "$major" != 24 ]; then
        fail "Node $version is not one the console is tested on (22.13+ or 24). Install Node 24 LTS, or switch with a version manager (fnm use 24)."
    fi
    LOCK="$FRONTEND/package-lock.json"
    MARKER="$FRONTEND/node_modules/.installed-lock-sha256"
    if [ "$REINSTALL" = 1 ] || [ ! -f "$MARKER" ] || [ "$(cat "$MARKER")" != "$(file_hash "$LOCK")" ]; then
        echo "Installing the frontend's locked packages (npm ci)..."
        (cd "$FRONTEND" && npm ci) || fail "npm ci failed (see above)"
        file_hash "$LOCK" > "$MARKER"
    fi
fi

# ── Run ────────────────────────────────────────────────────────────────
# The Docker admin console publishes 8000 too; say so instead of timing out.
port_in_use() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
if [ "$FRONTEND_ONLY" = 0 ] && port_in_use 8000; then
    fail "port 8000 is in use. If it is the Docker admin console: docker compose -f voice-pipes-admin/docker-compose.yml down"
fi
if [ "$BACKEND_ONLY" = 0 ] && port_in_use 5173; then
    fail "port 5173 is in use (another Vite dev server?)"
fi

echo ""
echo "Orchestrator: $ORCHESTRATOR_URL   Captures: $CAPTURES_DIR"
echo "Sign-in token: ADMIN_UI_TOKEN in $ADMIN_ENV"

run_backend() { cd "$BACKEND" && exec "$VENV_PY" -m uvicorn app.main:app --host 127.0.0.1 --port 8000; }
run_frontend() { cd "$FRONTEND" && exec npm run dev -- --port 5173 --strictPort; }

if [ "$BACKEND_ONLY" = 1 ]; then
    echo "Backend on http://localhost:8000 (Ctrl+C stops it)"
    run_backend
fi
if [ "$FRONTEND_ONLY" = 1 ]; then
    echo "Console on http://localhost:5173 (Ctrl+C stops it; the backend must be running)"
    run_frontend
fi

run_backend &
SERVER=$!
trap 'kill "$SERVER" 2>/dev/null || true' EXIT INT TERM
ready=0
for _ in $(seq 1 60); do
    kill -0 "$SERVER" 2>/dev/null || break
    if "$VENV_PY" -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/livez', timeout=2)" 2>/dev/null; then
        ready=1; break
    fi
    sleep 0.5
done
[ "$ready" = 1 ] || fail "the backend did not come up on port 8000 (see its log above)"
echo "Console on http://localhost:5173 (backend on :8000). Ctrl+C stops both."
(cd "$FRONTEND" && npm run dev -- --port 5173 --strictPort)
