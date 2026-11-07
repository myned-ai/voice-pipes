# Session History Details - Frontend Specification

Status: active
Owner: Admin Backend + Frontend
Last reviewed: 2026-05-11
Source of truth: yes

This document details the exact visual components, metrics, and graphics rendered on the Session History Details page (`SessionDetail.tsx`) within the Admin BFF frontend. 

It serves as the definitive specification for re-implementing or migrating the UI components responsible for visualising complex streaming voice pipeline interactions.

---

## 1. Overview and Layout

The Session Detail page is a chronological list of **Turns**. A turn encapsulates a single interaction cycle: User Speech/Input $\rightarrow$ STT $\rightarrow$ LLM $\rightarrow$ TTS $\rightarrow$ Playback.

Each Turn consists of four primary visual tiers:
1. **Turn Header**: High-level status and critical latency metrics.
2. **Pipeline Timeline**: A visual Gantt chart representing overlapping concurrent processing states.
3. **Conversation View**: Human-readable transcripts, playback controls, and barge-in visualization.
4. **Developer Artifacts**: Raw telemetry and events (Collapsible).

---

## 2. Turn Header & Badges (`TurnCardHeader.tsx`)

The header provides at-a-glance status information and latency metric chips.

* **Turn Badge**: Displays `#{utterance_id}`.
    * *Styles*: Indigo background for `Completed`, Orange background for `Terminated`.
* **Status Label**: E.g., `Turn`, or `Terminated after {stage}`.
* **Metric Chips (Right-aligned)**:
    * **Interruption Alerts**:
        * 🔸 `VAD barge-in`: When `summary.interrupt_trigger === 'server_vad'`.
        * 🔸 `Client cancel`: When `summary.interrupt_trigger === 'client_cancel'`.
        * 🔸 `Playback barge-in`: When server finished sending audio, but user spoke before client finished playing it (`!summary.interrupted && summary.client_interrupted`).
    * **Endpointing**: `VAD {ms}ms` (Wait time for silence detection).
    * **Round Trip Time**: `RTT {ms}ms` (Barge-in round-trip time).
    * **Time to First Token**: `TTFT {ms}ms`.
    * **Time to First Byte**: `TTFB {ms}ms` (First audio byte received).
    * **Total Latency**: `{ms}ms wall-clock`.

---

## 3. Pipeline Timeline (`PipelineTimeline` component)

The timeline is a sophisticated overlapping Gantt chart that visualizes the streaming nature of the pipeline.

### The Gantt Chart
* **X-Axis**: Proportional time scale based on `totalMs`.
* **Grid Lines**: Vertical dashed grid lines drawn proportionally across the container to help eyeball latencies.
* **Overlapping Bars**:
    * **STT Bar (Blue)**: Runs from `t=0` to `sttMs`.
        * *Visual Detail*: If `endpointingMs` is provided, a dashed vertical separator divides the bar into `Speaking` and `Silence (EOU)`.
    * **LLM Bar (Orange)**: Starts *after* the `Speaking` portion of STT. Runs for `llmMs` or `llmGenerationMs`.
        * *Visual Detail*: Contains internal segmented styling for `cache_reset_ms` (Red) and `prompt_ingest_ms` (Blue) if prefilling occurred.
        * *Visual Detail*: Vertical tick marks overlay the generation phase to represent `output_tokens` density.
    * **TTS Bar (Purple)**: Starts overlapping the LLM bar at `ttftMs` (Time to First Token).
        * *Visual Detail*: "1st token" and "1st audio" markers are placed at `ttftMs` and `ttfbMs` on the timeline respectively.

### Legend & Sub-Metrics
Below the Gantt chart, a legend details exact segmented latencies:
* 🔵 `Speaking {ms}`: Total STT time minus endpointing.
* 🔵 `Silence (EOU) {ms}`: Endpointing latency.
* 🟠 `TTFT {ms}`: Time from end of speaking to first LLM token.
* 🟠 `LLM gen {ms}`: Generation phase duration (and `~{t}t` tokens).
* 🟣 `TTS startup {ms}`: Time from first LLM token to first TTS audio byte (`ttfbMs - ttftMs`).
* 🟢 `Response {ms}`: Total wall-clock time (`totalMs`).

