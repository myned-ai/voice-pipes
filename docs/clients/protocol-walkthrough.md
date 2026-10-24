# Voice Pipeline WebSocket Protocol Documentation

Status: active
Owner: Admin Frontend
Last reviewed: 2026-09-19
Source of truth: no
Canonical reference: [../../docs/specs/MESSAGES-AND-EVENTS-SPEC.md](protocol.md)

> Note
> This document is a client/admin integration guide derived from the canonical wire and event contract.
> For protocol conflicts or implementation decisions, use [../../docs/specs/MESSAGES-AND-EVENTS-SPEC.md](protocol.md).

## 1. Message Order & Sequence
The server communicates with the client in a strictly ordered, event-driven lifecycle. A standard conversation turn follows this sequence:

1. **Session Setup:** Server acknowledges the connection, negotiates audio formats, and indicates it is ready to receive audio.
2. **User Turn (Listening):** The client streams microphone audio to the server. Once the user stops speaking, the server emits a transcription (`text`, role: `user`).
3. **Processing State:** The server enters processing mode (`state_changed` -> `processing`) while the LLM generates a response.
4. **Speaking State:** As soon as the text-to-speech (TTS) engine generates the first audio packet, the server moves to speaking mode (`state_changed` -> `speaking`).
5. **Streaming the Response:**
   * The server sends an `audio_meta` control message containing a fragment/sentence of text.
   * Immediately following, it pushes `audio` packets containing the TTS voice data and matching visual `caption` metadata.
   * This repeats for every sentence or fragment of the LLM's response.
   * If the worker has an audio annotator configured (`[[audio_post_processor]] mode = "annotate"`), `audio_annotation` frames for the same `utterance_id` interleave with the audio on the same socket (see §2.D). They are keyed by audio offset, not by packet, and may arrive slightly before or after the audio they describe.
6. **Turn Completion:**
   * The server indicates it has finished sending data for this turn (`state_changed` -> `draining` -> `listening`).
   * An empty `audio` packet (0 samples) is sent with the `full_text` payload as an End-of-Stream marker.
   * A final authoritative `text` message (role: `agent`) is sent containing the complete, unbroken LLM string.

### Handshake Rules (authoritative)

Before normal streaming begins, the client must send exactly one first control message:

1. `configure` to create a new session, or
2. `resume` with a previously issued `session_token` to reattach to a disconnected session.

Protocol guarantees:

- Any other first message is rejected with `action: "error"` (`code: 1003`, invalid handshake) and the socket closes.
- If no valid first handshake message arrives within 5 seconds, the server returns `action: "error"` (`code: 1000`, handshake timeout) and closes.
- Successful `resume` rotates the session token and returns the new token in `session_ready`.
- After `session_ready` in a resumed session, buffered server messages are replayed to the client.

---

## 2. Message Types & Parameters

### A. Control Messages (`kind: "control"`)
Used for state management, pipeline orchestration, and metadata handshakes.

