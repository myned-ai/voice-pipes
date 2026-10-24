# Audio Annotators — per-audio metadata off the critical path

Status: Implemented (2026-08)
Owner: Backend
Related: [custom components](../extending/custom-component.md) (inline chain), [playground extensions](../clients/playground-extensions.md) (client consumers)

How to attach a service that *derives metadata* from the agent's speech —
blendshapes for lip-sync, visemes, phonemes, emotion, VU levels, anything
per-audio — without letting that service slow the audio down. This page is
the author-facing reference; the plan holds the design rationale and the
file-level anchors.

---

## 1. Why not the inline chain?

The inline `AudioPostProcessor` chain is awaited
**per chunk, before the chunk is forwarded**, in both engines. That is the
right seam for processors that *mutate samples* (filters, resamplers) and the
wrong one for processors that *derive metadata*: the audio stream can never
leave the worker faster than the processor's per-call latency, and the
realtime engine calls it once per ~41 ms Gemini delta.

Measured (Tesla worker, Gemini Live, headed Edge, 40–60 s monologue):

| Configuration | Audio delivered / wall time |
|---|---|
| No processor | **1.90×** real time |
| Lip-sync processor inline, default context | **0.60×** — audio stutters after the first words |
| Same, with the service *stopped* (every chunk waits the 1 s timeout) | **0.06×** |

The processor itself costs **18 ms fixed + ~35 ms per second of audio** in its
window: fast when called on 160–500 ms batches (5–14× headroom), slow only
because the inline seam calls it ~25×/s on 1 s windows. A dead metadata
service should never throttle speech.

**Audio Annotators** are the second processor class that fixes this. vp tees
the outbound audio into a per-session task, batches it, calls the service
asynchronously with bounded concurrency, and forwards results as a separate
ordered wire frame. Audio never waits: the hot path adds one non-blocking
channel `try_send`. For the lip-sync case the plan's expected result is audio
at the provider's native rate (1.9× here), ~10× less annotator CPU (6 calls/s
on 160 ms batches instead of 25 calls/s on 1 s windows), first frames within
~120 ms of the first audio byte, and zero coupling between a slow or dead
metadata service and speech.

When to pick which mode:
*samples or must-be-on-the-chunk → inline; metadata about the audio → annotate.*

---

## 2. Architecture

```text
cascade:   tts_task ──► [inline chain] ──► runner ──► AudioPacer ──► WebSocket
                              │ tee (try_send, never blocks)
                              ▼
                     AnnotatorHub (per session task)
                       batch by utterance/offset ──► AudioAnnotator (gRPC stream | unary fallback)
                       ◄── annotations ──────────────┘
                       └──► ServerMessage::Annotation ──► out_tx ──► (pacer passes non-audio through) ──► WebSocket

realtime:  provider delta ──► emit_audio_chunk ──► [inline chain] ──► out_tx ──► ...
                                       │ tee
                                       ▼ (same hub)
```

- The tee sits exactly where the inline chain runs today, *after* inline
  processors — annotators see mutated samples. Both engines feed the same hub.
