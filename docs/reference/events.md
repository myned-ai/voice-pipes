# Pipeline events

**Status:** active
**Last verified:** 2026-09-21
**Owner:** Docs

`PipelineEvent` is the structured event stream every turn produces. Events
are written one per line to `pipeline_events.jsonl` in a session's capture
directory and broadcast in-process for audit sinks, webhooks and
aggregators. Source of truth: `voice-pipes/src/pipeline/events.rs`; the docs
gate checks every variant appears here.

Serialisation: JSON with a `kind` tag in `snake_case`. Each line in
`pipeline_events.jsonl` also carries a `timestamp`, the worker's wall clock
when the event was recorded, as `{"secs_since_epoch", "nanos_since_epoch"}`
— the same shape `events.jsonl` uses — so the file can be read as a
timeline without joining it to `events.jsonl`.

Every session has an event channel of its own, and its capture holds that
session's events and no others. In-process observers (webhooks, aggregators)
still receive every session's events on the worker-wide channel. Captures
written before 2026-09-21 by a worker that served several sessions at once
can contain other sessions' rows, `turn_metrics_ready` among them; the rows
carry no session id, so those files cannot be separated after the fact.

`utterance_id` starts at `1` for the first spoken turn of a session, of
every kind. Typed-text turns are numbered from `10001`.

| `kind` | Fields | When |
|---|---|---|
| `utterance_received` | `utterance_id`, `text`, `source` | A committed utterance arrived from STT or text input |
| `stt_completed` | `utterance_id`, `elapsed_ms?` | Final transcript available; `elapsed_ms` is measured from the endpointing decision, absent for text input |
| `llm_started` | `utterance_id` | |
| `llm_first_token` | `utterance_id`, `elapsed_ms` | |
| `llm_completed` | `utterance_id`, `text_len`, `elapsed_ms`, `is_partial` | `is_partial` when generation was cut short |
| `tts_started` | `utterance_id` | |
| `tts_first_audio` | `utterance_id`, `elapsed_ms` | |
| `tts_completed` | `utterance_id` | |
| `turn_completed` | `utterance_id`, `total_ms` | Uninterrupted turn |
| `interrupted` | `utterance_id`, `trigger`, `played_ms?` | The user interrupted; `trigger` is `server_vad`, `client_cancel` or `early_barge` |
| `barge_in_resolved` | `utterance_id`, `trigger`, `played_ms?`, `retained_text`, `barge_in_rtt_ms?` | Once per interrupted turn after history truncation; supersedes `interrupted` |
| `history_modified` | `modification { utterance_id, original_text, retained_text, method, played_ms? }` | History truncated; `method` is `sentence_boundary` or `playback_report` |
| `client_timing` | `utterance_id`, `timing_event`, `client_timestamp_ms` | A client `timing_report` (`speech_start`, `speech_end`, `audio_receive`, `playback_start`, `playback_end`) |
| `caption_emitted` | `utterance_id`, `fragment_idx`, `text` | An `audio_meta` caption was sent |
| `utterance_dropped` | `utterance_id`, `reason`, `text`, `had_interim_text`, `audio_ms` | Speech reached the recogniser and no turn came of it. `reason` is `empty_transcript` or `too_short`; `had_interim_text` tells a recogniser that produced words and then took them back (a fault) from one that heard nothing in a sound the VAD mistook for speech |
| `pipeline_error` | `utterance_id?`, `component`, `message` | |
| `turn_metrics_ready` | `utterance_id`, `metrics` | The full per-component `TurnMetricsSnapshot`; the event-stream twin of the wire `turn_metrics` |

## Related types

- `InterruptionTrigger`: `server_vad` (server-detected barge-in),
  `client_cancel` (explicit `cancel` control), `early_barge` (user spoke
  during LLM processing, before any audio).
- `TruncationMethod`: `sentence_boundary` (best effort without a client
  report), `playback_report` (sample-accurate from the client's offset).
- `TurnMetricsSnapshot`: `utterance_id`, `total_ms`, `interrupted`,
  `components[]` with `component_id`, `kind`, `started_at_ms`,
  `finished_at_ms`, `latency_ms`, `provider`, `cold_start`, `error`,
  `extras`. Field semantics in [VPO metrics](vpo-metrics.md); the wire
  shape in the [protocol](../clients/protocol.md).

## Trace events

`events.jsonl` in the same directory carries the lower-level `TraceEvent`
records (timestamps for every hop, provider request and response markers,
client timing) that the admin console's session view uses to draw the
timeline. They are described in the protocol specification's capture
artefacts section.
