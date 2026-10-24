# Playground Extensions

**Status:** active
**Last verified:** 2026-09-06 (moved into the public tree; content unchanged)
**Owner:** Docs

The admin Playground can host **external renderers and visualisers** — 3D avatars, viseme meters, custom
scopes — as ES modules loaded **at runtime**. No rebuild of the admin frontend, no dependency from this
repo on the extension, and the extension never touches the session core: it gets a small, documented
façade and a container `<div>` it fully owns.

This page is the author-facing reference for the extension API, the readiness gate and the
client playback-latency metrics.

---

## 1. Concepts

| Term | Meaning |
|---|---|
| **Extension** | An ESM module whose default export is a `PlaygroundExtension` object (or a zero-arg factory returning one). |
| **Manifest** | `/playground-extensions.json`, served from the admin frontend origin. Lists extension URLs + options. |
| **Extension Stage** | The collapsible panel above the transcript that mounts the active extension. Toggle: the **Stage** button in the connection bar (only rendered when at least one extension is configured). |
| **Host** | The object passed to `mount()`: session façade, theme, options, base URL, logger. |
| **Session façade** | Narrow, read-mostly view of the live session (`PlaygroundExtensionSession`). Never the core. |

Lifecycle:

```
page load ──► fetch manifest (+ env var) ──► import() each module ──► validate
                                                          │
   Stage shown / tab active ──► container has size ──► mount(container, host)
                                                          │
                              audioChunk / playbackChanged / stateChanged / … events
                                                          │
   Stage hidden / tab switched / route left / page unload ──► unmount()
```

Discovery + import happens **once per page load** and is shared across navigations. Mount/unmount happens
every time the extension becomes visible/hidden.

---

## 2. Configuring extensions

### 2.1 Manifest (recommended)

`/playground-extensions.json` at the admin frontend origin:

```json
{
  "extensions": [
    { "url": "builtin:blendshape-meter" },
    {
      "url": "/ext/my-avatar/my-avatar-extension.js",
      "options": { "assetUrl": "/ext/my-avatar/asset/avatar.zip" },
      "disabled": true
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `url` | ESM URL — absolute, root-relative to the admin origin, or `builtin:<name>`. |
| `options` | Free-form object passed as `host.options` (frozen). |
| `disabled` | Listed as a tab, but not mounted until the user clicks it. |

A stock build ships **no** manifest (`public/playground-extensions.example.json` is a template — copy it to
`playground-extensions.json` where the frontend is served from). Without a manifest and without the env var
the Playground renders exactly as before; the only side effect is one fetch of the manifest URL, which
harmlessly returns the SPA `index.html` (the loader checks the `Content-Type` and ignores non-JSON).

### 2.2 Env var (build-time fallback)

`VITE_PLAYGROUND_EXTENSIONS="/ext/a.js,https://cdn.example.com/b.js"` — comma-separated URLs, no options.
Appended after manifest entries (deduplicated by URL).

### 2.3 Built-ins

| `builtin:` name | What it does |
|---|---|
| `blendshape-meter` | Bar meter for blendshapes — validates an `AudioPostProcessor` (e.g. wav2arkit avatar-sync) without any avatar. Reads `audio_meta.extras.blendshapes` (inline processors) **and** `audioAnnotation` events with `kind === "blendshapes"` (annotators, `mode = "annotate"`), keeping frames in utterance-offset space against `playbackClock`. The two timelines are never mixed: annotations for the utterance the clock is playing win, otherwise the inline chunk data is used — the header line shows `src=annotation` / `src=inline`. Options: `{ maxBars?: number, highlight?: string[] }`. |

---

## 3. Hosting

**Same origin (simplest, no CORS).** With the Docker `prod` image, bind-mount your bundles and the manifest:

```yaml
frontend:
  volumes:
    - ./ext:/usr/share/nginx/html/ext:ro
    - ./playground-extensions.json:/usr/share/nginx/html/playground-extensions.json:ro
