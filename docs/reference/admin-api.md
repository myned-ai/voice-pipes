# Voice-Pipes Admin API — Interface Documentation

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

> **Audience:** Admin console implementor  
> **Architecture:** 3-tier (Orchestrator + Workers + OTel Backend)  
> **Protocol:** HTTP/1.1  
> **Content-Type:** `application/json` unless noted otherwise  

---

## Overview

The admin console interacts with **two** systems:

| System | Base URL | Purpose |
|--------|----------|---------|
| **Orchestrator** | `http://{orch_host}:{orch_port}/api/v1` (default: `http://127.0.0.1:9090/api/v1`) | Fleet management, aggregated sessions, config, audit files |
| **OTel Backend** | Varies (e.g., Prometheus on `:9090`, Tempo on `:3200`) | Historical metrics (Grafana), trace drill-down (Jaeger/Tempo) |

The console **never** connects to workers directly. All worker data flows through the orchestrator or OTel backend.

> **Note:** Workers running in standalone mode (without an `[orchestrator]` section) expose a local `/debug/*` API with health, config, providers, sessions, and audit file access. However, the admin console does not consume these endpoints — the orchestrator is required for the admin UI. See the [engine README](../../voice-pipes/README.md) for a standalone-worker vs. orchestrator capability comparison.

---

## Authentication

Two bearer tokens, on two different surfaces:

