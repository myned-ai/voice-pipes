# Pocket-TTS Candle (Native Rust)

`voice-pipes-tts-pocket-candle` is a pure-Rust implementation of **Kyutai Pocket-TTS** (`FlowLM` + LSD flow matching + streaming `Mimi` decoder & encoder) built on Candle (`0.11.0`). It eliminates Python/GIL contention and supports both **in-process execution** (`provider = "pocket-candle"`) and a **standalone horizontally scalable HTTP server** (`pocket-tts-server`) compatible with `provider = "pocket-http"`.

## Architecture Advantages
- **Zero-copy shared voice KV cache**: Precomputed voice prefixes are stored once in `Arc<VoiceKvPrefix>` and attended via online log-sum-exp split attention without cloning KV tensors per session.
- **Pipelined FlowLM ↔ Mimi streaming**: Overlaps FlowLM latent step `t+1` with Mimi 24 kHz waveform decoding of step `t` over a bounded 2-slot channel, emitting native 80 ms (`1920`-sample) frames with a 4 ms raised-cosine fade-in on frame 0.
- **Built-in Mimi encoder**: Supports both precomputed `.safetensors` voices and on-the-fly `.wav` zero-shot voice cloning, cached in a bounded LRU (`max_cached_voices`).
- **Early silent-start guard & instant barge-in**: Detects silent starts inside the autoregressive loop and retries with a fresh deterministic RNG seed; checks `CancellationToken` before every FlowLM step.

## In-Process Configuration (`provider = "pocket-candle"`)

Build with `--features tts-pocket-candle` (add `cuda` for NVIDIA GPU):

```toml
[tts]
provider = "pocket-candle"
model_dir = "models/pocket-tts"
voice = "alba"

# Optional tuning:
# voices_dir = "models/pocket-tts/voices"
# max_concurrent = 4
# lsd_steps = 1
# first_frame_lsd_steps = 1
# temperature = 0.7
# eos_threshold = -4.0
# max_frames = 375
# max_chars = 2000
# max_cached_voices = 32
# silent_start_ms = 1200
# silent_start_peak = 500
# silent_start_retries = 2
# fade_in_ms = 4
# pipeline_mimi = true
```

## Standalone Scalable HTTP Server (`pocket-tts-server`)

For production deployments serving **100s of concurrent users** (or dropping into Hugging Face Spaces as a multi-threaded replacement for Python `pocket-tts serve`):

```sh
cargo build --release -p voice-pipes-tts-pocket-candle --features server
./target/release/pocket-tts-server \
  --host 127.0.0.1 \
  --port 8002 \
  --model-dir /app/models/pocket-tts \
  --voice alba \
  --max-concurrent 4
```

Point `voice-pipes` workers at the server (or Kubernetes `least_conn` Service) using `provider = "pocket-http"`:

```toml
[tts]
provider = "pocket-http"
endpoint = "http://pocket-tts-svc:8002"
voice = "alba"
max_concurrent = 64
```

### Server Endpoints
- `POST /tts` — `application/x-www-form-urlencoded` (`text`, optional `voice_url`) → chunked `audio/wav` (24 kHz mono i16 PCM); cancels synthesis immediately on client disconnect.
- `GET /health` — JSON readiness & active/max concurrency.
- `GET /metrics` — Prometheus exposition (`pocket_tts_active_requests`, `pocket_tts_max_concurrent`, `pocket_tts_requests_total`, `pocket_tts_rejected_total`) for KEDA / HPA autoscaling.

## Production & High-Concurrency (100+ Calls) Sizing Guide

### 1. Critical CPU Tuning: `RAYON_NUM_THREADS=1`
Always set **`RAYON_NUM_THREADS=1`** when running `pocket-candle` or `pocket-tts-server` on CPU in containers or multi-core servers:
- Pocket-TTS FlowLM runs tiny `1×1024` autoregressive matrix multiplies. Letting Rayon fan out across 16–64 cores causes severe futex/barrier thrashing (up to **9× slower**).
- With `RAYON_NUM_THREADS=1`, each active synthesis uses 1 dedicated thread (`~0.26–0.51` RTF on modern x86_64 CPUs) and scales linearly across concurrent sessions without cross-session contention.
- Pair with `VP_ORT_THREADS=1..2` for ONNX STT/VAD on the same host.

### 2. Choosing In-Process vs. Dedicated `pocket-tts-server` Fleet
| Scale | Recommended Topology | Key Settings |
|---|---|---|
| **1–8 concurrent calls** (dev / HF Space / single node) | In-process `provider = "pocket-candle"` inside `voice-pipes` worker | `RAYON_NUM_THREADS=1`, `max_concurrent = 4` (or `vCPUs / 2`), `lsd_steps = 1`, `pipeline_mimi = true` |
| **10–50 concurrent calls** (single large CPU box, e.g. 32–64 vCPU) | Multiple `voice-pipes` workers or 1–2 `pocket-tts-server` instances behind Nginx/Envoy `least_conn` | `RAYON_NUM_THREADS=1`, `max_concurrent = 12..24` per 32-vCPU instance, `worker.max_active_sessions = 50` |
| **100+ concurrent calls** (Kubernetes / production) | Decoupled stateless `pocket-tts-server` Deployment + `voice-pipes` workers using `provider = "pocket-http"` | 8–16 vCPU pods × `max_concurrent = 6..10` per pod; HPA/KEDA autoscale on `pocket_tts_active_requests / pocket_tts_max_concurrent > 0.7`; `pocket-http` `max_concurrent = 64` |

### 3. Why 100 Concurrent Calls Needs Fewer TTS Slots Than You Think
In a real voice conversation, a caller is only **actively synthesizing TTS ~15–25% of the time** (the rest is user speaking, VAD/STT, LLM thinking, and audio playback of already-generated frames because `pocket-candle` runs at `~0.3–0.5` RTF):
- **100 concurrent active calls** ≈ **15–25 simultaneous TTS syntheses** at peak.
- At `RAYON_NUM_THREADS=1` (`~1 vCPU` per active synthesis, `~220 MB` shared model weights + `~6 MB` per cached voice):
  - **One 32-vCPU node** (or **4 × 8-vCPU pods**, `max_concurrent = 8` each) comfortably handles **100+ concurrent voice calls** on CPU with sub-150 ms TTFB and zero client starvation.
- Always pre-stage `models/pocket-tts` on fast local NVMe/emptyDir (not slow network/9p mounts) and enable `keep_warm_interval_s = 120` in production so OS page cache never evicts the 219 MB `model.safetensors`.

### 4. Recommended Production `[tts]` & Load-Test Config
```toml
# Worker side (pointing at a load-balanced pocket-tts-server fleet):
[tts]
provider = "pocket-http"
endpoint = "http://pocket-tts-svc:8002"
voice = "alba"
max_concurrent = 64
```
```sh
# Each pocket-tts-server pod (e.g. 8 vCPU, 2 GiB RAM):
RAYON_NUM_THREADS=1 pocket-tts-server \
  --host 0.0.0.0 \
  --port 8002 \
  --model-dir /models/pocket-tts \
  --voice alba \
  --max-concurrent 8 \
  --max-cached-voices 32
```
