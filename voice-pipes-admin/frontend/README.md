# Voice Pipes Admin — Frontend

React + TypeScript admin console and playground for Voice Pipes. Built with Vite, React 19, TailwindCSS, ReactFlow, and Recharts.

## Prerequisites

| Dependency | Version | Install |
|------------|---------|---------|
| Node.js | 24 (LTS) or 22.13+, the versions CI tests | `winget install OpenJS.NodeJS.LTS` or https://nodejs.org |

`package.json` declares the range and `.npmrc` sets `engine-strict`, so `npm`
refuses another version with `EBADENGINE`.

```powershell
node --version   # v24.x, or v22.13 or later
```

## Quick Start

To run the whole console (backend included), use `../start-native.ps1` or
`../start-native.sh`; see
[Admin console › Running it](../../docs/guides/admin-console.md#running-it).
The frontend alone:

```bash
npm ci        # exactly what package-lock.json pins; never `npm install` for a run
npm run dev
```

The native dev server runs on port 5173; the Docker Compose stack serves the
production build on port 3000 (see `../docker-compose.yml`).

## Running Tests

Unit tests use [Vitest](https://vitest.dev/) and run entirely in Node — no browser or server required.

```bash
# Run all tests once
npm test

# Watch mode during development
npm run test -- --watch
```

Tests live alongside the source in `src/**/__tests__/` directories. The test suite covers the core word-reveal timing logic (`src/lib/audio/word-timing-table.ts`) that drives the progressive transcript reveal in the Playground. In production it's served from the Docker Compose stack (see `../docker-compose.yml`), which also starts the Python BFF on port 8000.

## Pages

### Dashboard (`/`)

Analytics and KPI overview for the running pipeline.

- **KPI cards** — total sessions, utterances, average latency, p95 latency, tokens/sec
- **Latency charts** — area/line charts with provider-change reference lines for STT, TTS, and end-to-end pipeline latency
- **LLM charts** — tokens-per-second and timing breakdown (time-to-first-byte, time-to-first-token, total)
- **Provider tracking** — visual markers when the STT/TTS provider changes mid-session
- **Custom tooltips** — pipeline name, model, provider, session ID on hover

### Live Pipeline (`/live`)

Interactive ReactFlow diagram of the active pipeline topology.

- **Component cards** — each node shows its **type** (STT, TTS, LLM, VAD) as the top label and the exact provider implementation under "Provider:"
- **Distinct styling by role** — AI model nodes (STT / LLM / TTS) use indigo gradients with an "AI" badge; Input and Output nodes use green and orange gradients respectively; structural nodes (resample) are neutral
- **Sample rate badges** — IN / OUT kHz shown on every node
- **Configuration section** — per-node parameters rendered automatically from metadata
- **Live metrics** — toggle "Show Metrics" to overlay 60-second rolling P50/P90 latencies on STT and TTS nodes, plus an overall pipeline metrics panel (active workers, requests, latency, errors)
- **Resample toggle** — "Show Resamplers" adds or hides resample nodes; edges are automatically re-wired when hidden
- **Toolbar status** — right-aligned status bar shows (left to right): Show Metrics, Show Resamplers, a divider, then a status pill with **worker count** (active/total), **active sessions**, and an **Online / Offline** indicator with green/red dot
- **Double-click zoom** — double-click a node to zoom in; double-click again to fit the full graph
- **Draggable layout** — all nodes can be repositioned interactively

### Playground (`/playground`)

Interactive voice-chat test harness that connects to a pipeline worker over WebSocket.

- **Connection bar** — URL input, connect/disconnect, session + pipeline state indicators
- **Settings panel** (collapsible) — sample rate, channels, language, voice configuration
- **Chat transcript** — conversation history with user/agent roles
- **Audio visualizers** — real-time waveform bars for microphone and playback levels
- **Metrics bar** — TTFT, TTFB, total latency, RTT, interruption status
- **Input bar** — mic toggle, text input, send button
- **Event log** (collapsible right panel) — raw wire-protocol events for debugging
- **Voice and text modes** — supports both streaming audio and text-only interaction

### Sessions (`/sessions`)

Historical session browser with drill-down into individual sessions.

- **Time filters** — Today, Yesterday, Last 7 days, Last 30 days, All
- **Sort order** — Newest / Oldest first
- **Session cards** — session ID, pipeline name, STT provider + audio duration, TTS provider + voice, start timestamp
- **Pagination** — 25 items per page with prev/next controls
- **Session detail view** — audio playback with conversation player (user/AI audio fragments), playlist builder, playback controls (play/pause/stop/skip), LLM prompt and response cards with timing breakdown (stacked bar chart), telemetry panel

## Core Services

- **Transport layer** — `WebSocketAdapter` with typed binary PCM framing (6-byte header) and JSON control envelopes
- **Audio capture** — `AudioCaptureService` using AudioWorklet with ScriptProcessor fallback
- **Audio playback** — `AudioPlaybackService` with level metering at negotiated sample rates
- **Theme** — dark/light mode via React Context (`useTheme` hook), defaults to dark

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Framework | React 19, React Router 7 |
| Bundler | Vite 8 |
| Charting | Recharts 3.8 |
| Graph | ReactFlow 11.11 |
| Data fetching | TanStack React Query 5, Axios |
| Icons | Lucide React |
| Styling | Tailwind CSS 4 |

## Docker

The `docker-compose.yml` in the parent directory starts both the frontend (port 3000) and the Python BFF backend (port 8000). The BFF proxies all `/api/v1/*` requests to the Rust orchestrator at `host.docker.internal:9090` and shares the captures volume for audit file access.

## Playground Extensions

The Playground can host external renderers (3D avatars, viseme meters, custom
visualisers) as ESM modules loaded at runtime — no rebuild, no dependency on
them from this repo. Configure via `/playground-extensions.json` (see
`public/playground-extensions.example.json`) or the build-time env var
`VITE_PLAYGROUND_EXTENSIONS` (comma-separated module URLs). Full contract,
lifecycle and hosting notes: [../docs/PLAYGROUND-EXTENSIONS.md](../../docs/clients/playground-extensions.md).

| Env var | Purpose |
|---|---|
| `VITE_PLAYGROUND_EXTENSIONS` | Comma-separated extension module URLs appended to the manifest (build-time). |

Extensions that load heavy assets can implement `ready()`; Connect/Dispatch then
waits (≤ 10 s, `EXTENSION_READY_TIMEOUT_MS` in `src/pages/playground/extensionRegistry.ts`,
not configurable via env) for mounted extensions before opening the session.

## Transport support

The Playground talks to workers over **WebSocket**. WebRTC and WebTransport
adapters exist as gated stubs (`src/lib/transport/*-adapter.ts`, behind the
`EXPERIMENTAL_TRANSPORTS` flag) but are not implemented and are unreachable
from the UI — WebSocket is the only supported transport today.

## Session Negotiation Protocol

The frontend implements a bidirectional handshake with the Voice Pipes server so both sides agree on audio parameters before streaming begins.

### Flow

```
Browser                                   Server
  │                                         │
  │──── WebSocket connect ─────────────────→│
  │                                         │
  │──── Configure ─────────────────────────→│  client sends preferences
  │     {                                   │
  │       sample_rate: 48000,               │
  │       output_sample_rate: 48000,        │
  │       language: "en-US",                │
  │       voice: "af_heart",               │
  │       app_data: { ... }                 │
  │     }                                   │
  │                                         │  server merges with pipeline caps
  │←─── SessionReady ──────────────────────│
  │     {                                   │
  │       session_id, session_token,        │
  │       negotiated: {                     │
  │         input_sample_rate: 48000,       │
  │         output_sample_rate: 48000,      │
  │         stt_provider: "parakeet-tdt",   │
  │         tts_provider: "kokoro",         │
  │         ...                             │
  │       }                                 │
  │     }                                   │
  │                                         │
  │  capture AudioContext @ input rate       │
  │  playback AudioContext @ output rate     │
  │                                         │
  │──── PCM audio frames ──────────────────→│
  │←─── PCM audio frames ─────────────────│
```

### Client Behaviour

1. **Connect** — `WebSocketAdapter.connect(url, config)` opens the WebSocket and immediately sends a `configure` control message with the session config.

2. **Receive `session_ready`** — the adapter captures `negotiated` from the response and stores it on `WebSocketAdapter.negotiatedConfig`. The `usePlaygroundSession` hook reads `negotiated.output_sample_rate` and recreates the `AudioPlaybackService` at that rate.

3. **Capture** — the `AudioCaptureService` opens an `AudioContext` at the requested input rate (default 48 kHz). If the browser chooses a different rate, a follow-up `configure` message updates the server.

4. **Playback** — the `AudioPlaybackService` is constructed at `negotiated.output_sample_rate` so TTS audio plays at the correct speed.

5. **Duration calculation** — `WebSocketAdapter.handleBinary()` uses `negotiatedConfig.output_sample_rate` (not a hardcoded constant) to compute `durationMs` for audio metadata.

### Key Types

```typescript
// Client → Server (sent immediately after connect)
interface SessionConfig {
  sample_rate?: number;        // input capture rate (default 48000)
  output_sample_rate?: number; // preferred playback rate
  channels?: number;           // mono = 1 (default)
  audio_format?: string;       // "pcm_i16_le" (default)
  language?: string;           // BCP-47 tag, e.g. "en-US"
  voice?: string;              // TTS voice identifier
  app_data?: unknown;          // arbitrary app payload
  options?: Record<string, unknown>;
}

// Server → Client (inside session_ready control message)
interface NegotiatedConfig {
  input_sample_rate: number;   // authoritative capture rate
  output_sample_rate: number;  // authoritative playback rate
  audio_format: string;
  channels: number;
  language?: string;
  voice?: string;
  stt_provider?: string;       // e.g. "parakeet-tdt"
  tts_provider?: string;       // e.g. "kokoro"
  app_data?: unknown;
  options?: Record<string, unknown>;
}
```

### Other Control Messages

| Direction | Action | Purpose |
|-----------|--------|---------|
| Client → Server | `configure` | Declare audio preferences (sent on connect and if mic rate changes) |
| Client → Server | `ping` | Latency probe with `timestamp_ms` |
| Server → Client | `pong` | Echo with `client_timestamp_ms` + `server_timestamp_ms` |
| Server → Client | `state_changed` | Pipeline state: `"listening"`, `"processing"`, `"speaking"` |
| Client → Server | `cancel` | Interrupt in-flight processing/playback |
| Server → Client | `error` | Error with `code` and `message` |
| Server → Client | `closing` | Session ending with `reason` |
| Client → Server | `resume` | Reconnect with `session_token` |

### Binary Audio Frames

Audio uses a compact 6-byte header + raw PCM payload:

```
[msg_type: u8] [flags: u8] [seq_num: u32 LE] [pcm_i16_le samples...]
```

- `msg_type`: `0x01` = PCM i16 audio
- `flags`: bit 0 = end-of-utterance, bit 1 = end-of-response
- `seq_num`: monotonically increasing sequence number

## Project Structure

```
src/
├── hooks/
│   └── usePlaygroundSession.ts   # React hook: transport + audio orchestration
├── lib/
│   ├── audio/
│   │   ├── capture.ts            # AudioWorklet mic capture service
│   │   └── playback.ts           # PCM playback with level metering
│   └── transport/
│       ├── types.ts              # Wire protocol types (mirrors Rust transport types)
│       ├── adapter.ts            # TransportAdapter interface
│       ├── ws-adapter.ts         # WebSocket implementation
│       └── index.ts              # Barrel export + factory
├── pages/
│   ├── Playground.tsx            # Main playground UI
│   └── LivePipeline.tsx          # ReactFlow pipeline diagram
└── ...
```
