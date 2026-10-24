# Voice-Pipes Admin BFF

FastAPI backend-for-frontend for the admin console: proxies the orchestrator's
admin API, reads session capture artifacts from disk, computes analytics, and
serves the diagnostic report.

## Run

Use `../start-native.ps1` (Windows) or `../start-native.sh` (Linux), or the
compose stack (`../docker-compose.yml`, which also serves the frontend on
port 3000). [Admin console › Running it](../../docs/guides/admin-console.md#running-it)
has the steps, by hand too, and the troubleshooting.

Supported: **Python 3.12 only**, the version CI tests and the lock is resolved
for. The backend refuses another version at import
(`app/core/runtime.py`; `ADMIN_ALLOW_UNTESTED_PYTHON=1` overrides, unsupported).

## Dependencies

| File | What it is |
|---|---|
| `requirements.in` | The direct dependencies, pinned. Edit this one. |
| `requirements.txt` | The lock: every package, exact version and hashes, for Python 3.12 on every platform. Generated. |
| `requirements-dev.in` / `requirements-dev.txt` | The test and lint tools (pytest, ruff), locked the same way against `requirements.txt`. |

Install only from the lock, into a virtual environment:

```sh
python3.12 -m venv .venv
.venv/bin/python -m pip install --require-hashes -r requirements.txt -r requirements-dev.txt
```

To change a dependency, edit the `.in` file and regenerate both locks with
[uv](https://docs.astral.sh/uv/) (the command is also in each lock's header):

```sh
uv pip compile requirements.in --universal --python-version 3.12 --generate-hashes -o requirements.txt
uv pip compile requirements-dev.in --universal --python-version 3.12 --generate-hashes -c requirements.txt -o requirements-dev.txt
```

then run the tests from a fresh environment installed as above:
`.venv/bin/python -m pytest -q -rs app` and `.venv/bin/ruff check .`.
`app/test_runtime.py` fails if a lock entry is unpinned or has no hash, or a
direct dependency is missing from the lock.

## Configuration (environment variables)

| Variable | Default | Purpose |
|---|---|---|
| `ADMIN_UI_TOKEN` | *(none — startup fails)* | Bearer token the browser/scripts must present to **this** backend. Generate with `openssl rand -hex 32` |
| `ADMIN_UI_AUTH_REQUIRED` | `true` | Set `false` to run open on a loopback-only dev box (logs a warning) |
| `ORCHESTRATOR_URL` | `http://host.docker.internal:9090` | Orchestrator admin API base (the start scripts set `http://localhost:9090`) |
| `VP_ADMIN_TOKEN` | *(empty — auth disabled, dev only)* | Bearer token presented to the orchestrator; set the same value the orchestrator enforces |
| `ADMIN_CORS_ORIGINS` | localhost `:3000` / `:5173` origins | Comma-separated allowed origins. `"*"` is accepted but disables credentialed requests |
| `CAPTURES_DIR` | `/app/captures` | Fallback captures path; normally resolved from the orchestrator at startup (the start scripts set `voice-pipes/captures`) |
| `ADMIN_ALLOW_UNTESTED_PYTHON` | *(unset)* | `1` starts on a Python other than 3.12, logged as unsupported |
