# Multi-Tenancy, Data Isolation & Retention

**Status:** active  
**Last verified:** 2026-11-11  
**Owner:** Core Runtime & Admin Console  

Voice Pipes provides native, production-grade multi-tenancy designed for multi-tenant SaaS deployments, hosted evaluation playgrounds (such as Hugging Face Spaces or web demos), and privacy-regulated environments (HIPAA, GDPR, SOC 2).

Multi-tenancy in Voice Pipes guarantees three foundational properties:
1. **Cryptographic / Path-Safe Data Isolation**: Tenant data (audit captures, audio recordings, SQLite turn indexes) are physically partitioned on disk and in database queries by a validated `TenantId`.
2. **Zero Data Retention (ZDR) Option**: Tenants requiring strict compliance can disable disk persistence of raw microphone audio and synthesized speech while preserving real-time telemetry metrics.
3. **Noisy Neighbor & Quota Protection**: Concurrency and session lifetimes can be capped per tenant to prevent one tenant from monopolizing worker capacity.

---

## 1. Tenant Identification & Ingress Routing

Voice Pipes extracts the tenant identifier from client connections using three complementary mechanisms in precedence order:

```mermaid
flowchart TD
    Client["Client / SDK / Browser"] --> Request["WebSocket Upgrade Request"]
    Request --> CheckHeader{"Header:<br><code>X-Tenant-Id</code>?"}
    CheckHeader --"Yes"--> ValidHeader["Validate TenantId"]
    CheckHeader --"No"--> CheckBearer{"Bearer Token:<br><code>vp_live_&lt;tenant&gt;_*</code>?"}
    CheckBearer --"Yes"--> ValidBearer["Extract & Validate TenantId"]
    CheckBearer --"No"--> CheckQuery{"Query Param:<br><code>?tenant_id=...</code>?"}
    CheckQuery --"Yes"--> ValidQuery["Validate TenantId"]
    CheckQuery --"No"--> DefaultTenant["Unpartitioned / Default Tenant"]
    
    ValidHeader & ValidBearer & ValidQuery --> AttachMetadata["Attach to SessionMetadata"]
    DefaultTenant --> AttachMetadata
```

### Supported Ingress Formats

1. **Explicit HTTP Header**:
   ```http
   GET /ws HTTP/1.1
   Host: worker.voice-pipes.internal:8800
   Upgrade: websocket
   Connection: Upgrade
   X-Tenant-Id: acme-corp
   ```

2. **Bearer API Key Prefix**:
   Standardized API keys with tenant namespaces (`vp_live_<tenant>_<secret>` or `vp_test_<tenant>_<secret>`):
   ```http
   Authorization: Bearer vp_live_acme-corp_9f83a02c...
   ```

3. **Query Parameter (Browser WebSocket Fallback)**:
   For environments where browser `WebSocket` APIs cannot attach custom headers:
   ```
   wss://worker.voice-pipes.internal:8800/ws?tenant_id=acme-corp
   ```

### Path-Safe Identifier Validation

Every tenant identifier is validated against the strict pattern:
`^[A-Za-z0-9][A-Za-z0-9_-]*$` (maximum length 64 bytes).

Any attempt to supply directory traversal patterns (`..`, `/`, `\`), null bytes, or special shell characters is rejected immediately at ingress with HTTP `400 Bad Request`.

---

## 2. Capture Storage Partitioning

When audit capture is enabled (`[pipeline.audit] enabled = true`), session artifacts are automatically partitioned by tenant on disk:

```
captures/
├── acme-corp/
│   └── 3f8a410b-81ea-42b7-a3d8-21d3f9543e01/
│       ├── metadata.json
│       ├── events.jsonl
│       ├── 001_mic_raw.wav
│       └── 001_tts_frag_0.wav
├── globex-industries/
│   └── 7c2a11de-3b4e-4e6f-b148-3a992bc01289/
│       ├── metadata.json
│       └── events.jsonl
└── sess-legacy-flat/           <-- Unscoped / legacy fallback
    ├── metadata.json
    └── events.jsonl
```

### Cross-Tenant Isolation Guarantees
- The admin backend enforces strict tenant scoping: a request with `X-Tenant-Id: acme-corp` querying a session belonging to `globex-industries` returns `404 Not Found`.
- Direct audio streaming routes (`/audio/turn/{utt}/tts_concat.wav`, `/audio/turn/{utt}/as-heard.wav`, etc.) resolve only within the caller's tenant directory.

---

## 3. Zero Data Retention (ZDR)

For privacy-sensitive tenants or publicly accessible shared demos, Voice Pipes supports **Zero Data Retention**:

```toml
[pipeline.audit]
enabled = true
captures_dir = "captures"
zero_data_retention = true  # Audio waveforms are never written to disk
```

When `zero_data_retention = true`:
- **Telemetry Preserved**: Event logs (`events.jsonl`), latency metrics (TTFT, TTFB, RTF), and token usage records are captured.
- **Audio Discarded**: Microphone PCM input (`*_mic_raw.wav`), processed audio (`*_mic_16k.wav`), and synthesized TTS output fragments (`*_tts_frag_*.wav`) are completely dropped at the streaming sink.

---

## 4. Multi-Tenant Retention Sweeping

The background audit retention worker runs at boot and every 6 hours, cleaning expired captures according to retention policy:
- Recursively inspects every tenant subfolder.
- Evaluates the newest modified file in each session against `retention_days`.
- Deletes expired session directories.
- Prunes empty tenant subdirectories once all their sessions have expired.

---

## 5. Orchestrator Session Store Scoping

In fleet deployments with an orchestrator:
- SQLite session metadata includes an indexed `tenant_id` column (`idx_sessions_tenant`).
- Queries to `GET /api/v1/sessions?tenant_id=<id>` filter at the database engine level.
- Orchestrator capture reconciler (`reconcile_sessions_from_captures`) walks both flat and tenant-partitioned capture directories, recovering tenant metadata automatically if the database was recreated.
