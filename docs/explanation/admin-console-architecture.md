# Voice-Pipes Studio: Reference Architecture & Design

Status: active
Owner: Admin Backend + Frontend
Last reviewed: 2026-05-11
Source of truth: yes (admin architecture)

This document outlines the technical architecture, data flow, and design decisions for **Voice-Pipes Studio** (the administrative and observability console for the Voice-Pipes distributed voice AI pipeline). It serves as a historical reference to prevent regression bugs as the system scales.

---

## 1. High-Level Architecture (The 4-Tier Model)

The ecosystem is divided into four distinct planes to ensure horizontal scalability and strict separation of concerns:

1. **Frontend (React SPA):** The user-facing dashboard visualizing the Live Node Graph and Real-time Telemetry.
2. **BFF / API Gateway (FastAPI):** A lightweight Backend-For-Frontend that runs inside Docker. It proxies cross-origin requests securely to the backend.
3. **Control Plane (Rust Orchestrator):** The brain of the API. Maintains the worker registry, calculates cluster-wide P50/P90 latencies, and routes new voice turns.
4. **Data Plane (Rust Worker Nodes):** GPU/CPU-heavy nodes that load the actual ONNX/GGUF AI models (Llama 3.2, Piper, Parakeet-TDT).

---

## 2. Infrastructure & Port Mapping

To prevent networking crashes (especially when crossing the Docker boundary), the system follows strict port and interface binding rules:

* **React Frontend:** `http://localhost:5173` (Local Dev) or Docker mapped port.
* **FastAPI Backend:** `:8000` inside Docker, mapped to `http://localhost:8000`.
* **Rust Orchestrator (`cargo run -- orchestrate`):** `0.0.0.0:9090`
  * *Critical Rule:* Must bind to `0.0.0.0` (using an `orchestrator.toml` override). If it binds to `127.0.0.1`, the Docker containers (`host.docker.internal`) will be unable to route traffic to it.
* **Rust Workers (`cargo run -- serve`):** `127.0.0.1:8085`
  * Workers register dynamically, but port collisions (`os error 10048`) are common if zombie processes persist. Explicitly define unique `debug_port` values in a `worker.toml` config for multiple nodes.

---

## 3. Data Flow & Communication Lifecycle

### A. Worker Registration & Heartbeats
1. A Worker is started via CLI, loading the heavy AI pipeline config into RAM (GPU/CUDA).
2. A background `tokio::spawn` thread initializes a `reqwest` HTTP client.
3. It POSTs its capacity (`WorkerCapacity` struct) and physical graph implementation (`pipeline_config`) to `http://127.0.0.1:9090/internal/workers/register`.
4. Upon successful registration, the worker switches to sending an active heartbeat every 5 seconds to `/internal/workers/heartbeat`.

*Regression risk:* If the `WorkerCapacity` signature structurally changes (e.g. `max_concurrent: u32` vs the inner struct mapping), the HTTP payload serialization will fail, and workers will silently drop off the network.

### B. UI Graph Rendering & Telemetry
1. **On Mount:** `App.tsx` fetches `GET /api/v1/health` and `GET /api/v1/pipeline`.
2. **Graph Assembly:** The Orchestrator extracts the `pipeline_config` from the primary registered worker and maps it into a sequential logical graph (Mic -> VAD -> STT -> LLM -> TTS -> Speaker).
3. **Visual Rendering:** The frontend passes these into `ReactFlow`. We use custom layouts and specific badges for standard components (`CustomPipelineNode`).
4. **Metrics Polling:** When toggled, the React UI uses a `setInterval` of 5000ms to poll `/api/v1/metrics`. Data is piped down into individual Node states to alter UI colors and display P50/P90 numbers without re-rendering the entire canvas geometry.

---

## 4. Frontend Component Design (`App.tsx`)

