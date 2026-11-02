# Performance Tuning, Optimization & Production Hosting Guide

**Status:** active  
**Owner:** Observability & Performance  
**Audience:** Performance Engineers, Voice AI Developers, and DevOps/SREs

Voice Pipes is engineered to support sub-500ms turn latency when paired with local or low-latency streaming providers. Achieving and sustaining this in real-world deployments depends on your chosen models, hardware sizing, configuration parameters, and hosting topology.

This guide provides the authoritative reference for optimizing pipeline responsiveness, eliminating cold-start latency, and deploying robust production fleets.

---

## 1. Where the Milliseconds Go (The Turn Latency Budget)

In conversational voice AI, caller wait time (speech-end to hearing the first agent sound) is the sum of five distinct pipeline stages:

$$\text{Caller Wait} = \text{VAD Silence Timeout} + \text{STT Commit} + \text{LLM TTFT} + \text{TTS Startup} + \text{Audio Transport Send}$$

```
User Stops Speaking
       │
       ▼  [300 - 800 ms]  VAD silence timeout + turn commit
Turn Committed
       │
       ▼  [150 - 400 ms]  Prompt ingest + LLM first token (TTFT)
First Token Produced
       │
       ▼  [80 - 200 ms]   TTS first audio chunk generated (TTS TTFB)
First Audio Chunk Generated
       │
       ▼  [20 - 50 ms]    Audio post-processing + transport WebSocket packet
Audio Reaches Caller's Ear
```

To hit a sub-1000ms end-to-end target, every stage must operate concurrently or speculatively.

---

## 2. Built-in Engine Optimization Techniques

### 2.1 Speculative Execution (`[speculative]`)
In a traditional cascade, the LLM and TTS idle until the user completely stops speaking. Speculative execution overlaps inference with active speech.

* **Partial Transcript Prefill:** As streaming STT partials arrive and stabilize past `min_partial_interval_ms`, the engine launches a background speculative LLM generation against the partial text.
* **Speculative TTS Pre-Synthesis (`speculative_tts = true`):** If the LLM yields a complete initial sentence before the user finishes speaking, Voice Pipes speculatively synthesizes the first audio chunk in advance.
* **Zero-Cost Promotion:** When the turn commits, if the final transcript matches the speculative prefix, the pre-generated tokens and audio chunks are promoted immediately, yielding near-zero caller wait.
* **Safe Memory Rollback:** If the user continues speaking and alters the prompt, the speculative run is immediately canceled and rolled back (`rollback_last_turn`), ensuring conversation history is never polluted.
* **Configuration:**
  ```toml
  [speculative]
  enabled = true
  speculative_tts = true
  min_partial_interval_ms = 150
  ```
* **Metrics to monitor:** `speculative_tokens_generated`, `speculative_tokens_accepted`, `speculative_efficiency`, and `speculative_rollbacks`.

### 2.2 KV-Cache Prefill & History Truncation
* **`prefill_cache`:** For local GGUF models and OpenAI-compatible endpoints (vLLM, llama-server), Voice Pipes preserves the prompt KV cache across turns, eliminating repeated prompt evaluation costs.
* **Barge-in Context Truncation:** When a caller interrupts the agent mid-sentence, Voice Pipes automatically truncates the assistant's previous utterance in conversation memory to only the words that were actually played before playback was cut off (`{played_text}... [interrupted by user]`). This avoids prompt bloat and prevents the model from hallucinating context the caller never heard.

### 2.3 Sentence / Clause-Level Streaming
* The engine does not wait for the LLM to complete generation. It parses tokens incrementally and flushes text to TTS as soon as a punctuation mark (comma, period, question mark, semicolon) or a clause boundary (`first_sentence_chars`) is reached.
* **Silence Padding Trick for TTS:** When using streaming neural TTS models (e.g. Pocket-TTS) that emit small audio frames (80ms), the streaming client injects an imperceptible initial 100ms chunk of silence. This fills client jitter buffers instantly, allowing client audio to start immediately while subsequent neural chunks compute asynchronously.

