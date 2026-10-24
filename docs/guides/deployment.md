# Deployment

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

How the containerised stack is put together, what to change for something
other than a laptop, and the controls to turn on before exposing a worker to
people you do not know.

## The compose stack

`voice-pipes/deploy/docker/docker-compose.yml` defines:

| Service | Image | Role |
|---|---|---|
| `orchestrator` | `voice-pipes/Dockerfile` | Admin API on 9090 |
| `worker-custom`, `worker-custom-gpu` | `voice-pipes/Dockerfile`, `Dockerfile.cuda` with the wizard's `FEATURES` | The worker the launcher runs, on 8800 (debug 8085); the GPU build replaces it under profile `gpu` |
| `worker-english-corpus` | `voice-pipes/Dockerfile` | CPU worker on 8803 (debug 8086) that records the English corpus |
| `worker-english-moonshine`, `worker-english-nemotron35` | `voice-pipes/Dockerfile` | CPU workers for the English recogniser comparison, 8807 and 8810 (debug 8093 and 8095) |
| `worker-english-nemotron` | `voice-pipes/Dockerfile` | Profile `nemotron`, 8808 (debug 8094) |
| `worker-a`, `worker-b` | `voice-pipes/Dockerfile` | Profile `ab`: an A/B pair on one pipeline, 8801/8802 (debug 8096/8097) |
| `worker-gpu` | `voice-pipes/Dockerfile.cuda` | Profile `gpu`, port 8809 (debug 8087) |
| `worker-s2s-openai`, `worker-s2s-gemini`, `worker-s2s-elevenlabs` | `voice-pipes/Dockerfile` | Profile `bench-s2s`: speech-to-speech workers on 8804 to 8806 (debug 8088 to 8090) |
| `llama-server` | llama.cpp | Profile `llama`; serves the GGUF the wizard downloaded |
| `pocket-tts` | Kyutai image | Profile `pocket` |
| `vibevoice` | `voice-pipes/Dockerfile.vibevoice` | Profile `vibevoice`; needs GPU pass-through |
| admin `frontend` / `backend` | `voice-pipes-admin/*/Dockerfile` | Console on 3000, BFF on 8000 (`voice-pipes-admin/docker-compose.yml`) |

The launcher reads `deploy/docker/config/stack-manifest.env`, written by the
wizard, to decide which profiles and services to start, whether a GPU is
required, and which model paths to hand to sidecars. Adding a provider never
touches the launcher scripts; see [Wizard descriptor](../extending/wizard-descriptor.md).

Model files live in `voice-pipes/models/` on the host and are mounted at
`/app/models` in workers and `/models` in sidecars. API keys are passed to
containers by name from `voice-pipes/.env`; values never appear in the
compose file or in `docker inspect`.

## Images

`voice-pipes/Dockerfile` builds the CPU worker and orchestrator (same binary,
different command) on `rust:1.91`; `Dockerfile.cuda` builds with the CUDA
features; `Dockerfile.vibevoice` builds the GPU TTS sidecar. The admin
frontend image is a multi-stage nginx build of the production bundle; the
backend image runs FastAPI with uvicorn. The frontend's nginx sends
`X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY` with
`Content-Security-Policy: frame-ancestors 'none'` (the console cannot be
framed; the policy restricts nothing else), and
`Referrer-Policy: strict-origin-when-cross-origin` on every response;
`index.html` and the SPA fallback are `Cache-Control: no-cache`, the hashed
`/assets/` immutable. A reverse proxy in front need not add these.

The engine images carry a `HEALTHCHECK` that runs `voice-pipes healthz`
(the image has no other HTTP client) against a worker's `/healthz` on
8800; the orchestrator's compose service probes its own `/healthz` on
9090 instead, and every worker waits for the orchestrator to be healthy
before it starts.

**Stopping.** A worker given Ctrl-C or SIGTERM (`docker stop`) ends every
open session with a `closing` frame whose reason is `worker_shutdown`,
answers 503 at `/readyz`, gives the sessions up to ten seconds to close,
and tells the orchestrator it left, so nothing is dispatched to it in the
meantime. The orchestrator finishes in-flight requests and stops. Keep
`docker stop`'s grace period (ten seconds by default) at least that long.
A worker whose transport listener fails exits with an error instead of
staying up without it, so `restart: unless-stopped` brings it back.