---

## 4. Conversation View (`AgentTurnText.tsx`)

This section displays the actual spoken/typed content and provides media playback.

### "YOU" Block (User Input)
* **Icon**: 🎙️ (Mic) if STT, or ⌨️ (Keyboard) if text input.
* **Metadata**: `{sttMs}ms STT` or `text`.
* **Content**: `stt.text` or `user_text`.
* **Media**: HTML5 Audio player for `{utt}_mic_16k.wav` if available.

### "AI" Block (Agent Output)
* **Metadata**: `{total_latency_ms}ms`.
* **Content**: Evaluates `llm_output.text`.
    * **Barge-in / Cut-off text handling (✂️)**: If the turn was interrupted, the text is split using the utterance-level `retained_text` (passed as `retainedText={utt.retained_text}` to `AgentTurnText`; there is no `summary.retained_text`). 
        * The spoken portion is rendered normally.
        * A red scissors icon (✂️) is rendered at the exact cutoff point.
        * The unplayed/unsynthesized portion of the LLM generation is rendered in a faded, strike-through style (`line-through opacity-50`).
* **Media**: HTML5 Audio players for each `{utt}_tts_frag_{n}.wav`.

### Footer & Metadata
* **LLM Prompt Box**: Expandable section showing the exact LLM input if it differs from the transcribed STT text (e.g. system prompts or injected context).
* **TTS Input Inspector**: Expandable violet box rendered when spoken text differs from display text (Markdown stripped, whole-word lexicon substitutions, or inline tags applied/stripped).
    * **Trigger criteria**: Present when `llm_output.spoken_text != llm_output.text`, or `tts_directives` / `stripped_directives` arrays are non-empty.
    * **Label**: `TTS input · {N} directive(s) · {M} lexicon`, plus an amber `· {K} stripped ⚠` badge if directives were dropped.
    * **Expanded inspector**:
        * Displays the exact `spoken_text` sent to the TTS provider.
        * Displays applied directive chips: `<break time="..."/>`, `Kokoro → /ipa/`, or `SQL → sequel`.
        * Displays amber warning rows for stripped directives with provider reasons (e.g. `[whispers] — not in passthrough_tags for provider 'piper'`).
* **Bottom Metrics Bar**: 
    * `{tokens_per_sec} tok/s`
    * `{rtf}x RTF` (Real-Time Factor for TTS audio).
    * `{chars} chars` & `{audioDur}s audio`.
    * Provider Tracking: E.g., `STT: {stt_provider}` | `TTS: {tts_provider}`.

---

## 5. Developer Artifacts & Telemetry

Collapsible cards that dump raw session telemetry for debugging.
* **LLM Timing Breakdown**: Stacked bar chart showing `Cache reset` vs `Prompt ingest` vs `Generation`, alongside token counts.
* **TTS Timing Breakdown**: Displays RTF, Chars, Audio Duration, Sample Rate, and Voice ID.
* **TTS Input & Directive Audit**: Serialized in `{utt:03}_llm_output.json` (and `{utt:03}_tts_frag_{idx:03}_spoken.txt`), recording `spoken_text`, `tts_directives`, and `stripped_directives` for full pronunciation and SSML auditability.
* **Generic JSON Cards**: Renders raw `pipeline_events.jsonl` payloads.

---

## 6. Event Timeline View & Event Cards

The Event Timeline is an alternative view (switchable via the UI) that renders a raw chronological list of all pipeline events captured in events.jsonl, rather than grouping them by turn.