- One hub task per configured annotator per session. It coalesces audio per
  utterance into `batch_ms` segments tagged with `utterance_id` + `start_ms`
  (running sample count converted to ms of audio — the same accounting the
  client's playback service uses, so offsets match on both ends).
- Results go out as `ServerMessage::Annotation` on the session's normal
  output channel. Non-audio messages pass the pacer immediately, so an
  annotation is never queued behind paced audio.
- The end-of-utterance sentinel and barge-in reach the hub through the same
  tee plus the existing `suppress_audio` signal.
- vp never parses the payload: `extras` is opaque JSON and `kind` is a free
  string. There is no avatar or blendshape knowledge anywhere in vp.

---

## 3. Configuration

Annotators reuse the `[[audio_post_processor]]` table, so existing configs
stay valid. `mode` selects the class; keys marked *annotate only* have no
effect in inline mode.

```toml
[[audio_post_processor]]
type = "grpc"
endpoint = "http://avatar-sync:50051"
mode = "annotate"          # "inline" (default, today's behaviour) | "annotate"
batch_ms = 160             # annotate only
first_batch_ms = 80        # annotate only
max_in_flight = 2          # annotate only
timeout_ms = 250           # per segment (annotate) / per chunk (inline; default stays 1000)
warmup = true              # annotate only
[audio_post_processor.circuit_breaker]
failures = 3
cooldown_ms = 5000
[audio_post_processor.options]
kind = "blendshapes"
```

| Key | Default | Meaning |
|---|---|---|
| `type` | — | Processor implementation. `"grpc"` (feature `post-grpc`) is the generic bridge described here. Built-ins that support both modes: `"noop"` (consumes, emits nothing) and `"level"` (loudness: `kind = "audio_level"`, 25 fps `[rms, peak]` frames — always compiled in, so the whole annotate path can be exercised without any service; see §3.1). |
| `endpoint` | — | gRPC endpoint of the service. Connected lazily; a transiently-down service never fails worker startup. |
| `mode` | `"inline"` | `"inline"` = today's per-chunk chain (`audio_meta.extras`). `"annotate"` = off-path annotator (`audio_annotation` frames). |
| `batch_ms` | `160` | Steady-state batch size in ms of audio. The hub accumulates samples per utterance until `batch_ms` or end-of-utterance, then sends one segment. Larger = fewer calls, later metadata (see §6 latency budget). |
| `first_batch_ms` | `80` | Size of the *first* batch of each utterance, released early so the first annotations exist before the client's prebuffer elapses. |
| `max_in_flight` | `2` | Maximum segments outstanding towards the service. Beyond that, audio keeps accumulating into *bigger* batches — never more latency for the audio itself. |
| `timeout_ms` | `250` (annotate) / `1000` (inline) | Per-segment deadline. A metadata batch that takes longer than its own duration is useless, hence the low annotate default. On timeout the batch is dropped and counted (§5). |
| `warmup` | `true` | On session start, send a synthetic segment so the service warms its caches; the output is discarded. Removes the measured cold-start hole on the first turn. |
| `circuit_breaker.failures` | `3` | Consecutive failures that open the breaker (§5). |
| `circuit_breaker.cooldown_ms` | `5000` | How long the hub stops calling the service once the breaker is open, before probing again. |
| `options.*` | `{}` | Static string→string knobs forwarded verbatim to the service (on the first request of an `Annotate` stream; on every `Process` call in the unary fallback). `options.kind` doubles as the default `kind` for unary responses. |

Validation: `mode = "annotate"` on a type that has no annotator implementation
is a **startup** error (`does not support mode = "annotate"`). The
inline↔streaming capability of a *remote* service is probed lazily when the
first session stream opens (`GetInfo`, §4) — the factory's `build` is
synchronous and a transiently-down service must never fail worker startup —
so `mode = "inline"` against an `Annotate`-only service surfaces as per-chunk
`Process` failures in the log, not at boot. Existing configs without `mode`
behave byte-for-byte as before and emit no `audio_annotation` frames.

### 3.1 Trying it without a service: the built-in `level` annotator

```toml
[[audio_post_processor]]
type = "level"
mode = "annotate"
```

`level` emits one `audio_level` annotation per segment (`extras = { fps: 25,
names: ["rms", "peak"], frames: [[rms, peak], …] }`, values in `0..=1`). It
is deliberately domain-agnostic (no avatar knowledge in vp), but it drives the
whole path — hub batching, `audio_annotation` frames, the Playground Event Log
`annotation` rows, and the `annotator:level` component in the per-turn
metrics — so you can validate a deployment before pointing `type = "grpc"` at
your real service. In `mode = "inline"` the same type attaches
`extras.audio_level = { rms, peak }` per chunk instead.

### Migrating an existing unary service

If you already run a `Process`-only service (e.g. today's avatar-sync), the
whole migration is one line:

```toml
[[audio_post_processor]]
type = "grpc"
endpoint = "http://avatar-sync:50051"
mode = "annotate"                     # ← that's it
```

vp now batches audio to `batch_ms`, calls `Process` once per batch instead of
once per chunk, and wraps each response as an annotation
(`kind = options["kind"]`, or `"extras"` if unset). The service is untouched;
its output moves from `audio_meta.extras` to `audio_annotation` frames, so
update the client consumer (§6). Runnable inline examples to start from:
`examples/avatar/avatar-realtime.toml`, `examples/avatar/avatar-cascade.toml`;
a commented annotate block is in `config/pipeline.toml`.

---

## 4. Writing a service — the gRPC contract

`proto/audio_post_processor.proto` gains a second service in the same package.
One server may implement both; nothing about the existing
`AudioPostProcessor.Process` contract changes.

```proto
service AudioAnnotator {
  // One bidirectional stream per vp session. Requests arrive in audio order; responses may be sent at any time.
  rpc Annotate(stream AnnotateRequest) returns (stream AnnotateResponse);
  rpc GetInfo(InfoRequest) returns (InfoResponse);
}

message AnnotateRequest {
  string session_id = 1;
  uint32 utterance_id = 2;
  uint64 start_ms = 3;          // audio offset of the first sample within the utterance
  bytes  audio = 4;             // PCM s16le, `channels`-interleaved; empty on cancel and on a bare
                                // end_of_utterance sentinel; 160 ms of silence on warmup
  uint32 sample_rate = 5;
  uint32 channels = 6;
  bool   end_of_utterance = 7;
  bool   cancel = 8;            // drop pending work for utterance_id (barge-in)
  bool   warmup = 9;            // synthetic; discard output
  map<string, string> options = 10;  // [audio_post_processor.options], sent on the first request of the stream
}

message AnnotateResponse {
  uint32 utterance_id = 1;
  uint64 start_ms = 2;
  uint64 duration_ms = 3;
  string kind = 4;              // free-form, e.g. "blendshapes"
  string extras_json = 5;       // opaque JSON object, forwarded verbatim
}

// InfoResponse gains: repeated string services = 3;  // "Process", "Annotate"
```

### 4.1 Streaming `Annotate` (preferred)

- **One stream per vp session.** vp opens `Annotate` when the session starts
  (lazy connect + the existing auth channel), forwards segments in audio
  order, and closes the stream at session end or when the breaker trips.
- **Respond whenever you have frames.** Responses are not paired 1:1 with
  requests — any time after the segment, any granularity. You may answer one
  160 ms segment with one response, or accumulate context and answer later.
  Each response is self-describing (`utterance_id`, `start_ms`,
  `duration_ms`), and vp forwards it as-is.
- **`end_of_utterance`** rides the *last* segment of an utterance — usually
  together with its final audio; only when the utterance ends right after a
  batch was released does it arrive as a bare, audio-less sentinel. Either
  way it is your reset signal for that utterance. **`cancel`** carries no
  audio and means barge-in: drop pending work for `utterance_id` (and any
  earlier); anything you still send for it is discarded by vp. **`warmup`**
  is a synthetic segment at session start carrying 160 ms of silence under
  `utterance_id = 0` — run your normal path; the output is thrown away.
- **`options`** arrives on the first request of the stream only (and again on
  the first request of every reopened stream).
- **Advertise yourself** in `GetInfo`: `services = ["Annotate"]` (plus
  `"Process"` if you also implement the unary RPC). vp probes `GetInfo` once,
  when the first session stream opens, and caches the answer per annotator
  to choose between the stream and the fallback. An `Unimplemented` reply
  (an older, inline-only service) selects the fallback; a transport error is
  not cached, so a service that was down at first probe is re-probed after
  the breaker cooldown.
- **Reconnects.** When a stream is cut (server close, transport error) the hub
  counts it as a failure, waits `circuit_breaker.cooldown_ms`, and reopens
  the stream in half-open mode (one probe segment, then normal traffic on
  success). Because every request is self-describing, a reopened stream can
  start mid-utterance.

### 4.2 Unary `Process` fallback

If `GetInfo` reports no `Annotate`, vp calls the existing unary `Process`
once per *segment* (with `end_of_utterance` / `seq` as today) and wraps each
response as `AudioAnnotation { start_ms, duration_ms = segment duration,
kind = options["kind"] or "extras", extras }`. This is the migration path of
§3; the contract for `Process` is the inline chain's
(`proto/audio_post_processor.proto`).

### 4.3 Statelessness — what changes from the inline rule

The inline chain shares one processor instance across all sessions and
requires a fully stateless service.
An `Annotate` **stream is per session**, so a service *may* keep per-stream
context — an overlap window for smoother inference, a running phoneme state,
whatever it wants; that is the point of the stream. Two rules remain:

1. It must still be correct if a stream is cut and reopened **mid-utterance**:
   `start_ms` + `utterance_id` make every request self-describing, so never
   depend on having seen the previous request.
2. Never key state across streams. A new stream is a new session.

A unary `Process` service used through the fallback keeps the inline
statelessness rule — it is simply called less often, on bigger windows.

---

## 5. Failure semantics

Every failure mode below leaves the audio untouched. The only observable
effect of a misbehaving annotator is *missing metadata*.

| Event | What vp does | What the client sees |
|---|---|---|
| Segment exceeds `timeout_ms` (default 250 ms) | Drops that batch, counts a `timeout`, continues with the next segment. | No annotation for that span → neutral state (e.g. resting face) until the next one lands. |
| Tee channel full (256 chunks queued, annotator far behind) | Drops the **oldest** queued chunk, counts `dropped_chunks`. Audio is never delayed. | A gap in annotations, not a stall. |
| 3 consecutive failures (`circuit_breaker.failures`) | Opens the breaker: stops calling the service for `cooldown_ms` (5 s), logs **one** `warn!`, then probes with the next segment (half-open). | No annotations while open; they resume within `cooldown_ms` of the service coming back. `breaker_open = true` in the metrics (§7). |
| Barge-in / flush | Sends `Cancel { utterance_id }` to the service, drops everything queued for that utterance (and earlier), and discards annotations that arrive late for a cancelled utterance. The runner already emits `clear_audio`; the hub follows the same signal. | No annotation for the flushed utterance arrives after `clear_audio`; the SDK additionally drops any that would (mirrors `shouldDropAudio`). |
| Session start (`warmup = true`) | Sends one synthetic `Warmup` segment before the first real audio; output discarded. | First-turn annotations present even after the service sat idle (no "timed out" on the first batch). |
| Service down at worker start | Nothing: connect is lazy. The first real segments fail → breaker opens → probes every `cooldown_ms`. | Audio works from the first turn; annotations appear once the service is up. |
| Session end | Stream closed. | — |

Worst case is therefore "no lip-sync for a while", never "the agent stutters".
This is the acceptance criterion the plan tests with `docker stop
avatar-sync` mid-session: audio rate unchanged, breaker opens after 3
failures, annotations resume after `docker start`.

---

## 6. Consuming annotations on the client

### 6.1 Wire frame

Server → client, data plane, same socket as audio
([WEBSOCKET_PROTOCOL.md §2.D](../clients/protocol-walkthrough.md)):

```json
{ "type": "audio_annotation",
  "payload": { "utterance_id": 12, "start_ms": 160, "duration_ms": 160, "kind": "blendshapes",
               "extras": { "fps": 30, "frames": [[...52 floats...], ...], "names": [...] },
               "server_timestamp_ms": 1724700000000 } }
```

It may precede or follow the audio it describes and is never delayed by the
pacer. `audio_meta.extras` is unchanged and still carries inline-processor
output.

### 6.2 SDK event

The client SDK (`voice-pipes-admin/frontend/src/lib`) maps the frame to an
`audioAnnotation` event on `VoicePipesSessionCore`, forwarded through the
extension façade:

```ts
interface AudioAnnotation {
  utteranceId: number;
  startMs: number;        // offset of the span within the utterance, ms of audio
  durationMs: number;
  kind: string;           // e.g. "blendshapes"
  extras: Record<string, unknown>;
  serverTimestampMs?: number;
}

host.session.on("audioAnnotation", (a: AudioAnnotation) => { /* … */ });
```

Annotations for an utterance the client has already flushed (barge-in) are
dropped before the event fires. The type is re-exported from the SDK index.

### 6.3 Placing data: offset space + `playbackClock`

Do **not** derive timing from arrival order, chunk boundaries or
`AudioContext` arithmetic. Keep annotations in **utterance-offset space** and
read the existing `PlaybackClock`
(`ClockSnapshot { nowMs, utteranceId, isRunning }`):

```ts
// e.g. blendshape frames at `fps` inside one annotation
const frameAt = (i: number) => a.startMs + i * (1000 / fps);

// in the RAF loop
const clock = host.session.playbackClock?.snapshot();
if (clock?.isRunning && clock.utteranceId === a.utteranceId) {
  // pick the frame whose frameAt(i) is closest to clock.nowMs
}
```

Only compare annotations and clock snapshots with the **same `utteranceId`**.
`builtin:blendshape-meter` is the reference consumer: it accepts both
`audioChunk.meta.extras.blendshapes` (inline) and `audioAnnotation` with
`kind === "blendshapes"` (annotate), keeps frames in offset space, and reads
`clock.nowMs`.

### 6.4 Latency budget — why `batch_ms = 160`

For audio at utterance offset `T` and batch `B`: the segment `[T, T+B)` is
complete at audio-emit time `T+B`; the annotation reaches the client at
≈ `T + B + inference (~30 ms on 160 ms batches, measured) + stream (~2 ms) +
WS`. That audio starts playing at ≥ `T_arrival + prebuffer` (200 ms Playground
default, 450 ms kiosk) — and later still with bursty providers (Gemini
delivers ~1.9× faster than playback, so audio at offset `T` sits in the
client buffer for a growing lead).

- First batch `first_batch_ms = 80` → first frames ≈ 115 ms after the first
  audio byte, inside the 200 ms prebuffer.
- Worst case (provider at exactly 1.0×, 200 ms prebuffer): lag ≈ `B + 35 ms`
  = 195 ms < 200 ms with `B = 160`. That is why the default is 160 rather
  than 200.
- Late annotations are harmless: the client shows neutral until they land,
  and `lag_max_ms` (§7) tells the operator to lower `batch_ms`.

---

## 7. What operators see — metrics

Each annotator is one component in the per-turn `TurnMetricsSnapshot`
(`components[]`), `component_id = "annotator:<name>"` (`annotator:grpc` for
the gRPC bridge), `kind = "annotator"`:

| `extras` key | Meaning |
|---|---|
| `segments` | Batches sent to the service this utterance. |
| `annotations` | Annotations received back. |
| `latency_p50_ms` / `latency_max_ms` | Segment sent → annotation received (service round-trip, including inference). Budget: `batch_ms`. |
| `lag_max_ms` | Max over annotations of `audio_emitted_ms_at_receipt − (start_ms + duration_ms)`: how far the audio had run ahead of the metadata when the annotation arrived. Budget: 200 ms. |
| `dropped_chunks` | Tee overflow drops (oldest-first). |
| `timeouts` | Segments that exceeded `timeout_ms`. |
| `breaker_open` | Whether the circuit breaker was open at end of turn. |

`latency_p50_ms` and `lag_max_ms` are advertised as `MetricFieldDescriptor`s
on `session_ready` with `health.role = "latency"`, so the Playground's generic
per-component table and the session-detail page render them (green/amber/red
against the budget) with no frontend change. The Event Log gets one row per
utterance, e.g. `annotator: 23 segments p50=31ms lag_max=190ms`.

**Reading `lag_max_ms`.** It is measured server-side, at the moment the
annotation arrives in the hub, against how much audio has already been
emitted. Compare it with the client's prebuffer (200 ms Playground, 450 ms
kiosk): if `lag_max_ms` stays below the prebuffer, every annotation reached
the server's output before the audio it describes became audible (network
transit aside); if it exceeds the prebuffer, some spans were rendered neutral
before their metadata arrived — lower `batch_ms`, or raise the client
prebuffer if latency to first audio is less important than continuity. A
`lag_max_ms` that grows across a long utterance with `latency_p50_ms` flat
means the service is keeping up per call but the hub is coalescing bigger
batches because `max_in_flight` is saturated.

