# Admin console

**Status:** active
**Last verified:** 2026-10-17
**Owner:** Docs

The admin console is a React application with a small FastAPI backend. It
connects to the orchestrator, never to workers directly. The Playground's
WebSocket to a worker goes through the backend too (`/api/v1/playground/ws`),
except for a **Direct URL** session, which the browser opens itself.

## Running it

### With Docker (recommended)

- The [Docker launcher](../getting-started/quickstart-docker.md) starts it
  with the rest of the stack: http://localhost:3000.
- On its own: `cd voice-pipes-admin && docker compose up -d --build` serves
  the production build on 3000 and the backend on 8000.

The images pin everything: the backend runs Python 3.12 and installs from the
hash-checked lock, the frontend builds on Node 22 from `package-lock.json`.

### Natively, on Windows or Linux

Run natively to work on the console, or where Docker is not an option. Only
these versions are supported, because they are the ones CI tests:

| | Supported | Why only these |
|---|---|---|
| Python (backend) | **3.12** | CI tests 3.12 and the lock file is resolved for it. The backend refuses to start on another version and says so. |
| Node.js (frontend) | **24** (LTS) or **22.13+** | CI tests both. `npm` refuses any other version (`EBADENGINE`). |

The backend's packages come from `voice-pipes-admin/backend/requirements.txt`,
a lock file that pins every package (dependencies of dependencies included)
to an exact version with its hashes. The frontend's come from
`package-lock.json`. Neither install picks up a version CI has not run.

**1. Install the two runtimes once.**

Windows (PowerShell):

```powershell
winget install Python.Python.3.12
winget install OpenJS.NodeJS.LTS
# open a new terminal, then check:
py -3.12 --version     # Python 3.12.x
node --version         # v24.x (or v22.13 or later)
```