### 2.4 Anticipatory Turn-Taking (VAP & Smart Turn)
* Traditional silence timeouts (`silence_timeout_ms = 700..1000ms`) force a static delay after every utterance.
* **Smart Turn:** Evaluates semantic completion to detect whether the user asked a complete question or paused mid-thought.
* **Voice Activity Projection (VAP):** Uses continuous acoustic prosody, pitch, and energy modeling (`features/turn-vap.toml`) to predict turn completion ahead of time, reducing endpointing wait by 300–500ms without cutting off slow speakers.

### 2.5 Model Warmup & Swap Prevention (`keep_warm` & `noswap`)
* **The Idle Cold-Start Problem:** When local ONNX/GGUF models sit idle for several minutes, OS kernels swap out unused memory pages. The next caller pays a 2–3 second latency penalty while model weights are paged back in from disk.
* **Keep-Warm Background Pulse:** Configure `[pipeline] keep_warm_interval_s = 60..300`. An idle worker automatically decodes a 1-second burst of silence once per interval to keep weights warm in physical RAM.
* **Docker Memory Locking (`noswap`):** In containerized environments, use `docker-compose.noswap.yml` to enforce equal memory and swap limits (`mem_limit == memswap_limit`) and `mlock` model weights in RAM.

---

## 3. Configuration Tuning Checklist

| Parameter | Default | Tuned for Latency | Trade-off / Note |
|---|---|---|---|
| `[turn_detection] silence_timeout_ms` | `1000` | `500 - 700` | Faster responses, but slow or pausing speakers may be interrupted. |
| `[turn_detection] mode` | `"silence"` | `"smart"` or `"vap"` | Uses neural prosody/semantics to detect turn completion dynamically. |
| `[barge_in] min_chunks` | `3` (~60ms) | `2 - 3` | Lower interrupts faster; higher prevents false triggers from mic clicks. |
| `[barge_in] speech_threshold` | `2000.0` | `1000.0 - 2500.0` | Lower in quiet environments; higher in noisy rooms to avoid self-barge. |
| `[[text_processor]] max_tokens` | `256` | `128 - 180` | Short conversational replies reduce LLM generation time and TTS synthesis duration. |
| `[[text_processor]] temperature` | `0.7` | `0.5 - 0.7` | Lower temperature improves determinism and token predictability. |
| `[output] emit_rtf` | `0.9` | `0.8 - 0.9` | Must stay $\le 0.90$. Values $> 1.0$ indicate server starvation (client clicks/dropouts). |
| `[pipeline] keep_warm_interval_s` | `300` | `60 - 120` | Keeps local model weights warm in RAM during idle lulls. |

---

## 4. Hardware Sizing & Model Selection

### 4.1 CPU Deployments (Edge, Laptop, Standard VM)
* **Speech-to-Text:** Use **Parakeet TDT** (INT8 ONNX, ~600 MB) or **Silero VAD**. Parakeet TDT decodes at $5\times$ to $10\times$ faster than real time on a modern CPU.
* **Language Model:** Use **SmolLM2-360M** or **SmolLM2-1.7B** quantized to **Q4_K_M** via GGUF in-process, or host a local llama-server.
* **Text-to-Speech:** Use **Piper** (`en_US-lessac-medium`, RTF ~0.08) or **Kokoro 82M** (`af_heart`, RTF ~0.15). Both run faster than real time on CPU.
* **Thread Sizing:** Set ONNX intra-op threads to match physical CPU cores (avoid hyperthreading cores for compute-bound matrix math).

### 4.2 GPU Deployments (Cloud Instances, High Concurrency)
* **CUDA Execution Provider:** Export `VP_ORT_EP=cuda` to run ONNX sessions on Tensor Cores.
* **Speech-to-Text:** Use **Moonshine** on CUDA or **Nemotron 3.5**.
* **Text-to-Speech:** Use **VibeVoice** in a CUDA container for studio-quality human prosody.
* **VRAM Allocation:** Ensure total model VRAM (STT + TTS + LLM KV-cache) does not exceed 85% of GPU memory to prevent CUDA out-of-memory errors under concurrent sessions.

