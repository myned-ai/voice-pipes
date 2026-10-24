# Throughput observability

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-08

## Context

A worker emitted audio at 0.6× playback speed for a minute; the user heard
speech breaking up. Every surface said healthy: the verdict blamed the
model, the real-time factor was structurally null for speech-to-speech
turns, the worker log warned only on failure, and every playground metric
was first-byte oriented. vp measured latency and failures; this was a
throughput problem with no failures.

## Decision

Four metrics, one component, three anomaly rules and a verdict that can name
a post-processor:

- an `output` component on every turn with `emit_rtf` and `source_rtf`
  (wall time ÷ audio duration, above 1.0 means audio leaves slower than it
  plays) and `chain_overhead_ms`;
- one `audio_post:<name>` component per inline processor with
  `budget_ratio`, p50 and max latency and timeout count;
- the client's `playback_quality` gains `arrival_wall_ms`, `interrupted`
  and annotation-lateness fields, so arrival real-time factor separates
  server starvation from network jitter;
- a worker warning when a processor cannot keep up, and health rules
  (`output.emit_rtf_high`, budget and lateness rules) feeding the verdict.

## Consequences

- "Slower than real time" is visible and attributable to the model, the
  post-audio chain or the network.
- Descriptors carry the budgets, so the rules need no per-processor code.
- Batching on the uplink was measured and rejected as a lever.

## Where in code

`voice-pipes/src/pipeline/metrics.rs` (output and post-processor
descriptors), `src/pipeline/post_audio.rs` (per-processor timing and
warning), `voice-pipes-admin/backend/app/report/` (anomalies and verdict).
Glossary: [metrics](../../reference/metrics-glossary.md).
