# High-density CPU deployment (100+ concurrent sessions)

**Status:** active
**Last verified:** 2026-11-10
**Owner:** Docs

This guide covers running **100+ concurrent voice sessions** on CPU-only machines (single host, multi-node worker fleets, or disaggregated containers over gRPC split mode) with any combination of local ONNX/Candle models and Cloud, GPU, or CPU LLMs.

---

## 1. How Voice Pipes scales to 100+ sessions on CPU

In a conversational voice session, a participant speaks ~30–40% of the time and listens or pauses ~60–70% of the time. Voice Pipes exploits this statistical multiplexing so that a compact pool of inference execution contexts serves 100+ open WebSocket connections without loading 100 copies of model weights or holding ONNX/GGUF locks while a user is silent:

1. **Per-step STT slot checkout (`pool_size` << `max_sessions`):** Local ONNX STT providers (`parakeet-tdt`, `moonshine`, `nemotron`, `nemotron35`) hold per-session audio buffers and decoder recurrent tensors on the session task and check out a `PooledSlot` from `SessionPool` **only for the 5–40 ms duration of a `spawn_blocking` inference step**. A pool of `8..=32` ONNX slots serves `100` concurrent WebSocket sessions without `STT_BUSY` starvation.
2. **Process-wide pooled VAD & Turn Detection:** Per-session factories (`silero` VAD, `smart` turn detector, `vap` turn detector) share a process-wide `Arc<SessionPool<...>>` keyed by `(model_path, pool_size, threads)` instead of reading `.onnx` files from disk on every WebSocket handshake.
3. **Bounded intra-op threading (`VP_ORT_THREADS` / `threads`):** Every local ONNX session defaults to `VP_ORT_THREADS` (or `threads` in the stage config) with `with_spinning(false)` when `threads <= 2`, preventing $N_{\text{pool}} \times N_{\text{cores}}$ OpenMP/ORT thread-spinning explosions.
4. **Bounded `espeak-ng` subprocess concurrency (`VP_PHONEMIZE_JOBS`):** Phonemizer-backed TTS providers (`piper`, `kokoro`) gate `espeak-ng` child process spawns behind a process-wide semaphore (defaulting to `min(available_parallelism, 16)`, overridable via `VP_PHONEMIZE_JOBS`).
5. **Pooled GGUF KV-cache slots (`pool_size`):** In-process `llm` (`voice-pipes-llm-gguf`) supports `pool_size` (`1..=32`), creating $N$ independent KV-cache backends so concurrent turns generate tokens in parallel instead of serializing behind a single mutex.
6. **Optimistic fleet dispatch:** The orchestrator's `GET /api/v1/dispatch` optimistically increments `active_sessions` on the chosen worker and uses an atomic round-robin counter, distributing connection bursts evenly across workers between 5-second heartbeats.
7. **Containerized gRPC split-mode multiplexing:** `voice-pipes-component` and `GrpcTextProcessor` multiplex 100+ sessions over shared HTTP/2 channels with keep-alive, comma-separated multi-endpoint client load balancing (`endpoint = "http://stt-1:50051,http://stt-2:50051"`), and per-session conversation history isolation (`x-vp-session-id`).

---

## 2. Universal sizing formulas (any component)

Given a machine with $C$ vCPUs serving $S$ concurrent sessions (e.g., $S = 100$, $C = 64$):

### 2.1 Pool size (`pool_size`)

Let $D$ be the fraction of session time the stage is actively running inference on a speech frame or turn, and $\text{RTF}$ be the model's real-time factor on `threads` cores:

$$\text{Active Concurrent Inferences } \lambda = S \times D \times \text{RTF}$$

$$\text{Recommended } \text{pool\_size} = \lceil \lambda + 3\sqrt{\lambda} \rceil \quad (\text{clamped to } 1..=128)$$

| Stage type | Typical duty cycle ($D \times \text{RTF}$) | `pool_size` for $S = 100$ |
|---|---|---|
| Frame-level VAD (`silero`, 0.2 ms / 32 ms frame) | ~0.006 | `8..=16` |
| Streaming / step-level STT (`moonshine`, `parakeet-tdt`, `nemotron`, `nemotron35`) | ~0.05–0.12 | `12..=24` |
| Utterance-boundary Turn Detection (`smart`) | ~0.01 | `4..=8` |
| Continuous Turn Projection (`vap`, 2 s window) | ~0.08 | `8..=16` |
| Sentence-level TTS (`piper`, `kokoro`) | ~0.04–0.08 | `8..=16` |
| In-process GGUF LLM (`llm`, small CPU model) | ~0.05–0.15 | `4..=8` |

