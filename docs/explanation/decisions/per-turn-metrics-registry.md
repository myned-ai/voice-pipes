# Open-schema per-turn metrics

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-05

## Context

The turn-metrics message had one named field per stage (`stt_latency_ms`,
`ttft_ms`, `tts_ttfb_ms`, …). Adding a translator, a filter or a
post-processor meant editing nine files across the wire types, the audit
format, the benchmark runner and the frontend table. Teams stop measuring
when measuring costs that much.

## Decision

A turn snapshot is `{ utterance_id, total_ms, interrupted, components[] }`
where each component slice is `{ component_id, kind, started_at_ms,
finished_at_ms, provider, cold_start, error, extras }`. What the `extras`
mean is declared by a **descriptor** the component returns from
`turn_descriptor()`: field key, label, unit, format, role, budget,
direction, severity bands. Descriptors ride to the client in
`session_ready` and into `metadata.json`, so any consumer renders any
component generically. A `TurnRecorder` gives components a span API:
`begin`, `set_provider`, `extras_typed`, finish on drop.

## Consequences

- Adding a component touches exactly one file: the component.
- The frontend metrics table, the health rules and the benchmark diff all
  read by key and never assume a fixed stage list.
- Consumers must read `rtf` and `rtfx` and similar pairs by key; both may be
  present.

## Where in code

`voice-pipes/src/pipeline/metrics.rs` (`TurnMetricsSnapshot`,
`TurnComponentMetrics`, `ComponentMetricDescriptor`, `TurnRecorder`,
`default_descriptors`), `src/component/mod.rs` (`turn_descriptor`),
`voice-pipes-admin/frontend/src/components/metrics/TurnComponentMetricsTable.tsx`.
The standard the shape follows is [VPO metrics](../../reference/vpo-metrics.md).