### Event Routing & Rendering (EventCardRenderer)
Events are dynamically routed to specific card components based on their `event_type` or `type` field:
*   `llm_prompt` $\rightarrow$ **LLM Prompt Card**: Displays the messages array with role chips (e.g. `system`, `user`).
*   `llm_response` $\rightarrow$ **LLM Response Card**: Displays the raw completion text.
*   `llm_detail` $\rightarrow$ **LLM Timing Breakdown (LlmTimingCard)**: The rich graphical breakdown card containing `total_ms`, token usage, `first_token_ms`, and segmented bar charts (cache reset vs. prompt ingest vs. generation).
*   `llm_timing` $\rightarrow$ **Generic JSON Card**: A generic orchestrator-level event. It tracks high-level wall-clock `elapsed_ms` and total `tokens`. It is explicitly rendered as raw JSON to avoid visually duplicating the rich `llm_detail` graphical breakdown.
*   `tts_timing`, `tts_detail` $\rightarrow$ **TTS Timing Breakdown (TtsTimingCard)**: Displays TTS synthesis metrics (chars, audio duration, RTF, voice, sample rate).
*   `client_timing` $\rightarrow$ **Client Timing Card**: Custom pill-styled badges tracking playback events (e.g., `playback_start`, `playback_end`, `underrun`) along with the exact `client_timestamp_ms`.
*   *Default Fallback* $\rightarrow$ **Generic JSON Card**: Any unrecognized event types are rendered as a collapsible JSON code block.

### LLM Timing Precedence & Plausibility
When resolving turn-level LLM time-to-first-token (`resolve_turn_ttft`), both the `/turns` API and the session report reader check `llm.ttft_ms` (from the `turn_metrics` snapshot) first, falling back to `llm_detail.first_token_ms` and then `llm_timing.first_token_ms`. A zero `ttft_ms` on a turn where the LLM did real work (`input_tokens > 0`, `output_tokens > 0`, `processing_ms > 0`, or non-empty response) is rejected as a measurement fault (`measurement.zero_with_work`) rather than accepted as `0 ms`.

### Contiguous Telescoping Stage Latencies
Cascade turns emit three engine-measured contiguous post-commit stage latencies on a single `Instant` timeline anchored at `utterance.committed_at`:
* `llm.llm_wait_ms` (`t_commit` $\rightarrow$ `t_first_llm_fragment`)
* `tts.tts_wait_ms` (`t_first_llm_fragment` $\rightarrow$ `t_first_tts_audio`; integer-ms alias `tts.ttfb_ms`)
* `tts.output_wait_ms` (`t_first_tts_audio` $\rightarrow$ `t_first_audio_emitted`)
* `tts.commit_to_first_audio_ms` (`t_commit` $\rightarrow$ `t_first_audio_emitted`)

The transport runner preserves `tts.ttfb_ms` as TTS's own wait and updates `commit_to_first_audio_ms` and `output_wait_ms` when the WebSocket send timestamp is recorded. If `llm_wait_ms + tts_wait_ms + output_wait_ms` disagrees with `commit_to_first_audio_ms` by more than `50 ms` and `5%`, the report emits `measurement.stages_disagree`.

---

## 7. Right-Hand Telemetry Panel

A collapsible side-panel (toggled via `PanelRightOpen` / `PanelRightClose` icon button). Occupies ~1/3 of the viewport width.

### 7.1 Session Performance (Aggregated Metrics)

Computed by the generic `computeSessionMetrics()` utility in `detail/metrics.ts`. This function accepts `Utterance[]` and `SessionEvent[]` arrays and returns an `AggregatedSessionMetrics` object, making it reusable across single-session and multi-session analytics views.

#### Latency Averages
| Metric | Source | Description |
|---|---|---|
| **Avg TTFT** | `summary.ttft_ms` | Mean time from utterance commit to first LLM token. |
| **Avg TTFB** | `summary.ttfb_ms` | Mean time from utterance commit to first audio byte sent to client. |
| **Avg Endpointing** | `summary.endpointing_latency_ms` | Mean VAD silence-wait before speech is committed. |
| **Avg Barge-In RTT** | `summary.barge_in_rtt_ms` | Mean round-trip time for interruption acknowledgement. |

> Only non-terminated turns are included in latency averages.

#### Performance and Usage
| Metric | Source | Description |
|---|---|---|
| **Prompt Tokens** | `llm_detail` events | Total prompt tokens consumed across all turns. |
| **Output Tokens** | `llm_detail` events | Total output tokens generated across all turns. |
| **Avg Tok/s** | `llm_detail` events | Mean tokens-per-second across all LLM invocations. |
| **Avg RTF** | `tts_detail` / `tts_timing` events | Mean Real-Time Factor for TTS synthesis. |
| **Total Audio** | `tts_detail` / `tts_timing` events | Cumulative synthesized audio in seconds. |