| Action / Variant | Payload Example | Purpose of Parameters |
| :--- | :--- | :--- |
| **`configure`** | `{"action":"configure", "sample_rate":48000, "output_sample_rate":24000, "channels":1, "mode":"voice"}` | *(Client-to-Server, first message only)* Starts a new session and proposes client preferences. |
| **`resume`** | `{"action":"resume", "session_token":"<opaque-token>"}` | *(Client-to-Server, first message only)* Reconnects within grace period and requests replay of buffered server messages. |
| **`session_ready`** | `{"action":"session_ready", "client_config": {...}, "negotiated": {...}, "session_id": "...", "session_token": "...", "resumed": false}` | Fired when handshake succeeds. <br><br>• `resumed=false`: new session from `configure`.<br>• `resumed=true`: resumed session from `resume` (replay may follow).<br>• `session_token`: authoritative token for the next reconnect; token is rotated on successful resume. |
| **`state_changed`** | `{"action":"state_changed", "state":"processing"}` | Broadcasts the current active component of the pipeline.<br><br>• `state`: Can be `listening` (waiting for user voice), `processing` (LLM thinking/STT finalized), `speaking` (TTS sending voice packets), or `draining` (waiting for client playback to complete). |
| **`text_truncated`** | `{"action":"text_truncated", "utterance_id":1, "final_text":"Sure!", "reason":"barge_in"}` | Sent by the backend when an utterance is interrupted (barge-in or cancellation). Trims the chat transcript to exact spoken text instead of relying on client estimates.<br><br>• `utterance_id`: The ID of the interrupted utterance.<br>• `final_text`: The exact final string to replace the chat bubble with. |
| **`utterance_dropped`** | `{"action":"utterance_dropped", "utterance_id":12, "reason":"empty_transcript", "audio_ms":1254}` | Sent when the user spoke and no turn came of it; the pipeline stays in `listening`. Show it, or speech that interrupted the agent and was not recognised looks like an agent that refused to answer.<br><br>• `reason`: `empty_transcript` (no words), `too_short` (under the minimum) or `stt_timeout` (the recogniser never answered and was restarted).<br>• `audio_ms`: How much speech the recogniser was given. |
| **`playback_complete`** | `{"action":"playback_complete", "utterance_id":1}` | *(Client-to-Server)* Sent by the client to tell the backend that the UI has finished playing out the audio buffer. Moves the server from `draining` to `listening`. |
| **`timing_report`** | `{"action":"timing_report", "utterance_id":2, "event":"speech_end", "client_timestamp_ms":1724700000123}` | *(Client-to-Server, optional)* Client-side instrumentation stamps, persisted generically as `client_timing` trace events (the Session History reader pairs them by `utterance_id`).<br><br>• `event`: `speech_start` — the client VAD's start of speech (**changed**: it used to mean "first mic frame after the mic opened"); `speech_end` — the client VAD's end of speech (energy endpointer: enter 0.02 / exit 0.012 RMS, 300 ms hangover, 150 ms min speech), sent **at most once per user turn** and only while the user's turn is open (`listening` / `processing`, or a segment that began there) — never during agent playback unless the barge-in path has already taken the turn; the worker also uses a `speech_end` that arrives before the response as a fallback TTFB anchor (`ttfb_anchor_source: "client_speech_end"`), so it is sent promptly; `audio_receive` — first audio byte of the turn received; `playback_start` / `playback_end` — first sample audible / natural end of playback.<br>• `client_timestamp_ms`: `Date.now()` wall clock at the event. The reader tolerates clock offset via the ping/pong clock sync; the `speech_end` → `audio_receive` pairing is client-to-client, so skew cancels.<br>• `utterance_id`: for `speech_*` the client does not yet know the id of the turn it is about to trigger, so it sends **the highest id seen so far + 1** (`speech_start` during agent playback carries the interrupted utterance's id instead). |
| **`turn_metrics`** | `{"action":"turn_metrics", "snapshot": {"utterance_id":2, "total_ms":2830, "interrupted":false, "components":[{"component_id":"realtime","kind":"realtime","extras":{"ttfb_ms":1623,"first_text_ms":1081,"ttfb_anchor_source":"input_transcript_proxy","user_speech_end_epoch_ms":1724700000000,"vp_speech_end_epoch_ms":1724699999334,"provider_endpointing_ms":666,"audio_output_ms":1500}}]}}` | Per-turn metrics snapshot, one per utterance after the turn ends. `components[]` is registry-driven (`kind: "stt" \| "llm" \| "tts" \| "realtime" \| "output" \| "audio_post" \| …`); the field vocabulary is advertised on `session_ready.metric_descriptors`. Speech-to-speech sessions collapse stt/llm/tts into one `kind: "realtime"` component whose extras carry:<br><br>• `ttfb_ms`: end-of-speech anchor → first **audio** delta with samples (server side, pre-chain). Never the first transcript.<br>• `first_text_ms`: anchor → first output-transcript delta (diagnostic: "the model started"; absent on audio-only turns).<br>• `ttfb_anchor_source`: string — `provider_speech_stopped` (OpenAI `input_audio_buffer.speech_stopped`), `input_transcript_proxy` (Gemini: last input-transcript delta, lands 0.45–1.0 s late), `vp_vad` (the worker's uplink end-of-speech observer), `client_speech_end` (a client `timing_report speech_end` that arrived before the response), `text_submit`, or `turn_start` (no anchor — `ttfb_ms` ≈ 0 and must not be read as latency). Advertised with `unit: "raw"`.<br>• `user_speech_end_epoch_ms`: wall clock of the anchor above (only for anchored sources).<br>• `vp_speech_end_epoch_ms`: wall clock at which the worker's own VAD observer saw the uplink go silent (`[realtime_provider.endpoint_observer]`, default on).<br>• `provider_endpointing_ms`: provider anchor − `vp_speech_end_epoch_ms` — how long the provider took to notice the user stopped. The S2S `endpointing_ms`; latency on the critical path, budget 500 ms.<br>• `audio_output_ms`, `input_audio_ms`, `sample_rate`: as before.<br>• Input audio quality of the user speech the turn answers, metered by the same observer and absent when it is off: `input_speech_dbfs` (pooled RMS of the speech frames, dBFS), `input_peak_dbfs`, `input_noise_dbfs` (last ~1 s of non-speech audio; absent before any), `input_snr_db`, `input_clipped_ratio` (samples at or above −0.09 dBFS ÷ speech samples), `input_speech_ratio`, `input_speech_ms`, `input_level_source` (`vp_energy` \| `vp_silero`). Levels are as the server received them, after any browser AGC. Advertised with `unit: "db"` for the level keys.<br><br>All new keys are optional on the wire (absent on older workers / captures). |
| **`playback_quality`** | `{"action":"playback_quality", "utterance_id":1, "underrun_count":2, "total_silence_ms":550, "audio_ms":16120, "heard_ms":16120, "wall_ms":16670, "arrival_wall_ms":15980, "interrupted":false, "annotation_late_count":0, "annotation_late_ms_max":0, "gaps":[{"at_audio_ms":480,"silence_ms":300}]}` | *(Client-to-Server, optional)* Per-turn playback health, sent **once per utterance** — at playback-complete, or from the flush path (barge-in / cancel) with `interrupted: true`, so a starving turn that gets interrupted still reports. `gaps[]` positions each jitter-buffer underrun in the **ideal (gapless) audio timeline**, which lets the server splice the silences back into its stored TTS audio ("as-heard" reconstruction) and surface truthful underrun metrics in Session Detail. Skipped for empty turns.<br><br>• `arrival_wall_ms`: wall ms from the first to the last audio frame *received* by the client (`performance.now()` deltas, independent of playback). `arrival_wall_ms / audio_ms` is the client-side **arrival realtime factor**: > 1 means the server delivered audio slower than it plays, so any underruns are *starvation* (server output) rather than *jitter* (network / prebuffer).<br>• `interrupted`: `true` when the frame was emitted from the flush path instead of the natural end of playback.<br>• `annotation_late_count` / `annotation_late_ms_max`: `audio_annotation` frames (§D) that arrived after the audio they describe had already started playing (`late = playbackClock.nowMs − start_ms > 0`) — count and max lateness in ms; `0` without an annotator.<br><br>All four are optional on the wire (the server defaults them; absent on older clients and captures). |
| **`audio_meta`** | `{"action":"audio_meta", "fragment_idx":0, "is_final":false, "server_timestamp_ms":177..., "text":"Sure!", "utterance_id":1}` | A sideband message that precedes a block of TTS audio to declare exactly what text is about to be spoken.<br><br>• `fragment_idx`: Sequence number of the sentence block. <br>• `is_final`: Flag indicating if this is the last fragment of the utterance.<br>• `server_timestamp_ms`: Network synchronization timestamp.<br>• `total_fragment_duration_ms`: Optional field revealing exact duration of the audio in milliseconds before streaming.<br>• `text`: The string of text that will be spoken next.<br>• `utterance_id`: Unique tracking integer for the current back-and-forth conversational turn. Critical for dropping stale audio if a user barges in and interrupts the agent. |

### B. Audio Messages (`kind: "audio"`)
Streams the actual raw PCM audio frames generated by the TTS engine, sometimes bundled with UI hints.

| Payload Example | Purpose of Parameters |
| :--- | :--- |
| **Standard Voice Packet**<br>`{"timestampMs":177..., "durationMs":185.7, "fragment_idx":0, "text":"Sure!", "utterance_id":1, "caption": {"text": "Sure!", "word_timings": null}}` | • *Implicit raw Blob/buffer*: Contains the raw `pcm_i16_le` audio samples to be enqueued in the Web Audio context.<br>• `durationMs` / `timestampMs`: Mathematical timings of the chunk size. <br>• `caption`: Attached strictly to the *first* audio chunk of a new fragment. Provides the exact text that the `useWordReveal` RAF loop mounts on the screen to progressively reveal words as the audio plays.<br>• `utteranceId`: Matches the `audio_meta` turn tracker to prevent text bleed. |
| **End-of-Response Packet**<br>`{"durationMs":0, "full_text":"Sure! Here’s...", "utterance_id":1}` | • A zero-sample packet injected to cleanly close the audio queue.<br>• `full_text`: Used by the playback service to definitively mark the `utterance_id` as finished and trigger the `onPlaybackEnd` local hook. |

### C. Text Messages (`kind: "text"`)
Canonical text records guaranteed to represent the entire user input or the complete LLM output.

| Payload Example | Purpose of Parameters |
| :--- | :--- |
| `{"messageId":"stt-1", "role":"user"}`<br>*Raw Value*: `"Can you tell me a story?"` | Represents the Speech-to-Text inference of what the user just said. <br><br>• `messageId`: Prefixed with `stt-` to indicate human origin.<br>• `role`: Always assigned `"user"`. |
| `{"messageId":"llm-1", "role":"agent"}`<br>*Raw Value*: `"Sure! Here’s a short story..."` | The unified, post-stitched payload of everything the LLM generated during the turn.<br><br>• `messageId`: Prefixed with `llm-` and suffixed with the integer `utterance_id` (e.g. `llm-1`).<br>• `role`: Always assigned `"agent"`.<br><br>*Note*: While `caption` metadata updates the UI visually mid-sentence, this complete string replaces the chunks via `processFinalText` to correct any STT misalignments or spacing issues once the turn securely streamed sequence concludes. |

### D. Audio Annotation Messages (`type: "audio_annotation"`)
Server-to-client, data plane (top-level type, like `audio_meta`). Emitted only when the worker runs an **Audio Annotator** — an `[[audio_post_processor]]` with `mode = "annotate"` (see the [Audio Annotators guide](../guides/audio-annotators.md)). Carries metadata *derived from* a span of one utterance's audio — blendshapes, visemes, phonemes, emotion, levels — produced off the audio path. Without an annotator configured, no `audio_annotation` frame is ever sent.

```json
{ "type": "audio_annotation",
  "payload": { "utterance_id": 12, "start_ms": 160, "duration_ms": 160, "kind": "blendshapes",
               "extras": { "fps": 30, "frames": [[...52 floats...], ...], "names": [...] },
               "server_timestamp_ms": 1724700000000 } }
```

| Field | Type | Purpose |
| :--- | :--- | :--- |
| `utterance_id` | integer | The utterance the span belongs to — the same id carried by `audio_meta` / `audio` packets. Use it to drop annotations for a flushed (barged-in) utterance. |
| `start_ms` | integer | Offset of the span's first sample from the utterance's **first sample**, in milliseconds **of audio** (not wall time). Same accounting as the client's enqueued-duration counter, so offsets match on both ends. |
| `duration_ms` | integer | Length of the annotated span in ms of audio. Successive annotations for one utterance tile the audio (`Σ duration_ms ≈ audio duration ± one batch`). |
| `kind` | string | Free-form discriminator chosen by the annotator service (e.g. `"blendshapes"`, `"visemes"`, `"phonemes"`). Clients pick the kinds they understand and ignore the rest. |
| `extras` | object | Opaque JSON object forwarded **verbatim** from the service; the server never parses it. Shape is a contract between the service and the client renderer. |
| `server_timestamp_ms` | integer | Server wall clock when the annotation was emitted. Optional (clients treat it as absent when missing); do not use it to place data — use `start_ms`. |

Ordering guarantees:

* **Same socket as audio, ordered.** Annotations travel on the session's message stream with all other frames; there is no second channel to reconcile.
* **May precede or follow the audio it describes.** The annotator is asynchronous: an annotation for `[start_ms, start_ms + duration_ms)` typically lands shortly *after* the audio bytes for that span (server batches audio to `batch_ms`, default 160 ms, then calls the service) but can also land *before* them when the pacer is still holding the audio. Place data by `utterance_id` + `start_ms` against the playback clock, never by arrival order or chunk boundaries.
* **Never delayed by the pacer.** Non-audio messages pass the `AudioPacer` immediately, so an annotation is never queued behind paced audio — and audio is never held back for annotations (the tee is non-blocking; a slow or dead annotator only means *missing* annotations, never slower speech).
* **Dropped for flushed utterances.** After a barge-in (`clear_audio` / `text_truncated`), the server cancels pending work for that utterance and discards late results; a client must still drop any `audio_annotation` whose `utterance_id` it has already flushed, exactly as it drops stale audio.
* **Gaps are normal.** A span whose batch timed out, was dropped under back-pressure, or hit an open circuit breaker simply has no annotation. Clients show a neutral state (e.g. a resting face) for spans without data.

The SDK maps this frame to the `audioAnnotation` event (`AudioAnnotation { utteranceId, startMs, durationMs, kind, extras, serverTimestampMs? }`, snake→camel like `audio_meta`) and drops annotations for utterances it has already flushed. `audio_meta.extras` is unchanged and remains the channel for **inline** post-processors.

## 3. Server-Side Audio Pacing & Head-of-Line (HOL) Blocking

The backend implements explicit pacing (AudioPacer via PacerConfig in src/transport/pacer.rs) to mitigate **Head-of-Line (HOL) blocking** over the single, synchronized WebSocket TCP connection.

### The Problem
If the backend generated 10 seconds of audio and pushed it all into the WebSocket simultaneously, it would fill the OS network buffers. If a user interjected (barge-in) 1 second later, the server's clear_audio control message would be stuck in the queue behind the remaining 9 seconds of audio data, causing severe UI lag where the agent continues speaking despite being interrupted.

### The Pacing Strategy
Instead of sending data as fast as the LLM/TTS pipeline produces it, the server artificially throttles the outbound socket:
1. **Burst Window:** The first **500ms** of audio in a turn is flashed to the socket immediately. This minimizes Time-To-First-Byte (TTFB) and guarantees zero perceivable start latency.
2. **Paced Window:** All subsequent audio is dripped into the socket at exactly **1.5x real-time speed**. 

By holding future audio in server memory instead of the TCP buffer, the socket remains mostly clear. When an interruption occurs, the server instantly drops its internal buffer and sends the clear_audio packet, which arrives at the frontend within milliseconds.

## 3. Server-Side Audio Pacing & Chunking

The backend implements explicit pacing (AudioPacer via PacerConfig in src/transport/pacer.rs) to mitigate **Head-of-Line (HOL) blocking** over the single, synchronized WebSocket TCP connection.

### The Problem
If the backend generated 10 seconds of audio and pushed it all into the WebSocket simultaneously, it would fill the OS network buffers. If a user interjected (barge-in) 1 second later, the server's clear_audio control message would be stuck in the queue behind the remaining 9 seconds of audio data, causing severe UI lag where the agent continues speaking despite being interrupted.

### The Pacing Strategy
Instead of sending data as fast as the LLM/TTS pipeline produces it, the server artificially throttles the outbound socket:
1. **Burst Window:** The first **500ms** of audio in a turn is flashed to the socket immediately. This minimizes Time-To-First-Byte (TTFB) and guarantees zero perceivable start latency.
2. **Paced Window:** All subsequent audio is dripped into the socket at exactly **1.5x real-time speed**. 

By holding future audio in server memory instead of the TCP buffer, the socket remains mostly clear. When an interruption occurs, the server instantly drops its internal buffer and sends the clear_audio packet, which arrives at the frontend within milliseconds.

### Fixed Output Chunking (Avatars & Animation)
By default, the pipeline streams TTS audio fragments exactly as the engine produces them (often one massive audio frame per sentence). While highly efficient, this can make external synchronization difficult.

For UI use cases incorporating 3D Avatars or Lip-Sync where continuous, highly predictable audio streams are needed to sequence visemes and blendshapes, you can force the server to break audio down into granular, fixed-time chunks. In your initial WebSocket payload, specify:

```json
// Inside session connection handshake
"options": {
  "output_chunking_enabled": true,
  "output_chunk_frame_ms": 50
}
```
With `output_chunking_enabled: true` and `output_chunk_frame_ms: 50`, the orchestrator acts as a metronome. If it synthesizes 2 seconds of audio at once, it strictly slices it into forty `50ms` binary frames and streams them continuously. This guarantees absolute frame-level precision for animation keyframes on the frontend.

---

## 4. Strict Envelope Routing: Data Plane vs. Control Plane

To provide a clean, predictable protocol for custom UI integrations, the WebSocket communication strictly separates low-frequency lifecycle events from high-frequency streaming primitives.

### A. The Control Plane (	ype: "control")
Used **only** for slow-moving, session-level lifecycle states. These events do not carry real-time streaming data.
*   **Examples:** session_ready, state_changed, error, closing, 	urn_metrics.
*   **Structure:** Has a nested payload identifying the specific event:
`json
{
  "type": "control",
  "payload": {
    "action": "state_changed",
    "state": "speaking"
  }
}
`

### B. The Data Plane (Top-Level Types)
High-frequency streaming primitives are emitted as **top-level types** so the frontend router can dispatch them instantly without deep inspection.
*   **Examples:** udio (binary), 	ext, udio_meta.
*   **Structure:** No nested ction routing. The 	ype defines the envelope.
`json
{
  "type": "audio_meta",
  "utterance_id": 1,
  "text": "Hello!",
  "duration_ms": 300,
  "word_timings": []
}
`
**Important:** udio_meta must never be tunneled inside a control message. It acts as the immediate data-plane header for the raw binary PCM frames that follow it.

---

## 5. Wire Data Model vs. Client Data Model

To prevent redundancy and property contamination in client-side state:

1.  **Strict Wire Casing (snake_case):** All JSON payloads sent over the WebSocket strictly use snake_case keys (e.g., utterance_id, duration_ms, word_timings). This is the definitive network schema.
2.  **Explicit Client Mapping (camelCase):** Client adapters MUST NOT use greedy spread operators (...raw) to merge raw wire data into their internal state. Instead, clients should explicitly pluck snake_case keys from the wire and construct pristine camelCase objects (e.g., mapping utterance_id to utteranceId).

This guarantees that clients traversing their internal state will only ever see clean camelCase properties, eliminating duplicate tracking keys.

---

## 6. Common Audio Playback Pitfalls

When consuming binary audio packets on the client, the server sets specific flags inside the underlying binary protocol envelope to indicate sentence and response completion states. It's critical to interpret these flags correctly:

*   `FLAG_END_OF_UTTERANCE` (Bit 0): Indicates the conclusion of the AI's current generated utterance. Clients **MUST** bind their local fallback finalized state, playback-end events, and UI conversation bubble closures to this exact flag.
*   `FLAG_END_OF_RESPONSE` (Bit 1): Reserved for multi-turn/complex completions. It generally defaults to `false`. **DO NOT** bind your client's playback finalizing logic strictly to this flag, otherwise the UI may never realize the server has legitimately finished speaking a normal sentence.