Linux: on Ubuntu 24.04, `sudo apt install python3 python3-venv` gives Python
3.12; on other distributions install 3.12 from the package manager or pyenv,
so that `python3.12 --version` works. For Node, install 24 LTS from
[nodejs.org](https://nodejs.org) or a version manager (`fnm install 24`);
distribution packages are often older than 22.13.

**2. Start it.** From the repository root:

```sh
.\voice-pipes-admin\start-native.ps1     # Windows PowerShell
bash voice-pipes-admin/start-native.sh   # Linux
```

The script:

1. creates `voice-pipes-admin/backend/.venv` with Python 3.12 (or recreates
   one made with another version) and installs the backend lock into it,
   with `pip install --require-hashes`;
2. checks the Node version and runs `npm ci` in `voice-pipes-admin/frontend`;
3. generates `ADMIN_UI_TOKEN` into `voice-pipes-admin/.env` if there is none,
   and takes `VP_ADMIN_TOKEN` and `VP_CLIENT_TOKEN` from `voice-pipes/.env`,
   where the Docker launcher keeps them;
4. points the backend at the orchestrator on `http://localhost:9090` and at
   the captures in `voice-pipes/captures`;
5. starts the backend on http://localhost:8000 and the console on
   http://localhost:5173. `Ctrl+C` stops both.

A re-run reinstalls only when a lock file changed. Values already set in
your shell win over the files (for example `ORCHESTRATOR_URL` for an
orchestrator elsewhere). Options: `-BackendOnly` / `--backend-only`,
`-FrontendOnly` / `--frontend-only`, and `-Reinstall` / `--reinstall` to
rebuild the environments from scratch.

**3. Sign in.** Open http://localhost:5173 and paste the value of
`ADMIN_UI_TOKEN` from `voice-pipes-admin/.env`.

The console shows the fleet of an orchestrator. If the Docker launcher's
stack is running, the script finds it on port 9090 with its tokens and
nothing else is needed; stop the Docker admin console first
(`docker compose -f voice-pipes-admin/docker-compose.yml down`), since it
holds port 8000.

**Everything from source, no Docker.** After the
[source quickstart](../getting-started/quickstart-source.md) (binary
installed, `voice-pipes models fetch` done), run three processes, each in its
own terminal from the `voice-pipes/` directory except the last:

```sh
# 1. orchestrator on http://127.0.0.1:9090
voice-pipes orchestrate --orchestrator-config examples/orchestrate/orchestrator.toml

# 2. a worker running the local pipeline; it registers with the orchestrator
#    and records sessions to voice-pipes/captures for the Sessions page
voice-pipes --config examples/minimal.toml --captures-dir captures serve --worker-config examples/orchestrate/worker.toml

# 3. the console, from the repository root
.\voice-pipes-admin\start-native.ps1     # Windows
bash voice-pipes-admin/start-native.sh   # Linux
```

Open http://localhost:5173, sign in, and the Dashboard lists `worker-1`.
In **Playground**, choose **Pipeline**, select `minimal` and press **Start
session**: the backend proxies the session to the worker. This fleet runs
without tokens, so every port is bound to `127.0.0.1` only; to expose it,
set `VP_ADMIN_TOKEN`, `VP_INTERNAL_TOKEN` and `VP_CLIENT_TOKEN` first (see
[Authentication](#authentication)).

**Doing it by hand.** The same steps without the script, from
`voice-pipes-admin/backend`:

```powershell
# Windows
py -3.12 -m venv .venv
.\.venv\Scripts\python.exe -m pip install --require-hashes -r requirements.txt
$env:ADMIN_UI_TOKEN = "<a long random value>"
$env:ORCHESTRATOR_URL = "http://localhost:9090"
.\.venv\Scripts\python.exe -m uvicorn app.main:app --host 127.0.0.1 --port 8000
```

```sh
# Linux
python3.12 -m venv .venv
.venv/bin/python -m pip install --require-hashes -r requirements.txt
export ADMIN_UI_TOKEN="$(openssl rand -hex 32)" ORCHESTRATOR_URL=http://localhost:9090
.venv/bin/python -m uvicorn app.main:app --host 127.0.0.1 --port 8000
```

and in a second terminal, from `voice-pipes-admin/frontend`: `npm ci`, then
`npm run dev`. Always install into the `.venv`, never with a bare
`pip install`: that goes into whichever Python is first on `PATH`.

**Troubleshooting**

- `WARNING: The script uvicorn.exe is installed in '...\Python314\Scripts'
  which is not on PATH`: `pip` ran outside a virtual environment, on another
  Python, and installed into your user profile. Use the script (or the
  `.venv` steps above). `py -3.14 -m pip uninstall -r
  voice-pipes-admin/backend/requirements.in` removes the packages it named
  from that Python.
- `error: the admin backend is tested on Python 3.12 and this is Python 3.x`:
  the environment was made with another Python. `-Reinstall` /
  `--reinstall` rebuilds it with 3.12.
- `npm error code EBADENGINE`: the Node version is not 22.13+ or 24.
- `port 8000 is in use`: the Docker admin console (or an earlier run) is
  still up.
- The fleet is empty and the backend log says `orchestrator REJECTED our
  credential`: `VP_ADMIN_TOKEN` does not match the orchestrator's. The
  launcher's is in `voice-pipes/.env`.
- `orchestrator UNREACHABLE`: no orchestrator on `ORCHESTRATOR_URL`.
- The console lists workers you did not start, or the Live Graph is empty
  for workers that look healthy: a Docker stack from an earlier run is still
  up and answering on the same ports. On Windows `localhost` reaches it
  first (over `::1`) even with your native orchestrator running. Stop it:
  `docker compose -f voice-pipes/deploy/docker/docker-compose.yml stop`
  (and `docker-compose.greek.yml` if you used it). A native orchestrator or
  worker does not share a port another process already answers on: it logs
  `already served on … by another process` and takes the next free port, so
  check its `listening` line for the port it actually got.

Backend settings: `ADMIN_UI_TOKEN` (required, see below), `ORCHESTRATOR_URL`
(default `http://host.docker.internal:9090`), `VP_ADMIN_TOKEN` (the bearer
token the orchestrator enforces, empty in dev), `VP_CLIENT_TOKEN` (the
workers' WebSocket token, which the Playground proxy presents; empty when
the workers run open), `PLAYGROUND_LOOPBACK_HOST` (the name under which the
backend reaches a worker that registered a loopback or `0.0.0.0` address;
the compose file sets `host.docker.internal`), `ADMIN_CORS_ORIGINS`,
`LOG_LEVEL` (default `INFO`), and `CAPTURES_DIR` as a fallback when the
orchestrator does not report its captures directory. The frontend reads the API base from `VITE_API_URL` and optional production telemetry mode from `VITE_DASHBOARD_PRODUCTION_MODE` (`true` or `1`; defaults to `false`).

The backend keeps its issues feed, analytics rollup cache (two SQLite files),
and dashboard clear watermarks (`analytics_cleared.json`) in
`ADMIN_STATE_DIR`. The image leaves it unset and uses `uid-<uid>` under
`/app/state` instead, a volume in the compose file that any uid can write,
so the stores work whatever uid `user:` runs the container as. A checkout
uses `voice-pipes-admin/backend/tmp/`. If the directory cannot be written,
the backend logs it once and the dashboard's issues feed says nothing is
being recorded.

## Authentication

The backend serves session transcripts and audio, live worker parameter
edits and a WebSocket proxy into the transport, so every route requires a
credential. Two tokens are involved and they are deliberately different:

| Variable | Set on | Presented by | Protects |
|---|---|---|---|
| `ADMIN_UI_TOKEN` | admin backend | the browser (you) | the admin backend itself (port 8000) |
| `VP_ADMIN_TOKEN` | orchestrator and admin backend | the admin backend | the orchestrator admin API (port 9090) |
| `VP_CLIENT_TOKEN` | workers and admin backend | the admin backend's Playground proxy | the workers' `/ws` (port 8800) |

`VP_CLIENT_TOKEN` never reaches the browser: no response carries it and the
console stores none. A Playground session started from **Pipeline** opens
`/api/v1/playground/ws?worker_id=<id>` on the backend, signed in by the
login cookie; the backend connects only to a worker the orchestrator lists
(any other id is refused) and presents the token to it.

Generate a token (`openssl rand -hex 32`), put it in `voice-pipes-admin/.env`
as `ADMIN_UI_TOKEN=…` (copy `.env.example`), and paste the same value into
the sign-in form the console shows on first load. The browser keeps it in
`sessionStorage` for that tab and sends it as a bearer header; a login
cookie (`HttpOnly`, `SameSite=Lax`) covers the audio players and the
server-sent fleet stream, which cannot set headers. Any 401 later returns
the console to the sign-in form.

The backend refuses to start when `ADMIN_UI_TOKEN` is unset. For a
loopback-only development setup you can set `ADMIN_UI_AUTH_REQUIRED=false`;
the backend then logs a warning and runs open.

Scripted access uses the same token:

```sh
curl -H "Authorization: Bearer $ADMIN_UI_TOKEN" http://localhost:8000/api/v1/sessions
```

`/livez` is the only route that never needs a credential; `/api/v1/auth/check`
answers whether a token is required and whether the one presented is valid.

Because the cookie is `SameSite=Lax`, the frontend and backend must be on
the same site (same registrable domain; ports may differ) or served from one
origin behind a proxy.

## Pages

### Dashboard

By default (`DASHBOARD_PRODUCTION_MODE = false`), the Dashboard renders a
streamlined v1 developer view tailored for testing components and comparing
pipeline variants:

- **Pipelines & Component Health:** Live worker cards and component health
  pills (`STT`, `LLM`, `TTS`, `VAD`) streamed over SSE.
- **Core KPI Cards:** Four headline metrics for the selected window
  (**Sessions**, **Turns**, **Error Rate**, and **Avg Latency** with `p95`),
  defaulting to **Today** (`days=1`) and using the fast `lite=true` analytics
  scan (which skips full-session anomaly grading and prior-window double
  fetches).
- **Failed Sessions & Per-Pipeline Comparison:** A table of failed sessions
  (each with a **dismiss** button to hide individual broken test runs) and a
  per-pipeline comparison matrix (`p50`, `p95`, `TTFT avg`, `Endpointing avg`,
  `tok/s`, `Error rate`, and a per-pipeline **clear** button).
- **Clear Metrics:** The **Clear Metrics** button in the header opens a
  confirmation dialog where you can reset metrics across **All pipelines** or a
  **single pipeline**. By default, clearing sets a persisted watermark in
  `analytics_cleared.json` so dashboard counters start fresh while keeping
  session recordings in **Sessions**; checking **Also delete recorded session
  capture files from disk** (`delete_captures: true`) also deletes the matching
  capture directories from `captures/`.

Setting `VITE_DASHBOARD_PRODUCTION_MODE=true` (or flipping
`DASHBOARD_PRODUCTION_MODE` in `src/pages/dashboard/sections.ts`) unlocks the
full production/SRE/FinOps suite: the system SLO status strip, fleet
concurrency capacity bar, provider monthly quota alerts, 8 KPI cards with
prior-period $\Delta\%$ badges, client-perceived audio quality KPIs, business
outcome/CSAT cards, bucketed latency and LLM cost charts, per-turn latency /
LLM / token time-series charts, provider spend tables, and the anomaly/issues
activity feed.

### Live pipeline

A ReactFlow diagram of the active pipeline assembled from the first
registered worker's configuration: one node per stage with provider name,
sample rates and parameters, optional resampler nodes, and a rolling
60-second p50/p90 overlay. The parameter editor edits live-tunable values
(VAD thresholds, LLM sampling) on the worker without a restart.

### Playground

A voice-chat harness against a worker. Connect, grant the microphone, talk;
the transcript reveals word by word in sync with playback; interrupt
mid-sentence to see barge-in. Speech the worker dropped (no words
recognised, too short, or a recogniser that never answered) shows as a
system line in the transcript, such as "Heard 1.3 s of speech but
recognised no words", instead of silence. It shows per-turn metrics (time to first token
and first byte, total, round trip, interruption status), a raw wire-event log
with filters, and a settings panel for what the browser controls: playback
rate, the jitter buffer, output chunking and microphone processing. Language,
voice and provider tuning are the worker's configuration and are not set
here. It
runs in voice or text mode and supports client-side tools and runtime-loaded
[extensions](../clients/playground-extensions.md) such as avatar renderers
and viseme meters.

The bar at the top asks one question — where the session should go — and has
one button, **Start session**. Choose **Pipeline** and the orchestrator picks
the least busy worker serving the pipeline you select (or any pipeline); this
is the default whenever the orchestrator has a pipeline to offer, and the
session runs through the backend's proxy, which holds the worker token.
Choose **Direct URL** to connect to one worker you name, `ws://localhost:8800/ws`
for a standalone one; the browser opens that URL itself and presents no
token, so it reaches only a worker that runs without `VP_CLIENT_TOKEN`. The
bar falls back to this, and says why, when the orchestrator cannot be
reached or has nothing registered. Once connected the
bar shows where the session landed, as `pipeline › worker`, and your choice of
source is remembered.

Two diagnostic dialogs sit in the settings panel, under the browser
microphone-processing switches:

- **Mic check** uses the browser's audio APIs alone — none of the
  framework's capture code and no session. It lists the input devices,
  shows the live level, the device's and the audio context's sample rates,
  and the echo-cancellation, noise-suppression and gain-control settings the
  browser actually applied, and says whether the signal is absent, silent,
  too quiet or clipping. Use it first when a session hears nothing: it
  separates "the browser is not getting audio" from "the pipeline is not
  understanding it".
- **Component check** runs the worker's VAD, recogniser, language model and
  synthesiser **one at a time**, each against a known input, through the
  [probe endpoints](../reference/http-api.md#worker-debug-api-debug_port-default-8085).
  The input is a shipped recording with known words, or your own voice
  recorded in the dialog (with the same browser processing the Playground is
  set to) plus a line saying what you said. Each stage gets a verdict and
  its evidence: the speech the VAD dropped before the onset, the transcript
  word for word, the model's reply and its time to first token, the
  synthesiser's real-time factor and a player for what it produced. The
  stages are not chained — the recogniser is given the whole recording, not
  the VAD's cut of it — so a failure belongs to the stage that shows it.
  The check runs on the worker your session is on when there is one,
  otherwise on a worker of the selected pipeline, and says which. **Copy
  report** puts the whole result on the clipboard as text.

### Sessions

Every recorded session with time filters and pagination. In multi-tenant environments, sessions and audit files are isolated by tenant (`captures/<tenant_id>/<session_id>/`). Supplying an `X-Tenant-Id` header or `?tenant_id=` query parameter scopes session listings and capture file playback strictly to that tenant's recordings, ensuring strict tenant isolation and preventing unauthorized access. Zero Data Retention (ZDR) sessions display full turn transcripts and latency metrics while omitting discarded audio waveforms.

The detail view
plays the conversation from the capture files (user and agent fragments,
"as-heard" reconstruction with the client's underrun gaps spliced in), shows
the per-turn component metrics table rendered from descriptors, the LLM
prompt and response with a timing breakdown, interruption dividers with the
truncated text, playback health, and the health verdict with its anomaly
flags. Speech that made no turn (the recogniser found no words, too few, or
never answered) appears as a "Heard, not recognised" card before the turn
that took its id, with the reason and a player for the recording the
capture kept, so noise the caller made can be told from speech the
recogniser missed; a session the recogniser never answered is no longer an
empty page. A verdict reached under an operator's own
[grading rules](configuration.md#grading-optional-advanced) is badged
`custom rules` and its headline lists every value that differs from the
default. "Report for LLM" produces a text summary of a session suitable for
pasting into an assistant.

### Benchmarks

Create test suites from audio files, define experiments as pipeline
variants, run them against tagged workers and compare results side by side.
See [Benchmarking and evals](benchmarking-and-evals.md).

## Architecture

Four tiers: the React SPA, the FastAPI backend-for-frontend that proxies the
orchestrator and reads capture files, the Rust orchestrator (control plane),
and Rust workers (data plane). The BFF exists so the browser has one origin
to talk to and so capture files can be read from a shared volume; it is not
on the audio path. Design notes and regression risks are in
[Admin console architecture](../explanation/admin-console-architecture.md);
the BFF's routes are in the [Admin API reference](../reference/admin-api.md).

## Transport support

WebSocket only. WebRTC and WebTransport adapters exist as gated stubs behind
an experimental flag and are unreachable from the UI.
