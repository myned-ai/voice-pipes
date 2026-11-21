# Admission Control, FIFO Waiting Queue & Production Autoscaling

**Status:** active  
**Last verified:** 2026-11-20  
**Owner:** Core Runtime, Orchestrator & Admin Console  

Voice Pipes includes a built-in **Admission Control, FIFO Waiting Queue, and Concurrency Telemetry** subsystem inside the Rust Orchestrator (`voice-pipes orchestrate`). It serves two complementary deployment scenarios with one unified architecture:

1. **Hosted Demos & Shared Sandboxes (e.g. Hugging Face Spaces)**: Protects fixed-capacity CPU/GPU hardware from noisy-neighbor overload by capping simultaneous active voice sessions, limiting concurrent sessions per user (`max_per_tenant`), enforcing an optional maximum call duration (`max_session_duration_s`), and placing extra visitors in a live FIFO waiting room.
2. **Production Kubernetes & Cloud Fleets (KEDA / HPA)**: Acts as a zero-drop traffic shock absorber during traffic spikes while exporting real-time Prometheus `OpenMetrics` (`vp_dispatch_queue_depth`, `vp_fleet_utilization_ratio`) so autoscalers spin up new worker pods dynamically and drain the waiting queue automatically as new workers register.

---

## 1. Configuration (`orchestrator.toml` & Environment Overrides)

Admission control is configured under `[dispatch.admission]` in `orchestrator.toml` and can be overridden at runtime via `VP_ADMISSION_*` environment variables (ideal for Docker Compose, Kubernetes ConfigMaps, and Hugging Face Space Variables):

```toml
[dispatch]
policy = "least-connections"

[dispatch.admission]
enabled = true                   # Env: VP_ADMISSION_ENABLED=true
max_active_sessions = 3          # Env: VP_ADMISSION_MAX_ACTIVE=3 (0 = sum of healthy worker max_concurrent)
max_per_tenant = 1               # Env: VP_ADMISSION_MAX_PER_TENANT=1 (counts active + leased + queued per tenant)
max_queue_size = 15              # Env: VP_ADMISSION_MAX_QUEUE=15 (0 = fast-reject with HTTP 429)
queue_timeout_s = 120            # Env: VP_ADMISSION_QUEUE_TIMEOUT_S=120
lease_ttl_s = 15                 # Env: VP_ADMISSION_LEASE_TTL_S=15
avg_session_duration_hint_s = 90 # Initial ETA estimate per slot
```

### Worker Session Duration Guard (`pipeline.toml`)

To prevent idle or forgotten tabs from holding a demo slot indefinitely, configure `max_session_duration_s` under `[transport]` in your worker pipeline TOML (or set `VP_MAX_SESSION_DURATION_S`):

```toml
[transport]
enabled = true
bind = "0.0.0.0"
port = 8800
max_session_duration_s = 300     # Env: VP_MAX_SESSION_DURATION_S=300 (5 min; 0 = unlimited)
```

When `max_session_duration_s > 0`:
- **30 seconds before expiration**: The worker sends a recoverable `ServerControl::Error` warning (`code = 1008` / `SessionDurationWarning`, `"Demo session ends in 30 seconds."`).
- **At expiration**: The worker sends `ServerControl::Closing { reason: "session_duration_limit" }`, closes the WebSocket, frees the slot, and triggers the orchestrator to promote the next waiting caller in the FIFO queue.

---

## 2. Ticket & Lease Dispatch Protocol

### 2.1 Acquire a Slot or Queue Ticket (`POST /api/v1/dispatch/acquire`)

Request:
```json
{
  "worker_id": "worker-hf-cpu",
  "pipeline_name": "hf-cpu-assistant",
  "tenant_id": "hf-alice"
}
```