### 2.2 Intra-op threads (`threads` / `VP_ORT_THREADS`)

To prevent OS run-queue saturation under peak load:

$$\sum_{\text{stage}} (\text{pool\_size}_{\text{stage}} \times \text{threads}_{\text{stage}} \times \text{utilization}_{\text{stage}}) \le 0.85 \times C$$

- **Recommended `threads` per ONNX session at high concurrency ($S \ge 50$):** `1` or `2` (`VP_ORT_THREADS=1` or `VP_ORT_THREADS=2`).
- Setting `threads = 1` or `2` automatically disables ONNX Runtime busy-wait thread spinning (`with_spinning(false)`), saving 30–50% CPU under bursty conversational traffic.

---

## 3. Component compatibility & sizing catalog (100 CPU sessions)

### 3.1 Speech-to-Text (`[stt]`)

| Provider | Type | `pool_size` ($S=100$) | `threads` | Notes for 100 CPU sessions |
|---|---|---|---|---|
| `moonshine` | Local ONNX | `12..=16` | `1..=2` | Lowest CPU footprint (~27M params); ideal for 32–64 vCPU hosts |
| `parakeet-tdt` | Local ONNX | `16..=24` | `2` | High accuracy; checks out encoder/decoder slots per step |
| `nemotron` | Local ONNX | `16..=24` | `2` | Cache-aware streaming; per-session `DecoderState` held outside pool |
| `nemotron35` | Local ONNX | `16..=24` | `2` | Per-session `Engine` state decoupled from pooled `Sessions` |
| `assemblyai` / `google` | Cloud | — | — | Zero local inference CPU; ensure outbound TLS connection limits |
| `grpc` | Split-mode | — | — | Offloads STT to `voice-pipes-component` container(s) |

### 3.2 Text-to-Speech (`[tts]`)

| Provider | Type | `pool_size` ($S=100$) | `threads` | Notes for 100 CPU sessions |
|---|---|---|---|---|
| `piper` | Local ONNX + `espeak-ng` | `8..=12` | `2` | Very fast on CPU (RTF ~0.04); set `VP_PHONEMIZE_JOBS=16` |
| `kokoro` | Local ONNX + `espeak-ng` | `12..=16` | `2..=4` | Higher naturalness, heavier CPU (RTF ~0.18); requires 64+ vCPUs or split-mode container |
| `pocket` | Local Candle | — | `RAYON_NUM_THREADS=4` | Pure Rust (no `espeak-ng`); shares weights across sessions |
| `openai` / `gemini` / `elevenlabs` | Cloud | — | — | Zero local TTS CPU |
| `grpc` | Split-mode | — | — | Offloads TTS to `voice-pipes-component` container(s) |

### 3.3 Voice Activity Detection (`[vad]`) & Turn Detection (`[turn_detection]`)

| Component | Mode | `pool_size` ($S=100$) | `threads` | Notes for 100 CPU sessions |
|---|---|---|---|---|
| VAD | `silero` | `8..=16` | `1` | Process-wide pooled ONNX sessions; per-session LSTM state stays on session |
| VAD | `energy` | — | — | Pure Rust RMS calculation; near-zero CPU |
| Turn Detection | `silence` / `layered` | — | — | Timer-based; zero ONNX overhead |
| Turn Detection | `smart` | `4..=8` | `1..=2` | Runs once per end-of-speech candidate; pooled across sessions |
| Turn Detection | `vap` | `8..=16` | `1..=2` | Use a 2 s context export (`vap_min_interval_ms = 200`) on CPU |

### 3.4 Ingress Audio Filter (`[[audio_filter]]`)

| Provider | Mode | Notes for 100 CPU sessions |
|---|---|---|
| `deepfilter` | Pure-Rust DSP (`model` omitted) | Lightweight per-frame spectral subtraction; safe for 100 sessions on 32+ vCPUs |
| `deepfilter` | Full ONNX (`model` set) | Runs 3 ONNX sub-graphs every 10 ms frame per session; reserve for $\le 25$ sessions per worker or dedicated filter hosts |

---

## 4. LLM hosting options for 100 sessions

### Option A: Cloud LLM (`gemini`, `openai`, `anthropic`, `groq` via `openai-compat`)
- **CPU impact on Voice Pipes worker:** Near zero (async HTTP/2 SSE streaming).
- **What to watch:** Vendor RPM/TPM rate limits (~200–400 RPM at 100 active sessions). Configure `timeout_secs = 30` and `max_retries = 2`.