**Client-side truth: `annotation_late_count` / `annotation_late_ms_max`.**
`lag_max_ms` is measured at the server; the SDK measures the same thing where
it matters. For every `audioAnnotation` it computes
`late = playbackClock.nowMs(utterance) − startMs` at receipt — positive means
the audio the annotation describes had already started playing — and reports
the count and the maximum per utterance in the `playback_quality` frame
(`annotation_late_count`, `annotation_late_ms_max`;
[WEBSOCKET_PROTOCOL.md](../clients/protocol-walkthrough.md)),
also from the flush path when the turn is interrupted. Both are `0` without an
annotator. Budget 0: any late annotation was rendered neutral first.

**Session History anomalies.** The admin backend turns these into their own
anomaly kinds (see [VPO telemetry §5.5](../reference/vpo-telemetry.md)) — a separate *metadata* dimension, apart from audio health:

| Kind | Condition | Level |
|---|---|---|
| `annotator.lagging` | `lag_max_ms > annotation_lag_budget_ms` (200 — the client prebuffer) **or** client `annotation_late_ms_max > 0` | warn |
| `annotator.degraded` | `timeouts > 0` or `dropped_chunks > 0` in the turn | warn |
| `annotator.unavailable` | `breaker_open` at any point in the turn (no annotations delivered) | error (metadata); session level at most `fair` |

