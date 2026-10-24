# Component trait and health registry

**Status:** active
**Last verified:** 2026-09-06
**Owner:** Docs
**Decided:** 2026-06

## Context

Providers reported health as a string derived from configuration, so a
worker whose TTS sidecar had died still said "healthy". Retry policy was
ad hoc per call site, and there was no place to record per-stage latency or
error rates that the console could show.

## Decision

Every pipeline stage implements one `Component` trait: a stable id
(`kind:name`), a `ComponentCore` with atomic counters, a bounded latency
window and a read-write health state, an `error_policy()` and an optional
per-turn metric descriptor. A `ComponentRegistry` (an `Arc` of a concurrent
map) holds every component in the worker.

Health is **passive**: an `AutoDegrader` task flips components between
Healthy and Degraded from rolling error rate and p99 latency, and a failed
`warm_up()` at session start marks the component Unhealthy and sends the
client a non-fatal warning. Nothing polls sidecars between sessions.

Streaming errors are classified at the LLM and TTS boundaries; TTS retries
replay synthesis behind a `frame_seen` gate so the client never hears
duplicated audio. STT is deliberately not retried: bidirectional audio cannot
be replayed mid-utterance.

## Consequences

- One `curl /health` reveals a down backend; the console shows one pill per
  component. Heartbeats carry component snapshots to the orchestrator.
- Warm-up surfacing only catches providers that implement a real
  `warm_up()` probe; the rest surface at first use.
- A backend that dies between sessions is not noticed until the next
  warm-up or turn. Active probing and `ErrorPolicy::Fallback` are deferred.

## Where in code

`voice-pipes/src/component/mod.rs` (trait, core, registry, degrader,
`classify_streaming_error`), `src/api/worker_debug.rs` and
`src/transport/serve/router.rs` (`/debug/components`, `/health`),
`src/transport/error.rs` (warm-up error codes and backend hints).