#### Interruptions
| Metric | Source | Description |
|---|---|---|
| **Server VAD Barge-Ins** | `summary.interrupt_trigger === 'server_vad'` | Count of turns where the server VAD detected speech during playback. |
| **Client Cancels** | `summary.interrupt_trigger === 'client_cancel'` | Count of explicit client-side cancellation requests. |
| **Playback Barge-Ins** | `summary.client_interrupted and not summary.interrupted` | Turns where the server finished sending audio but the user spoke before the client finished playing. |
| **Terminated Turns** | `summary.terminated` | Turns that failed or were aborted before completion. |

### 7.2 Telemetry Data
Raw telemetry events from `telemetry.jsonl`. Each entry shows `[timestamp_ms]` and the raw JSON payload. Collapsed by default.

### 7.3 Session Metadata
A flat key-value table of the session metadata: the capture's `metadata.json`
overlaid with the orchestrator's session record. The orchestrator's value wins
only where it has one, so a field the orchestrator never received (for
example on a session recovered from captures) still shows the capture's value.
- `id`, `pipeline_name`, `pipeline_config_hash`
- `stt_provider`, `stt_mode`, `tts_provider`, `tts_voice`
- `turn_detection` — `{ mode, silence_timeout_ms }`, how the session decided a
  turn had ended. `silence_timeout_ms` is the floor under `endpointing_ms`: no
  turn can commit sooner than that after the speaker stops. The session report
  uses it to tell a wait that was *configured* from one the recogniser caused,
  and names **turn detection** rather than the STT as the culprit when the
  timeout alone is past the endpointing target. Absent on captures written
  before it was recorded, where the report makes no such claim.
- `input_sample_rate`, `output_sample_rate`
- `status`, `started_at`, `ended_at`
- `worker_id`, `turn_count`, `tags`

### 7.4 Metrics Guide
A reference glossary explaining each metric abbreviation (TTFT, TTFB, RTF, etc.). Collapsed by default.

---

## 8. Data Contract: metrics.ts

The `computeSessionMetrics(utterances, events)` function is **generic and reusable**. It can be called with data from a single session or with concatenated data from multiple sessions for cross-session analytics dashboards.

The function returns an `AggregatedSessionMetrics` object with:
- `completedTurns`, `terminatedTurns`, `totalTurns`
- `avgTtftMs`, `avgTtfbMs`, `avgEndpointingMs`, `avgBargeInRttMs`
- `serverBargeIns`, `clientBargeIns`, `playbackBargeIns`
- `totalPromptTokens`, `totalOutputTokens`, `avgTokensPerSec`
- `totalAudioSeconds`, `avgRtf`

---

## 9. Event → Utterance Matching Contract

This section documents how telemetry events emitted to `events.jsonl` map back to a specific `utterance_id`. It is the source of truth for the matcher used by the BFF (`_read_capture_artifacts`) and any frontend fallback logic.

### 9.1 Event payload identifiers

Audited against capture `voice-pipes/captures/03912606-e758-43a0-9e66-98fe8ccf1732` (5 utterances, mixed barge-ins). The table records what is *actually* present in payloads today.

| Event type      | `utterance_id` in payload? | Other identifiers in payload         | Matching strategy |
|-----------------|----------------------------|---------------------------------------|-------------------|
| `client_timing` | ✅ Yes                     | `event`, `client_timestamp_ms`        | Direct: `payload.utterance_id`. |
| `barge_in_rtt`  | ✅ Yes                     | `rtt_ms`, `last_played_offset_ms`     | Direct: `payload.utterance_id`. |
| `stt_timing`    | ❌ No                      | `chars`, `elapsed_ms`, `endpointing_latency_ms`, `provider` | Sequence + pipeline_events lifecycle. |
| `llm_request`   | ❌ No                      | `messages_count`, `model`, `stream`   | Sequence + pipeline_events lifecycle. |
| `llm_timing`    | ❌ No                      | `elapsed_ms`, `provider`, `tokens`    | Sequence + pipeline_events lifecycle. |
| `llm_detail`    | ❌ No                      | `elapsed_ms`, `first_token_ms`, `model`, `stream`, `usage` | Sequence + pipeline_events lifecycle. |
| `tts_timing`    | ❌ No                      | `audio_duration_s`, `chars`, `elapsed_ms`, `provider`, `rtf`, `sample_rate` | Sequence + pipeline_events lifecycle. |
| `tts_detail`    | ❌ No (same shape as `tts_timing`) | (same as above)              | Sequence + pipeline_events lifecycle. |