| Surface | Header | Token | Notes |
|---|---|---|---|
| Orchestrator admin API (`:9090/api/v1/*`) | `Authorization: Bearer <VP_ADMIN_TOKEN>` | `VP_ADMIN_TOKEN` | Enforced only when the variable is set on the orchestrator; `[auth] required = true` makes an unset variable a startup error. See [Deployment › Authentication](../guides/deployment.md#authentication). |
| Admin backend / BFF (`:8000/api/v1/*`, `/api/v1/stream/*`, `/api/v1/playground/ws`) | `Authorization: Bearer <ADMIN_UI_TOKEN>` | `ADMIN_UI_TOKEN` | Always required unless `ADMIN_UI_AUTH_REQUIRED=false`. Also accepted as the `vp_admin_ui` cookie set by `POST /api/v1/auth/login`, and as a `token` query parameter only where a browser cannot send a header: the WebSocket upgrade, the SSE streams (`/api/v1/stream/*`) and the audio files (`.../audio/...`). Anywhere else a `?token=` is ignored, so the request is refused rather than the token written to access logs. Unauthenticated requests get `401` with the standard error envelope (`code: UNAUTHORIZED`); WebSocket upgrades are closed with code `4401`. `GET /livez`, `GET /api/v1/auth/check`, `POST /api/v1/auth/login` and `POST /api/v1/auth/logout` are exempt. |

Comparison is constant-time on both surfaces. Neither token is ever echoed in a response.

The admin backend refuses a request body over 1 MiB (2 MiB on the worker probe routes,
`POST /api/v1/workers/{id}/probe/{kind}`) with `413` and `code: PAYLOAD_TOO_LARGE`, before the
route reads it.

---

## Common Response Envelope

All JSON endpoints return:

```json
{
  "data": { ... },
  "error": null
}
```

On error:

```json
{
  "data": null,
  "error": {
    "code": "NOT_FOUND",
    "message": "Session req-999 not found"
  }
}
```

HTTP status codes: 200 OK, 400 Bad Request, 404 Not Found, 500 Internal Server Error.

---

## Orchestrator API Endpoints

### 1. Health

#### `GET /api/v1/health`

Returns orchestrator liveness and fleet summary.

**Response** `200 OK`

```json
{
  "data": {
    "status": "healthy",
    "uptime_s": 3612,
    "version": "0.1.0",
    "workers": {
      "total": 3,
      "active": 3,
      "draining": 0,
      "dead": 0
    },
    "sessions": {
      "total_processed": 14230,
      "active": 5,
      "errors_total": 12
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | `"healthy"` or `"degraded"` (degraded if >0 dead workers or no active workers) |
| `uptime_s` | integer | Seconds since orchestrator start |
| `workers` | object | Fleet worker counts by status |
| `sessions` | object | Aggregate session counts |

---

### 2. Pipeline Configuration

#### `GET /api/v1/config`

Returns the orchestrator's own configuration.

**Response** `200 OK`

```json
{
  "data": {
    "port": 9090,
    "bind": "127.0.0.1",
    "cors_origins": ["http://localhost:3000"],
    "workers": {
      "heartbeat_timeout_s": 30,
      "reap_interval_s": 10
    },
    "sessions": {
      "max_entries": 50000,
      "persist": false
    },
    "dispatch": {
      "policy": "least-connections"
    },
    "audit": {
      "shared_dir": "audit/"
    }
  }
}
```

#### `GET /api/v1/config/pipeline`

Returns the common pipeline configuration template that workers run. If workers have heterogeneous configs, returns the config reported by the first registered worker (with a warning).

**Response** `200 OK`

```json
{
  "data": {
    "pipeline": {
      "name": "my-voice-pipeline",
      "log_format": "compact",
      "audit": {
        "enabled": true,
        "dir": "audit/",
        "buffer_size": 256
      },
      "audit_tap": {
        "enabled": true,
        "side": "both",
        "dir": "audit_tap/"
      }
    },
    "stt": {
      "provider": "parakeet-tdt",
      "model_dir": "models/parakeet-tdt",
      "model": null
    },
    "tts": {
      "provider": "piper",
      "model": "models/en_US-lessac-medium.onnx"
    },
    "output": {
      "sample_rate": 48000
    }
  }
}
```

---

### 3. Pipeline Graph

#### `GET /api/v1/pipeline`

Returns a structured graph descriptor of the pipeline, suitable for rendering a visual node-edge diagram in the UI.

**Response** `200 OK`

```json
{
  "data": {
    "name": "my-voice-pipeline",
    "nodes": [
      {
        "id": "wav_input",
        "type": "input",
        "label": "WAV Input",
        "output_sample_rate": null,
        "output_channels": null,
        "metadata": {}
      },
      {
        "id": "vad",
        "type": "vad",
        "label": "VAD (auto)",
        "provider": "auto",
        "metadata": {
          "mode": "auto",
          "model": null,
          "threshold": null
        }
      },
      {
        "id": "stt_resample",
        "type": "resample",
        "label": "Resample → STT",
        "input_sample_rate": null,
        "output_sample_rate": 16000,
        "algorithm": "windowed-sinc",
        "metadata": {
          "sinc_zeros": 8,
          "table_oversampling": 64
        }
      },
      {
        "id": "stt",
        "type": "stt",
        "label": "Parakeet TDT",
        "provider": "parakeet-tdt",
        "expected_sample_rate": 16000,
        "metadata": {
          "model_dir": "models/parakeet-tdt"
        }
      },
      {
        "id": "tts",
        "type": "tts",
        "label": "Piper",
        "provider": "piper",
        "native_sample_rate": 22050,
        "metadata": {
          "model": "models/en_US-lessac-medium.onnx"
        }
      },
      {
        "id": "output_resample",
        "type": "resample",
        "label": "Resample → Output",
        "input_sample_rate": 22050,
        "output_sample_rate": 48000,
        "algorithm": "windowed-sinc",
        "metadata": {}
      },
      {
        "id": "wav_output",
        "type": "output",
        "label": "WAV Output",
        "input_sample_rate": 48000,
        "metadata": {}
      }
    ],
    "edges": [
      { "from": "wav_input", "to": "vad", "data_type": "audio", "sample_rate": null },
      { "from": "vad", "to": "stt_resample", "data_type": "audio", "sample_rate": null },
      { "from": "stt_resample", "to": "stt", "data_type": "audio", "sample_rate": 16000 },
      { "from": "stt", "to": "tts", "data_type": "text", "sample_rate": null },
      { "from": "tts", "to": "output_resample", "data_type": "audio", "sample_rate": 22050 },
      { "from": "output_resample", "to": "wav_output", "data_type": "audio", "sample_rate": 48000 }
    ],
    "audit_tap": {
      "enabled": true,
      "side": "both",
      "tapped_nodes": ["stt_resample", "output_resample"]
    }
  }
}
```

**Node types:**

| Type | Description |
|------|-------------|
| `input` | Audio source (WAV file, microphone, WebSocket) |
| `output` | Audio sink (WAV file, speaker, WebSocket) |
| `vad` | Voice Activity Detection — filters speech from silence before STT |
| `stt` | Speech-to-text provider |
| `tts` | Text-to-speech provider |
| `resample` | Sample rate conversion stage |

**Edge `data_type` values:** `"audio"` (PCM samples flow), `"text"` (transcribed text flows).

**Note on `null` sample rates:** The `wav_input` node's output rate is unknown until a file is actually processed. The graph shows *configured/expected* rates; actual rates for a specific session are in the session detail.

---

### 4. Metrics

#### `GET /api/v1/metrics`

Returns an aggregated metric snapshot computed from session-complete events received from workers. This is a lightweight convenience — for production dashboards, query the OTel backend (Prometheus) directly.

**Query parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `window` | string | `"1m"` | Aggregation window: `"10s"`, `"1m"`, `"5m"`, `"1h"` |

**Response** `200 OK`

```json
{
  "data": {
    "timestamp": "2026-03-29T10:15:30.123Z",
    "window": "1m",
    "workers_active": 3,
    "pipeline": {
      "requests_total": 14230,
      "requests_active": 5,
      "errors_total": 12,
      "latency_ms": {
        "p50": 480,
        "p90": 720,
        "p99": 1100,
        "mean": 530,
        "min": 210,
        "max": 2400
      }
    },
    "stt": {
      "latency_ms": { "p50": 280, "p90": 420, "p99": 680, "mean": 310, "min": 120, "max": 1500 }
    },
    "tts": {
      "latency_ms": { "p50": 180, "p90": 290, "p99": 450, "mean": 210, "min": 80, "max": 900 }
    },
    "audit": {
      "records_total": 14200,
      "records_dropped": 0
    }
  }
}
```

**Histogram fields:** All histogram metrics include `p50`, `p90`, `p99`, `mean`, `min`, `max`.

**Note:** The `stt` and `tts` sections contain only `latency_ms` histograms. Additional fields like `ttfb_ms`, `realtime_factor`, and `audio_duration_s` are available via OTel metrics (Prometheus) but are not aggregated by the orchestrator in v1.

**Note:** These metrics are aggregated across ALL workers. For per-worker breakdown, query Prometheus with `worker_id` label filtering.

---

### 5. Workers

#### `GET /api/v1/workers`

List all registered workers with current status.

**Response** `200 OK`

```json
{
  "data": {
    "total": 3,
    "items": [
      {
        "id": "worker-1",
        "address": "10.0.1.5:8080",
        "registered_at": "2026-03-29T08:00:00.000Z",
        "last_heartbeat": "2026-03-29T10:15:29.000Z",
        "status": "active",
        "capacity": {
          "max_concurrent": 10,
          "active_sessions": 2
        },
        "pipeline_config_hash": "a1b2c3d4",
        "version": "0.1.0"
      },
      {
        "id": "worker-2",
        "address": "10.0.1.6:8080",
        "registered_at": "2026-03-29T08:00:01.000Z",
        "last_heartbeat": "2026-03-29T10:15:28.000Z",
        "status": "active",
        "capacity": {
          "max_concurrent": 10,
          "active_sessions": 3
        },
        "pipeline_config_hash": "a1b2c3d4",
        "version": "0.1.0"
      }
    ]
  }
}
```

#### `GET /api/v1/workers/{id}`

Detail for a single worker including its reported pipeline configuration.

**Response** `200 OK`

```json
{
  "data": {
    "id": "worker-1",
    "address": "10.0.1.5:8080",
    "registered_at": "2026-03-29T08:00:00.000Z",
    "last_heartbeat": "2026-03-29T10:15:29.000Z",
    "status": "active",
    "capacity": {
      "max_concurrent": 10,
      "active_sessions": 2
    },
    "pipeline_config_hash": "a1b2c3d4",
    "pipeline_config": {
      "pipeline": { "name": "my-voice-pipeline" },
      "stt": { "provider": "parakeet-tdt" },
      "tts": { "provider": "piper" },
      "output": { "sample_rate": 48000 }
    },
    "version": "0.1.0",
    "sessions_processed": 4720,
    "errors_total": 3
  }
}
```

**Response** `404 Not Found` — If worker ID does not exist.

#### `POST /api/v1/workers/{id}/drain`

Initiate graceful drain on a worker — it stops accepting new jobs but finishes active sessions. Useful for rolling deployments.

**Response** `200 OK`

```json
{
  "data": {
    "worker_id": "worker-1",
    "status": "draining",
    "active_sessions_remaining": 2
  }
}
```

**Worker status values:**

| Value | Meaning |
|-------|---------|
| `"active"` | Accepting and processing jobs |
| `"draining"` | Finishing active jobs, not accepting new ones |
| `"dead"` | Missed heartbeat timeout — presumed down |

---

### 6. Sessions

#### `GET /api/v1/sessions`

Paginated list of sessions across ALL workers (newest first). This is the centralized session index maintained by the orchestrator.

**Query parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `page` | integer | `1` | Page number (1-based) |
| `per_page` | integer | `50` | Items per page (max 200) |
| `status` | string | (all) | Filter: `"completed"`, `"error"`, `"in_progress"` |
| `since` | string | (none) | ISO 8601 — only sessions after this time |
| `until` | string | (none) | ISO 8601 — only sessions before this time |
| `search` | string | (none) | Full-text search in transcribed text |
| `worker_id` | string | (all) | Filter by specific worker |
| `tenant_id` | string | (none) | Filter sessions by tenant identifier (also accepts `X-Tenant-Id` request header) |

**Response** `200 OK`

```json
{
  "data": {
    "total": 14230,
    "page": 1,
    "per_page": 50,
    "items": [
      {
        "id": "req-14230",
        "worker_id": "worker-2",
        "started_at": "2026-03-29T10:15:30.123Z",
        "completed_at": "2026-03-29T10:15:31.456Z",
        "status": "completed",
        "total_latency_ms": 530,
        "stt_provider": "parakeet-tdt",
        "tts_provider": "piper",
        "text_preview": "Hello world, this is a test of the voice pi..."
      }
    ]
  }
}
```

---

#### `GET /api/v1/sessions/{id}`

Full detail for a single session.

When requested with an `X-Tenant-Id` header or `?tenant_id=` query parameter, resolution is strictly scoped to that tenant's captures partition (`captures/<tenant_id>/<session_id>/`). If the session exists under another tenant, a `404 Not Found` is returned. Unscoped requests (e.g. platform admin) discover sessions across all partitions transparently.

**Response** `200 OK`

```json
{
  "data": {
    "id": "req-42",
    "worker_id": "worker-1",
    "started_at": "2026-03-29T10:15:30.123Z",
    "completed_at": "2026-03-29T10:15:31.456Z",
    "status": "completed",
    "pipeline_name": "my-voice-pipeline",
    "input": {
      "source": "wav_file",
      "path": "hello.wav",
      "sample_rate": 44100,
      "channels": 1,
      "duration_s": 2.1
    },
    "stt": {
      "provider": "parakeet-tdt",
      "input_sample_rate": 16000,
      "resampled_from": 44100,
      "latency_ms": 320,
      "text": "Hello world, this is a test of the voice pipeline."
    },
    "tts": {
      "provider": "piper",
      "latency_ms": 210,
      "native_sample_rate": 22050,
      "output_sample_rate": 48000,
      "resampled": true,
      "audio_duration_s": 1.8
    },
    "total_latency_ms": 530,
    "realtime_factor": 3.96,
    "error": null,
    "otel_trace_id": "4bf92f3577b34da6a3ce929d0e0e4736"
  }
}
```

**Notes:**
- `worker_id` identifies which worker processed this session.
- `otel_trace_id` is the OpenTelemetry trace ID for cross-referencing in Jaeger/Tempo.
- `detail` contains extended information when available; it may be `null` for sessions reported without detail.
- `stt.text` contains the transcribed text. Segment-level timing is not yet included in the session record — use the OTel trace for fine-grained timing.
- Audit file references are not included in the session record — use the audit file endpoints with the session ID filter.

**Response** `404 Not Found`

```json
{
  "data": null,
  "error": {
    "code": "NOT_FOUND",
    "message": "Session req-999 not found"
  }
}
```

---

### 7. Audit Files

#### `GET /api/v1/audit/files`

List audit record files. In shared-storage mode, lists from the shared directory. In proxy mode, aggregates from workers.

**Query parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `page` | integer | `1` | Page number |
| `per_page` | integer | `50` | Items per page (max 200) |
| `session_id` | string | (all) | Filter by session/request ID |
| `type` | string | (all) | Filter: `"txt"`, `"wav"` |

**Response** `200 OK`

```json
{
  "data": {
    "total": 28460,
    "page": 1,
    "per_page": 50,
    "items": [
      {
        "filename": "1711699530123_req-42.txt",
        "size_bytes": 128,
        "type": "txt",
        "session_id": "req-42",
        "worker_id": "worker-1",
        "created_at": "2026-03-29T10:15:31.456Z"
      },
      {
        "filename": "1711699530123_req-42.wav",
        "size_bytes": 84044,
        "type": "wav",
        "session_id": "req-42",
        "worker_id": "worker-1",
        "created_at": "2026-03-29T10:15:31.460Z"
      }
    ]
  }
}
```

---

#### `GET /api/v1/audit/files/{filename}`

Download a specific audit file.

**Path parameters:**
- `filename` — Exact filename (e.g., `1711699530123_req-42.wav`). Must not contain path separators.

**Response** `200 OK`

| File type | Content-Type | Body |
|-----------|-------------|------|
| `.wav` | `audio/wav` | Raw WAV bytes |
| `.txt` | `text/plain; charset=utf-8` | Audit text content |

The response includes `Content-Disposition: inline` so browsers can play audio and display text directly.

**Response** `404 Not Found` — If the file does not exist.

**Response** `400 Bad Request` — If filename contains path traversal characters.

---

#### `GET /api/v1/audit/taps`

List audit tap WAV files (from `AuditTap` resampling snapshots).

**Query parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `page` | integer | `1` | Page number |
| `per_page` | integer | `50` | Items per page (max 200) |
| `label` | string | (all) | Filter: `"stt_in"`, `"tts_out"` |
| `tag` | string | (all) | Filter: `"pre"`, `"post"` |

**Response** `200 OK`

```json
{
  "data": {
    "total": 3400,
    "page": 1,
    "per_page": 50,
    "items": [
      {
        "filename": "0084_stt_in_pre_44100hz.wav",
        "size_bytes": 185000,
        "label": "stt_in",
        "tag": "pre",
        "sample_rate": 44100,
        "sequence": 84,
        "worker_id": "worker-1",
        "created_at": "2026-03-29T10:15:30.125Z"
      },
      {
        "filename": "0084_stt_in_post_16000hz.wav",
        "size_bytes": 67200,
        "label": "stt_in",
        "tag": "post",
        "sample_rate": 16000,
        "sequence": 84,
        "worker_id": "worker-1",
        "created_at": "2026-03-29T10:15:30.128Z"
      }
    ]
  }
}
```

**Parsed filename fields:** The filename pattern `{seq:04}_{label}_{tag}_{rate}hz.wav` is parsed into structured fields for filtering.

---

#### `GET /api/v1/audit/taps/{filename}`

Download a specific audit tap WAV file.

**Response:** Same behavior as `GET /audit/files/{filename}` — returns `audio/wav` with inline disposition.

---

## Worker Debug API

> **Base URL:** `http://{worker_host}:{debug_port}/debug` (default: `http://127.0.0.1:8081/debug`)  
> **Access:** Local only — for ops engineers debugging a specific worker instance. NOT used by the admin console.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/debug/health` | GET | Worker liveness + component readiness |
| `/debug/config` | GET | This worker's resolved PipelineConfig |
| `/debug/providers` | GET | Runtime STT, TTS, and VAD provider info with capabilities |
| `/debug/sessions` | GET | Last N sessions on this worker only (small local ring buffer) |
| `/debug/audit/files` | GET | List local audit files |
| `/debug/audit/files/{filename}` | GET | Serve a local audit file |

These endpoints return the same JSON envelope format as the orchestrator API. No CORS headers, no authentication — strictly local-network access.

### `/debug/health`

Returns worker liveness with per-component readiness, including the VAD backend.

**Response** `200 OK`

```json
{
  "data": {
    "status": "healthy",
    "uptime_s": 3600,
    "version": "0.1.0",
    "components": {
      "stt": { "status": "ready", "provider": "parakeet-tdt" },
      "tts": { "status": "ready", "provider": "piper" },
      "vad": { "status": "ready", "provider": "silero" }
    }
  }
}
```

| Component | Description |
|-----------|-------------|
| `stt` | Speech-to-text engine readiness |
| `tts` | Text-to-speech engine readiness |
| `vad` | Voice Activity Detection backend readiness |

### `/debug/providers`

Returns detailed runtime information about the configured STT, TTS, and VAD providers, including capabilities and sample rates. Useful for the admin console to display provider details without querying each provider directly.

**Response** `200 OK`

```json
{
  "data": {
    "stt": {
      "name": "parakeet-tdt",
      "expected_sample_rate": 16000,
      "capabilities": ["Streaming", "WordTimestamps"]
    },
    "tts": {
      "name": "piper",
      "native_sample_rate": 22050
    },
    "vad": {
      "mode": "silero",
      "backend": "silero",
      "model": "models/silero_vad.onnx",
      "threshold": 0.5,
      "neg_threshold": 0.15,
      "min_speech_ms": 250,
      "min_silence_ms": 100
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `stt.name` | string | Active STT provider name |
| `stt.expected_sample_rate` | integer \| null | Sample rate the STT provider expects (null if unknown) |
| `stt.capabilities` | string[] | List of STT capabilities: `Streaming`, `InterimResults`, `WordTimestamps`, `BuiltinVad`, `EndOfTurn`, `Diarization`, `LanguageDetection`, `CustomVocabulary` |
| `tts.name` | string | Active TTS provider name |
| `tts.native_sample_rate` | integer | TTS provider's native output sample rate |
| `vad.mode` | string | Configured VAD mode: `auto`, `energy`, `silero` |
| `vad.backend` | string | Resolved backend name (e.g., `energy` when `auto` falls back) |
| `vad.model` | string \| null | Path to VAD model file (silero only) |
| `vad.threshold` | float | Speech detection threshold (0.0–1.0) |
| `vad.neg_threshold` | float | Silence detection threshold |
| `vad.min_speech_ms` | integer | Minimum speech duration to trigger |
| `vad.min_silence_ms` | integer | Minimum silence duration to end utterance |

---

## Data Type Reference

### Pagination

All paginated endpoints follow the same pattern:

```json
{
  "total": 14230,
  "page": 1,
  "per_page": 50,
  "items": [ ... ]
}
```

Total page count: `ceil(total / per_page)`.

### Histogram

Latency and duration histograms are returned as:

```json
{
  "p50": 280,
  "p90": 420,
  "p99": 680,
  "mean": 310,
  "min": 120,
  "max": 1500
}
```

All latency values are in milliseconds (integer). Duration values are in seconds (float).

### Timestamps

All timestamps are ISO 8601 UTC with millisecond precision: `"2026-03-29T10:15:30.123Z"`.

### Session Status

| Value | Meaning |
|-------|---------|
| `"in_progress"` | Currently being processed |
| `"completed"` | Finished successfully |
| `"error"` | Failed (see `error` field) |

### Worker Status

| Value | Meaning |
|-------|---------|
| `"active"` | Accepting and processing jobs |
| `"draining"` | Finishing active jobs, not accepting new ones |
| `"dead"` | Missed heartbeat timeout — presumed down |

### Node Types

| Value | Description |
|-------|-------------|
| `"input"` | Audio source |
| `"output"` | Audio sink |
| `"vad"` | Voice Activity Detection |
| `"stt"` | Speech-to-text |
| `"tts"` | Text-to-speech |
| `"resample"` | Sample rate conversion |

### Edge Data Types

| Value | Description |
|-------|-------------|
| `"audio"` | PCM audio samples flowing between nodes |
| `"text"` | Transcribed text string |

---

## CORS

The orchestrator admin API includes CORS headers based on `[orchestrator].cors_origins`. During development:

```toml
[orchestrator]
cors_origins = ["http://localhost:3000"]
```

Allowed methods: `GET`, `POST`, `PATCH`, `PUT`, `DELETE`, `OPTIONS`.  
Allowed headers: `Content-Type`, `Accept`, `Authorization`, `X-Api-Key`.

The CORS layer sits outside the authentication layer, so a browser
preflight (which carries no token) is answered; the actual request still
needs the admin token.

The worker debug API does NOT include CORS headers (local access only).

---

## Error Codes

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `NOT_FOUND` | 404 | Resource does not exist |
| `BAD_REQUEST` | 400 | Invalid query parameters or path traversal attempt |
| `INTERNAL_ERROR` | 500 | Unexpected server error |
| `WORKER_UNAVAILABLE` | 503 | No active workers available for proxy request |

---

## OpenTelemetry Integration Notes

For richer historical data, the admin console can query an OTel backend directly:

- **Traces:** Each session includes an `otel_trace_id` field. Use this to look up the full trace in Jaeger (`/api/traces/{traceID}`) or Tempo (`/api/traces/{traceID}`).
- **Metrics:** If a Prometheus instance scrapes the OTel Collector, query Prometheus for long-term metric aggregation, per-worker breakdown (`worker_id` label), and alerting.
- **Service name:** Workers register as `"voice-pipes"` in OTel with `service.instance.id={worker_id}`.
- **Recommended setup:** OTel Collector (gateway mode) → Tempo (traces) + Prometheus (metrics) → Grafana (dashboards).

The orchestrator's `/api/v1/metrics` provides a lightweight aggregated view suitable for simple setups without a full OTel stack. For production, prefer the OTel backend path.

---

## Admin Backend (BFF) Analytics Endpoints

> **Base URL:** `http://{admin_backend_host}:8000/api/v1`  
> **Authentication:** `Authorization: Bearer <ADMIN_UI_TOKEN>` (or `vp_admin_ui` login cookie)

### `GET /api/v1/analytics`

Computes or serves cached dashboard analytics over the local `captures/` directory (merged with a best-effort orchestrator metrics overlay).

**Query parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `days` | integer | `7` | Window length in days (`1`..`365`) |
| `offset_days` | integer | `0` | Shift the window back by N days (used for prior-period $\Delta\%$ comparisons in production mode) |
| `pipeline` | string | (all) | Comma-separated list of pipeline names to include |
| `lite` | boolean | `false` | When `true` (default in the simplified v1 Dashboard), skips per-session anomaly detection and background SQLite issue persistence for fast initial load |

### `POST /api/v1/analytics/clear`

Resets dashboard metrics globally, for a single pipeline, or for a single session. Records a clear watermark in `ADMIN_STATE_DIR/analytics_cleared.json`, purges matching entries from `rollups.db` and `issues.db`, and invalidates the in-memory analytics cache.

**Request body:**

```json
{
  "pipeline": "my-broken-pipeline",
  "session_id": null,
  "delete_captures": false
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `pipeline` | string \| null | `null` | When set (and `session_id` is null), resets metrics only for this pipeline |
| `session_id` | string \| null | `null` | When set, dismisses this specific session ID from dashboard metrics and failed-session alerts |
| `delete_captures` | boolean | `false` | When `true`, also deletes the matching session capture directories from `captures/` on disk |

**Response** `200 OK`

```json
{
  "data": {
    "cleared": {
      "global_cleared_at": 0.0,
      "pipelines": { "my-broken-pipeline": 1760734800.0 },
      "sessions": []
    },
    "rollups_removed": 3,
    "issues_removed": 1,
    "deleted_captures": 0
  },
  "error": null
}
```

