# Fleet: orchestrator and workers

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

A **worker** runs one pipeline and accepts sessions. An **orchestrator**
manages many workers: registration, heartbeats, dispatch, drain, aggregated
history and metrics. You can run a worker alone; you need an orchestrator for
more than one worker and for the admin console.

## Standalone worker

```sh
voice-pipes serve --worker-config worker-standalone.toml --config my-agent.toml
```

```toml
# worker-standalone.toml
id = "worker-standalone"
max_concurrent = 10
debug_port = 8085
debug_bind = "127.0.0.1"
# no [orchestrator] section
```

The pipeline file's `[transport]` section opens the WebSocket endpoint
(`/ws`, port 8800 by default) and `GET /health` on the same port reports
component health. The debug API on `debug_port` exposes the worker's own
config, providers, components, local session history and audit files.

| Capability | Standalone worker | With orchestrator |
|---|---|---|
| WebSocket voice pipeline | Yes | Yes |
| Health check | Self only (`/health`, `/debug/health`) | Fleet-wide, plus `/healthz` and `/readyz` probes |
| Session and turn history | Local only | Aggregated across workers, filterable |
| Latency metrics (p50/p90/p99) | — | Yes |
| Worker registry, drain, tags | — | Yes |
| Session dispatch and routing | — | Yes |
| Config grouping and diff by hash | — | Yes |
| Benchmark experiments | — | Yes |
| Admin console | **No** | **Required** |

The admin console talks only to the orchestrator's `/api/v1/*`; it never
calls a worker's `/debug/*` directly. To use it with a single worker, run an
orchestrator anyway.

## Orchestrator plus workers

```sh
# terminal 1
voice-pipes orchestrate --orchestrator-config orchestrator.toml
# terminal 2..n
voice-pipes serve --worker-config worker.toml --config my-agent.toml
```

```toml
# orchestrator.toml
bind = "0.0.0.0"                 # 127.0.0.1 is unreachable from Docker containers
port = 9090
cors_origins = ["http://localhost:3000"]
benchmarks_dir = "benchmarks/"
captures_dir = "captures/"       # canonical captures dir; the admin backend discovers it here

[workers]
heartbeat_timeout_s = 30         # a worker silent this long is marked dead
reap_interval_s = 10

[sessions]
max_entries = 50000

[dispatch]
policy = "least-connections"     # or "round-robin" / "random"
require_pipeline_match = false   # true: only dispatch to workers with the requested config hash

[audit]
shared_dir = "audit/"
```

```toml
# worker.toml
id = "worker-1"
max_concurrent = 10
debug_port = 8085
debug_bind = "127.0.0.1"
tags = ["gpu:a100", "region:eu-west"]

[orchestrator]
endpoint = "http://127.0.0.1:9090"
heartbeat_interval_s = 5
register_timeout_s = 10

[output.chunking]                # optional: fixed-size output frames for finer barge-in
enabled = true
frame_ms = 50
```

On start the worker registers with its capacity, its pipeline config and the
config's hash, then heartbeats every `heartbeat_interval_s` with its active
session count and totals. Completed turns are pushed to the orchestrator as
they finish, which is what the admin console's session history shows.

## Dispatch

A client (or the admin Playground) asks the orchestrator where to connect:

```
GET /api/v1/dispatch?pipeline_name=…&config_hash=…&tags=gpu:a100,region:eu-west
→ { "worker_id": "…", "transport_address": "…", "ws_url": "ws://…/ws" }
```

`least-connections` picks the registered, non-draining worker with the fewest
active sessions among those matching the filters. Tags are exact-match
strings; a request without tags matches every worker.

### Addresses in containers

Two settings exist only for containerised fleets:

- `advertise_address` — the debug-API address the worker registers when its
  bind address is not reachable from the orchestrator (separate containers).
  Use the compose service name: `"worker-custom:8085"`.
- `transport_advertise_address` — the browser-reachable WebSocket address the
  orchestrator hands out from `/dispatch`. Without it, two containerised
  workers publishing different host ports both dispatch to `127.0.0.1:8800`.
  Set it to what the browser can reach, e.g. `"localhost:8801"`.

## Drain and reap

`POST /api/v1/workers/{id}/drain` stops new dispatch to a worker while its
current sessions finish. A worker whose heartbeat is older than
`heartbeat_timeout_s` is marked dead by the reaper and disappears from
dispatch; it re-registers when it comes back. A dead record is removed
`dead_grace_s` (default 600) later, so workers that mint a fresh id per
process do not accumulate; only live records count against
`max_registered`.

Registration is authenticated with the fleet-wide internal token, which
carries no per-worker identity. The orchestrator therefore refuses (409) a
registration that would move a *live* worker's id to a different host; a
worker restarting on a new address must wait to be marked dead or use a
new id. `[workers] allowed_hosts` additionally restricts the hosts any
worker may advertise. Every request between orchestrator and worker is
bounded (3 s connect, 10 s request; `proxy_timeout_s` for the params
proxy), so a peer that accepts a connection and never answers cannot hold
a heartbeat loop or an admin request open.

## Config hash and pipeline groups

Every worker reports a 32-character SHA-256 prefix of its pipeline-relevant
settings. `GET /api/v1/pipelines` groups the fleet by it,
`GET /api/v1/pipelines/diff` shows what differs between two hashes, and
`require_pipeline_match` turns the hash into a dispatch constraint. Tagging
workers by role (`tags`) and grouping by hash together are what the
[benchmark runner](benchmarking-and-evals.md) uses to run two variants side
by side.

## Health

- Worker transport `GET /health`: aggregate health plus one entry per
  component with state and reason. One `curl` reveals a down STT, TTS or LLM
  backend.
- Orchestrator `GET /api/v1/health`: worker counts (active, draining, dead)
  and session totals; `/readyz` returns 503 with no active worker.
- `GET /api/v1/components` and `/api/v1/workers/{id}/components`: the
  component snapshots every heartbeat carries.

## The Docker stack

The compose file under `voice-pipes/deploy/docker/` wires exactly this:
`orchestrator`, `worker-a`/`worker-b` (CPU), `worker-gpu` and the
wizard-driven `worker-custom` / `worker-custom-gpu`, the speech-to-speech
workers, and sidecars (`llama-server`, `pocket-tts`, `vibevoice`) behind
compose profiles. The launcher's `stack-manifest.env` selects profiles and
services generically; see [Deployment](deployment.md).

## Reference

[HTTP API](../reference/http-api.md) lists every orchestrator, worker-debug
and internal route with its fields.
