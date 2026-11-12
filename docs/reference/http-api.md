# HTTP API

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

The engine exposes four HTTP surfaces: the **worker transport** port (where
clients connect), the **worker debug API**, the **orchestrator admin API**
and the **internal** worker-to-orchestrator API. The admin console's own
backend adds a proxy layer documented in the [Admin API](admin-api.md).

Responses use one envelope:

```json
{ "status": "ok", "data": { … } }
{ "status": "error", "error": "message" }
```

Paginated lists return `{ total, page, per_page, items }`.

## Worker transport (`[transport] port`, default 8800)

| Method | Path | Description |
|---|---|---|
| `GET` | `/ws` (upgrade) | The client WebSocket; see the [wire protocol](../clients/protocol.md). Supports tenant scoping via `X-Tenant-Id` header, `Authorization: Bearer vp_live_<tenant>_*` / `vp_test_<tenant>_*`, or query param `?tenant_id=<id>` |
| `GET` | `/health` | `aggregate_health` plus one entry per component (`id`, `kind`, `state`, `reason`) from the shared registry. One request reveals a down STT, TTS or LLM backend |

## Worker debug API (`debug_port`, default 8085)

| Method | Path | Description |
|---|---|---|
| `GET` | `/debug/health` | Worker health, uptime, per-component status |
| `GET` | `/debug/config` | Full pipeline config as JSON (secrets redacted) |
| `GET` | `/debug/providers` | STT, TTS and VAD provider details and capabilities |
| `GET` | `/debug/components` | Every registered component with counters, latency window and health |
| `PATCH` | `/debug/params` | Live-edit a runtime parameter (VAD thresholds, LLM sampling, …) |
| `GET` | `/debug/sessions` | Local session history with latency metrics (paginated) |
| `GET` | `/debug/audit/files` | Local audit files (paginated) |
| `GET` | `/debug/audit/files/{filename}` | Download one |
| `GET` | `/debug/probe/fixtures` | The recordings shipped with the worker for probing, with the words each one holds |
| `POST` | `/debug/probe/vad` | Run the VAD and segmenter over a recording; reports each utterance found and how much **speech** was discarded before its onset |
| `POST` | `/debug/probe/stt` | Transcribe a recording with the configured recogniser; reports the transcript, processing time and real-time factor |
| `POST` | `/debug/probe/llm` | Send a text through the text-processor chain; reports the reply, time to first token and total time |
| `POST` | `/debug/probe/tts` | Synthesise a text; reports synthesis time and real-time factor, and names an audio file to fetch |
| `GET` | `/debug/probe/audio/{name}` | The WAV a TTS probe produced |

The audio probes take either `?fixture=<name>` or a 16 kHz mono WAV as the
request body (30 s at most); the text probes take JSON (`{"text": …}`, plus
an optional `system_prompt` for the LLM). A probe runs the worker's **real**
component on the worker's own threads, so one runs at a time per worker: a
second concurrent probe is answered `429` rather than queued, and a probe
that runs past two minutes is abandoned. Probes live on the token-gated debug
API and never on the transport port.

## Orchestrator admin API (`port`, default 9090)

### Health

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/health` | `status` (`healthy` / `degraded`), uptime, version, worker counts (`total`, `active`, `draining`, `dead`), session totals |
| `GET` | `/healthz` | Liveness, always 200 |
| `GET` | `/readyz` | Readiness: 200 with at least one active worker, else 503 |

### Configuration

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/config` | Orchestrator configuration (the admin backend reads `captures_dir` here) |
| `GET` | `/api/v1/config/pipeline` | Pipeline config of the first registered worker |