```

`nginx.conf` already serves `/ext/` and the manifest with `Cache-Control: no-cache`, so redeploying a bundle
does not need a cache bust.

**Other origin.** Plain browser module semantics: the *serving* origin must send `Access-Control-Allow-Origin`
for the module (script CORS). The admin backend is not involved. Serve with `no-cache` or version the URL.

**Vite dev server.** Put the manifest and bundles under `frontend/public/` (served at `/`).

---

## 4. Writing an extension

### 4.1 Minimal (`data:` URL one-liner — handy for smoke tests)

```json
{ "extensions": [ { "url": "data:text/javascript,export%20default%20%7B%20id%3A%20%22demo.hello%22%2C%20name%3A%20%22Hello%22%2C%20mount(el)%20%7B%20el.textContent%20%3D%20%22hi%22%3B%20%7D%2C%20unmount()%20%7B%7D%20%7D" } ] }
```

### 4.2 Real module

```ts
// my-avatar-extension.ts — bundle to a single ESM file (e.g. with esbuild/vite lib mode).
// Bundle EVERYTHING you need (three, etc.). The host provides no shared libraries
// and you must never assume the host's React.
import type { PlaygroundExtension, PlaygroundExtensionHost, PlaybackChunk } from "voice-pipes-admin/src/lib/extensions";

export default function create(): PlaygroundExtension {
  let host: PlaygroundExtensionHost;
  let audioCtx: AudioContext | null = null;
  const unsubs: Array<() => void> = [];
  let raf = 0;

  return {
    id: "myned.avatar",
    name: "Avatar",
    version: "0.1.0",
    slot: "stage",

    async mount(container, h) {
      host = h;
      const assetUrl = new URL(String(h.options.assetUrl ?? "asset/avatar.zip"), h.baseUrl).href;
      // ... create renderer into `container` (read container.getBoundingClientRect() — it is sized) ...

      // Late mount into a live session: bind to whatever is playing NOW.
      audioCtx = h.session.playbackService?.audioContext ?? null;

      unsubs.push(
        h.session.on("playbackChanged", (pb) => { audioCtx = pb?.audioContext ?? null; }),
        h.session.on("audioChunk", (chunk: PlaybackChunk) => {
          // chunk.startTime / endTime are AudioContext seconds; compare with audioCtx.currentTime.
          // chunk.meta.extras?.blendshapes carries the post-processor payload for THIS chunk.
        }),
        h.session.on("audioFlushed", () => { /* barge-in: clear queued animation */ }),
        h.session.on("stateChanged", (s) => { /* listening | processing | speaking | draining | null */ }),
        h.theme.onChange((isDark) => { /* restyle */ }),
      );
      const loop = () => { /* draw using audioCtx?.currentTime */ raf = requestAnimationFrame(loop); };
      raf = requestAnimationFrame(loop);
    },

    resize(w, h) { /* optional; a ResizeObserver on the container works too */ },

    unmount() {
      cancelAnimationFrame(raf);
      unsubs.splice(0).forEach((u) => u());
      // dispose WebGL contexts, workers, timers …
    },
  };
}
```

### 4.3 The contract (`src/lib/extensions/types.ts`)

```ts
export const PLAYGROUND_EXTENSION_API_VERSION = 1;

interface PlaygroundExtension {
  id: string;                     // reverse-DNS-ish, unique
  name: string;                   // tab label
  version?: string;
  slot?: "stage";                 // v1: only "stage"
  mount(container: HTMLElement, host: PlaygroundExtensionHost): void | Promise<void>;
  unmount(): void | Promise<void>;
  resize?(width: number, height: number): void;
  ready?(): Promise<void>;         // optional: resolves once heavy assets are up; gates Connect (see §5)
}

interface PlaygroundExtensionHost {
  apiVersion: 1;
  session: PlaygroundExtensionSession;
  theme: { isDark: boolean; onChange(cb: (isDark: boolean) => void): () => void };
  options: Readonly<Record<string, unknown>>;   // manifest entry options
  baseUrl: string;                              // absolute module URL (or "builtin:<name>")
  log(level: "debug" | "info" | "warn" | "error", message: string, detail?: unknown): void;
}