- **Immediate Grant (`200 OK`)** — a slot is free and reserved for `expires_in_s` seconds:
  ```json
  {
    "data": {
      "status": "granted",
      "worker_id": "worker-hf-cpu",
      "ws_url": "ws://127.0.0.1:8800/ws",
      "pipeline_name": "hf-cpu-assistant",
      "pipeline_config_hash": "a1b2c3d4",
      "lease_id": "vp_lease_000001_12ab34cd",
      "expires_in_s": 15
    },
    "error": null
  }
  ```
- **Queued in FIFO Line (`202 Accepted`)** — all active slots are busy:
  ```json
  {
    "data": {
      "status": "queued",
      "ticket_id": "vp_ticket_000002_98ef76ab",
      "position": 2,
      "ahead": 1,
      "queue_depth": 2,
      "active_sessions": 3,
      "capacity_sessions": 3,
      "estimated_wait_s": 60
    },
    "error": null
  }
  ```
- **Rejected (`429 Too Many Requests`)** — `error.code` is `"TENANT_LIMIT_REACHED"`, `"QUEUE_FULL"`, or `"CAPACITY_FULL"`.

### 2.2 Long-Poll or Cancel a Ticket
- **Long-Poll**: `GET /api/v1/dispatch/tickets/{ticket_id}?wait_ms=10000` awaits state changes on an event-driven `tokio::sync::watch` channel (zero busy-polling) and returns immediately when the caller's position shifts or transitions to `"granted"`.
- **Cancel / Release**: `DELETE /api/v1/dispatch/tickets/{ticket_id}` cancels a waiting ticket (or releases an unused lease) and immediately advances everyone behind it in line.

---

## 3. Production Autoscaling with Kubernetes & KEDA

Scrape `GET /metrics` on the orchestrator (`:9090/metrics`) with Prometheus or KEDA:

| Metric | Type | Description |
| :--- | :--- | :--- |
| `vp_fleet_active_sessions` | Gauge | Active connected voice sessions across healthy workers. |
| `vp_fleet_leased_sessions` | Gauge | Granted leases currently in flight (awaiting `/ws` upgrade). |
| `vp_fleet_capacity_sessions` | Gauge | Total `max_concurrent` capacity across all healthy workers. |
| `vp_fleet_effective_limit_sessions` | Gauge | Effective session ceiling after applying `max_active_sessions`. |
| `vp_fleet_utilization_ratio` | Gauge | `(active + leased) / effective_limit` (`0.0` to `1.0`). |
| `vp_dispatch_queue_depth` | Gauge | Number of callers currently waiting in the FIFO queue. |
| `vp_active_tenants_total` | Gauge | Distinct tenants holding an active or leased session. |
| `vp_peak_concurrent_sessions{window="1h"\|"24h"}` | Gauge | Rolling 1-hour and 24-hour peak concurrent sessions. |
| `vp_dispatch_rejections_total{reason}` | Counter | Rejections by reason (`queue_full`, `tenant_limit`, `no_workers`, `timeout`). |
| `vp_dispatch_queue_wait_seconds_{sum,count}` | Summary | Cumulative wait time and count of promoted queue tickets. |

### Example KEDA `ScaledObject`

```yaml
apiVersion: keda.sh/v1alpha1
kind: ScaledObject
metadata:
  name: voice-pipes-worker-autoscaler
spec:
  scaleTargetRef:
    name: voice-pipes-worker
  pollingInterval: 5
  cooldownPeriod: 120
  minReplicaCount: 2
  maxReplicaCount: 25
  triggers:
    # 1. Scale up proactively when fleet slot utilization crosses 75%
    - type: prometheus
      metadata:
        serverAddress: http://prometheus.monitoring.svc:9090
        metricName: vp_fleet_utilization_ratio
        query: vp_fleet_utilization_ratio
        threshold: "0.75"
    # 2. Scale up immediately whenever any caller enters the FIFO queue
    - type: prometheus
      metadata:
        serverAddress: http://prometheus.monitoring.svc:9090
        metricName: vp_dispatch_queue_depth
        query: vp_dispatch_queue_depth
        threshold: "1"
```