---

## 5. Hosting Topologies: Testing to Production

```
┌────────────────────────────────────────────────────────────────────────┐
│                        HOSTING ARCHITECTURES                           │
├───────────────────────┬────────────────────────────────────────────────┤
│ Environment           │ Topology & Setup                               │
├───────────────────────┼────────────────────────────────────────────────┤
│ 1. Local Testing      │ `voice-pipes --config <path> live` (terminal)  │
│                       │ `voice-pipes --config <path> check` (headless) │
├───────────────────────┼────────────────────────────────────────────────┤
│ 2. Automated Staging  │ Headless CI test suite                         │
│                       │ `voice-pipes-bench` regression suite           │
│                       │ Diffing latency p50/p90 percentiles, RTF & WER │
├───────────────────────┼────────────────────────────────────────────────┤
│ 3. Production Fleet   │ Central Orchestrator (`:9090`)                 │
│                       │ Multiple stateless Workers (`:8800..8810`)     │
│                       │ Least-connections dispatch & drain lifecycle   │
│                       │ gRPC split-mode for heavy GPU models           │
│                       │ Docker Compose (`noswap.yml`) or Kubernetes    │
└───────────────────────┴────────────────────────────────────────────────┘
```

### 5.1 Local Development & Fast Verification
1. **Interactive Testing:** Use terminal mode (`voice-pipes live`) for instant mic-to-speaker iteration without starting a web server.
2. **Deterministic Headless Check:**
   ```sh
   voice-pipes --config examples/minimal.toml check
   ```
   Runs known audio waveforms through VAD, STT, LLM, and TTS, verifying that every stage passes SLA targets without needing microphone input.

### 5.2 Staging & Automated Regression Benchmarking
Before deploying a pipeline change:
* Run the automated benchmark suite:
  ```sh
  voice-pipes-bench --config my-pipeline.toml --scenarios scenarios/dialogue.json
  ```
* Evaluates hundreds of multi-turn interactions with synthetic callers, recording latency percentiles (p50, p90, p99), Realtime Factor (RTF), and Word Error Rate (WER).
* Diff results against baseline configs in the Admin Console Benchmarks tab.

### 5.3 Production Fleet Deployment
In production, separate orchestration from worker execution:

1. **Central Orchestrator (`examples/orchestrate/orchestrator.toml`):**
   * Acts as the fleet registry, routing caller WebSocket connections to workers via **least-connections** dispatch.
   * Periodically health-checks workers and supports **graceful drain** (finishing open sessions before taking a worker offline for upgrades).
   * Exposes admin APIs on `:9090` protected by `VP_ADMIN_TOKEN`.
2. **Stateless Workers (`examples/orchestrate/worker.toml`):**
   * Workers register dynamically with the orchestrator over `VP_INTERNAL_TOKEN`.
   * Each worker handles incoming audio on WebSocket `/ws` (`:8800..8810`).
   * Bind debug ports (`:8085..8097`) strictly to `127.0.0.1` or internal container networks.
3. **gRPC Split-Mode for Heavy Models (`examples/grpc/`):**
   * Heavy or crash-prone models (e.g. VibeVoice GPU TTS, Triton, remote Python agents) should run as separate component servers over gRPC.
   * Isolates memory leaks or GPU restarts from the core audio transport worker.
4. **Production Health & Liveness Probes:**
   * **Kubernetes Liveness Probe:** `GET http://<worker-ip>:8800/health` (returns `200 OK` with individual component statuses; non-200 triggers pod restart).
   * **Orchestrator Readiness Probe:** `GET http://<orch-ip>:9090/api/v1/health`.
5. **Fail-Closed Security:**
   * Always set `[auth] required = true` in production configs so containers immediately abort startup if authentication tokens are missing.
