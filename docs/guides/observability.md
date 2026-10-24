# Observability

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs

When a reply feels slow, you should be able to see which stage spent the
milliseconds and whether the client, the network or a model is to blame.
Voice Pipes records four things for every session: per-turn metrics, an
event stream, the audio of both sides, and what the client actually played.
This page is where each lives and how to read it. The reasoning behind the
metric design is in [Per-turn metrics](../explanation/per-turn-metrics.md);
the field-level standard is [VPO metrics](../reference/vpo-metrics.md).

## Turn on the audit trail

```toml
[pipeline.audit]
enabled = true
captures_dir = "captures"
```

or `--captures-dir captures` on the command line. Each session then writes:

```
captures/<session_id>/
  metadata.json            session summary, providers, negotiated rates, per-turn metrics and descriptors
  pipeline_events.jsonl    one PipelineEvent per line (see the events reference)
  events.jsonl             trace events with timestamps
  NNN_mic_raw.wav          microphone audio as received (48 kHz by default)
  NNN_mic_16k.wav          the same, resampled to what STT consumed
  NNN_tts_frag_MMM.wav     each synthesised fragment of the reply
  NNN_tts_frag_MMM.txt     the text of that fragment
  session_audio.wav        both sides on one timeline (opt-in; see below)
```

`NNN` is the utterance id. The admin console's session view is a renderer
over exactly these files, and its "as-heard" playback splices the client's
reported underrun gaps back into the TTS audio.

`metadata.json` is written before anything else, so a directory is
recognisable as a session from its first file. Every artefact is written to
a temporary sibling and renamed into place, so a reader never sees a partial
file.

**What this records, and for how long.** A capture is the raw audio of both
sides of a conversation plus its transcripts, on the worker's disk,
unencrypted. Treat `captures/` as personal data. Recording is off unless a
config turns it on: the launcher's default profile does (it asks during the
wizard), the other shipped profiles do not. Each worker deletes session
directories whose newest file is older than `retention_days` (default 7)
when it starts and every six hours after that; set `retention_days = 0` to
keep everything, and set a shorter value or turn recording off before
handling anyone else's calls. The sweep treats a directory holding
`metadata.json`, an events log or a mic recording as a session; it never
follows symbolic links.

**Budgets.** One session may write at most `max_session_bytes` (default
512 MiB); past that, audio artefacts are dropped with one warning while
transcripts and events keep being recorded. The mic audio held for the turn
in progress is capped at 120 s (the newest audio is kept), so a turn that
never finalises cannot grow memory without bound. A writer that fails ten
times in a row (disk full, permissions) disables itself for the rest of the
session with one warning. The `llm_prompt` trace event carries the message
count and a hash of the prompt; set `full_prompts = true` to record every
prompt in full.

## Continuous session audio

The per-utterance files above answer "what did the recogniser hear". They
cannot answer anything about timing *between* the two speakers: they are taken
after the VAD, so the silence either side of a turn is gone, and they are
stored one file per utterance per side with nothing recording where those sat
relative to each other.

Turn-taking models read exactly that removed structure, and so does any
evaluation of interruption handling — you cannot score whether an agent
recovered well from being cut off using audio that has had the overlap cut out
of it. Turn this on when you need it:

```toml
[pipeline.audit]
enabled = true
record_session_audio = true
session_audio_max_seconds = 900   # about 3.8 MB per minute
```

That writes one `session_audio.wav` per session: **channel 0 the user** (before
the VAD gates it), **channel 1 the agent** (after the inline post-audio chain,
so it is what the client was actually sent), 16 kHz, on a single timeline. Both
engines produce it, so a speech-to-speech session yields the same artefact a
cascade one does, and `live` records too when given `--save-input`:

```sh
voice-pipes --config examples/live/english-turn-corpus.toml live --save-input captures
```

which is usually the quickest way to collect a few conversations to score.

**What the timeline is worth.** Chunks are placed at their arrival offset with
gaps zero-filled. Arrival is not capture time — the agent's audio is teed when
it is *sent*, not when the client plays it — so the two channels are aligned to
transport jitter, not to the sample. That is the right resolution for
turn-taking, whose unit is hundreds of milliseconds, and the wrong one for
anything that needs sample alignment between the channels. If the writer falls
behind, a dropped chunk leaves a silent gap rather than shifting everything
after it, so an overloaded worker degrades the recording without lying about
when things happened.