Published images for tagged releases are part of the
[release process](../contributing/release-process.md); until then build
locally with the launcher or `docker compose … --build`.

**Every image this repository builds runs unprivileged.** The engine, CUDA,
VibeVoice and admin backend images create a system user `vp` (uid 10001);
the admin frontend uses the unprivileged nginx image (uid 101). The two
third-party sidecars, `llama-server` and `pocket-tts`, run as the host
user's ids too (`user:` in the compose file; neither writes outside its
mounted model or cache directory). None of the three model-server sidecars
authenticates its callers, so their host ports are bound to `127.0.0.1`;
the workers reach them over the compose network. The compose files set
`user: "${VP_UID:-10001}:${VP_GID:-10001}"` on the services that
bind-mount host directories (`models/`, `captures/`, `audit/`, `sessions/`,
`benchmarks/`). On Linux those directories belong to whoever ran the
launcher, so the launcher exports `VP_UID`/`VP_GID` as that user's ids and
the containers write to them as-is. If you start compose by hand on Linux,
export the two variables yourself (`export VP_UID=$(id -u) VP_GID=$(id -g)`)
or `chown -R 10001:10001` the data directories. Docker Desktop on macOS and
Windows does not map host ownership onto bind mounts, but it does keep the
owner of a directory a container created: one an earlier root-run container
made stays root-owned and read-only to the unprivileged user. That is why
pocket-tts now keeps its caches in `models/.cache/pocket-tts`, which the
launcher creates; the old caches beside it are no longer read and can be
deleted. The nemo-export utility image takes `--user $(id -u):$(id -g)` on
`docker run` for the same reason.

## Ports and addresses

| Port | What |
|---|---|
| 8800 (8801 to 8810 for the other compose workers) | Worker WebSocket `/ws` and `/health` |
| 8085 (8086 to 8097 for the other compose workers, loopback only) | Worker debug API; requires `VP_INTERNAL_TOKEN` |
| 9090 | Orchestrator admin API |
| 3000 | Admin console |
| 8000 | Admin BFF |
| 50051+ | gRPC component servers, if used |

Starting a service by hand with `docker compose … up` needs
`--env-file voice-pipes/.env`: the compose file passes the tokens by name
from the environment, which the launcher exports, so without it compose
recreates the orchestrator with no tokens and it refuses to serve, in a
restart loop, until started again with them.

