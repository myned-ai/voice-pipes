# Voice-Pipes Messages, Events & Capture Artefacts — Complete Specification

Status: active
Owner: Backend
Last reviewed: 2026-09-19
Source of truth: yes
Supersedes: protocol descriptions in project-local docs

This document is the authoritative reference for every message, event, command and metric that flows through the voice-pipes system: on the WebSocket wire between pipeline and client, inside the pipeline as internal events, in the on-disk capture artefacts, and in the admin UI.

---

## Table of Contents

1. [Architecture overview](#1-architecture-overview)
2. [Wire protocol — framing](#2-wire-protocol--framing)
3. [Client → Server messages](#3-client--server-messages)
4. [Server → Client messages](#4-server--client-messages)
5. [Internal pipeline events (`PipelineEvent`)](#5-internal-pipeline-events-pipelineevent)
6. [Audit trail — on-disk capture artefacts](#6-audit-trail--on-disk-capture-artefacts)
   - 6.1 [Capture directory layout](#61-capture-directory-layout)
   - 6.2 [`NNN_summary.json`](#62-nnn_summaryjson)
   - 6.3 [`NNN_stt.json`](#63-nnn_sttjson)
   - 6.4 [`NNN_llm_output.json`](#64-nnn_llm_outputjson)
   - 6.5 [`NNN_llm_input.txt`](#65-nnn_llm_inputtxt)
   - 6.6 [`NNN_tts_frag_MMM.txt` / `.wav`](#66-nnn_tts_frag_mmmtxt--wav)
   - 6.7 [`NNN_mic_raw.wav` / `NNN_mic_16k.wav`](#67-nnn_mic_rawwav--nnn_mic_16kwav)
   - 6.8 [`pipeline_events.jsonl`](#68-pipeline_eventsjsonl)
   - 6.9 [`events.jsonl` (trace events)](#69-eventsjsonl-trace-events)
   - 6.10 [`metadata.json`](#610-metadatajson)
   - 6.11 [`NNN_playback.json`](#611-nnn_playbackjson)
7. [Orchestrator REST API](#7-orchestrator-rest-api)
8. [Admin BFF REST API](#8-admin-bff-rest-api)
9. [Admin UI data consumption](#9-admin-ui-data-consumption)
   - 9.1 [Dashboard KPIs & analytics](#91-dashboard-kpis--analytics)
   - 9.2 [Sessions list](#92-sessions-list)
   - 9.3 [Session detail](#93-session-detail)
   - 9.4 [Playground](#94-playground)
10. [Derived/computed fields](#10-derivedcomputed-fields)
11. [Latency metric definitions](#11-latency-metric-definitions)

---

## 1. Architecture overview

```
Browser / Voice Client
        │
        │  WebSocket  (ws://host:8800/ws)
        │  Binary frames  ← PCM audio
        │  JSON frames   ← control / text / data
        ▼
   Pipeline Worker  (Rust)
        │
        │  HTTP  (http://host:9090)
        ▼
   Orchestrator  (Rust)
        │
        │  HTTP proxy
        ▼
   Admin BFF  (Python / FastAPI)  (http://host:8000)
        │
        │  REST + WebSocket proxy
        ▼
   Admin UI  (React / TypeScript)  (http://host:5173)
```

The pipeline worker also writes **capture artefacts** to disk at `captures/{session_id}/`.
The Admin BFF reads those artefacts and merges them with orchestrator data.

---

## 2. Wire protocol — framing

All transport is over a single WebSocket connection.

### 2.1 Binary frames — PCM audio

Used exclusively for audio data (Client→Server and Server→Client).

```
┌──────────┬──────────┬──────────────────────┬───────────────────────────┐
│ msg_type │ flags    │ seq_num              │ payload                   │
│ 1 byte   │ 1 byte   │ 4 bytes (LE u32)    │ variable — PCM i16-LE     │
└──────────┴──────────┴──────────────────────┴───────────────────────────┘
```

| Field | Value | Meaning |
|---|---|---|
| `msg_type` | `0x01` | PCM signed 16-bit little-endian audio |
| `flags` bit 0 | `0x01` | `FLAG_END_OF_UTTERANCE` — last chunk of current speech turn |
| `flags` bit 1 | `0x02` | `FLAG_END_OF_RESPONSE` — last chunk of entire server response |
| `seq_num` | monotonic u32 | per-session counter; resets to 0 on reconnect |
> **⚠️ IMPLEMENTATION WARNING: Binding Audio Queues**
> When implementing custom clients, you **MUST** track `FLAG_END_OF_UTTERANCE` to determine when the AI's turn is finished and when to finalize UI state (like closing the transcription bubble). 
> 
> Do **NOT** bind your playback finalize logic to `FLAG_END_OF_RESPONSE`. The `END_OF_RESPONSE` flag is reserved for multi-turn/complex completions and is currently largely unimplemented (defaulting to `false`). Binding UI state to `END_OF_RESPONSE` will cause your client to never realize the server has finished speaking.
Default sample rate: **48 000 Hz**, 1 channel (mono).

### 2.2 JSON frames — control / text / data

All non-audio messages are UTF-8 JSON text frames with a shared envelope:

```json
{
  "type": "<message-type>",
  "message_id": "<uuid-v4>",          // optional
  "timestamp_ms": 1714305600000,      // epoch ms, optional
  "payload": { ... }                  // type-specific
}
```

`message_id` and `timestamp_ms` are always emitted by the server; clients
may omit them.

---

## 3. Client → Server messages

### 3.1 Binary — audio stream

Raw PCM frames as described in §2.1.  
The client continuously streams audio while the user speaks. Setting
`FLAG_END_OF_UTTERANCE` signals that the local VAD has detected end-of-speech
(used in push-to-talk modes; ignored in server-VAD mode).

### 3.2 `"message"` — unified structured message

```json
{
  "type": "message",
  "payload": {
    "kind": "text",           // "text" | "tool_result" | "context" | "card" | "image" | ...
    "text": "Tell me a story",
    "role": "user",
    "turn_id": "<uuid>",
    "message_id": "<uuid>",
    "payload": null,          // kind-specific JSON, or null
    "binary": null            // base64-encoded attachment, or null
  }
}
```

`kind` values understood by the pipeline:

| `kind` | Description |
|---|---|
| `text` | Chat text message from the user |
| `tool_result` | Response to a tool call issued by the LLM |
| `context` | Additional system context to inject |
| `image` | Image attachment (binary field populated) |
| `trigger_action` | Ask pipeline to execute a named action |
| `suggestions` | Client-provided follow-up suggestion list |

### 3.3 `"text"` — legacy text input *(removed — no longer decoded)*

> ⚠️ The current WS decoder (`transport/ws/codec.rs`) only accepts `"message"` and `"control"`
> text frames; any other `type` is rejected with `Codec("unknown message type")`. The historical
> `"text"` frame below is **not** accepted by the server. Use `"message"` with `kind: "text"`.

```json
{
  "type": "text",
  "payload": {
    "text": "Hello!",
    "message_id": "<uuid>"
  }
}
```

### 3.4 `"data"` — legacy structured data *(removed — no longer decoded)*

> ⚠️ Same as §3.3: `"data"` frames are rejected by the current decoder. Use `"message"` with the
> appropriate `kind` (e.g. `"image"`).

```json
{
  "type": "data",
  "payload": {
    "kind": "image",
    "payload": { ... },
    "binary": "<base64>"
  }
}
```

### 3.5 `"control"` — client control signals

All control messages share the envelope `{ "type": "control", "payload": { "action": "...", ... } }`.

#### `action: "configure"` — start a new session (handshake)

Must be the first client control message after WebSocket open.

```json
{
  "action": "configure",
  "audio_format": "pcm_i16_le",    // "pcm_i16_le" | "pcm_f32_le" | "opus" (accepted, always downgraded to PCM)
  "sample_rate": 48000,
  "output_sample_rate": 24000,     // optional — preferred TTS output rate
  "channels": 1,
  "language": "en-US",             // optional BCP-47
  "voice": "af_sky",               // optional TTS voice
  "mode": "voice",                 // "voice" | "text" | "hybrid" | "voice_in_text_out" | "text_in_voice_out"
  "protocol_version": 1,           // optional; the server assumes the current version when absent
  "accepts_negotiated_input_rate": true, // optional, default false — see below
  "tags": ["live", "prod"],        // optional session labels
  "app_data": { ... },             // optional opaque client data
  "options": {                     // optional provider-specific overrides
    "output_chunking_enabled": true, // Whether to pace and chunk TTS output into small fixed frames (e.g. 50ms)
    "output_chunk_frame_ms": 50      // The duration of each chunk if chunking is enabled
  }
}
```

`accepts_negotiated_input_rate` tells the server the client can resample and
will capture at whatever `negotiated.input_sample_rate` comes back in
`session_ready`, rather than at the `sample_rate` it proposed. The input rate
is bound once at the handshake and never renegotiated, so a client that cannot
resample must leave this `false` (the default): the server then honours
`sample_rate` exactly. The browser SDK sets it to `true`.

Server behavior:

- On success: sends `session_ready` with `resumed: false` and a fresh `session_token`.
- Opus is not implemented. `audio_format: "opus"` is accepted so that a client can
  probe for it, but every worker downgrades the session to `pcm_i16_le`, logs a
  warning, and reports `negotiated.options.audio_format_fallback = "opus_to_pcm_i16_le"`.
  The binary frame type `0x02` is reserved for it and never sent. Clients must
  read the negotiated format rather than assume the one they asked for.
- On capacity/validation failure: sends `action: "error"` (`code: 1001`, session create failed) and closes.
- If no valid first handshake message arrives within 5 seconds: sends `action: "error"` (`code: 1000`, handshake timeout) and closes.

#### `action: "playback_complete"` — local playback finished

Sent by the client when it fully finishes playing the received audio buffer for a given utterance. Instructs the server to move from `draining` to `listening`.

```json
{
  "action": "playback_complete",
  "utterance_id": 1
}
```

#### `action: "cancel"` — barge-in / cancel

```json
{ "action": "cancel" }
```

Stops the current LLM+TTS turn immediately.  
Triggers `PipelineEvent::Interrupted { trigger: ClientCancel }` internally.

#### `action: "ping"` — liveness probe

```json
{
  "action": "ping",
  "timestamp_ms": 1714305600000
}
```

Server responds with `action: "pong"` echoing `client_timestamp_ms`.

#### `action: "resume"` — reconnect to previous session

Must be the first client control message after WebSocket open.

```json
{
  "action": "resume",
  "session_token": "<opaque-token>"
}
```

Server behavior:

- On success: sends `session_ready` with `resumed: true`, returns a rotated `session_token`, then replays buffered server messages for the resumed session.
- On invalid/expired token: sends `action: "error"` (`code: 1002`, session resume failed) and closes.
- Any non-`configure`/non-`resume` first message: sends `action: "error"` (`code: 1003`, invalid handshake) and closes.

#### `action: "inject_audio"` — benchmark audio injection

```json
{
  "action": "inject_audio",
  "source": "/path/to/file.wav",
  "scenario_id": "my-bench-01",     // optional
  "experiment_id": "exp-42",        // optional
  "speed": 1.0                      // 1.0 = real-time, 0.0 = as-fast-as-possible
}
```

#### `action: "playback_report"` — client audio offset report

Sent by the client after it starts playing audio to report how much it
actually played (sample-accurate barge-in support).

```json
{
  "action": "playback_report",
  "utterance_id": 3,
  "last_played_offset_ms": 2340,
  "last_played_text": "Once upon a time"   // optional, best-effort
}
```

Used to compute `retained_text` in `HistoryModified` and to set
`client_interrupted: true` in the per-utterance summary.

#### `action: "timing_report"` — client timing event

```json
{
  "action": "timing_report",
  "utterance_id": 3,
  "event": "playback_start",            // see table below
  "client_timestamp_ms": 1714305601234,
  "detail": { ... }                     // optional
}
```

`event` values:

| `event` | Meaning |
|---|---|
| `speech_start` | Client microphone captured first speech frame |
| `speech_end` | Client VAD detected end of speech. May be sent **more than once** per turn — see below |
| `playback_start` | Client started playing TTS audio |
| `audio_receive` | Client received first audio byte from server |
| `playback_end` | Client finished playing TTS audio |
| `first_byte_played` | Client played first PCM sample |
| `underrun` | Client audio buffer underrun (glitch) |

**`speech_end` and pauses.** A voice-activity detector cannot tell a pause from
the end of a turn; only the server's commit can. A speaker who hesitates —
"…the internal bond in, *uh*, particle boards" — produces a `speech_end` at
the hesitation and another when they actually stop. Send both, under the same
`utterance_id`: every consumer of these reports anchors the turn on the
**last `speech_end` that precedes the reply's first audio**, which is when the
speaker stopped. Report a later end only while the server is still
`listening`; once it has moved to `processing` the utterance is committed,
and speech after that belongs to something else.

Sending only the first — the older contract was "at most one per turn" —
measures the reply from the pause. On a real session that reported a 1.1 s
response as 7.7 s.

#### `action: "playback_quality"` — per-turn playback health

Sent once per turn, at playback-complete, by clients that track output
scheduling (the reference SDK does). Reports jitter-buffer underruns as a gap
list positioned in the **ideal (gapless) audio timeline**, plus aggregates.
Drives the `{utt:03}_playback.json` capture artifact,
`SessionDetail.playback_health`, and the admin backend's "as-heard" audio
reconstruction (`VP_PLAYBACK_AUDIT_FIDELITY_PLAN`). Optional — omit for empty
turns (`audio_ms == 0`); older clients simply never send it.

```json
{
  "action": "playback_quality",
  "utterance_id": 42,
  "underrun_count": 33,
  "total_silence_ms": 9040,
  "audio_ms": 16120,
  "heard_ms": 16120,
  "wall_ms": 25160,
  "gaps": [ { "at_audio_ms": 480, "silence_ms": 288 } ]
}
```

| Field | Meaning |
|---|---|
| `underrun_count` | Number of playback breaks (== `gaps.length`) |
| `total_silence_ms` | Total inserted silence (== Σ `gaps[].silence_ms`) |
| `audio_ms` | Ideal audio delivered to the client (Σ chunk durations) |
| `heard_ms` | Audio actually heard (enqueue-bounded, per the SD-25 rule) |
| `wall_ms` | Client wall-clock span of the turn's playback |
| `gaps[].at_audio_ms` | Underrun position in the ideal timeline — indexes straight into the server-side TTS fragment concat |
| `gaps[].silence_ms` | How long the output was silent there |

#### `action: "client_tool_result"` — client-side tool outcome

Reports the outcome of a client-fulfilled tool call — the server-issued
`trigger_action` command (see §4.2). Correlated by the `call_id` from the
command's payload.

```json
{
  "action": "client_tool_result",
  "call_id": "call_abc123",
  "phase": "completed",          // "started" | "progress" | "completed" | "failed"
  "result": { "screen": "settings" },   // optional, kind-specific JSON
  "speak": false                 // optional: request a spoken confirmation
}
```

- `started` / `progress` are telemetry only; the call stays in flight.
- `completed` / `failed` resolve the call: the (untrusted, size-capped)
  `result` is injected into the model's conversation context — silently by
  default, spoken as a generated confirmation turn only when the tool was
  declared with `speak_on_result = true` or the event sets `speak: true`.
- Unknown, expired, or already-resolved `call_id`s are logged and dropped
  (duplicate `completed` is a no-op after the first).

---

## 4. Server → Client messages

### 4.1 Binary — TTS audio stream

PCM frames as described in §2.1.  
Before each TTS fragment's first binary frame, the server sends an
`"audio_meta"` JSON frame (see §4.7).

### 4.2 `"message"` — unified structured message

Same envelope as client `"message"`.  Used for LLM text responses and
tool calls emitted by the pipeline when in text/hybrid mode.

Notable server-emitted `kind`s:

| `kind` | Description |
|---|---|
| `tool_call` / `tool_result` | Server/agent tool transparency — *facts about tool activity, not transcripts*; render as activity indicators, never as spoken content |
| `trigger_action` | **Client-side tool command**: payload `{ call_id, name, arguments }`. The client executes the command and reports back with `action: "client_tool_result"` (§3.5). Issued silently — no spoken filler accompanies it |

### 4.3 `"text"` — legacy text output *(deprecated)*

LLM text response.

### 4.4 `"data"` — legacy structured data *(deprecated)*

Rich structured pipeline output.

### 4.5 `"control"` — server control signals

#### `action: "session_ready"` — handshake complete

```json
{
  "action": "session_ready",
  "session_id": "09026b59-45a3-4ab7-ae02-019d51646253",
  "session_token": "<opaque-token>",
  "resumed": false,
  "client_config": {
    "audio_format": "pcm_i16_le",
    "sample_rate": 48000,
    "output_sample_rate": 24000,
    "channels": 1,
    "language": "en-US",
    "voice": "af_sky",
    "mode": "voice",
    "tags": ["live", "prod"],
    "app_data": { "session_source": "web" },
    "options": { "output_chunking_enabled": true, "output_chunk_frame_ms": 50 }
  },
  "negotiated": {
    "audio_format": "pcm_i16_le",
    "input_sample_rate": 48000,
    "output_sample_rate": 24000,
    "channels": 1,
    "language": "en-US",
    "voice": "af_sky",
    "mode": "voice",
    "first_utterance_id": 1,
    "text_utterance_id_base": 10000
  }
}
```

Notes:

- `negotiated.first_utterance_id` is the id the server gives the session's
  first **spoken** utterance, and spoken ids count up from it by one. It is
  `1` in every session kind; read it rather than assuming it. A client that
  files its own [`timing_report`](#action-timing_report--client-timing-marker)s
  has to name the turn a `speech_end` belongs to before the server has
  announced that turn, so it predicts the id — the first turn is
  `first_utterance_id`, and each one after is one past the highest spoken id
  seen so far. A server too old to send the field uses `1`.
- `negotiated.text_utterance_id_base`: ids above this belong to **typed-text**
  turns, which are numbered from a range of their own (`10001`, `10002`, …)
  so they never collide with spoken ones. Leave them out when predicting the
  next spoken id, or a single typed message moves the prediction past ten
  thousand and every spoken turn after it is mislabelled.
- `resumed: false` indicates a new session created by `configure`.
- `resumed: true` indicates session reattachment via `resume`.
- The returned `session_token` is authoritative for the next reconnect attempt.

#### `action: "state_changed"` — pipeline processing state

```json
{
  "action": "state_changed",
  "state": "listening"          // "listening" | "processing" | "speaking" | "draining"
}
```

| State | Meaning |
|---|---|
| `listening` | Waiting for user speech |
| `processing` | STT/LLM/TTS running |
| `speaking` | TTS audio is being streamed to client |
| `draining` | Server finished streaming, waiting for client playback to conclude |

Every `state_changed` reports a state the server entered; the frame is never sent for a transition the server refused. A new utterance committed while `draining` moves straight to `processing` (the previous turn is finished on the server; a `clear_audio` for its tail precedes the new turn). A `cancel` with no turn in flight produces only a `clear_audio` and no state change.

#### `action: "text_truncated"` — accurate server-side transcript truncation

Sent during a barge-in event. Indicates exactly what fraction of the agent's response was spoken so the client can trim its message history definitively.

```json
{
  "action": "text_truncated",
  "utterance_id": 1,
  "final_text": "Once upon a time",
  "reason": "barge_in"
}
```

#### `action: "utterance_dropped"` — speech that made no turn

Sent when the user spoke and no turn came of it: the recogniser returned no words (`empty_transcript`), too few for the configured minimum (`too_short`), or never answered the end of the utterance (`stt_timeout`, after which the worker restarts the recogniser). The pipeline stays in `listening`; no `state_changed` follows. `utterance_id` is the id the speech would have become, which the next committed utterance then takes. `audio_ms` is how much speech the recogniser was given.

Show it: from the client's side speech ended and nothing followed, and after a barge-in that looks like an agent that was cut off and then refused to answer. The Playground puts a system line in the transcript.

```json
{
  "action": "utterance_dropped",
  "utterance_id": 12,
  "reason": "empty_transcript",
  "audio_ms": 1254
}
```

#### `action: "turn_metrics"` — per-turn latency report

Sent after each turn completes or is interrupted.

```json
{
  "action": "turn_metrics",
  "utterance_id": 2,
  "total_ms": 1658,
  "ttft_ms": 1019,
  "ttfb_ms": 1179,
  "interrupted": false,
  "stt_latency_ms": 6761,
  "llm_generation_ms": 1278,
  "llm_tokens": 22,
  "tts_ttfb_ms": 159,
  "endpointing_latency_ms": 16
}
```

See §11 for metric definitions.

#### `action: "clear_audio"` — flush playback buffer

Sent on barge-in; instructs the client to stop playing stale audio
immediately.

```json
{
  "action": "clear_audio",
  "utterance_id": 1,
  "reason": "barge_in"         // "barge_in" | "cancel"
}
```

#### `action: "pong"` — ping response

```json
{
  "action": "pong",
  "client_timestamp_ms": 1714305600000,
  "server_timestamp_ms": 1714305600005
}
```

#### `action: "quality_update"` — connection health

```json
{
  "action": "quality_update",
  "quality": "excellent",      // "excellent" | "good" | "poor" | "lost"
  "tier": "full"               // "full" | "reduced" | "minimal" | "critical"
}
```

Sent when the session's fidelity `tier` changes, not on every measurement.
`quality` is the ping/pong bucket at that moment:

| Quality | Condition |
|---|---|
| `excellent` | smoothed RTT ≤ 100 ms |
| `good` | smoothed RTT ≤ 300 ms |
| `poor` | smoothed RTT above 300 ms |
| `lost` | two consecutive pings unanswered |

`tier` is what the server decided from every link signal it has (ping RTT,
the client's `net_report` and `playback_quality`, its own socket-write
latency). It only ever leaves `full` when `[adaptive_quality]` is enabled on
the worker. Clients should degrade in step: a deeper jitter buffer, a lower
avatar update rate, a "poor connection" hint.

#### `action: "error"` — pipeline error

```json
{
  "action": "error",
  "code": 4001,
  "message": "TTS synthesis failed: the TTS backend appears to be unreachable",
  "severity": "error",         // "warning" | "error" | "fatal"
  "turn_id": "3",              // optional
  "component": "tts"           // "stt" | "llm" | "tts" | "pipeline" | "transport"
}
```

`code` is the numeric `ErrorCode` (see [src/transport/error.rs](../../voice-pipes/src/transport/error.rs)),
never a string: 1000–1999 transport and connection, 2000–2999 VAD and STT,
3000–3999 LLM, 4000–4999 TTS. The label names used elsewhere in this document
(`INVALID_HANDSHAKE`, `TTS_SYNTHESIS_FAILED`, …) are documentation aids; the
full code-to-recovery matrix is in [Building a client §8](building-a-client.md#8-comprehensive-error-namespace--standard-recovery).
`severity` drives client behaviour: `fatal` tears the session down; `error`
marks the current turn failed but keeps the session; `warning` is informational
(the pipeline continues).

**Connect-time component warm-up warnings.** Immediately after `session_ready`,
the worker warms up the STT/TTS/LLM backends in the background. If a backend is
unreachable or fails to load, the worker emits a non-fatal `warning` error
*before the first turn* — so a misconfigured or down sidecar (e.g. the local
VibeVoice TTS server not running) is visible in the client's event log the
moment it connects, instead of only surfacing when the user first speaks. The
same failure flips that component to `unhealthy` on `GET /health`. Codes:

| Code | Numeric | `component` | Meaning |
|---|---|---|---|
| `STT_WARMUP_FAILED` | 2001 | `stt` | STT provider warm-up failed (model load / connection). |
| `LLM_WARMUP_FAILED` | 3001 | `llm` | A text processor / LLM backend warm-up failed. |
| `TTS_WARMUP_FAILED` | 4002 | `tts` | TTS provider warm-up failed (e.g. sidecar refused connection). |

These are distinct from the runtime turn-failure codes (e.g. `TTS_SYNTHESIS_FAILED` 4001)
so a client can tell "the backend never came up" from "a turn failed".

#### `action: "closing"` — graceful shutdown

```json
{
  "action": "closing",
  "reason": "session_ended"
}
```

The last frame of a session the server ends. `reason` is `session_ended`
when the session itself finished, and `worker_shutdown` when the worker is
stopping (Ctrl-C or SIGTERM): it ends every open session this way, turns
away new ones at `/readyz`, and waits up to ten seconds for them to close.
Reconnecting gets another worker.

### 4.6 `"audio_meta"` — TTS caption frame

Sent as a JSON frame immediately **before** the first binary audio frame
of each TTS fragment.

```json
{
  "type": "audio_meta",
  "payload": {
    "utterance_id": 1,                 // always present
    "text": "Once upon a time in a small village,",
    "fragment_idx": 0,
    "is_final": false,                 // last fragment of the utterance
    "total_fragment_duration_ms": 2500,
    "word_timings": [
      { "word": "Once", "start_ms": 0,   "end_ms": 120 },
      { "word": "upon", "start_ms": 130, "end_ms": 220 },
      ...
    ],
    "full_text": "Once upon a time ...",// optional: full utterance text
    "extras": { },                     // optional: provider-specific metadata
    "server_timestamp_ms": 1719792000000  // always present
  }
}
```

`utterance_id` and `server_timestamp_ms` are always present. `text`, `fragment_idx`, `is_final`,
`total_fragment_duration_ms`, and `word_timings` are present when a caption is attached;
`full_text` and `extras` are emitted only when supplied.

`total_fragment_duration_ms` is optional; present if the backend precalcualted the entire string duration before starting audio transfer.
`word_timings` is optional; present only when the TTS provider supplies
word-level alignment (e.g. ElevenLabs with timestamps enabled).

---

## 5. Internal pipeline events (`PipelineEvent`)

These events flow over an internal `tokio::sync::broadcast` channel.
They are **not** sent to the client directly. They are consumed by:

- The **streaming audit sink** → written to `pipeline_events.jsonl`
- Any external analytics / agentic memory subscriber

```json
// Serialisation: {"kind": "<snake_case>", ...fields}
```

| `kind` | Fields | Meaning |
|---|---|---|
| `utterance_received` | `utterance_id`, `text`, `source` ("Audio"/"Text") | New utterance arrived from STT or text input |
| `stt_completed` | `utterance_id`, `elapsed_ms?` | STT produced the final transcript (`elapsed_ms` None for text-source utterances) |
| `llm_started` | `utterance_id` | LLM chain started processing |
| `llm_first_token` | `utterance_id`, `elapsed_ms` | Time from utterance commit to first LLM token |
| `llm_completed` | `utterance_id`, `text_len`, `elapsed_ms`, `is_partial` | Full LLM generation finished (`is_partial` = truncated by interruption) |
| `tts_started` | `utterance_id` | TTS synthesis started |
| `tts_first_audio` | `utterance_id`, `elapsed_ms` | First TTS audio chunk ready |
| `tts_completed` | `utterance_id` | TTS finished all audio |
| `turn_completed` | `utterance_id`, `total_ms` | Turn ended without interruption |
| `interrupted` | `utterance_id`, `trigger`, `played_ms?` | User interrupted the assistant |
| `barge_in_resolved` | `utterance_id`, `trigger`, `played_ms?`, `retained_text`, `barge_in_rtt_ms?` | Resolved barge-in record, emitted once per interrupted turn after history truncation; supersedes `interrupted` |
| `history_modified` | `modification` (see below) | Conversation history truncated after interruption |
| `client_timing` | `utterance_id`, `timing_event`, `client_timestamp_ms` | Client-reported timing forwarded into the pipeline event bus |
| `caption_emitted` | `utterance_id`, `fragment_idx`, `text` | Caption text emitted for an audio segment |
| `pipeline_error` | `utterance_id?`, `component`, `message` | A pipeline component reported an error |
| `turn_metrics_ready` | `utterance_id`, `metrics` (`TurnMetricsSnapshot`) | Per-turn latency breakdown, surfaced after the turn completes |

### `interrupted.trigger` values

| Value | Meaning |
|---|---|
| `server_vad` | Server-side VAD detected user speech during playback |
| `client_cancel` | Client sent `action: "cancel"` |
| `early_barge` | User spoke over the assistant during LLM processing (early barge-in) |

### `history_modified.modification` object

```json
{
  "utterance_id": 1,
  "original_text": "Once upon a time ... end of story.",
  "retained_text": "Once upon a time in a small village,",
  "method": "sentence_boundary",     // "sentence_boundary" | "playback_report"
  "played_ms": null                   // ms of audio played; present only for "playback_report"
}
```

---

## 6. Audit trail — on-disk capture artefacts

Written by the pipeline's `StreamingAuditSink` to a local filesystem
(configurable via `captures_dir` in `orchestrator.toml`).

### 6.1 Capture directory layout

```
captures/
  {session_id}/                        UUID v4, e.g. 09026b59-45a3-4ab7-ae02-019d51646253
    metadata.json                      Session-level metadata
    pipeline_events.jsonl              One PipelineEvent JSON object per line
    events.jsonl                       One TraceEvent JSON object per line
    NNN_mic_raw.wav                    Raw microphone audio at input sample rate
    NNN_mic_16k.wav                    Microphone audio resampled to 16 kHz (STT input)
    NNN_stt.json                       STT transcription result
    NNN_llm_input.txt                  Verbatim text sent to the LLM
    NNN_llm_output.json                Complete LLM text response (may be absent if interrupted early)
    NNN_tts_frag_MMM.txt               Text of TTS fragment MMM
    NNN_tts_frag_MMM.wav               Audio of TTS fragment MMM
    NNN_summary.json                   Per-turn metrics written at turn end
    NNN_playback.json                  Client-reported playback health (underrun gaps), when the client sent `playback_quality`
```

`NNN` = zero-padded 3-digit utterance index (000, 001, 002, …).  
`MMM` = zero-padded 3-digit TTS fragment index (000, 001, …).

### 6.2 `NNN_summary.json`

Written by `AuditEvent::UtteranceDone` at the end of every turn.

> **Important:** When the pipeline is interrupted mid-generation this file
> may be written **before** the `interrupted` event is recorded in
> `pipeline_events.jsonl`. The admin BFF overrides `interrupted` with `true`
> when a `kind: "interrupted"` line exists in `pipeline_events.jsonl`.

```json
{
  "utterance_id": 1,
  "interrupted": false,
  "terminated": false,
  "total_latency_ms": 4938,
  "stt_provider": "assemblyai",
  "tts_provider": "piper",
  "stt_latency_ms": 5375,
  "ttft_ms": 1617,
  "ttfb_ms": 1814,
  "llm_generation_ms": 1279,
  "tts_ttfb_ms": 196,
  "endpointing_latency_ms": 19,
  "timestamp": 1714305601234
}
```

| Field | Type | Description |
|---|---|---|
| `utterance_id` | `u32` | 0-based index within the session |
| `interrupted` | `bool` | Server detected barge-in before TTS completed |
| `terminated` | `bool` | Session disconnected while turn was in-flight |
| `total_latency_ms` | `u64` | Wall-clock from utterance commit to end of TTS |
| `stt_provider` | `string` | e.g. `"assemblyai"`, `"parakeet-tdt"` |
| `tts_provider` | `string` | e.g. `"piper"`, `"kokoro"`, `"elevenlabs"` |
| `stt_latency_ms` | `u64?` | End-of-speech flush → final transcript |
| `ttft_ms` | `u64?` | Utterance commit → first LLM token |
| `ttfb_ms` | `u64?` | Utterance commit → first TTS audio byte |
| `llm_generation_ms` | `u64?` | First LLM token → last LLM token |
| `tts_ttfb_ms` | `u64?` | First LLM text fragment → first TTS audio sample |
| `endpointing_latency_ms` | `u64?` | Last speech frame → VAD turn-commit decision |
| `timestamp` | `u64` | Unix epoch ms when file was written |

### 6.3 `NNN_stt.json`

```json
{
  "utterance_id": 1,
  "text": "Tell me a story.",
  "latency_ms": 1603
}
```

Absent for text-mode turns (no audio input).

### 6.4 `NNN_llm_output.json`

```json
{
  "utterance_id": 1,
  "text": "Once upon a time in a small village...",
  "latency_ms": 1410
}
```

**May be absent** when the turn was interrupted before LLM generation
completed. The admin BFF synthesises this field from the `NNN_tts_frag_*.txt`
files in that case.

### 6.5 `NNN_llm_input.txt`

Plain-text prompt that was passed to the LLM (the assembled history +
system prompt + current utterance). Same content as the `llm_input`
displayed in the session detail collapsible section.

### 6.6 `NNN_tts_frag_MMM.txt` / `.wav`

One pair of files per TTS synthesis fragment. The `.txt` contains the
text sentence/phrase and the `.wav` contains the corresponding PCM audio
at the TTS provider's native sample rate.

Fragment index starts at 000 and increments for each sentence the LLM
produces. Fragments are streamed — the number of fragments per turn
varies.

### 6.7 `NNN_mic_raw.wav` / `NNN_mic_16k.wav`

- `_mic_raw.wav` — audio as received from the client (48 kHz by default)
- `_mic_16k.wav` — audio after resampling to 16 kHz for STT input

Both are mono PCM WAV files.

### 6.8 `pipeline_events.jsonl`

One JSON object per line; each line is a serialised `PipelineEvent`
(see §5). Written in real-time as the pipeline processes each turn.

This file is the **authoritative** record of:
- Whether a turn was interrupted (`kind: "interrupted"`)
- The retained text after barge-in (`kind: "history_modified"`)
- Client timing events (`kind: "client_timing"`)
- Caption text per fragment (`kind: "caption_emitted"`)

Example excerpt:

```jsonl
{"kind":"utterance_received","utterance_id":1,"text":"Tell me a story.","source":"Audio"}
{"kind":"llm_started","utterance_id":1}
{"kind":"llm_first_token","utterance_id":1,"elapsed_ms":1617}
{"kind":"tts_started","utterance_id":1}
{"kind":"tts_first_audio","utterance_id":1,"elapsed_ms":1814}
{"kind":"caption_emitted","utterance_id":1,"fragment_idx":0,"text":"Once upon a time in a small village,"}
{"kind":"client_timing","utterance_id":2,"timing_event":"playback_start","client_timestamp_ms":1777393385002}
{"kind":"interrupted","utterance_id":1,"trigger":"server_vad","played_ms":null}
{"kind":"history_modified","modification":{"utterance_id":1,"original_text":"...","retained_text":"Once upon a time","method":"sentence_boundary","played_ms":null}}
{"kind":"tts_completed","utterance_id":1}
{"kind":"turn_completed","utterance_id":2,"total_ms":1658}
```

### 6.9 `events.jsonl` (trace events)

One JSON object per line; each line is a `TraceEvent` emitted by
individual pipeline components for observability.

```json
{
  "timestamp": "2026-04-28T16:22:53Z",
  "component": "stt:assemblyai",
  "event_type": "llm_timing",
  "payload": {
    "utterance_id": 1,
    "elapsed_ms": 1410,
    "first_token_ms": 1617,
    "generate_ms": 1279,
    "output_tokens": 24,
    "tokens_per_sec": 18.8,
    "model": "gpt-4o-mini"
  }
}
```

Common `event_type` values (emitted by pipeline components):

| `event_type` | `component` | `payload` fields |
|---|---|---|
| `llm_timing` | `llm:<model>` | `utterance_id`, `elapsed_ms`, `first_token_ms`, `generate_ms`, `output_tokens`, `prompt_tokens`, `tokens_per_sec`, `model`, `cache_reset_ms`, `prompt_ingest_ms`, `device`, `prefill_cache` |
| `stt_usage` | `stt:<provider>` | `audio_duration_seconds`, `session_duration_seconds`, `speech_model` |
| `tts_usage` | `tts:<provider>` | `chars`, `audio_duration_s`, `voice` |
| `client_timing` | `transport` | `utterance_id`, `event`, `client_timestamp_ms` |
| `barge_in_rtt` | `pipeline` | `utterance_id`, `rtt_ms`, `last_played_offset_ms` |
| `transcription` | `stt:<provider>` | `text`, `confidence`, `latency_ms` |

### 6.10 `metadata.json`

Session-level metadata, written at session start before any other artefact.

```json
{
  "id": "09026b59-45a3-4ab7-ae02-019d51646253",
  "start_time": "2026-04-28T16:22:53.612Z",
  "pipeline_name": "live-openai",
  "pipeline_config_hash": "d0b9d2d9f601dc2903e18ce23647d522",
  "stt_provider": "assemblyai",
  "tts_provider": "piper",
  "stt_mode": "streaming",
  "tts_voice": "en_US-lessac-medium",
  "input_sample_rate": 48000,
  "output_sample_rate": 48000,
  "tags": []
}
```

| Field | Meaning |
|---|---|
| `pipeline_config_hash` | Hash of the pipeline-relevant config (transport and audit settings excluded), the same value the worker registers with |
| `stt_mode` | `"streaming"` when the STT provider declares the `Streaming` capability, otherwise `"batch"`; `null` on realtime sessions. The diagnostic report uses it to decide whether STT processing time counts towards the user's wait |
| `tts_voice` | The voice the session speaks with: the client's `configure` voice, else the configured `[tts] voice`; `null` when neither is set |

The file also carries the resolved model identity and compute device per
component (`stt_model`, `llm_model`, `tts_model`, `*_device`) and the metric
descriptors used by the session view.

---

### 6.11 `NNN_playback.json`

Written when the client sends the optional `playback_quality` control frame
(§3.5): the per-turn playback-health payload verbatim, minus the utterance id
(implied by the filename). Absent for older clients, empty turns, and turns
whose playback never completed.

```json
{
  "underrun_count": 2,
  "total_silence_ms": 550,
  "audio_ms": 16120,
  "heard_ms": 16120,
  "wall_ms": 16670,
  "gaps": [ { "at_audio_ms": 480, "silence_ms": 300 } ]
}
```

Consumers: the admin BFF projects the aggregates onto the turn summary
(`audio_underruns`, `total_silence_ms`, `wall_clock_ms`, `playback_gaps`) and
the `…/audio/turn/{utt}/as-heard.wav` route splices the gap silences back into
the TTS fragment concat to reconstruct what the listener actually heard.
The same payload is stored on the orchestrator turn record as
`SessionDetail.playback_health` (best-effort — the frame usually arrives after
the server-side turn record is built; the capture artifact is authoritative).

## 7. Orchestrator REST API

Base URL: `http://host:9090`

All responses use the envelope `{ "data": T | null, "error": { "code": string, "message": string } | null }`.

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/health` | Fleet health: worker counts, active sessions, uptime |
| `GET` | `/healthz` | Kubernetes liveness probe |
| `GET` | `/readyz` | Kubernetes readiness probe (503 when no workers) |
| `GET` | `/api/v1/config` | Orchestrator config (includes `captures_dir`) |
| `GET` | `/api/v1/config/pipeline` | Pipeline TOML config from first registered worker |
| `GET` | `/api/v1/pipeline` | Pipeline graph (nodes + edges) for the UI visualisation |
| `GET` | `/api/v1/metrics` | Aggregate latency stats, request counts, error totals |
| `GET` | `/api/v1/workers` | List all registered workers with status and capacity |
| `GET` | `/api/v1/workers/{id}` | Single worker detail |
| `GET` | `/api/v1/pipelines` | Workers grouped by pipeline config hash |
| `GET` | `/api/v1/pipelines/diff` | Structural diff between two pipeline config hashes |
| `GET` | `/api/v1/dispatch` | Intelligent worker dispatch (select best worker) |
| `GET` | `/api/v1/sessions` | Paginated session list |
| `GET` | `/api/v1/sessions/{id}` | Single session detail + turns |
| `GET` | `/api/v1/sessions/{id}/turns` | Turns for a session |
| `GET` | `/api/v1/turns` | Flat paginated list of all turns (filterable) |
| `GET` | `/api/v1/turns/{id}` | Single turn detail |
| `GET` | `/api/v1/benchmark/experiments` | List benchmark experiments |
| `GET` | `/api/v1/benchmark/experiments/{id}` | Single experiment |
| `GET` | `/api/v1/benchmark/experiments/{id}/results` | Results for an experiment |
| `POST` | `/api/v1/benchmark/experiments` | Create a new benchmark experiment |

### Worker registration (worker → orchestrator)

Workers register over HTTP on startup and send periodic heartbeats.
The orchestrator uses the worker's `pipeline_config` JSON (a serialised
`PipelineConfig`) to build the pipeline graph and diff view.

---

## 8. Admin BFF REST API

Base URL: `http://host:8000`

The BFF proxies most requests to the orchestrator and enriches them with
local capture artefacts.

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/health` | Proxy to orchestrator health |
| `GET` | `/api/v1/sessions` | Proxy sessions list |
| `GET` | `/api/v1/sessions/{id}` | Sessions detail merged with local capture artefacts |
| `GET` | `/api/v1/sessions/{id}/turns` | Proxy turns for session |
| `GET` | `/api/v1/sessions/{id}/audio/{filename}` | Serve WAV file from capture dir |
| `GET` | `/api/v1/turns` | Proxy flat turns list (filterable by `status`, `pipeline_name`, `worker_id`, `search`) |
| `GET` | `/api/v1/turns/{id}` | Proxy single turn |
| `GET` | `/api/v1/workers` | Proxy workers list |
| `GET` | `/api/v1/workers/{id}` | Proxy worker detail |
| `GET` | `/api/v1/pipeline` | Proxy pipeline graph |
| `GET` | `/api/v1/pipelines` | Proxy pipelines grouped by hash |
| `GET` | `/api/v1/pipelines/diff` | Proxy pipeline diff |
| `GET` | `/api/v1/dispatch` | Proxy dispatch; answers `ws_path` (the Playground proxy for the chosen worker) in place of the worker's own `ws_url` |
| `GET` | `/api/v1/metrics` | Proxy orchestrator metrics |
| `GET` | `/api/v1/analytics` | Aggregate analytics from captures (KPIs + per-utterance data) |
| `GET` | `/api/v1/benchmark/experiments` | Proxy benchmark experiments list |
| `GET` | `/api/v1/benchmark/experiments/{id}` | Proxy single experiment |
| `GET` | `/api/v1/benchmark/experiments/{id}/results` | Proxy experiment results |
| `POST` | `/api/v1/benchmark/experiments` | Proxy create experiment |
| `WS` | `/api/v1/playground/ws?worker_id=<id>` | Bidirectional WebSocket proxy to that worker's transport (`/ws`); only a worker the orchestrator lists (others closed `4403`), with `VP_CLIENT_TOKEN` presented by the BFF |

### `GET /api/v1/sessions/{id}` — enriched session object

The BFF merges orchestrator session data with local capture artefacts
into a single response:

```json
{
  "data": {
    "id": "09026b59-...",
    "metadata": {
      "status": "completed",
      "started_at": "2026-04-28T16:22:53.609Z",
      "ended_at": "2026-04-28T16:23:17.468Z",
      "pipeline_name": "live-openai",
      "pipeline_config_hash": "d0b9d2d9f601dc2903e18ce23647d522",
      "stt_provider": "assemblyai",
      "tts_provider": "piper",
      "worker_id": "worker-1",
      "turn_count": 2,
      "input_sample_rate": 48000,
      "output_sample_rate": 48000,
      "tags": []
    },
    "turns": [ ... ],           // orchestrator TurnRecord list
    "utterances": [             // enriched from capture artefacts
      {
        "utterance_id": 0,
        "stt": { "text": "Hello.", "latency_ms": 1603 },
        "llm_input": "...",
        "llm_output": { "text": "Hello! How can I assist you today?", "latency_ms": 1410 },
        "retained_text": null,
        "summary": { ... },     // NNN_summary.json + BFF-computed fields
        "audio_files": {
          "mic_raw": "000_mic_raw.wav",
          "tts_frag_000": "000_tts_frag_000.wav"
        }
      }
    ],
    "events": [ ... ],          // events.jsonl (paginated, 200/page)
    "events_total": 42,
    "telemetry": [ ... ]        // telemetry.jsonl
  }
}
```

### BFF-computed fields injected into `utterances[].summary`

| Field | Source | Description |
|---|---|---|
| `interrupted` | `pipeline_events.jsonl` `kind: "interrupted"` | Overrides `summary.json` value (fixes race condition) |
| `interrupt_trigger` | `pipeline_events.jsonl` `kind: "interrupted"` | `"server_vad"` or `"client_cancel"` |
| `client_interrupted` | `pipeline_events.jsonl` `kind: "history_modified"` with `method: "playback_report"` | True when server didn't interrupt but client reported playback offset |
| `barge_in_rtt_ms` | `events.jsonl` `event_type: "barge_in_rtt"` | Round-trip time from server VAD detection to interrupt |
| `last_played_offset_ms` | `events.jsonl` `event_type: "barge_in_rtt"` | ms of audio played at interrupt |

### BFF-computed fields injected into `utterances[]`

| Field | Source | Description |
|---|---|---|
| `retained_text` | `pipeline_events.jsonl` `kind: "history_modified"` | Authoritative truncated text |
| `retained_text` (fallback) | TTS `.txt` + `.wav` files + `last_played_offset_ms` | Proportional word-slice when `history_modified` absent |
| `llm_output` (fallback) | All `NNN_tts_frag_*.txt` files joined | Reconstructed when `NNN_llm_output.json` absent (interrupted early) |
| `user_text` | `pipeline_events.jsonl` `kind: "utterance_received"` with `source: "Text"` | User message for text-mode turns |

---

## 9. Admin UI data consumption

### 9.1 Dashboard KPIs & analytics

Source: `GET /api/v1/analytics?days=N`

Computed by the BFF by scanning all `captures/{session_id}/NNN_summary.json`
and `events.jsonl` files within the requested time window.

**KPI block:**

| Field | Description |
|---|---|
| `total_sessions` | Sessions within the window |
| `total_utterances` | Utterance turns within the window |
| `avg_latency_ms` | Mean `total_latency_ms` across all utterances |
| `p95_latency_ms` | 95th-percentile `total_latency_ms` |
| `avg_tokens_per_sec` | Mean LLM generation throughput |

**Per-utterance analytics rows** (one per turn, displayed in latency chart):

| Field | Source |
|---|---|
| `total_latency_ms` | `summary.json` |
| `ttfb_ms` | `summary.json` |
| `ttft_ms` | `summary.json` |
| `endpointing_latency_ms` | `summary.json` |
| `stt_provider` / `tts_provider` | `summary.json` / `metadata.json` |
| `pipeline_name` | `metadata.json` |
| `tokens_per_sec` | `events.jsonl` `llm_timing` |
| `first_token_ms` | `events.jsonl` `llm_timing` |
| `prompt_tokens` / `output_tokens` | `events.jsonl` `llm_timing` |
| `generate_ms` | `events.jsonl` `llm_timing` |
| `model` | `events.jsonl` `llm_timing` |
| `stt_audio_duration_s` | `events.jsonl` `stt_usage` |
| `tts_chars` / `tts_audio_duration_s` / `tts_voice` | `events.jsonl` `tts_usage` |
| `client_ttfa_ms` | `events.jsonl` `client_timing` `playback_start` − `speech_end` (client-clock duration, ms) |
| `audio_underruns` | `events.jsonl` `client_timing` `underrun` count |
| `barge_in_rtt_ms` | `events.jsonl` `barge_in_rtt` |

**Provider/pipeline change events** — detected by comparing consecutive
sessions' `stt_provider`, `tts_provider`, `pipeline_name` fields.

### 9.2 Sessions list

Source: `GET /api/v1/sessions`  
Proxy to orchestrator. Fields per session: `id`, `status`, `started_at`,
`ended_at`, `pipeline_name`, `worker_id`, `turn_count`, `tags`.

### 9.3 Session detail

Source: `GET /api/v1/sessions/{id}`  
BFF-enriched response (see §8). UI components and the fields they consume:

| UI Component | Fields consumed |
|---|---|
| Session metadata panel | `metadata.*`, `id` |
| Turn card header | `summary.interrupted`, `summary.client_interrupted`, `summary.interrupt_trigger`, `summary.endpointing_latency_ms`, `summary.barge_in_rtt_ms`, `summary.ttft_ms`, `summary.ttfb_ms`, `summary.total_latency_ms`, `summary.terminated`, orchestrator `status` |
| Pipeline timeline chart | `summary.stt_latency_ms`, `summary.ttft_ms`, `summary.ttfb_ms`, `summary.llm_generation_ms`, `summary.tts_ttfb_ms`, `summary.total_latency_ms`, `summary.endpointing_latency_ms`, `events.jsonl` `llm_timing` |
| User speech bubble | `stt.text`, `user_text`, `audio_files.mic_raw` |
| AI response bubble | `llm_output.text`, `retained_text`, `summary.interrupted`, `summary.client_interrupted`, `summary.last_played_offset_ms`, TTS fragment audio files |
| LLM prompt collapsible | `llm_input` |
| Stats row (tok/s, RTF, chars) | `events.jsonl` `llm_timing`, `tts_usage`, `stt_usage` |
| Events log | `events` (paginated `events.jsonl` trace events) |
| Interruption chip ("VAD barge-in") | `summary.interrupted` + `summary.interrupt_trigger` |
| Interruption scissors ✂ | `retained_text` + `summary.interrupted/client_interrupted` |
| Interruption tooltip | `summary.last_played_offset_ms`, `summary.barge_in_rtt_ms`, `summary.interrupt_trigger` |
| Audio fragment player | `audio_files.tts_frag_*` → served by `/api/v1/sessions/{id}/audio/{filename}` |
| Telemetry panel | `metadata.*` (displayed as key-value table) |

### 9.4 Playground

Source: `WS /api/v1/playground/ws?worker_id=<id>` (the BFF proxies to that worker's `/ws`, presenting `VP_CLIENT_TOKEN` itself), or a Direct URL the browser opens itself

The Playground page connects via WebSocket and exchanges the full
Client↔Server protocol defined in §3 and §4.

---

## 10. Derived/computed fields

These fields do not exist in any raw file; they are computed at read time.

| Field | Where computed | Formula |
|---|---|---|
| `retained_text` (BFF fallback) | Admin BFF | Proportional word-slice of TTS fragments weighted by WAV file duration up to `last_played_offset_ms` |
| `llm_output.text` (BFF fallback) | Admin BFF | All `NNN_tts_frag_*.txt` files joined with spaces |
| `client_ttfa_ms` | Admin BFF analytics | `client_timing.playback_start.client_timestamp_ms` − `client_timing.speech_end.client_timestamp_ms` (client-clock duration, ms; `None` if either endpoint missing) |
| `audio_underruns` | Admin BFF analytics | Count of `client_timing` events with `event: "underrun"` |
| `tokens_per_sec` | Admin BFF analytics | `output_tokens / (elapsed_ms / 1000)` |
| `interrupted` override | Admin BFF | Set to `true` when `pipeline_events.jsonl` contains `kind: "interrupted"` for the utterance |
| Pipeline graph | Orchestrator | Derived from worker's `PipelineConfig` TOML via `build_pipeline_graph()` |
| `p95_latency_ms` | Admin BFF analytics | Sort all `total_latency_ms` values; take index at 95% |

---

## 11. Latency metric definitions

All timings are measured from **utterance commit** (= the moment the STT
transcript is finalised and the pipeline begins LLM processing), except
where noted.

| Metric | Abbreviation | Definition | Where stored |
|---|---|---|---|
| Time to First Token | TTFT | Utterance commit → first LLM output token | `summary.ttft_ms` |
| Time to First Byte | TTFB | Utterance commit → first TTS audio sample sent to client | `summary.ttfb_ms` |
| Total latency | — | Utterance commit → last TTS audio frame sent | `summary.total_latency_ms` |
| STT latency | — | End-of-speech flush → final STT transcript | `summary.stt_latency_ms` |
| LLM generation | — | First LLM token → last LLM token | `summary.llm_generation_ms` |
| TTS TTFB | — | First LLM text fragment → first TTS audio sample (pipeline-internal) | `summary.tts_ttfb_ms` |
| Endpointing latency | — | Last speech audio frame → VAD turn-commit decision | `summary.endpointing_latency_ms` |
| Barge-in RTT | — | Server VAD interrupt signal → client receives `clear_audio` | `events.jsonl barge_in_rtt` / `summary.barge_in_rtt_ms` |
| Client TTFA | — | Utterance commit → `playback_start` client timing event | `analytics.client_ttfa_ms` |
| LLM first token (within generate) | — | Time within `generate()` call until first token produced | `events.jsonl llm_timing.first_token_ms` |
| Real-time factor (RTF) | RTF | `total_latency_ms / (audio_duration_ms * 1000)` | `events.jsonl llm_timing.rtf` |

**Response latency** displayed in the turn card header = `summary.ttfb_ms`  
(time from utterance commit to first audio heard by the user — the most
user-visible latency number).