### Workers

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/workers` | All workers: id, address, status, capacity, config hash, pipeline name, version, tags, transport address |
| `GET` | `/api/v1/workers/{id}` | Worker detail including `pipeline_config` |
| `GET` | `/api/v1/workers/{id}/components` | Component snapshots from the last heartbeat |
| `PATCH` | `/api/v1/workers/{id}/params` | Proxy of the worker's live parameter edit |
| `GET` | `/api/v1/workers/{id}/probe/fixtures` | Proxy of the worker's probe fixture list |
| `POST` | `/api/v1/workers/{id}/probe/{kind}` | Proxy of a component probe; `kind` is `vad`, `stt`, `llm` or `tts` |
| `GET` | `/api/v1/workers/{id}/probe/audio/{name}` | Proxy of a TTS probe's audio |
| `POST` | `/api/v1/workers/{id}/drain` | Stop dispatching to this worker |
| `GET` | `/api/v1/components` | Component snapshots across the fleet |

### Sessions and turns

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/sessions` | Paginated; filters `status`, `worker_id`, `pipeline_name`, `config_hash`, `tags`, `tenant_id` |
| `GET` | `/api/v1/sessions/{id}` | Session with turns |
| `GET` | `/api/v1/sessions/{id}/turns` | Turns of a session |
| `GET` | `/api/v1/turns` | All turns (paginated) |
| `GET` | `/api/v1/turns/{id}` | Turn detail: latency breakdown, providers, trace id, events |

### Metrics

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/metrics?window=1m&worker_id=…` | Pipeline request counts and latency histograms (`avg`, `p50`, `p90`, `p99`, `min`, `max`, `count`) for pipeline, STT and TTS, plus audit counters |

### Dispatch and pipelines

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/dispatch?config_hash=…&pipeline_name=…&tags=a,b` | Best worker for a new session: `worker_id`, `transport_address`, `ws_url` |
| `GET` | `/api/v1/pipelines` | Workers grouped by config hash |
| `GET` | `/api/v1/pipelines/diff?a=…&b=…` | Difference between two config hashes |
| `GET` | `/api/v1/pipeline?worker_id=…` | Pipeline graph: nodes, edges, audit taps |

### Audit

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/audit/files` | Audit files (filters `session_id`, `file_type`; paginated) |
| `GET` | `/api/v1/audit/files/{filename}` | Download |
| `GET` | `/api/v1/audit/taps` | Audit tap recordings |
| `GET` | `/api/v1/audit/taps/{filename}` | Download |

### Benchmarks

| Method | Path | Description |
|---|---|---|
| `GET` / `POST` | `/api/v1/benchmark/suites` | List / create test suites |
| `GET` / `PUT` / `DELETE` | `/api/v1/benchmark/suites/{id}` | Read / update / delete a suite |
| `GET` / `POST` | `/api/v1/benchmark/experiments` | List / create experiments |
| `GET` / `DELETE` | `/api/v1/benchmark/experiments/{id}` | Read / delete |
| `POST` | `/api/v1/benchmark/experiments/{id}/run` | Run |
| `GET` | `/api/v1/benchmark/experiments/{id}/results` | Results |
| `GET` | `/api/v1/benchmark/experiments/{eid}/variants/{vid}/config` | Variant config snapshot |
| `GET` | `/api/v1/benchmark/experiments/{eid}/variants/{vid}/scenarios/{sid}/entries/{entry_id}/audio` | Synthesized WAV for a TTS-only corpus entry |
| `GET` | `/api/v1/benchmark/configs` | Available pipeline configs |

See [Benchmarking and evals](../guides/benchmarking-and-evals.md).

## Internal API (worker → orchestrator)

Not for external clients. Authenticated with the internal token when
`[auth]` is configured.

| Method | Path | Body |
|---|---|---|
| `POST` | `/internal/workers/register` | `worker_id`, `address`, `capacity {max_concurrent, active_sessions}`, `pipeline_config_hash`, `pipeline_name`, `pipeline_config`, `version`, `tags`, `transport_address` |
| `POST` | `/internal/workers/heartbeat` | `worker_id`, `active_sessions`, `sessions_processed`, `errors_total`, `components`, `aggregate_health` |
| `POST` | `/internal/workers/deregister` | `worker_id`; sent by a worker shutting down, which is then marked dead at once |
| `POST` | `/internal/events/session` | Session lifecycle event |
| `POST` | `/internal/events/turn` | Completed turn record |

## CORS

`cors_origins` in the orchestrator file: empty or `["*"]` allows any origin;
otherwise only the listed origins. Methods `GET`, `POST`, `OPTIONS`; headers
`Content-Type`, `Accept`.