**It is off by default, and it should stay off unless you are using it.**
Unlike the per-utterance artefacts, this records continuously: the silence, the
background, and audio no turn ever committed. Auditing a session and recording
it wall to wall are different decisions. Everything in the retention and budget
notes above applies to it, and `session_audio_max_seconds` caps one session;
past the cap audio is dropped and the recording is marked truncated in the log.

**The browser alters the microphone before you see it.** Chrome and Edge apply
echo cancellation, noise suppression and automatic gain to a `getUserMedia`
stream, and they are aggressive enough to gate quiet or distant speech to
near-silence -- which reaches the recogniser as a pause rather than as words,
and reaches this recording as silence. They are on by default because for an
ordinary agent they help. When the audio is the subject, pass them off:
`startMic(rate, { noiseSuppression: false, autoGainControl: false,
echoCancellation: false })` on the session core. A corpus recorded through
noise suppression is a corpus of what the browser thought was worth keeping.

A use for it: picking the context window for the
[`vap` turn detector](turn-detection-and-interruptions.md#vap-anticipatory-experimental),
which needs real two-sided conversation to choose on evidence rather than on a
default.

## Per-turn metrics

After every turn the worker emits a `turn_metrics` snapshot: to the client
as a control message, to the event stream as `turn_metrics_ready`, and into
`metadata.json`. It is an ordered list of **component spans**, one per stage
that ran, each with start and end times relative to the turn, the provider
name, a cold-start flag, and an `extras` map of provider-specific fields.

The fields every consumer looks at first:

| Component | Field | Meaning |
|---|---|---|
| `stt` | `endpointing_ms` | End of speech to the committed transcript; the turn-detection cost |
| `stt` | `processing_ms` | STT inference wall time |
| `llm` | `ttft_ms` | Transcript in to first token out |
| `llm` | `generation_ms` | First to last token |
| `tts` | `ttfb_ms` | Synthesis request to first audio byte |
| `tts` | `rtf` | Synthesis wall time ÷ audio duration; below 1.0 keeps up with playback |
| `output` | `emit_rtf` | Wall time to emit the audio ÷ its duration; above 1.0 means audio leaves the worker slower than it plays |
| `audio_post:<name>` | `budget_ratio` | An inline post-processor's share of its per-chunk budget |
| `realtime` | `ttfb_ms`, `ttfb_anchor_source`, `provider_endpointing_ms` | Speech-to-speech: first *audio* byte, what the timer was anchored on, and how late the provider noticed the user stopped |
| `client_tools` | | Present when client-side tools ran; post-tool latency is re-anchored so the tool is not blamed on TTS |

Components describe their own fields with **descriptors** (label, unit,
format, budget, direction), sent to the client in `session_ready` and stored
in `metadata.json`, so a new component shows up in every table without
frontend changes. The [metrics glossary](../reference/metrics-glossary.md)
lists the canonical keys.

The one number everyone asks for, end-to-end perceived latency, is measured
from the client's own end-of-speech timestamp to the first audio it received,
because only the client knows both.

## What the client reports

Server-side timers cannot see playback. Clients send:

- `timing_report` events (`speech_start`, `speech_end`, `audio_receive`,
  `playback_start`, `playback_end`) stamped with their wall clock; the
  ping/pong clock sync makes them comparable to server timestamps. A
  detector cannot tell a pause from the end of a turn, so `speech_end` may
  arrive several times in one turn; every consumer anchors on the last one
  before the reply's first audio, which is when the wait really began;
- `playback_report` after a barge-in with the exact offset played;
- `playback_quality` once per turn: underrun count, total silence, the gap
  positions in the ideal audio timeline, arrival wall time (so arrival
  real-time factor separates server starvation from network jitter), and
  whether the turn was interrupted.

The session view uses these to show playback health next to server latency.

Clients also send `net_report` every couple of seconds (playback buffer
depth, underruns, audio received versus wall time, uplink backlog). Together
with the ping RTT and the server's own socket-write latency it feeds the
per-session link view; when `[adaptive_quality]` is enabled the server
answers with a `quality_update` whenever the fidelity tier changes, carrying
the current ping bucket and the new tier.

## The event stream

`PipelineEvent`s are the backbone of everything above: utterance received,
STT completed, LLM first token and completed, TTS first audio and completed,
turn completed, interrupted, barge-in resolved, history modified, client
timing, caption emitted, pipeline error, turn metrics ready. They are
written to `pipeline_events.jsonl` and broadcast in-process for webhooks and
aggregators. Each session records its own events only, one timestamped line
per event. See the [events reference](../reference/events.md).

## Health

Every provider, the VAD, the output stage and each post-processor is a
component with rolling counters, a latency window and a health state
(healthy, degraded, unhealthy). An auto-degrader flips components on rolling
error rate and p99 latency; a failed warm-up at session start marks the
component unhealthy and sends the client a non-fatal warning
(`STT_WARMUP_FAILED` 2001, `LLM_WARMUP_FAILED` 3001, `TTS_WARMUP_FAILED` 4002).

- Worker: `GET /health` on the transport port, `GET /debug/components`.
- Fleet: `GET /api/v1/components`, and the component pills on the admin
  console's dashboard.

## Health verdicts

The admin console's backend runs a deterministic report engine over the
captures: anomaly rules per component (`tts.ttfb_high`, `tts.rtf_high`,
`output.emit_rtf_high`, annotator lateness, playback underruns, …) with
severity bands from the descriptors' budgets, and a verdict that names the
culprit stage. It makes no LLM calls; the same captures replayed produce the
same verdict, which is what its golden tests check.

What the verdict will and will not say:

- **Speed is not the only thing graded.** A turn that commits on a
  transcript with no words in it (`".."`) raises `stt.transcript_empty`,
  severe, however fast every stage was.
- **No evidence is not health.** A session with nothing to measure reads
  `unknown`.
- **A caller who was ignored is not a fast turn.** Speech that reaches the
  recogniser and produces no turn is recorded as an `utterance_dropped` event
  and raised as `stt.utterance_dropped`, severe, when the recogniser had words
  and returned none. A turn the caller waited far longer for than the server
  timed — the shape of a lost first attempt — raises `turn.unanswered_wait`
  (over `unanswered_wait_ms`, 3 s by default); when the wait spans dropped
  utterances the rule says so, severe if the recogniser had words and lost
  them, moderate if it found none. A tool-invocation turn has no spoken
  reply, so its wait is measured from speech end to the tool call's
  dispatch and charged to the model. Sound the VAD forwarded that
  held no words is noted as `vad.non_speech_forwarded` and never counted
  against the session, with one exception: the first such sound after a
  server-side barge-in is the speech that cut the agent off, which the VAD
  confirmed as the caller talking, so it is raised as
  `stt.interruption_unheard` against the recogniser. The client is told of
  every dropped utterance (`utterance_dropped`), and each one's audio is in
  the capture as `NNN_dropped_<reason>_mic_raw.wav` (a second drop under the
  same id as `..._<reason>_2_mic_raw.wav`). The console's session detail
  shows each as a "Heard, not recognised" card with a player, before the
  turn that took its id, so noise the caller made can be told from speech
  the recogniser missed.
- **The cause is named, not the stage it surfaced in.** `endpointing_ms`
  is reported by the recogniser but includes the configured
  `silence_timeout_ms`. When that timeout accounts for the overage the
  finding belongs to **turn detection**; the session's `metadata.json`
  records `turn_detection { mode, silence_timeout_ms }` so the engine can
  tell. A batch recogniser's stability is judged on `endpointing_ms` as
  well — its `processing_ms` grows with how long the caller spoke, which is
  not instability.
- **The headline counts what was felt.** Flags of negligible severity and
  flags off the critical path are listed as diagnostic, not counted as
  problems.
- **Your rules, disclosed.** A pipeline can set its own targets and level
  bands in [`[grading]`](configuration.md#grading-optional-advanced). They
  travel with the session, and a verdict reached under them says so and
  lists each value that differs from the default.

To test a stage outside a conversation, see
[Checking one component at a time](troubleshooting.md#checking-one-component-at-a-time).

## Logs and OpenTelemetry

- `RUST_LOG` filters logs (`voice_pipes=info` by default); `--log-format json`
  for structured output.
- With the `telemetry` feature (default) the worker exports traces over
  OTLP using the exporter's standard environment variables
  (`OTEL_EXPORTER_OTLP_ENDPOINT` and friends). If no collector answers,
  export is skipped and only local logging runs. Every turn carries an
  `otel_trace_id` in its record; clients can pass a W3C `traceparent` in
  `configure` options to join their own traces.
- The orchestrator's `GET /api/v1/metrics` returns latency histograms per
  window; a Prometheus scrape endpoint is on the [roadmap](../roadmap.md).

## Throughput, not just latency

A stream that starts on time and then trickles is invisible to
time-to-first-byte metrics. The `output` component's real-time factors and
each post-processor's budget ratio exist for that case, and the worker logs
a warning when a processor cannot keep up. If playback stutters after the
first words, look there before blaming the network.