Two rules save hours in containers: bind the orchestrator to `0.0.0.0` (a
`127.0.0.1` bind is unreachable from other containers), and set each
worker's `advertise_address` and `transport_advertise_address` to what the
orchestrator and the browser can actually reach. See [Fleet](fleet.md#addresses-in-containers).

## GPU

The launcher probes for NVIDIA Container Toolkit pass-through and picks the
CUDA worker image when it finds it. VibeVoice requires it; Moonshine,
Nemotron and the local GGUF LLM benefit from it. Details in
[GPU and CUDA](gpu-and-cuda.md).

## Memory, swap and idle workers

A worker with local models (speech recognition, Piper, a GGUF LLM) holds
them in RAM. On a host short of memory the kernel swaps out whatever has
gone longest unused, which is an idle worker's models, and its next caller
waits while they are read back. Measured on a test box: a worker with 0.9 GB
swapped out answered its first turn in 2.9–3.3 s from the end of speech,
against 1.3 s warm, almost all of it in speech recognition.

Keep the voice path out of swap:

```sh
docker compose -f voice-pipes/deploy/docker/docker-compose.yml \
    -f voice-pipes/deploy/docker/docker-compose.noswap.yml up -d ...
```

`docker-compose.noswap.yml` gives every service a memory limit and an equal
memory-plus-swap limit, so it cannot be swapped, and locks llama-server's
weights in RAM. The trade: a container that outgrows its limit is killed
and restarted instead of slowed. Size the limits from `docker stats` under
load with the `VP_MEM_<SERVICE>` variables named in the file, and make sure
their sum fits the host with room for the OS. A host that cannot hold every
model at once needs more memory or fewer workers; no setting changes that.
On Docker Desktop the limit that matters is the memory given to its VM.

An idle worker also keeps its own recogniser resident: with
`[pipeline] keep_warm_interval_s` (default 300) it decodes a second of
silence once per interval while no session is open, so the model's pages
stay in use even where the host is not locked out of swap. It covers the
recognisers warmed once per process (the local models); `0` turns it off.
Without it, on a 15.6 GB Docker Desktop VM already 3 GB into swap, a
Nemotron 3.5 worker left alone for 38 minutes took longer to page back in
than the pipeline waits for a final, and its session got nothing.

What the workers hold, measured as resident memory on that VM: a Nemotron
3.5 worker about 4 GB on the CPU image and 6.5 GB on the CUDA image, a
Moonshine worker 0.5 GB, the Gemini worker under 0.1 GB, llama-server 0.5 GB
in RAM with the model on the GPU. Two Nemotron workers and the sidecar on a
16 GB VM is already over.

To see what idle costs on your host, `voice-pipes/scripts/idle_soak.py`
leaves a worker untouched for chosen gaps, then compares its first turn with
a warm one, stage by stage, alongside the worker's swapped memory and page
faults; `--squeeze-mb` swaps a worker out on purpose to reproduce the cost
in minutes.

## Authentication

Three bearer tokens, each read from an environment variable:

| Variable | Protects |
|---|---|
| `VP_CLIENT_TOKEN` | The end-user WebSocket transport (`/ws`) |
| `VP_ADMIN_TOKEN` | The orchestrator admin API; the admin BFF presents the same value |
| `VP_INTERNAL_TOKEN` | Worker ↔ orchestrator registration, heartbeats, events and the worker debug API |

**The shipped deploy configs fail closed.** Every pipeline file and the
orchestrator file under `voice-pipes/deploy/docker/config/` carries

```toml
[auth]
required = true          # abort startup if a surface's token variable is unset
```

so a container started from them exits with an error naming the missing
variable rather than serving an open surface. The launcher generates the
three tokens into `voice-pipes/.env` on first run (values are random 32-byte
hex); `docker-compose.yml` passes them through by name to the orchestrator
and every worker. Rotate a token by editing `.env` and restarting the
stack. Worker debug ports (8085 to 8097) are published on `127.0.0.1` only;
`scripts/compose-check.py` checks that, the sidecars' loopback binding, and
that no two services started together publish the same host port.

An unset variable leaves a surface open **only** when `required` is
`false`, which is the right setting for a laptop with everything on
loopback; the process logs a warning for every open surface bound to a
non-loopback address.

The admin console's backend has a fourth, always-required token,
`ADMIN_UI_TOKEN`, which the browser presents to it; see
[Admin console › Authentication](admin-console.md#authentication). Its
compose file publishes ports 8000 and 3000 on loopback only. The browser
never receives `VP_CLIENT_TOKEN`: the Playground reaches token-protected
workers through the backend's `/api/v1/playground/ws` proxy, which connects
only to workers the orchestrator lists and presents the token itself.

The admin backend's CORS origins come from `ADMIN_CORS_ORIGINS`; the default
allows only the localhost console origins, and a wildcard automatically
disables credentialed requests. The orchestrator's `cors_origins` should list
the console's origin explicitly.

## Hardening checklist

Before exposing a worker to untrusted clients:

- Keep `required = true` (the shipped default) and the four tokens set; never
  commit `.env`.
- Put TLS in front of the WebSocket and HTTP ports (a reverse proxy); the
  engine speaks plain WS/HTTP.
- Keep the debug API (`debug_bind = "127.0.0.1"`) off the public interface.
- Size `max_sessions` in `[transport]` and `max_concurrent` in the worker
  file to what the machine can serve; STT and TTS pools are bounded and a
  full pool returns a controlled error rather than queuing forever.
- Decide whether to record at all: `[pipeline.audit] enabled` stores audio of
  both sides for every session under `captures/`. Only the launcher's dev
  profile enables it. If you do record, set `retention_days` (default 7) to
  your policy; workers prune older sessions at startup.
- Keep workers out of swap ([Memory, swap and idle workers](#memory-swap-and-idle-workers)):
  an idle worker whose models were swapped out makes its next caller wait
  seconds.
- Watch `/health` and `/readyz`: `readyz` returns 503 with no active worker.
- Run the [benchmark suite](benchmarking-and-evals.md) against your own
  providers before trusting anyone's latency numbers.

## Running without Docker

A worker is a single binary plus model files. Run `voice-pipes serve` under
your process supervisor with the pipeline and worker files, an orchestrator
next to it if you want the console, and the admin console from its own
compose file or natively (`npm run dev` and `uvicorn`). eSpeak NG must be
installed on the host for Piper and Kokoro.