interface PlaygroundExtensionSession {
  sessionState: "idle" | "connecting" | "active" | "error";
  pipelineState: "listening" | "processing" | "speaking" | "draining" | null;
  playbackService: AudioPlaybackService | null;   // may change per session — listen to playbackChanged
  playbackClock: PlaybackClock | null;
  activeUtteranceId: number | null;
  on(event, listener): () => void;               // returns unsubscribe
  sendClientToolResult(callId, phase, result?, speak?): void;
}

// Events: audioChunk, playbackChanged, stateChanged, sessionStateChanged,
//         audioFlushed, playbackFinished, sessionReady, sessionTerminated, pipelineMessage,
//         metricsEmitted, playbackMetrics, audioAnnotation
```

`audio_meta.extras` reaches you as `chunk.meta.extras` on `audioChunk`. It is **per chunk** — continuation
chunks of the same fragment do not inherit it. `chunk.meta.serverTimestampMs` is the server wall-clock when
the chunk left the pacer.

`audioAnnotation` carries the output of an **Audio Annotator** (`[[audio_post_processor]] mode = "annotate"`
— a metadata service that runs off the audio path; see
[../../voice-pipes/docs/AUDIO-ANNOTATORS.md](../guides/audio-annotators.md)):
`AudioAnnotation { utteranceId, startMs, durationMs, kind, extras, serverTimestampMs? }`. It is keyed by
**utterance offset**, not by chunk: place `startMs + i·(1000/fps)` (or whatever your `extras` encodes) against
`playbackClock` snapshots with the same `utteranceId`, never against `AudioContext` time or chunk boundaries.
An annotation may arrive before or after the audio it describes; spans with no annotation (timeout, breaker
open) should render neutral. Annotations for an utterance that has been flushed by barge-in are dropped
before the event fires.

`playbackMetrics` fires once per utterance when its first chunk is scheduled, with the client-side timings
the core computes from the Ping/Pong clock offset and the AudioContext schedule (`ClientPlaybackMetrics`):

| Field | Meaning |
|---|---|
| `clientReceiveToPlayMs` | first audio byte received → first sample audible (jitter buffer + scheduling) |
| `clientTtfaMs` | speech_end → first sample audible (absent for agent-initiated turns) |
| `clientPlaybackLatencyMs` | server pacer emit → audible, clock-offset corrected (absent until a Pong arrived) |

It may arrive before **or** after `metricsEmitted` for the same utterance (short answers finish server-side
while the jitter buffer is still holding); `metricsEmitted` carries the same three fields when they were
already known. Do not try to recompute these from `chunk.meta.serverTimestampMs` and `Date.now()` — those
are unsynchronised wall clocks; the core owns the offset.

---

## 5. Rules of the road

- **Never put `audioChunk` into React state** (or any per-frame store that re-renders UI). It fires ~20–25×/s;
  keep a timeline in a plain array/ring buffer and read the AudioContext clock in your RAF loop.
- **Bind late.** The Stage may mount you into a session that is already speaking. Read
  `session.playbackService`, `pipelineState`, `activeUtteranceId` in `mount()`; the host does **not** replay
  events.
- **Re-bind on `playbackChanged`.** The playback service (and its `AudioContext`) is recreated on every
  session start; a cached context goes stale.
- **Honour `unmount()`.** Release WebGL contexts, workers, RAF loops, subscriptions. The container is
  discarded afterwards; the next mount gets a fresh one.
- **Render only while visible.** You are unmounted when the stage is hidden — do not keep loops alive across
  it (you cannot; the container is gone).
- **Bundle everything.** No shared libraries are provided. Do not assume the host's React or its version.
- **Resolve assets against `host.baseUrl`.**
- **Implement `ready()` if you load heavy assets.** Some providers greet the moment the session opens (Gemini
  Live sends its opener on `configure`); without a gate the first line is spoken to your loading spinner. When
  `ready()` is present the Playground holds **Start session** until it resolves — for **mounted**
  extensions only, bounded by `EXTENSION_READY_TIMEOUT_MS` (10 s), after which it connects anyway and warns
  on the console. It is called once per mount, after `mount()` resolved. It must **resolve on failure** (never
  reject, never hang — resolve when you give up); a rejection is logged and treated as ready. A hidden stage
  or an inactive tab is never waited for: nothing is rendering there.

Versioning: `host.apiVersion` is `1`; the contract is **additive-only** within a major. New events/fields
may appear; nothing listed above will be removed or change shape.

Security: extensions run with **full page privileges** on the admin origin. Only load URLs you trust. The
manifest is served from the admin origin, which is already trusted; treat any other-origin module URL as
you would a `<script src>`.

---

## 6. Debugging

- Failed loads show an amber chip in the stage header (hover for the error). A failing `mount()` shows a red
  chip inside the stage; the Playground is unaffected either way.
- `host.log(...)` prefixes console output with `[ext:<id>]`.
- Event Log → `in audio` rows carry the full `meta` (including `extras`) in their detail — confirm the server
  is actually sending extras before debugging your renderer.
- `builtin:blendshape-meter` is the reference consumer: if it animates, the pipeline (post-processor →
  `audio_meta.extras` → transport → `audioChunk`, or annotator → `audio_annotation` → `audioAnnotation`) is
  working; its header says which source it is drawing from (`src=inline` / `src=annotation`). It also prints
  the last turn's `ttfa/buffer/delivery` from `playbackMetrics`.
- Event Log → `in annotation` rows (one per `audio_annotation` frame, collapsed like `audio`) carry the full
  `AudioAnnotation` in their detail: `annotation kind=blendshapes utt=12 start=160ms dur=160ms`.
- While the readiness gate holds a connect, the **Start session** button reads "Waiting for `<name>`…" with a spinner.
  If an extension never resolves `ready()`, the console warns
  `[playground] extensions not ready after 10000ms, connecting anyway: [...]` and the session opens regardless.
- Event Log → `playback timings:` rows (one per turn) and the `metrics:` row's `ttfa= buffer= delivery=`
  fields show the same numbers the MetricsBar pills **TTFA / Buffer / Delivery** display.

---

## 7. Internals (for vp maintainers)

| Piece | File |
|---|---|
| Contract / types | `frontend/src/lib/extensions/types.ts` |
| Session façade | `frontend/src/lib/extensions/session-facade.ts` (created once per hook in `useVoicePipesSession`) |
| Discovery + loader | `frontend/src/lib/extensions/loader.ts` |
| Built-ins | `frontend/src/lib/extensions/builtin/` |
| Stage UI | `frontend/src/pages/playground/ExtensionStage.tsx` |
| Readiness registry | `frontend/src/pages/playground/extensionRegistry.ts` — `setExtensionReadiness` (Stage, after mount), `pendingExtensions` / `waitForExtensionsReady` (Playground gate), `EXTENSION_READY_TIMEOUT_MS` |
| Core events | `VoicePipesSessionCore` — `audioChunk`, `playbackChanged`, `playbackClock` getter |
| Core events — `playbackMetrics` | `VoicePipesSessionCore.recordFirstChunkTimings` (in the `pb.onChunk` wrapper); merged into `turn_metrics` envelopes; late arrivals upserted by `session/metricsMerge.ts` in the hook |
| Transport | `ws-adapter.ts` passes `audio_meta.extras` / `server_timestamp_ms` through (per-chunk, not sticky); maps `audio_annotation` → `{ kind: "annotation" }` |
| Core events — `audioAnnotation` | `VoicePipesSessionCore.handleAnnotationMessage` — drops flushed utterances via `shouldDropAudio`, logs an `annotation` Event Log row (grouped like `audio`), emits the event |
| Caching | `nginx.conf` — `/ext/` and the manifest are `no-cache` |

Out of scope for v1 (tracked in the plan): publishing the types to npm, extension-contributed settings,
additional slots, routing extension logs into the Event Log, replaying the last N events on late mount,
multi-instance mounting.