### Option B: Self-hosted GPU LLM (`vLLM`, `SGLang`, `TGI`, `TensorRT-LLM`, `Ollama`)
- **Recommended connection:** `[[text_processor]] type = "openai-compat"` pointing at your GPU server's `/v1` endpoint (`endpoint = "http://gpu-llm.internal:8000/v1"`).
- **Why external GPU server beats in-process LLM at 100 sessions:** Continuous batching (PagedAttention) evaluates 10–15 concurrent user turns in a single GPU forward pass with sub-100 ms TTFT, keeping Voice Pipes workers 100% stateless CPU instances.

### Option C: Self-hosted CPU LLM
- **Recommended (disaggregated):** Run `llama-server` (`llama.cpp`) with `--parallel 8 --cont-batching -t 32` on dedicated high-memory-bandwidth CPU nodes (e.g. AMD EPYC DDR5) and connect via `type = "openai-compat"`.
- **In-process / Component Server (`voice-pipes-llm-gguf`):** Use a compact Q4_K_M model (`SmolLM2-360M` or `Qwen2.5-0.5B/1.5B`) and set `pool_size = 4..=8` in `[[text_processor]]` so multiple KV-cache slots execute concurrent turns in parallel.

---

## 5. Scaling topologies

### 5.1 Single 64-vCPU machine (100 sessions in one worker)

Set environment variables before starting the worker:

```sh
export VP_ORT_THREADS=2
export VP_PHONEMIZE_JOBS=16
export RAYON_NUM_THREADS=4
export OMP_NUM_THREADS=1
export MKL_NUM_THREADS=1
ulimit -n 65535
```

Example `worker-100-cpu.toml`:

```toml
[pipeline]
name = "prod-cpu-100"
log_format = "json"
keep_warm_interval_s = 120

[pipeline.audit]
enabled = false

[transport]
enabled = true
bind = "0.0.0.0"
port = 8800
max_sessions = 100
write_timeout_ms = 5000

[vad]
mode = "silero"
model = "models/silero_vad.onnx"
speech_prob = 0.5

[vad.options]
pool_size = 12
threads = 1

[turn_detection]
mode = "smart"
silence_timeout_ms = 700
smart_model_path = "models/smart_turn_v3.onnx"
smart_pool_size = 6
smart_threads = 1

[stt]
provider = "parakeet-tdt"
model_dir = "models/parakeet-tdt-0.6b-v3"

[stt.options]
pool_size = 16
threads = 2

[[text_processor]]
type = "openai-compat"
endpoint = "https://generativelanguage.googleapis.com/v1beta/openai"
api_key_env = "GEMINI_API_KEY"
model = "gemini-2.5-flash"
timeout_secs = 20
max_retries = 2

[tts]
provider = "piper"
model = "models/en_US-lessac-medium.onnx"

[tts.options]
pool_size = 10
threads = 2
```

### 5.2 Horizontal fleet across $N$ smaller CPU machines

Instead of one 64-vCPU box, you can run **4 × 16-vCPU workers** (`max_sessions = 25` each) registered with `voice-pipes orchestrate`:

- Each worker scales down its pools proportionally (`stt.options.pool_size = 6`, `tts.options.pool_size = 4`, `vad.options.pool_size = 4`).
- Clients either query `GET /api/v1/dispatch?pipeline_name=prod-cpu` (which uses optimistic active-session accounting to spread connection bursts evenly across workers) or connect directly through an L7 WebSocket load balancer (Envoy / AWS ALB / NGINX) in front of the workers.

### 5.3 Disaggregated containers with gRPC split mode

You can run Gateway, STT, TTS, and LLM in independently autoscaled containers using `voice-pipes-component`:

```toml
# gateway.toml (runs VAD + Turn + WebSocket transport, delegates STT/LLM/TTS over gRPC)
[transport]
enabled = true
bind = "0.0.0.0"
port = 8800
max_sessions = 100

[vad]
mode = "silero"
model = "models/silero_vad.onnx"
[vad.options]
pool_size = 12
threads = 1

[stt]
provider = "grpc"
# Comma-separated replica endpoints are load-balanced round-robin over HTTP/2:
endpoint = "http://stt-1:50051,http://stt-2:50051"

[[text_processor]]
type = "grpc"
endpoint = "http://llm-1:50052,http://llm-2:50052"

[tts]
provider = "grpc"
endpoint = "http://tts-1:50053,http://tts-2:50053"
```

> [!IMPORTANT]
> Standard Kubernetes ClusterIP `Service` VIPs balance at **L4 (TCP connection)**, not per HTTP/2 stream. When scaling `voice-pipes-component` containers behind one logical service, either pass a comma-separated list of pod/headless endpoints in `endpoint` (which uses Tonic's built-in `Channel::balance_list` HTTP/2 load balancer) or route through an L7 gRPC proxy (Envoy / Linkerd).
