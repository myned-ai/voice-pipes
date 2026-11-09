# Voice Pipes — Agent Instructions (AGENTS.md)

This file provides system instructions for AI coding assistants (Claude Code, Cursor, Codex, Copilot, Antigravity, Gemini CLI) working on or with the Voice Pipes codebase.

---

## 1. Project Overview & Architecture

Voice Pipes is an ultra-low-latency, modular conversational voice pipeline runtime written in Rust. It orchestrates real-time speech over WebSockets with sub-500ms glass-to-glass latency, immediate audio interruption (barge-in), and modular plugin architecture.

### Workspace Structure
- **Core Engine (`voice-pipes`)**: Pipeline orchestration, transport, VAD composition, session loop, and audio routing.
- **Plugin SDK (`voice-pipes-sdk`)**: SemVer-stable SDK (1.x). All plugins implement traits from this crate.
- **Shared Crates**:
  - `voice-pipes-net`: Shared network client, TLS, and gRPC connection management.
  - `voice-pipes-onnx`: Shared ONNX session pools, CUDA detection, and model loading.
  - `voice-pipes-bench`: Automated latency, RTF, and WER benchmarking suite.
  - `voice-pipes-cli`: Command-line interface and configuration wizard.
  - `voice-pipes-gym`: Reinforcement-learning environment and regression harness for voice agents.
- **Plugin Crates (20 crates in `plugins/{stt,tts,llm,s2s,turn,vad}/`)**:
  - STT: `voice-pipes-stt-parakeet`, `voice-pipes-stt-nemotron`, `voice-pipes-stt-nemotron35`, `voice-pipes-stt-moonshine`, `voice-pipes-stt-assemblyai`, `voice-pipes-stt-google`.
  - TTS: `voice-pipes-tts-kokoro`, `voice-pipes-tts-piper`, `voice-pipes-tts-pocket`, `voice-pipes-tts-openai`, `voice-pipes-tts-gemini`, `voice-pipes-tts-elevenlabs`.
  - LLM: `voice-pipes-llm-openai-compat`, `voice-pipes-llm-gguf`.
  - S2S: `voice-pipes-s2s-openai`, `voice-pipes-s2s-gemini`, `voice-pipes-s2s-elevenlabs`.
  - VAD / Turn: `voice-pipes-vad-silero`, `voice-pipes-turn-smart`, `voice-pipes-turn-vap`.
- **Admin Console (`voice-pipes-admin`)**:
  - `backend/`: FastAPI application (Python 3.12+), Server-Sent Events fleet stream, and WebSocket playground proxy.
  - `frontend/`: React 18 + Vite + TypeScript dashboard, live audio player, karaoke transcript reveal, and session inspector.

---

## 2. Invariants & Rules (Strictly Enforced)

1. **Memory Safety**: Every Rust crate in the workspace must have `#![forbid(unsafe_code)]`. Never suggest or use `unsafe`.
2. **Clippy & Linter Policy**: The workspace compiles with `-D warnings`. Zero warnings allowed.
   - Non-test code must deny indexing slicing and unwrap:
     `clippy::indexing_slicing`, `clippy::unwrap_used`, `clippy::expect_used`, `clippy::cast_possible_truncation`.
   - Use inlined format arguments: `format!("{variable}")` instead of `format!("{}", variable)`.
3. **SDK Isolation**: Plugins must depend ONLY on `voice-pipes-sdk`. They must NEVER import internal crates (`voice-pipes`).
4. **Subprocess Safety**:
   - If invoking external binaries (e.g., `espeak-ng`), sanitize all arguments against a strict whitelist regex. Never pass raw user strings directly as command-line options.
   - Always enforce an execution timeout with child process termination to prevent threadpool exhaustion.
5. **Observability & Privacy**:
   - Never log raw audio frames or transcripts at `INFO` or `WARN` levels (`log_lines_carry_no_payload` rule).
   - Use tracing spans with typed fields: `#[instrument(skip_all, fields(...))]`.