* **React Flow Initialization:**
  `nodeTypes` holds the reference to the `CustomPipelineNode`. 
  *Regression risk:* React Flow will throw `[React Flow]: It looks like you've created a new nodeTypes...` (Error #002) if this object loses strict reference equality during Hot Module Reloading (HMR). 
  *Fix:* It must be wrapped in `useMemo(() => ({ custom: CustomPipelineNode }), [])` or declared statically outside the main App component.
* **Theme System:** Defaults to Dark Mode. Handled via React Context (`ThemeContext`). UI styling is deeply integrated with Tailwind-like inline shades (`#1e293b`, `#334155`).
* **Icons:** Dynamic rendering via `lucide-react`. The `iconMap` safely matches pipeline string component types (`"stt"`, `"llm"`) to corresponding SVGs (`Activity`, `BrainCircuit`).

---

## 5. System Config Examples

### Worker Node Override (`worker.toml`)
Must be passed alongside the main pipeline config to connect the data plane to the control plane.
```toml
debug_port = 8085
[orchestrator]
endpoint = "http://127.0.0.1:9090"
```

### Orchestrator Node Override (`orchestrator.toml`)
Must be passed to expose the orchestrator to Docker.
```toml
bind = "0.0.0.0"
port = 9090
```

## 6. Known Edge Cases to Monitor
1. **Zombie Processes:** Cargo commands backgrounded in non-interactive terminals can leave locks on `target/debug`. Always sweep with `Stop-Process -Name "voice-pipes"` before clean restarts.
2. **Disconnected Pipeline:** If the orchestrator shows 0 workers, the FastAPI proxy handles this gracefully via fallback mock data, simulating a live environment so UI engineers can continue working. Ensure `/api/v1/workers` is actively queried to debug connection failures.

---

## 7. Playground Architecture

The `/playground` page is a real-time interactive testing console that connects directly to the orchestrator transport endpoint (`ws://localhost:8800/ws`) via WebSocket. It does **not** go through the FastAPI BFF — this avoids an extra proxy hop and makes latency measurements more accurate.

### 7.1 Layer overview

```
Playground.tsx (page)
    │
    ├── session prop (PlaygroundSession)
    │       │
    │       └── usePlaygroundSession (hook in App.tsx — persists across navigation)
    │               ├── WebSocketAdapter          lib/transport/ws-adapter.ts
    │               │     └── ClockSync           NTP-lite Ping/Pong clock offset
    │               ├── AudioCaptureService       lib/audio/capture.ts
    │               ├── AudioPlaybackService      lib/audio/playback.ts
    │               └── CaptionSync               lib/audio/caption-sync.ts
    │
    └── components/playground/
            ├── ConnectionBar.tsx    (URL, pipeline selector, mode, state badge, clear)
            ├── SettingsPanel.tsx    (sample rate, voice, scheduling options)
            ├── ChatTranscript.tsx   (messages, interrupted dividers, karaoke highlights)
            ├── EventLog.tsx         (wire-frame log, filters, grouping)
            ├── AudioVisualiser.tsx  (RMS bar waveform)
            └── InputBar.tsx         (mic toggle, text input, send, disconnect)
```

### 7.2 Barge-in / interruption

Interruption is the most latency-sensitive path and has **two distinct triggers** depending on input mode.

#### The core timing problem

The Web Audio API scheduler buffers 2–4 s of audio ahead of the actual speaker output. The server sends `state → listening` when it finishes *sending* audio bytes — not when the client finishes *playing* them. This creates a window where:
- `pipelineStateRef.current === "listening"` (server is idle)
- `scheduledAheadMs > 0` (client still has seconds of audio queued)

Without explicit handling both paths below will play the old audio to completion while the new response starts queuing on top of it.

#### Path A — Text input (`sendText`)

Triggered when the user types a message while audio is playing.

1. **Active check**: `isActive = (pipelineState === "speaking" || pipelineState === "processing") || scheduledAheadMs > 0`. The `scheduledAheadMs` term covers the "server done, client still playing" gap.
2. **Conditional server cancel**: `ClientControl::cancel` is only sent when the server is active (`speaking | processing`). When the server is already `listening`, cancel is skipped — there is no active turn to cancel.
3. **Stop RAF + capture word index**: `stopProgressRaf()` halts the word-reveal loop; `highlightInfoRef.current` holds the last revealed `wordIndex` synchronously.
4. **Transcript trim**: The active agent entry is sliced to `wordIndex + 1` words so only what the user actually heard remains visible.
5. **Local flush**: `playbackRef.current.flush()` stops all `AudioBufferSourceNode`s immediately and returns `playedMs`.
6. **`preFlushPlayedMsRef`**: Captures `playedMs` *before* flush. The `clear_audio` handler uses this value in `PlaybackReport` — a post-flush call to `flush()` would return 0.
7. **`flushedUtteranceRef = utteranceIdRef`**: Marks the current utterance as flushed so stray in-flight audio frames are dropped.
8. **`clear_audio` response**: Server sends `clear_audio` after processing the cancel. The handler adds a `type:"interrupted"` entry → orange ⚡ divider in transcript.

#### Path B — Voice input (VAD-triggered)

Triggered when the user speaks while audio is playing. The server VAD detects end-of-speech and transitions: `speaking → listening → processing`. The client never sends a cancel — the server is already idle.

1. **Trigger point**: `state → processing` arriving when `prevState === "listening"` AND `scheduledAheadMs > 0`.
2. **Stop RAF + capture word index**: `stopProgressRaf()` halts the word-reveal loop; `highlightInfoRef.current` holds the last revealed `wordIndex`.
3. **Transcript trim**: The active agent entry is sliced to `wordIndex + 1` words.
4. **Local flush**: Same `flush()` call and `preFlushPlayedMsRef` capture as Path A.
5. **Utterance ID advance** *(critical)*: `utteranceIdRef.current++` and `flushedUtteranceRef.current = -1` are set **immediately at flush time**, not waiting for `state → listening`. Without this, `flushedUtteranceRef === utteranceIdRef` for the entire new response and every incoming audio frame for the new turn is silently dropped.
6. **`receivedFirstAudioRef` reset**: Cleared so the `audio_receive` timing event fires correctly on the new turn's first frame.
7. **`beginUtterance(utteranceIdRef.current)`**: Notifies the playback service of the new utterance ID.
8. **Interrupted divider**: Added directly in the state-change handler (no `clear_audio` from server — it never sent one because it wasn't active).
9. **`state → listening`** (arrives after the new response finishes): Increments `utteranceIdRef` again — this double-increment is harmless because `flushedUtteranceRef` is already `-1`.

#### Navigate-away

`Playground.tsx` unmount effect calls `session.flushLocalAudio()`, stopping audio without cancelling the server pipeline. The WebSocket session stays alive. `flushLocalAudio()` also sets `flushedUtteranceRef = utteranceIdRef` so frames that arrive while the user is on another page are dropped cleanly.

### 7.3 Session persistence

`usePlaygroundSession` lives in `AppContent` (`App.tsx`) and is passed as a `session` prop to `Playground.tsx`. This means the WebSocket connection, transcript, events, and metrics all survive navigation to other pages and back.

*Regression risk:* Ref state can become stale across reconnects. `connect()` resets all interrupt refs (`pipelineStateRef`, `utteranceIdRef`, `flushedUtteranceRef`, etc.) and clears the old ping interval before each new session. `onStateChange("disconnected")` also resets `pipelineStateRef` to `null`.

### 7.4 Latency measurement

| Metric | How measured |
|--------|-------------|
| RTT | Ping/Pong round-trip; rolling median via `ClockSync` |
| TTFT | Server-reported: first token from LLM |
| TTFB | Server-reported: first audio byte from TTS |
| STT | Server-reported: VAD end → transcript |
| LLM | Server-reported: transcript → first token |
| TTS TTFB | Server-reported: first token → first audio byte |
| Client E2E | `speech_end` (client) → first `audio` frame received |

All server metrics arrive in `TurnMetrics` messages and are accumulated in `turnMetrics` state, surfaced in the Metrics panel.

### 7.5 Progressive word reveal

The transcript shows only the words the user has actually *heard* during playback, not the full LLM output. This prevents unheard text from appearing in the transcript when a response is interrupted.

#### Why caption-based approaches don't work

The Rust server does **not** emit `audio_meta` or caption sideband frames. Searching the full transport layer (`src/transport/ws/codec.rs`, `src/transport/serve/`) confirms zero matches for `audio_meta`. This means `CaptionSync.addSentence()` is never called and `onWordHighlight` never fires.

#### Actual mechanism — RAF proportional estimator

A `requestAnimationFrame` loop in `usePlaygroundSession` runs while a turn is active:

```
fraction  = getPlayedDurationMs() / totalEnqueuedDurationMs   (clamped 0..1)
wordIndex = min(wordCount-1, floor(fraction × wordCount))
→ setHighlightInfo({ entryIndex, wordIndex })
```

- `getPlayedDurationMs()` uses `audioCtx.currentTime - utteranceStartTime`, clamped to `lastScheduledEndTime` — tracks actual speaker output, not buffered-ahead scheduling time.
- `totalEnqueuedDurationMs` grows as audio chunks arrive and resets on `beginUtterance()`.
- The loop starts in `onPlaybackStart` (first audio chunk scheduled) and also restarts if the `"text"` message arrives after audio has already begun.
- `highlightInfoRef` mirrors the `highlightInfo` state so interrupt callbacks can read the last word index synchronously without a stale-closure problem.

#### Progressive reveal in `ChatTranscript`

`ChatTranscript.tsx` has three rendering branches for agent entries:

| Condition | Render |
|-----------|--------|
| `highlightInfo?.entryIndex === i` | Progressive reveal: tokens beyond `wordIndex + 1` hidden via `return null` |
| `speakingEntryIndex === i && !highlightInfo` | Loading dots (audio not yet started for this entry) |
| Neither | Full text (turn complete) |

#### Trim on interrupt

In all three interrupt paths, just before flushing audio:
1. `stopProgressRaf()` — halts the loop
2. `heardWordCount = highlightInfoRef.current.wordIndex + 1`
3. `trimmedText = words.slice(0, heardWordCount).join(" ")`
4. Transcript entry replaced with `trimmedText`

If no words were revealed yet (`highlightInfoRef.current` is null — interruption before first audio frame), the entry is left unchanged.