The culprit is `annotator:<name>`, labelled **`ANNOTATOR (<name>) — metadata
only, audio unaffected`**; it is never collapsed into the `s2s` / `tts`
culprit, and an annotator anomaly alone can lower the session level to at most
`fair`. If the same session also shows `output.emit_rtf > 1` or an
`audio_post.over_budget` anomaly, that is the *inline* chain, not the
annotator.

---

## 8. Troubleshooting

| Symptom | Check |
|---|---|
| No `audio_annotation` frames at all | `mode = "annotate"` is set on the table (default is inline → output goes to `audio_meta.extras` instead). Then `GetInfo.services` on the service: without `"Annotate"` vp uses the unary `Process` fallback — fine, but the service must implement it. Then the worker log for a breaker `warn!` (service unreachable / failing). Then the Event Log `annotator:` row: `segments > 0, annotations = 0` means the service is receiving and not answering. |
| Annotations arrive but the meter / avatar stays neutral | Client is filtering on `kind` — check the service's `kind` (or `options.kind` for unary fallback) matches what the consumer expects. Confirm the consumer places by `startMs` against `playbackClock` with the same `utteranceId`, not by arrival order. |
| `lag_max_ms` > client prebuffer | Lower `batch_ms` (and/or `first_batch_ms`); check `latency_p50_ms` against `batch_ms` — if the service is slower than the batch it annotates, no batch size will keep up (bigger model, fewer contexts, or `max_in_flight` up). |
| `annotation_late_count` > 0 on the client while `lag_max_ms` < prebuffer | The annotation left the server in time but reached the client after its span began playing: network transit, or a client prebuffer that is smaller than assumed. Raise the client prebuffer or lower `batch_ms`; Session History flags `annotator.lagging` either way. |
| Session verdict reads `ANNOTATOR (<name>) — metadata only, audio unaffected` | Expected wording: `annotator.lagging` / `annotator.degraded` / `annotator.unavailable` are capped at session level `fair` and never blame the model. Pick the row by kind: lagging → `batch_ms` / prebuffer (above); degraded → `timeouts` / `dropped_chunks` (below); unavailable → the breaker `warn!` in the worker log (service down or unreachable). |
| `timeouts` climbing | Service round-trip exceeds `timeout_ms` (250 ms). Either the service is too slow for `batch_ms` audio, or the network is. Raise `timeout_ms` only if `latency_max_ms` shows the calls *do* finish; otherwise fix the service. |
| `dropped_chunks` > 0 | The annotator fell more than 256 chunks behind; expected during a breaker cooldown, otherwise the service is far too slow. Audio is unaffected by design. |
| Worker fails at startup: inline mode with a stream-only service | The service advertises only `"Annotate"` in `GetInfo.services`; set `mode = "annotate"`. |
| First turn after idle has no annotations | `warmup = false`? Set it to `true` (default). |
| Audio stutters | Not this feature: the tee never blocks. Check the inline chain (`mode = "inline"` processors still run per chunk) and the usual TTS/provider metrics. |
| Annotations for a barged-in utterance keep arriving | The SDK drops them for flushed utterances; a custom client must do the same by `utterance_id`. Server-side, late results for a cancelled utterance are discarded by the hub. |

---

## 9. Where things live

| Piece | Location |
|---|---|
| Trait + hub | `src/pipeline/annotate.rs`, `src/pipeline/annotate/hub.rs` |
| Config (`mode`, `AnnotatorConfig`) | `src/config/types.rs` next to `AudioPostProcessorConfig` |
| gRPC bridge (stream + unary fallback) | `proto/audio_post_processor.proto`, `src/pipeline/post_audio/grpc.rs` (`post-grpc` feature) |
| Wire frame | `src/transport/ws/mod.rs` (`audio_annotation`), documented in [WEBSOCKET_PROTOCOL.md](../clients/protocol-walkthrough.md) |
| Client SDK | `voice-pipes-admin/frontend/src/lib/{transport,sdk,extensions}`; reference consumer `extensions/builtin/blendshape-meter.ts` |
| Inline chain (the other mode) | [custom-component.md](../extending/custom-component.md) |
| Registering any new component for health/metrics | [ADDING-A-CUSTOM-COMPONENT.md](../extending/custom-component.md) |