### 9.2 Lifecycle source: `pipeline_events.jsonl`

`pipeline_events.jsonl` is the authoritative ordered record of utterance lifecycle markers. Each entry carries `kind` and `utterance_id` (no timestamp). The relevant kinds for matching are:

| `kind`             | Emitted when                                     |
|--------------------|--------------------------------------------------|
| `utterance_received` | New utterance accepted (audio or text input). |
| `llm_started`      | LLM call begins.                                 |
| `llm_first_token`  | First token returned from LLM.                   |
| `llm_completed`    | LLM call finished — guarantees a subsequent `llm_timing`/`llm_detail` event in `events.jsonl`. |
| `tts_started`      | TTS synthesis begins.                            |
| `tts_first_audio`  | First audio chunk produced.                      |
| `tts_completed`    | TTS finished — guarantees a subsequent `tts_timing`/`tts_detail` event in `events.jsonl`. |
| `interrupted`      | Server-side barge-in (server_vad / client_cancel) — may suppress the trailing `llm_completed`/`tts_completed` and the corresponding `*_timing` events. |
| `turn_completed`   | Turn fully resolved.                             |
| `barge_in_resolved`| Server processed a barge-in.                     |

### 9.3 Matching algorithm (canonical)

Given `events: SessionEvent[]` and `pipeline_events: PipelineEvent[]`, build the per-utterance bundle as follows:

1. **Direct keying** — for each event whose payload contains `utterance_id` (`client_timing`, `barge_in_rtt`), assign directly.
2. **Expected-sequence build** — walk `pipeline_events` in order. For each `llm_completed(uid=N)`, push `N` to `expectedLlmSeq`. For each `tts_completed(uid=N)`, push `N` to `expectedTtsSeq`. For each `utterance_received(uid=N)`, push `N` to `expectedSttSeq` (STT timing fires once per utterance ingestion).
3. **Sequence assignment** — walk `events.jsonl` in order. The k-th `llm_timing` or `llm_detail` event maps to `expectedLlmSeq[k]`. Same for TTS and STT.
4. **Fallback for terminated turns** — if `len(events of type T) < len(expectedSeq[T])`, the missing entries correspond to interrupted/terminated utterances that did not emit the trailing telemetry. They must remain unmatched (no synthetic data).
5. **Never** match by raw array index across different event types — the count of `llm_*` and `tts_*` events is independent.

### 9.4 Why index-based pairing is wrong

The legacy frontend implementation in `SessionDetail.tsx` (pre-fix) built three flat arrays `llmTimings`/`ttsTimings`/`sttTimings` from `events.filter(...)` and passed `arr[i]` to the i-th `TurnCard`. This breaks whenever any utterance fails to emit a given event type. Concrete failure on capture `03912606-…`: 5 utterances, 5 `llm_timing`, **only 2** `tts_timing`. Index pairing assigned the second `tts_timing` (which belongs to utterance 4 per `pipeline_events.jsonl`) to turn 1.

### 9.5 Implementation guidance

- The matcher SHOULD live in the BFF (`_read_capture_artifacts`) so each `utterance` object served by `GET /sessions/{id}` already carries normalized `stt_timing`, `llm_timing`, `llm_detail`, `tts_timing`, `tts_detail`, `client_timing`, and `barge_in_rtt` fields. The frontend then renders without heuristics.
- A frontend-only fallback (using the same algorithm) is acceptable as a transitional step but must consume `pipeline_events.jsonl` (or a server-provided per-utterance lifecycle index), not just `events.jsonl`.
- New telemetry event types added to `events.jsonl` SHOULD include `utterance_id` in the payload from day one. Treat the absence of `utterance_id` on `stt_*`/`llm_*`/`tts_*` as legacy debt to be paid down.