6. **Audio Formatting**: Voice Pipes standardizes on **16 kHz 16-bit mono PCM** (`TARGET_SAMPLE_RATE = 16_000`). Resample before feeding internal pipeline buffers.
7. **Metric Integrity** (per-component observability is the product; a wrong metric is a bug):
   - One definition per metric name — start anchor, end anchor, and what is inside it (queue/lock wait, connect, prompt ingest, retries) — in `docs/reference/metrics-glossary.md`, the same for every provider.
   - Critical-path latency is measured by the engine at the trait seam with one clock for every provider; a provider-reported number is a breakdown that must fit inside that span, never a replacement for it.
   - Missing is `null`, never `0`. An impossible value is a measurement fault the report flags as such, not slowness charged to the next component.
   - A new provider or metric ships with a conformance test whose deliberately slow fake (connect, prompt ingest, first chunk) proves the delay shows up in the metric.
   - After changing a pipeline or provider, check one real session report against its raw events before calling the work done.

---

## 3. Essential Commands

### Build & Lint
```sh
# Workspace compile check across all 27 crates
cargo check --workspace

# Workspace clippy (must pass with 0 warnings)
cargo clippy --workspace -- -D warnings

# Security & supply chain audit
cargo deny check

# Architectural boundary & invariant tests (51 rules)
cargo test -p voice-pipes-cli --test workspace_rules

# Quickstart commands, example configs, and runtime readiness/skip report
cargo test -p voice-pipes-cli --test quickstart_and_examples -- --nocapture
```

### Admin Backend (Python)
```sh
cd voice-pipes-admin/backend
# Run the full unit & integration test suite
# Linux / macOS
PYTHONPATH=. python3 -m unittest discover -s app -p "test_*.py" -t .
# Windows PowerShell
$env:PYTHONPATH="."; py -3.12 -m unittest discover -s app -p "test_*.py" -t .
```

### Admin Frontend (React / Vite)
```sh
cd voice-pipes-admin/frontend
npm run lint
npx tsc -b
npm test
```

### Launch Stack Locally
```sh
# Linux / macOS
./launch-stack.sh

# Windows PowerShell
.\launch-stack.ps1
```

---

## 4. How to Write a New Plugin (Step-by-Step)

When implementing a new AI model provider (e.g. STT, TTS, or LLM):

1. **Create the Crate**:
   Create a new crate `plugins/<stage>/<name>` (package name `voice-pipes-<stage>-<name>`) in the workspace.
   In `Cargo.toml`:
   ```toml
   [dependencies]
   voice-pipes-sdk.workspace = true
   async-trait.workspace = true
   tracing.workspace = true
   # runtime dependencies (e.g., ort, reqwest, candle)
   ```

2. **Implement the Stage Trait**:
   Implement `SttProvider`, `TtsProvider`, `TextProcessor`, or `VadEngine`.
   Always implement `as_component(&self) -> Option<Arc<dyn Component>>`:
   ```rust
   fn as_component(&self) -> Option<Arc<dyn Component>> {
       Some(Arc::new(CoreComponent::new(Arc::clone(&self.core))))
   }
   ```

3. **Record Telemetry on Hot Paths**:
   Inside inference methods, update `self.core`:
   - Success: `self.core.record_ok(elapsed);`
   - Failure: `self.core.record_error(&err);`

4. **Implement the Factory Trait**:
   Implement `SttFactory` (or `TtsFactory`, etc.) with `validate()` and `build()`.

5. **Expose Registration**:
   Provide a public registration function:
   ```rust
   pub fn register() -> Result<(), ConfigError> {
       voice_pipes_sdk::stt::register_stt_factory("my-provider", Arc::new(MyFactory))
   }
   ```

---

## 5. Troubleshooting & Common Pitfalls

- **WebSocket Close Code 4400**: Missing required query parameters (e.g. `worker_id` when accessing the playground proxy).
- **WebSocket Close Code 4403**: The requested `worker_id` is not registered in the orchestrator catalog.
- **WebSocket Close Code 4502**: Target worker process is unreachable; verify worker transport bind address and network connectivity.
- **SSE Stream Freezing (GZip Buffering)**: a gzipped `text/event-stream` freezes in the browser (zlib buffers the small frames). The admin backend uses Starlette's own `GZipMiddleware`, which excludes `text/event-stream` by default; do not add a custom gzip middleware or an SSE exemption of your own. Serve every SSE route with `media_type="text/event-stream"`. `app/test_sse_not_compressed.py` pins both halves (large JSON gzipped, SSE not).
- **STT Recovery Cascades**: Always respect `RecognitionRequest.slot_wait` when checking out sessions from `SessionPool` (`pool.checkout_within(wait)`).
- **Model Cold-Start Latency Spike**: Local ONNX models (2+ GB) may be paged out by the OS during idle periods. Configure `keep_warm_interval_s = 60..300` in deployment configurations.
